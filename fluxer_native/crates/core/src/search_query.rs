// SPDX-License-Identifier: AGPL-3.0-or-later
//! SearchQueryParser.ts + SearchUtils.toApiParams. Unknown/invalid recognized
//! filters are consumed exactly as in the existing client, never sent as IDs.
use crate::{ChannelType, ClientState, Snowflake};
use chrono::{
    DateTime, Datelike, Days, Duration, FixedOffset, Local, Months, NaiveDate, NaiveDateTime,
    TimeZone, Timelike,
};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

const EPOCH: i64 = 1_420_070_400_000;
const KEYS: &[&str] = &[
    "from",
    "mentions",
    "in",
    "before",
    "during",
    "on",
    "after",
    "has",
    "pinned",
    "author-type",
    "sort",
    "order",
    "mature",
    "embed-type",
    "embed-provider",
    "link",
    "filename",
    "ext",
    "last",
    "beforeid",
    "afterid",
    "any",
    "scope",
];
const QUOTES: &[char] = &[
    '"', '\u{201c}', '\u{201d}', '\u{201f}', '\u{2033}', '\u{ab}', '\u{bb}',
];
const SCOPES: &[&str] = &[
    "current",
    "all_dms",
    "open_dms",
    "all",
    "open_dms_and_all_guilds",
    "all_guilds",
];

#[derive(Clone, Debug, Default)]
pub struct SearchHints {
    pub users_by_tag: BTreeMap<String, String>,
    pub channels_by_name: BTreeMap<String, String>,
}
#[derive(Clone, Debug, Default)]
pub struct SearchParseOptions {
    pub guild_id: Option<Snowflake>,
    pub unique_usernames: bool,
    pub hints: SearchHints,
}
#[derive(Clone, Debug, PartialEq)]
pub struct ParsedSearchQuery {
    pub params: Value,
    pub has_query: bool,
}
pub fn parse_search_query(query: &str, state: &ClientState) -> ParsedSearchQuery {
    let options = SearchParseOptions {
        guild_id: state
            .selected_channel
            .and_then(|id| state.channels.get(&id))
            .and_then(|c| c.guild_id),
        ..Default::default()
    };
    parse_search_query_with_options(query, state, &options)
}
pub fn parse_search_query_with_options(
    query: &str,
    state: &ClientState,
    options: &SearchParseOptions,
) -> ParsedSearchQuery {
    let clock = Clock {
        now: Local::now().fixed_offset(),
        local_zone: true,
        week_start: system_week_start(),
    };
    parse_at(query, state, options, &clock)
}

