// SPDX-License-Identifier: AGPL-3.0-or-later
//! SearchQueryParser.ts + SearchUtils.toApiParams. Unknown/invalid recognized
//! filters are consumed exactly as in the existing client, never sent as IDs.
use crate::{ChannelType, ClientState, Snowflake};
use chrono::{
    DateTime, Datelike, Days, Duration, FixedOffset, Local, Months, NaiveDate, NaiveDateTime,
    TimeZone,
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
        named_zone: None,
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
                        params.insert("min_id".into(), snowflake_at(lower.start).into());
                        params.insert("max_id".into(), snowflake_at(upper.end).into());
                    }
                } else if let Some(period) = clock.period(value) {
                    let (start, end) = (period.start, period.end);
                    if key != "before" {
                        params.insert(
                            "min_id".into(),
                            snowflake_at(if key == "after" { end } else { start }).into(),
                        );
                    }
                    if key != "after" {
                        let end = if key == "before" {
                            start
                        } else {
                            period.during_end
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

#[derive(Clone, Copy)]
enum DateZone {
    Local,
    Fixed(FixedOffset),
    Named(chrono_tz::Tz),
}
struct SearchDatePeriod {
    start: DateTime<FixedOffset>,
    end: DateTime<FixedOffset>,
    during_end: DateTime<FixedOffset>,
}
struct Clock {
    now: DateTime<FixedOffset>,
    local_zone: bool,
    week_start: u32,
    named_zone: Option<chrono_tz::Tz>,
}
fn local_datetime<T: TimeZone>(
    zone: &T,
    date: NaiveDateTime,
    guess: FixedOffset,
) -> Option<DateTime<FixedOffset>> {
    use chrono::{LocalResult, Offset};
    match zone.from_local_datetime(&date) {
        LocalResult::Single(value) => Some(value.fixed_offset()),
        LocalResult::Ambiguous(a, b) => {
            // Luxon fixOffset starts with the zone's offset at Settings.now.
            // An overlap therefore selects the matching current-offset branch.
            if b.offset().fix() == guess {
                Some(b.fixed_offset())
            } else {
                Some(a.fixed_offset())
            }
        }
        LocalResult::None => {
            // Luxon moves nonexistent spring-forward wall times across the gap
            // using the smaller surrounding offset; it does not reject the date.
            let before = zone
                .offset_from_utc_datetime(&date.checked_sub_signed(Duration::days(1))?)
                .fix();
            let after = zone
                .offset_from_utc_datetime(&date.checked_add_signed(Duration::days(1))?)
                .fix();
            let offset = before.local_minus_utc().min(after.local_minus_utc());
            let utc = date.checked_sub_signed(Duration::seconds(offset as i64))?;
            Some(zone.from_utc_datetime(&utc).fixed_offset())
        }
    }
}
impl DateZone {
    fn at(self, date: NaiveDateTime, now: DateTime<FixedOffset>) -> Option<DateTime<FixedOffset>> {
        use chrono::Offset;
        match self {
            Self::Fixed(zone) => zone.from_local_datetime(&date).single(),
            Self::Local => local_datetime(
                &Local,
                date,
                Local.offset_from_utc_datetime(&now.naive_utc()).fix(),
            ),
            Self::Named(zone) => local_datetime(
                &zone,
                date,
                zone.offset_from_utc_datetime(&now.naive_utc()).fix(),
            ),
        }
    }
    fn instant(self, utc: DateTime<FixedOffset>) -> DateTime<FixedOffset> {
        match self {
            Self::Fixed(zone) => utc.with_timezone(&zone),
            Self::Local => utc.with_timezone(&Local).fixed_offset(),
            Self::Named(zone) => utc.with_timezone(&zone).fixed_offset(),
        }
    }
    fn day(self, instant: DateTime<FixedOffset>) -> Option<SearchDatePeriod> {
        let date = instant.date_naive();
        let start = self.at(date.and_hms_opt(0, 0, 0)?, instant)?;
        let end = self.at(date.succ_opt()?.and_hms_opt(0, 0, 0)?, instant)?;
        Some(SearchDatePeriod {
            start,
            end,
            during_end: end,
        })
    }
    fn point(self, instant: DateTime<FixedOffset>) -> Option<SearchDatePeriod> {
        Some(SearchDatePeriod {
            start: instant,
            end: instant,
            during_end: self.day(instant)?.end,
        })
    }
}
impl Clock {
    fn zone(&self) -> DateZone {
        self.named_zone.map(DateZone::Named).unwrap_or_else(|| {
            if self.local_zone {
                DateZone::Local
            } else {
                DateZone::Fixed(*self.now.offset())
            }
        })
    }
    fn at(&self, date: NaiveDateTime) -> Option<DateTime<FixedOffset>> {
        self.zone().at(date, self.now)
    }
    fn period(&self, input: &str) -> Option<SearchDatePeriod> {
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
        let zone = self.zone();
        let today = zone.instant(self.now).date_naive();
        let whole = |start: NaiveDate, end: NaiveDate| {
            let start = self.at(start.and_hms_opt(0, 0, 0)?)?;
            let end = self.at(end.and_hms_opt(0, 0, 0)?)?;
            Some(SearchDatePeriod {
                start,
                end,
                during_end: end,
            })
        };
        match word.as_str() {
            "now" => return zone.point(zone.instant(self.now)),
            "today" => return zone.day(zone.instant(self.now)),
            "yesterday" => return whole(today.pred_opt()?, today),
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
                return whole(
                    NaiveDate::from_ymd_opt(today.year(), 1, 1)?,
                    NaiveDate::from_ymd_opt(today.year() + 1, 1, 1)?,
                );
            }
            _ => {}
        }
        if let Some(month) = MONTHS.iter().position(|v| *v == word) {
            let start = NaiveDate::from_ymd_opt(today.year(), month as u32 + 1, 1)?;
            return whole(start, start.checked_add_months(Months::new(1))?);
        }
        if let Some(day) = WEEKDAYS.iter().position(|v| *v == word) {
            let delta = (today.weekday().number_from_monday() + 7 - self.week_start) % 7;
            let start = today
                .checked_sub_days(Days::new(delta as u64))?
                .checked_add_days(Days::new((day as u32 + 1 + 7 - self.week_start) as u64 % 7))?;
            return whole(start, start.succ_opt()?);
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
        let (instant, zone) = self.parse_instant(input)?;
        if explicit_clock_time(input) {
            zone.point(instant)
        } else {
            zone.day(instant)
        }
    }
    fn parse_instant(&self, input: &str) -> Option<(DateTime<FixedOffset>, DateZone)> {
        let normalized = input.replacen('_', "T", 1).replacen(' ', "T", 1);
        parse_iso(&normalized, self)
            .or_else(|| parse_js_date_fallback(input, self).map(|instant| (instant, self.zone())))
    }
}
// Exact source EXPLICIT_CLOCK_TIME_RE; timezone colons and a trailing Z must
// not accidentally turn hour-only or dashed compact clocks into point periods.
fn explicit_clock_time(input: &str) -> bool {
    let chars = input.as_bytes();
    for i in 0..chars.len() {
        if matches!(chars[i], b'T' | b'_' | b' ') {
            let tail = &chars[i + 1..];
            let digits = tail.iter().take_while(|b| b.is_ascii_digit()).count();
            if (1..=2).contains(&digits)
                && tail.get(digits) == Some(&b':')
                && tail.get(digits + 1).is_some_and(u8::is_ascii_digit)
                && tail.get(digits + 2).is_some_and(u8::is_ascii_digit)
            {
                return true;
            }
            if digits == tail.len() && ((1..=2).contains(&digits) || (4..=6).contains(&digits)) {
                return true;
            }
        }
        if chars[i] == b'T'
            && i >= 8
            && chars[i - 8..i].iter().all(u8::is_ascii_digit)
            && chars
                .get(i + 1..i + 5)
                .is_some_and(|v| v.iter().all(u8::is_ascii_digit))
        {
            return true;
        }
    }
    false
}
fn named_zone(value: &str) -> Option<chrono_tz::Tz> {
    value.parse().ok().or_else(|| {
        chrono_tz::TZ_VARIANTS
            .iter()
            .find(|zone| zone.name().eq_ignore_ascii_case(value))
            .copied()
    })
}
fn iso_date(value: &str) -> Option<NaiveDate> {
    use chrono::Weekday;
    if let Some((year, week)) = value.split_once("-W").or_else(|| value.split_once('W')) {
        if year.len() != 4 {
            return None;
        }
        let (week, day) = if let Some((week, day)) = week.split_once('-') {
            (week, day)
        } else if week.len() == 3 {
            (&week[..2], &week[2..])
        } else {
            (week, "1")
        };
        if week.len() != 2 || day.len() != 1 {
            return None;
        }
        let day = match day.parse::<u8>().ok()? {
            1 => Weekday::Mon,
            2 => Weekday::Tue,
            3 => Weekday::Wed,
            4 => Weekday::Thu,
            5 => Weekday::Fri,
            6 => Weekday::Sat,
            7 => Weekday::Sun,
            _ => return None,
        };
        return NaiveDate::from_isoywd_opt(year.parse().ok()?, week.parse().ok()?, day);
    }
    let signed = value.starts_with(['+', '-']);
    let year_len = if signed { 7 } else { 4 };
    let year_text = value.get(..year_len)?;
    if !year_text
        .bytes()
        .skip(usize::from(signed))
        .all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let year = year_text.parse::<i32>().ok()?;
    let suffix = value.get(year_len..)?;
    let mut compact = String::new();
    let mut separator = false;
    for (index, byte) in suffix.bytes().enumerate() {
        if byte.is_ascii_digit() {
            compact.push(byte as char);
            separator = false;
        } else if byte == b'-'
            && [0, 2].contains(&compact.len())
            && !separator
            && index + 1 < suffix.len()
        {
            separator = true;
        } else {
            return None;
        }
    }
    match compact.len() {
        0 => NaiveDate::from_ymd_opt(year, 1, 1),
        2 => NaiveDate::from_ymd_opt(year, compact.parse().ok()?, 1),
        3 => NaiveDate::from_yo_opt(year, compact.parse().ok()?),
        4 => NaiveDate::from_ymd_opt(year, compact[..2].parse().ok()?, compact[2..].parse().ok()?),
        _ => None,
    }
}
fn parse_iso(input: &str, clock: &Clock) -> Option<(DateTime<FixedOffset>, DateZone)> {
    let mut text = input;
    let mut zone = clock.zone();
    let mut has_named = false;
    if text.ends_with(']') {
        let start = text.rfind('[')?;
        zone = DateZone::Named(named_zone(&text[start + 1..text.len() - 1])?);
        has_named = true;
        text = &text[..start];
    }
    let (date, time) = if let Some((date, time)) = text.split_once(['T', 't']) {
        (Some(date), Some(time))
    } else if text.contains(':') {
        (None, Some(text))
    } else {
        (Some(text), None)
    };
    let has_time = time.is_some();
    let mut time = time.unwrap_or("");
    if (has_time && time.is_empty()) || (has_named && !has_time) {
        return None;
    }
    if time.ends_with(['Z', 'z']) {
        if !has_named {
            zone = DateZone::Fixed(FixedOffset::east_opt(0)?);
        }
        time = &time[..time.len() - 1];
    } else if let Some(index) = time.rfind(['+', '-']) {
        let offset = &time[index + 1..];
        if offset.contains(':') && (offset.len() != 5 || offset.as_bytes()[2] != b':') {
            return None;
        }
        let compact = offset.replace(':', "");
        if ![2, 4].contains(&compact.len()) || !compact.bytes().all(|v| v.is_ascii_digit()) {
            return None;
        }
        let hours = compact[..2].parse::<i32>().ok()?;
        let minutes = if compact.len() == 4 {
            compact[2..].parse::<i32>().ok()?
        } else {
            0
        };
        if minutes > 59 {
            return None;
        }
        let sign = if time.as_bytes()[index] == b'-' {
            -1
        } else {
            1
        };
        if !has_named {
            zone = DateZone::Fixed(FixedOffset::east_opt(sign * (hours * 3600 + minutes * 60))?);
        }
        time = &time[..index];
    }
    let mut date = if let Some(date) = date {
        iso_date(date)?
    } else {
        zone.instant(clock.now).date_naive()
    };
    if has_time && time.is_empty() {
        return None;
    }
    let fraction_parts = time.split_once(['.', ',']);
    let (integer, fraction) = fraction_parts.unwrap_or((time, ""));
    if fraction_parts.is_some() && fraction.is_empty() {
        return None;
    }
    let mut compact = String::new();
    let mut colon = false;
    for (index, byte) in integer.bytes().enumerate() {
        if byte.is_ascii_digit() {
            compact.push(byte as char);
            colon = false;
        } else if byte == b':'
            && [2, 4].contains(&compact.len())
            && !colon
            && index + 1 < integer.len()
        {
            colon = true;
        } else {
            return None;
        }
    }
    if ![0, 2, 4, 6].contains(&compact.len())
        || !compact.bytes().all(|b| b.is_ascii_digit())
        || !fraction.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let part = |start, end| compact.get(start..end).unwrap_or("0").parse::<u32>().ok();
    let mut hour = part(0, 2)?;
    let minute = part(2, 4)?;
    let second = part(4, 6)?;
    let millis = if fraction.is_empty() {
        0
    } else {
        format!("{:0<3}", &fraction[..fraction.len().min(3)])
            .parse::<u32>()
            .ok()?
    };
    if !fraction.is_empty() && compact.len() != 6 {
        return None;
    }
    if hour == 24 && minute == 0 && second == 0 && millis == 0 {
        date = date.succ_opt()?;
        hour = 0;
    }
    let instant = zone.at(
        date.and_hms_milli_opt(hour, minute, second, millis)?,
        clock.now,
    )?;
    Some((instant, zone))
}
fn parse_js_date_fallback(input: &str, clock: &Clock) -> Option<DateTime<FixedOffset>> {
    // Source's final new Date(input) is implementation-defined beyond ISO.
    // These formats and date-overflow cases are checked against its actual V8.
    let zone = clock.zone();
    // Actual V8 Date accepts ASCII-quoted legacy dates as local wall dates,
    // including the parser's quoted filter values. Curly quotes are not accepted.
    let quoted = (input.starts_with('"') && input.ends_with('"'))
        || (input.starts_with('\'') && input.ends_with('\''));
    let input = if quoted && input.len() >= 2 {
        &input[1..input.len() - 1]
    } else {
        input
    };
    if let Ok(instant) = DateTime::parse_from_rfc2822(input) {
        return Some(zone.instant(instant));
    }
    for date in [
        "%b %d %Y",
        "%B %d %Y",
        "%b %d, %Y",
        "%B %d, %Y",
        "%m/%d/%Y",
        "%Y/%m/%d",
    ] {
        for time in ["%H:%M", "%H:%M:%S", "%I:%M %p", "%I:%M:%S %p"] {
            // V8 rejects an ASCII closing quote immediately after a numeric
            // clock, but accepts the same quoted legacy format ending AM/PM.
            if quoted && !time.contains("%p") {
                continue;
            }
            if let Ok(instant) = NaiveDateTime::parse_from_str(input, &format!("{date} {time}")) {
                let date = legacy_year(instant.date())?;
                return zone.at(date.and_time(instant.time()), clock.now);
            }
        }
    }
    let raw = input
        .strip_suffix('Z')
        .or_else(|| input.strip_suffix('z'))
        .unwrap_or(input);
    let dash = raw.split('-').collect::<Vec<_>>();
    if (dash.len() == 2 || dash.len() == 3)
        && dash[0].len() == 4
        && (1..=2).contains(&dash[1].len())
        && (dash.len() == 2 || (1..=2).contains(&dash[2].len()))
    {
        let year = dash[0].parse().ok()?;
        let month = dash[1].parse().ok()?;
        let day = if dash.len() == 3 {
            dash[2].parse::<u64>().ok()?
        } else {
            1
        };
        if !(1..=31).contains(&day) {
            return None;
        }
        let date = NaiveDate::from_ymd_opt(year, month, 1)?.checked_add_days(Days::new(day - 1))?;
        let canonical = dash.len() == 3 && dash[1].len() == 2 && dash[2].len() == 2;
        if quoted || !canonical {
            // V8 does not treat a leading zero-padded small number as a
            // four-digit year in its legacy numeric grammar (e.g. quoted0026).
            if year < 100 {
                return None;
            }
            return zone.at(date.and_hms_opt(0, 0, 0)?, clock.now);
        }
        let instant = FixedOffset::east_opt(0)?
            .from_local_datetime(&date.and_hms_opt(0, 0, 0)?)
            .single()?;
        return Some(zone.instant(instant));
    }

    [
        "%m/%d/%Y",
        "%Y/%m/%d",
        "%b-%d-%Y",
        "%B-%d-%Y",
        "%d-%b-%Y",
        "%d-%B-%Y",
        "%b %d %Y",
        "%B %d %Y",
        "%b %d, %Y",
        "%B %d, %Y",
    ]
    .iter()
    .find_map(|format| NaiveDate::parse_from_str(input, format).ok())
    .and_then(|date| zone.at(legacy_year(date)?.and_hms_opt(0, 0, 0)?, clock.now))
}
fn legacy_year(date: NaiveDate) -> Option<NaiveDate> {
    // V8 legacy Date parsing uses 2000..2049 / 1950..1999 for numeric years
    // below 100; this is separate from ISO's literal four/six-digit year.
    if (0..100).contains(&date.year()) {
        date.with_year(date.year() + if date.year() < 50 { 2000 } else { 1900 })
    } else {
        Some(date)
    }
}
fn system_week_start() -> u32 {
    // ICU's host-locale environment priority, verified by isolated actual-Intl
    // subprocess fixtures. Empty present variables resolve to und, not LANG.
    host_week_start(
        std::env::var("LC_ALL").ok().as_deref(),
        std::env::var("LC_MESSAGES").ok().as_deref(),
        std::env::var("LANG").ok().as_deref(),
    )
}
fn host_week_start(lc_all: Option<&str>, lc_messages: Option<&str>, lang: Option<&str>) -> u32 {
    locale_week_start(lc_all.or(lc_messages).or(lang).unwrap_or("en-US"))
}
fn locale_week_start(locale: &str) -> u32 {
    // Compact non-Monday tables are generated from Intl.Locale week settings;
    // fixtures also cover all 34 Locales.ts entries and likely default regions.
    fn member(list: &str, value: &str) -> bool {
        list.split_ascii_whitespace()
            .any(|v| v.eq_ignore_ascii_case(value))
    }
    fn region(region: &str) -> u32 {
        if region.eq_ignore_ascii_case("MV") {
            5
        } else if member("AF BH DJ DZ EG IQ IR JO KW LY OM QA SD SY", region) {
            6
        } else if member(
            "AG AS BD BR BS BT BU BW BZ CA CO DM DO ET GT GU HK HN ID IL IN IS JM JP JT KE KH KR LA MH MI MM MO MT MX MZ NI NP NT PA PE PH PK PR PT PU PY PZ RH SA SG SV TH TT TW UM US VE VI WK WS YD YE ZA ZW",
            region,
        ) {
            7
        } else {
            1
        }
    }
    let normalized = locale
        .split(['.', '@'])
        .next()
        .unwrap_or("")
        .replace('_', "-");
    if normalized.is_empty()
        || normalized.eq_ignore_ascii_case("C")
        || normalized.eq_ignore_ascii_case("POSIX")
    {
        return 7;
    }
    let tags = normalized.split('-').collect::<Vec<_>>();
    let language = tags.first().copied().unwrap_or("");
    let extension = tags.iter().position(|value| *value == "u");
    if let Some(index) = extension {
        for pair in tags[index + 1..].windows(2) {
            if pair[0] == "fw"
                && let Some(day) = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"]
                    .iter()
                    .position(|day| day.eq_ignore_ascii_case(pair[1]))
            {
                return day as u32 + 1;
            }
            if pair[0] == "rg"
                && pair[1].len() == 6
                && pair[1].is_ascii()
                && pair[1][2..].eq_ignore_ascii_case("zzzz")
            {
                return region(&pair[1][..2]);
            }
        }
    }
    let base = &tags[..extension.unwrap_or(tags.len())];
    if let Some(code) = base.iter().skip(1).find(|part| {
        (part.len() == 2 && part.bytes().all(|b| b.is_ascii_alphabetic()))
            || (part.len() == 3 && part.bytes().all(|b| b.is_ascii_digit()))
    }) {
        return region(code);
    }
    if language.eq_ignore_ascii_case("zh")
        && base
            .iter()
            .any(|script| script.eq_ignore_ascii_case("Hant"))
    {
        return 7;
    }
    if member("dv", language) {
        5
    } else if member("ae ar fa ps", language) {
        6
    } else if member(
        "aa af am as bh bn ch cr dz en gn gu he hi id ik in is iu iw ja jv jw ki km kn ko ks lo mh ml mr mt my nd ne nr nv oj om or pa pt qu sa sd sm sn ss st su ta te th ti tl tn ts ur ve xh zu und",
        language,
    ) {
        7
    } else {
        1
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
            named_zone: None,
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
    fn date_params_match_executed_source_luxon_fixtures() {
        let data: Value =
            serde_json::from_str(include_str!("../tests/fixtures/search_query_dates.json"))
                .unwrap();
        let state = fixture_state();
        let mut failures = Vec::new();
        for fixture in data["fixtures"].as_array().unwrap() {
            let zone: chrono_tz::Tz = fixture["zone"].as_str().unwrap().parse().unwrap();
            let now = DateTime::parse_from_rfc3339(fixture["now"].as_str().unwrap())
                .unwrap()
                .with_timezone(&zone)
                .fixed_offset();
            let clock = Clock {
                now,
                local_zone: false,
                named_zone: Some(zone),
                week_start: if fixture["locale"] == "en-US" { 7 } else { 1 },
            };
            let parsed = parse_at(
                fixture["query"].as_str().unwrap(),
                &state,
                &SearchParseOptions::default(),
                &clock,
            );
            if parsed.params != fixture["params"] {
                failures.push(format!(
                    "{} {} {} {}: actual={} expected={}",
                    fixture["now"],
                    fixture["zone"],
                    fixture["locale"],
                    fixture["query"],
                    parsed.params,
                    fixture["params"]
                ));
            }
        }
        assert!(
            failures.is_empty(),
            "{} date fixture mismatches:\n{}",
            failures.len(),
            failures
                .iter()
                .take(30)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    #[test]
    fn locale_week_boundaries_match_executed_intl_and_host_environment_fixtures() {
        let data: Value =
            serde_json::from_str(include_str!("../tests/fixtures/search_query_week.json")).unwrap();
        for fixture in data["fixtures"].as_array().unwrap() {
            let locale = fixture["locale"].as_str().unwrap();
            assert_eq!(
                locale_week_start(locale),
                fixture["first_day"].as_u64().unwrap() as u32,
                "{locale}"
            );
        }
        for fixture in data["host_fixtures"].as_array().unwrap() {
            let env = &fixture["environment"];
            assert_eq!(
                host_week_start(
                    env["LC_ALL"].as_str(),
                    env["LC_MESSAGES"].as_str(),
                    env["LANG"].as_str()
                ),
                fixture["first_day"].as_u64().unwrap() as u32,
                "{fixture}"
            );
        }
        for first in b'A'..=b'Z' {
            for second in b'A'..=b'Z' {
                let region = String::from_utf8(vec![first, second]).unwrap();
                let language = region.to_ascii_lowercase();
                for (category, value, locale) in [
                    ("territories", region.clone(), format!("und-{region}")),
                    ("languages", language.clone(), language),
                ] {
                    let expected = data[category]
                        .as_object()
                        .unwrap()
                        .iter()
                        .find(|(_, values)| {
                            values
                                .as_array()
                                .unwrap()
                                .iter()
                                .any(|v| v.as_str() == Some(&value))
                        })
                        .map(|(day, _)| day.parse::<u32>().unwrap())
                        .unwrap_or(1);
                    assert_eq!(locale_week_start(&locale), expected, "{locale}");
                }
            }
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