struct Token {
    key: String,
    value: String,
    exclude: bool,
}
fn quote(c: char) -> bool {
    QUOTES.contains(&c)
}
fn key_at(chars: &[char], start: usize) -> Option<(String, bool, usize)> {
    let exclude = chars.get(start) == Some(&'-');
    let first = start + usize::from(exclude);
    let end = chars[first..].iter().position(|c| *c == ':' || *c == ' ')? + first;
    if chars[end] != ':' || end == first {
        return None;
    }
    let key = chars[first..end].iter().collect::<String>().to_lowercase();
    KEYS.contains(&key.as_str())
        .then_some((key, exclude, end + 1))
}
fn tokenize(query: &str) -> (Vec<Token>, String, Vec<String>) {
    let chars = query.chars().collect::<Vec<_>>();
    let mut consumed = vec![false; chars.len()];
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == ' ' {
            index += 1;
            continue;
        }
        let Some((key, exclude, start)) = key_at(&chars, index) else {
            index += 1;
            continue;
        };
        let mut value_start = start;
        while chars.get(value_start) == Some(&' ') {
            value_start += 1;
        }
        if value_start == chars.len()
            || (value_start > start && key_at(&chars, value_start).is_some())
        {
            consumed[index..start].fill(true);
            index = start;
            continue;
        }
        let mut end = value_start;
        let mut quoted = false;
        let mut escaped = false;
        while end < chars.len() {
            let ch = chars[end];
            if escaped {
                escaped = false;
                end += 1;
                continue;
            }
            if ch == '\\' {
                escaped = true;
                end += 1;
                continue;
            }
            if quote(ch) {
                quoted = !quoted;
                end += 1;
                continue;
            }
            if !quoted && ch == ' ' {
                break;
            }
            end += 1;
        }
        tokens.push(Token {
            key,
            value: chars[value_start..end].iter().collect(),
            exclude,
        });
        consumed[index..end].fill(true);
        index = end;
    }
    let rest = chars
        .into_iter()
        .zip(consumed)
        .filter_map(|(ch, used)| (!used).then_some(ch))
        .collect::<Vec<_>>();
    let mut content = String::new();
    let mut phrases = Vec::new();
    let mut index = 0;
    while index < rest.len() {
        if !quote(rest[index]) {
            content.push(rest[index]);
            index += 1;
            continue;
        }
        index += 1;
        let mut phrase = String::new();
        let mut escaped = false;
        while index < rest.len() {
            let ch = rest[index];
            index += 1;
            if escaped {
                phrase.push(ch);
                escaped = false;
                continue;
            }
            if ch == '\\' {
                escaped = true;
                continue;
            }
            if quote(ch) {
                break;
            }
            phrase.push(ch);
        }
        let phrase = js_trim(&phrase);
        if !phrase.is_empty() {
            phrases.push(phrase.into());
        }
    }
    let content = content
        .split(js_space)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    (tokens, content, phrases)
}
fn js_space(ch: char) -> bool {
    matches!(ch as u32,0x0009..=0x000d|0x0020|0x00a0|0x1680|0x2000..=0x200a|0x2028|0x2029|0x202f|0x205f|0x3000|0xfeff)
}
fn js_trim(value: &str) -> &str {
    value.trim_matches(js_space)
}
fn csv(input: &str) -> Vec<String> {
    let chars = input.chars().collect::<Vec<_>>();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        while i < chars.len() && (js_space(chars[i]) || chars[i] == ',') {
            i += 1;
        }
        if i == chars.len() {
            break;
        }
        let quoted = quote(chars[i]);
        if quoted {
            i += 1;
        }
        let mut value = String::new();
        let mut escaped = false;
        while i < chars.len() {
            let ch = chars[i];
            if escaped {
                value.push(ch);
                escaped = false;
                i += 1;
                continue;
            }
            if ch == '\\' {
                escaped = true;
                i += 1;
                continue;
            }
            if quoted && quote(ch) {
                i += 1;
                break;
            }
            if !quoted && ch == ',' {
                break;
            }
            value.push(ch);
            i += 1;
        }
        let trimmed = js_trim(&value);
        if !trimmed.is_empty() {
            out.push(trimmed.into());
        }
        while i < chars.len() && chars[i] != ',' {
            i += 1;
        }
        if i < chars.len() {
            i += 1;
        }
    }
    out
}
fn add(params: &mut Map<String, Value>, key: &str, value: String) {
    let values = params
        .entry(key)
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .expect("array filter");
    let value = Value::String(value);
    if !values.contains(&value) {
        values.push(value);
    }
}
fn probably_snowflake(value: &str) -> bool {
    // JS BigInt accepts a leading + and radix prefixes; preserve its original
    // string rather than rounding IDs through floating point.
    let raw = value.strip_prefix('+').unwrap_or(value);
    let (digits, radix) = if let Some(v) = raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X"))
    {
        (v, 16)
    } else if let Some(v) = raw.strip_prefix("0o").or_else(|| raw.strip_prefix("0O")) {
        (v, 8)
    } else if let Some(v) = raw.strip_prefix("0b").or_else(|| raw.strip_prefix("0B")) {
        (v, 2)
    } else {
        (raw, 10)
    };
    !(value.starts_with('+') && radix != 10)
        && !digits.is_empty()
        && digits.chars().all(|ch| ch.is_digit(radix))
        && digits.chars().any(|ch| ch != '0')
}
fn resolve_user(value: &str, state: &ClientState, options: &SearchParseOptions) -> Option<String> {
    let value = js_trim(value);
    if probably_snowflake(value) {
        return Some(value.into());
    }
    if value.eq_ignore_ascii_case("@me") {
        return state.current_user.as_ref().map(|u| u.id.to_string());
    }
    if let Some(id) = options.hints.users_by_tag.get(value) {
        return Some(id.clone());
    }
    let bare = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
    let valid_tag = value.rsplit_once('#').is_some_and(|(name, tag)| {
        bare(name) && tag.len() == 4 && tag.bytes().all(|b| b.is_ascii_digit())
    });
    if !valid_tag && !(options.unique_usernames && bare(value)) {
        return None;
    }
    state
        .users
        .values()
        .chain(state.current_user.iter())
        .find(|user| {
            format!("{}#{}", user.username, user.discriminator) == value
                || (options.unique_usernames && user.username.eq_ignore_ascii_case(value))
        })
        .map(|u| u.id.to_string())
}
fn resolve_channel(
    value: &str,
    state: &ClientState,
    options: &SearchParseOptions,
) -> Option<String> {
    if let Some(id) = options.hints.channels_by_name.get(value) {
        return Some(id.clone());
    }
    let candidates = state
        .channels
        .values()
        .filter(|channel| match options.guild_id {
            Some(guild) => channel.guild_id == Some(guild),
            None => channel.kind.is_private(),
        })
        .map(|channel| {
            let name = if channel.kind == ChannelType::Dm {
                channel
                    .recipients
                    .iter()
                    .find(|recipient| {
                        state
                            .current_user
                            .as_ref()
                            .is_none_or(|me| recipient.id != me.id)
                    })
                    .map(|user| {
                        if options.unique_usernames {
                            user.username.clone()
                        } else {
                            format!("{}#{}", user.username, user.discriminator)
                        }
                    })
                    .unwrap_or_default()
            } else {
                channel.name.trim().into()
            };
            (channel.id, name.to_lowercase())
        })
        .collect::<Vec<_>>();
    let value = value.to_lowercase();
    candidates
        .iter()
        .find(|(_, name)| *name == value)
        .or_else(|| candidates.iter().find(|(_, name)| name.contains(&value)))
        .map(|(id, _)| id.to_string())
}
fn parse_at(
    query: &str,
    state: &ClientState,
    options: &SearchParseOptions,
    clock: &Clock,
) -> ParsedSearchQuery {
    let (tokens, content, phrases) = tokenize(query);
    let mut params = Map::new();
    if !content.is_empty() {
        params.insert("content".into(), content.into());
    }
    if !phrases.is_empty() {
        params.insert("exact_phrases".into(), phrases.into());
    }
    for token in tokens {
        let key = if token.key == "on" {
            "during"
        } else {
            token.key.as_str()
        };
        let value = js_trim(&token.value);
        let lowered = value.to_lowercase();
        match key {
            "from" | "mentions" => {
                for part in csv(value) {
                    if key == "mentions"
                        && (part.eq_ignore_ascii_case("everyone")
                            || part.eq_ignore_ascii_case("here"))
                    {
                        params.insert("mention_everyone".into(), true.into());
                        continue;
                    }
                    if let Some(id) = resolve_user(&part, state, options) {
                        add(
                            &mut params,
                            match (key, token.exclude) {
                                ("from", false) => "author_id",
                                ("from", true) => "exclude_author_id",
                                (_, true) => "exclude_mentions",
                                _ => "mentions",
                            },
                            id,
                        );
                    }
                }
            }
            "in" => {
                for part in csv(value) {
                    if let Some(id) = resolve_channel(&part, state, options) {
                        add(
                            &mut params,
                            if token.exclude {
                                "exclude_channel_id"
                            } else {
                                "channel_id"
                            },
                            id,
                        );
                    }
                }
            }
            "has" | "author-type" => {
                for part in value.split(',').map(js_trim).filter(|s| !s.is_empty()) {
                    let part = part.to_lowercase();
                    let part = if key == "has" && part == "forward" {
                        "snapshot"
                    } else {
                        &part
                    };
                    let valid = if key == "has" {
                        [
                            "image", "sound", "video", "file", "sticker", "embed", "link", "poll",
                            "snapshot",
                        ]
                        .contains(&part)
                    } else {
                        ["user", "bot", "webhook"].contains(&part)
                    };
                    if valid {
                        add(
                            &mut params,
                            if key == "author-type" {
                                "author_type"
                            } else if token.exclude {
                                "exclude_has"
                            } else {
                                "has"
                            },
                            part.into(),
                        );
                    }
                }
            }
            "pinned" => {
                if ["true", "false", "yes", "no", "1", "0"].contains(&lowered.as_str()) {
                    params.insert(
                        "pinned".into(),
                        ["true", "yes", "1"].contains(&lowered.as_str()).into(),
                    );
                }
            }
            "sort" => {
                if ["timestamp", "relevance"].contains(&lowered.as_str()) {
                    params.insert("sort_by".into(), lowered.into());
                }
            }
            "order" => {
                if ["asc", "desc"].contains(&lowered.as_str()) {
                    params.insert("sort_order".into(), lowered.into());
                }
            }
            "mature" => {
                if ["true", "false"].contains(&lowered.as_str()) {
                    params.insert("include_nsfw".into(), (lowered == "true").into());
                }
            }
            "scope" => {
                if let Some(scope) = csv(value)
                    .into_iter()
                    .map(|v| v.to_lowercase())
                    .find(|v| SCOPES.contains(&v.as_str()))
                {
                    params.insert("scope".into(), scope.into());
                }
            }
            "embed-type" | "embed-provider" | "link" | "filename" | "ext" | "any" => {
                for mut part in csv(value) {
                    let name = match key {
                        "embed-type" => {
                            part = part.to_lowercase();
                            if !["image", "video", "sound", "article"].contains(&part.as_str()) {
                                continue;
                            }
                            "embed_type"
                        }
                        "embed-provider" => "embed_provider",
                        "link" => "link_hostname",
                        "filename" => "attachment_filename",
                        "ext" => {
                            part = part.strip_prefix('.').unwrap_or(&part).to_owned();
                            "attachment_extension"
                        }
                        _ => "contents",
                    };
                    let name = if token.exclude && key != "any" {
                        format!("exclude_{name}")
                    } else {
                        name.into()
                    };
                    add(&mut params, &name, part);
                }
            }
            "beforeid" | "afterid" => {
                if !value.is_empty() {
                    params.insert(
                        if key == "beforeid" {
                            "max_id"
                        } else {
                            "min_id"
                        }
                        .into(),
                        value.into(),
                    );
                }
            }
            "last" => {
                if let Some(duration) = parse_duration(value)
                    .and_then(|duration| Duration::try_milliseconds(duration as i64))
                    && let Some(date) = clock.now.checked_sub_signed(duration)
                {
                    params.insert("min_id".into(), snowflake_at(date).into());
                }
            }
            "before" | "after" | "during" => {
                if value.contains("..") {
                    let mut values = value.split("..");
                    let (a, b) = (
                        values.next().unwrap_or_default(),
                        values.next().unwrap_or_default(),
                    );
                    if let (Some(lower), Some(upper)) = (clock.period(a), clock.period(b)) {
                        params.insert("min_id".into(), snowflake_at(lower.0).into());
                        params.insert("max_id".into(), snowflake_at(upper.1).into());
                    }
                } else if let Some((start, end)) = clock.period(value) {
                    if key != "before" {
                        params.insert(
                            "min_id".into(),
                            snowflake_at(if key == "after" { end } else { start }).into(),
                        );
                    }
                    if key != "after" {
                        let end = if key == "before" {
                            start
                        } else if end > start {
                            end
                        } else {
                            clock.next_day(start).unwrap_or(end)
                        };
                        params.insert("max_id".into(), snowflake_at(end).into());
                    }
                }
            }
            _ => {}
        }
    }
    ParsedSearchQuery {
        has_query: !params.is_empty(),
        params: Value::Object(params),
    }
}
fn parse_duration(value: &str) -> Option<u64> {
    let end = value.bytes().position(|b| !b.is_ascii_digit())?;
    let n = value[..end].parse::<u64>().ok()?;
    let factor = match value[end..].to_lowercase().as_str() {
        "ms" => 1,
        "s" => 1000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        "w" => 604_800_000,
        _ => return None,
    };
    n.checked_mul(factor).filter(|v| *v <= i64::MAX as u64)
}
fn snowflake_at(date: DateTime<FixedOffset>) -> String {
    ((date.timestamp_millis().saturating_sub(EPOCH).max(0) as u128) << 22).to_string()
}

struct Clock {
    now: DateTime<FixedOffset>,
    local_zone: bool,
    week_start: u32,
}
impl Clock {
    fn at(&self, date: NaiveDateTime) -> Option<DateTime<FixedOffset>> {
        if self.local_zone {
            Local
                .from_local_datetime(&date)
                .earliest()
                .map(|v| v.fixed_offset())
        } else {
            self.now.offset().from_local_datetime(&date).single()
        }
    }
    fn day(&self, date: NaiveDate) -> Option<(DateTime<FixedOffset>, DateTime<FixedOffset>)> {
        Some((
            self.at(date.and_hms_opt(0, 0, 0)?)?,
            self.at(date.succ_opt()?.and_hms_opt(0, 0, 0)?)?,
        ))
    }
    fn next_day(&self, instant: DateTime<FixedOffset>) -> Option<DateTime<FixedOffset>> {
        // Explicit-offset dates retain their own zone in Luxon.
        let date = instant.date_naive().succ_opt()?.and_hms_opt(0, 0, 0)?;
        if self.local_zone
            && Local.offset_from_utc_datetime(&instant.naive_utc()) == *instant.offset()
        {
            self.at(date)
        } else {
            instant.offset().from_local_datetime(&date).single()
        }
    }
    fn period(&self, input: &str) -> Option<(DateTime<FixedOffset>, DateTime<FixedOffset>)> {
        const MONTHS: [&str; 12] = [
            "january",
            "february",
            "march",
            "april",
            "may",
            "june",
            "july",
            "august",
            "september",
            "october",
            "november",
            "december",
        ];
        const WEEKDAYS: [&str; 7] = [
            "monday",
            "tuesday",
            "wednesday",
            "thursday",
            "friday",
            "saturday",
            "sunday",
        ];
        let input = js_trim(input);
        let word = input.to_lowercase();
        let today = self.now.date_naive();
        let whole = |start: NaiveDate, end: NaiveDate| {
            Some((
                self.at(start.and_hms_opt(0, 0, 0)?)?,
                self.at(end.and_hms_opt(0, 0, 0)?)?,
            ))
        };
        match word.as_str() {
            "now" => return Some((self.now, self.now)),
            "today" => return self.day(today),
            "yesterday" => return self.day(today.pred_opt()?),
            "week" => {
                let delta = (today.weekday().number_from_monday() + 7 - self.week_start) % 7;
                let start = today.checked_sub_days(Days::new(delta as u64))?;
                return whole(start, start.checked_add_days(Days::new(7))?);
            }
            "month" => {
                let start = today.with_day(1)?;
                return whole(start, start.checked_add_months(Months::new(1))?);
            }
            "year" => {
                let start = NaiveDate::from_ymd_opt(today.year(), 1, 1)?;
                return whole(start, NaiveDate::from_ymd_opt(today.year() + 1, 1, 1)?);
            }
            _ => {}
        }
        if let Some(month) = MONTHS.iter().position(|value| *value == word) {
            let start = NaiveDate::from_ymd_opt(today.year(), month as u32 + 1, 1)?;
            return whole(start, start.checked_add_months(Months::new(1))?);
        }
        if let Some(day) = WEEKDAYS.iter().position(|value| *value == word) {
            let delta = (today.weekday().number_from_monday() + 7 - self.week_start) % 7;
            let start = today.checked_sub_days(Days::new(delta as u64))?;
            return self.day(
                start.checked_add_days(Days::new(
                    (day as u32 + 1 + 7 - self.week_start) as u64 % 7,
                ))?,
            );
        }
        if word.len() == 4 && word.bytes().all(|b| b.is_ascii_digit()) {
            let year = word.parse::<i32>().ok()?;
            if year >= 2015 && year <= today.year() {
                return whole(
                    NaiveDate::from_ymd_opt(year, 1, 1)?,
                    NaiveDate::from_ymd_opt(year + 1, 1, 1)?,
                );
            }
        }
        let normalized = input.replacen('_', "T", 1).replacen(' ', "T", 1);
        let explicit = normalized.split_once('T').is_some_and(|(_, time)| {
            !time.is_empty()
                && (time.contains(':') || [2, 4, 6].contains(&time.trim_end_matches('Z').len()))
        });
        let instant = DateTime::parse_from_rfc3339(&normalized)
            .ok()
            .or_else(|| {
                [
                    "%Y-%m-%dT%H:%M:%S%.f%:z",
                    "%Y-%m-%dT%H:%M%:z",
                    "%Y-%m-%dT%H%:z",
                    "%Y%m%dT%H%M%S%:z",
                    "%Y%m%dT%H%M%:z",
                ]
                .iter()
                .find_map(|format| DateTime::parse_from_str(&normalized, format).ok())
            })
            .or_else(|| {
                [
                    "%Y-%m-%dT%H:%M:%S%.f",
                    "%Y-%m-%dT%H:%M",
                    "%Y-%m-%dT%H",
                    "%Y%m%dT%H%M%S",
                    "%Y%m%dT%H%M",
                    "%Y%m%dT%H%M%SZ",
                    "%Y%m%dT%H%MZ",
                ]
                .iter()
                .find_map(|format| NaiveDateTime::parse_from_str(&normalized, format).ok())
                .and_then(|date| self.at(date))
            })
            .or_else(|| {
                ["%Y-%m-%d", "%Y%m%d", "%m/%d/%Y", "%b %d %Y", "%B %d %Y"]
                    .iter()
                    .find_map(|format| NaiveDate::parse_from_str(input, format).ok())
                    .and_then(|date| self.at(date.and_hms_opt(0, 0, 0)?))
            })
            .or_else(|| {
                let parts = input.split('-').collect::<Vec<_>>();
                if parts.len() == 2 && parts[0].len() == 4 {
                    self.at(NaiveDate::from_ymd_opt(
                        parts[0].parse().ok()?,
                        parts[1].parse().ok()?,
                        1,
                    )?
                    .and_hms_opt(0, 0, 0)?)
                } else {
                    None
                }
            })?;
        if explicit {
            return Some((instant, instant));
        }
        let start = instant
            .with_hour(0)?
            .with_minute(0)?
            .with_second(0)?
            .with_nanosecond(0)?;
        Some((start, self.next_day(start)?))
    }
}
fn system_week_start() -> u32 {
    // Luxon localeWeekStart uses the host Intl locale, not account UI language.
    // Values for all Fluxer Locales.ts entries were checked with Intl.Locale.
    let locale = std::env::var("LC_ALL")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var("LANG").ok())
        .unwrap_or_else(|| "en-US".into())
        .replace('_', "-");
    let locale = locale.split('.').next().unwrap_or("en-US");
    match locale {
        "ar" => 6,
        "en-US" | "he" | "hi" | "id" | "ja" | "ko" | "pt-BR" | "th" | "zh-TW" => 7,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture_state() -> ClientState {
        let mut state = ClientState::default();
        let alice =
            serde_json::from_value(json!({"id":"100","username":"alice","discriminator":"0001"}))
                .unwrap();
        let bob = serde_json::from_value(json!({"id":"101","username":"Bob"})).unwrap();
        state.current_user = Some(alice);
        state
            .users
            .insert(100.into(), state.current_user.clone().unwrap());
        state.users.insert(101.into(), bob);
        for channel in [
            json!({"id":"200","name":"general","guild_id":"10"}),
            json!({"id":"201","name":"general-chat","guild_id":"10"}),
            json!({"id":"202","name":"photos","guild_id":"10"}),
            json!({"id":"203","name":"other","guild_id":"11"}),
        ] {
            let channel: crate::Channel = serde_json::from_value(channel).unwrap();
            state.channels.insert(channel.id, channel);
        }
        state
    }
    fn clock() -> Clock {
        Clock {
            now: DateTime::parse_from_rfc3339("2026-11-12T16:20:30Z").unwrap(),
            local_zone: false,
            week_start: 7,
        }
    }
    #[test]
    fn wire_params_match_executed_existing_parser_fixtures() {
        let fixtures: Value =
            serde_json::from_str(include_str!("../tests/fixtures/search_query.json")).unwrap();
        let state = fixture_state();
        for fixture in fixtures["fixtures"].as_array().unwrap() {
            let options = SearchParseOptions {
                guild_id: Some(10.into()),
                unique_usernames: fixture["unique"].as_bool().unwrap(),
                hints: SearchHints {
                    users_by_tag: BTreeMap::from([("Chosen Person".into(), "101".into())]),
                    channels_by_name: BTreeMap::from([("chosen-channel".into(), "202".into())]),
                },
            };
            let parsed = parse_at(
                fixture["query"].as_str().unwrap(),
                &state,
                &options,
                &clock(),
            );
            assert_eq!(
                parsed.params, fixture["params"],
                "query={:?}, unique={}",
                fixture["query"], options.unique_usernames
            );
            assert_eq!(
                parsed.has_query,
                !fixture["params"].as_object().unwrap().is_empty()
            );
        }
    }
    fn at(date: &str) -> String {
        snowflake_at(DateTime::parse_from_rfc3339(date).unwrap())
    }
    #[test]
    fn date_filters_preserve_source_boundaries_epoch_and_local_periods() {
        let state = fixture_state();
        let options = SearchParseOptions::default();
        for (query, expected) in [
            (
                "on:2026-11-12",
                json!({"min_id":at("2026-11-12T00:00:00Z"),"max_id":at("2026-11-13T00:00:00Z")}),
            ),
            (
                "after:today before:tomorrow",
                json!({"min_id":at("2026-11-13T00:00:00Z")}),
            ),
            (
                "before:yesterday",
                json!({"max_id":at("2026-11-11T00:00:00Z")}),
            ),
            (
                "during:week",
                json!({"min_id":at("2026-11-08T00:00:00Z"),"max_id":at("2026-11-15T00:00:00Z")}),
            ),
            (
                "during:november",
                json!({"min_id":at("2026-11-01T00:00:00Z"),"max_id":at("2026-12-01T00:00:00Z")}),
            ),
            (
                "during:monday",
                json!({"min_id":at("2026-11-09T00:00:00Z"),"max_id":at("2026-11-10T00:00:00Z")}),
            ),
            (
                "during:2026",
                json!({"min_id":at("2026-01-01T00:00:00Z"),"max_id":at("2027-01-01T00:00:00Z")}),
            ),
            (
                "after:2026-11-12T14:30:00+02:00",
                json!({"min_id":at("2026-11-12T12:30:00Z")}),
            ),
            (
                "during:2026-11-10..2026-11-12",
                json!({"min_id":at("2026-11-10T00:00:00Z"),"max_id":at("2026-11-13T00:00:00Z")}),
            ),
            ("last:2h", json!({"min_id":at("2026-11-12T14:20:30Z")})),
            ("before:2010-01-01", json!({"max_id":"0"})),
            ("during:not-a-date last:invalid", json!({})),
        ] {
            assert_eq!(
                parse_at(query, &state, &options, &clock()).params,
                expected,
                "{query}"
            );
        }
    }
    #[test]
    fn hint_resolution_preserves_explicit_chosen_ids_and_repeated_values() {
        let options = SearchParseOptions {
            hints: SearchHints {
                users_by_tag: BTreeMap::from([(
                    "Chosen Person".into(),
                    "9007199254740993123".into(),
                )]),
                channels_by_name: BTreeMap::from([(
                    "chosen-channel".into(),
                    "9007199254740993999".into(),
                )]),
            },
            ..Default::default()
        };
        let parsed = parse_at(
            "from:\"Chosen Person\" in:chosen-channel from:\"Chosen Person\"",
            &fixture_state(),
            &options,
            &clock(),
        );
        assert_eq!(
            parsed.params,
            json!({"author_id":["9007199254740993123"],"channel_id":["9007199254740993999"]})
        );
    }
}
