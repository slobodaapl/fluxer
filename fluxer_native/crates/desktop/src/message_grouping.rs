// SPDX-License-Identifier: AGPL-3.0-or-later
//! Source: MessageGroupingUtils.ts/isNewMessageGroup + createChannelStream,
//! MessageLayoutSpec.ts, MessageGroupSpacing.ts and ChannelMessageStream.tsx.
use chrono::{DateTime, Local, NaiveDate, TimeZone};
use fluxer_core::Message;
use std::borrow::Borrow;

pub(super) const COZY_AVATAR_SIZE: f32 = 40.;
pub(super) const COZY_GUTTER: f32 = 12.;
pub(super) const COZY_LINE_HEIGHT: f32 = 22.;
pub(super) const COZY_SPACING_Y: f32 = 2.;
const GROUP_TIMEOUT_MS: i64 = 7 * 60 * 1000;
const SUPPRESS_NOTIFICATIONS: u32 = 1 << 12;
const IS_CROSSPOST: u32 = 1 << 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct MessageRowLayout {
    pub starts_group: bool,
    pub date_divider: Option<String>,
    pub short_time: String,
    pub space_before: u8,
}
struct Moment {
    millis: i64,
    day: NaiveDate,
    short_time: String,
}

pub(super) fn message_layouts(messages: &[impl Borrow<Message>]) -> Vec<MessageRowLayout> {
    layouts_in_zone(messages, &Local)
}

fn layouts_in_zone<Tz: TimeZone>(
    messages: &[impl Borrow<Message>],
    zone: &Tz,
) -> Vec<MessageRowLayout> {
    let moments: Vec<_> = messages
        .iter()
        .map(Borrow::borrow)
        .map(|message| {
            DateTime::parse_from_rfc3339(&message.timestamp)
                .ok()
                .map(|date| {
                    let local = date.with_timezone(zone);
                    Moment {
                        millis: local.timestamp_millis(),
                        day: local.date_naive(),
                        short_time: local.time().format("%H:%M").to_string(),
                    }
                })
        })
        .collect();
    let mut group_start: Option<usize> = None;
    let mut last_date: Option<NaiveDate> = None;
    messages
        .iter()
        .map(Borrow::borrow)
        .enumerate()
        .map(|(index, message)| {
            let moment = moments[index].as_ref();
            // Malformed/local demo timestamps cannot establish a calendar day.
            // Keep their original header time instead of inventing date labels.
            let date_divider = moment
                .filter(|moment| last_date != Some(moment.day))
                .map(|moment| moment.day.format("%A, %B %-d, %Y").to_string());
            let divider = date_divider.is_some();
            last_date = moment.map(|moment| moment.day);
            let starts_group = group_start.is_none_or(|start| {
                new_group(
                    messages[start].borrow(),
                    message,
                    moments[start].as_ref(),
                    moment,
                )
            });
            let space_before = if starts_group && !divider {
                group_start
                    .map(|start| {
                        if display_system(messages[start].borrow()) && display_system(message) {
                            8
                        } else {
                            16
                        }
                    })
                    .unwrap_or(0)
            } else {
                0
            };
            // Deliberately keep the FIRST message of the group, not the preceding
            // message. Source createChannelStream updates lastMessageInGroup only
            // when groupId === message.id, despite that variable's name.
            if starts_group {
                group_start = Some(index);
            }
            MessageRowLayout {
                starts_group,
                date_divider,
                short_time: moment
                    .map(|moment| moment.short_time.clone())
                    .unwrap_or_default(),
                space_before,
            }
        })
        .collect()
}

fn display_system(message: &Message) -> bool {
    !matches!(message.kind, 0 | 19)
}
fn user_content(message: &Message) -> bool {
    matches!(message.kind, 0 | 19 | 99)
}
fn new_group(
    previous: &Message,
    current: &Message,
    previous_time: Option<&Moment>,
    current_time: Option<&Moment>,
) -> bool {
    if current.kind == 19 || current.flags & IS_CROSSPOST != 0 {
        return true;
    }
    let current_system = display_system(current);
    let previous_system = display_system(previous);
    if current_system != previous_system {
        return true;
    }
    if !(current_system && previous_system)
        && current.kind != previous.kind
        && !(user_content(current) && user_content(previous))
    {
        return true;
    }
    if previous.kind <= 19 && previous.author.id != current.author.id {
        return true;
    }
    let webhook = current.extra.get("webhook_id").is_some_and(|value| {
        value.as_str().is_some_and(|value| !value.is_empty())
            || value.as_u64().is_some_and(|value| value != 0)
    });
    if webhook && previous.author.username != current.author.username {
        return true;
    }
    let (Some(previous_time), Some(current_time)) = (previous_time, current_time) else {
        return true;
    };
    if previous_time.day != current_time.day
        || current_time.millis - previous_time.millis > GROUP_TIMEOUT_MS
    {
        return true;
    }
    let previous_suppressed = previous.flags & SUPPRESS_NOTIFICATIONS != 0;
    let current_suppressed = current.flags & SUPPRESS_NOTIFICATIONS != 0;
    if previous_suppressed != current_suppressed {
        if !previous_suppressed && current_suppressed {
            return true;
        }
        if previous_suppressed
            && !current_suppressed
            && (!current.mentions.is_empty()
                || !current.mention_roles.is_empty()
                || current.mention_everyone)
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::{COZY_AVATAR_SIZE, COZY_GUTTER, COZY_LINE_HEIGHT, COZY_SPACING_Y, layouts_in_zone};
    use chrono::{FixedOffset, Utc};
    use fluxer_core::{Message, Snowflake, User};
    use serde_json::json;
    fn message(id: u64, time: &str) -> Message {
        let mut message = Message::text(
            Snowflake(id),
            Snowflake(11),
            User {
                id: Snowflake(1),
                username: "ada".into(),
                ..Default::default()
            },
            "text",
        );
        message.timestamp = time.into();
        message
    }
    fn starts(messages: &[Message]) -> Vec<bool> {
        layouts_in_zone(messages, &Utc)
            .iter()
            .map(|row| row.starts_group)
            .collect()
    }
    #[test]
    fn timeout_is_strict_and_measured_from_group_start() {
        let messages = vec![
            message(1, "2026-11-12T10:00:00Z"),
            message(2, "2026-11-12T10:06:00Z"),
            message(3, "2026-11-12T10:07:00Z"),
            message(4, "2026-11-12T10:07:00.001Z"),
            message(5, "2026-11-12T10:13:00Z"),
        ];
        assert_eq!(starts(&messages), vec![true, false, false, true, false]);
        assert_eq!(layouts_in_zone(&messages, &Utc)[3].space_before, 16);
    }
    #[test]
    fn day_boundaries_are_local_and_dividers_do_not_add_message_rows() {
        let messages = vec![
            message(1, "2026-11-12T05:59:00Z"),
            message(2, "2026-11-12T06:01:00Z"),
        ];
        let rows = layouts_in_zone(&messages, &FixedOffset::west_opt(6 * 3600).unwrap());
        assert_eq!(rows.len(), messages.len());
        assert!(
            rows.iter()
                .all(|row| row.starts_group && row.date_divider.is_some())
        );
        assert_eq!(
            rows[0].date_divider.as_deref(),
            Some("Wednesday, November 11, 2026")
        );
        assert_eq!(
            rows[1].date_divider.as_deref(),
            Some("Thursday, November 12, 2026")
        );
        assert_eq!(rows[1].space_before, 0);
        assert_eq!(
            starts(&messages),
            vec![true, false],
            "UTC has no midnight at this instant"
        );
    }
    #[test]
    fn reply_crosspost_and_display_system_transitions_break_groups() {
        let first = message(1, "2026-11-12T10:00:00Z");
        let mut second = message(2, "2026-11-12T10:00:01Z");
        for kind in [19, 6, 7, 99] {
            second.kind = kind;
            assert_eq!(starts(&[first.clone(), second.clone()]), vec![true, true]);
        }
        second.kind = 0;
        second.flags = 1 << 1;
        assert_eq!(starts(&[first.clone(), second.clone()]), vec![true, true]);
        second.flags = 1 << 0;
        assert_eq!(
            starts(&[first, second]),
            vec![true, false],
            "published original is not a crosspost copy"
        );
    }
    #[test]
    fn ordinary_message_can_continue_a_reply_group_but_another_reply_cannot() {
        let mut first = message(1, "2026-11-12T10:00:00Z");
        first.kind = 19;
        let second = message(2, "2026-11-12T10:00:01Z");
        let mut third = message(3, "2026-11-12T10:00:02Z");
        third.kind = 19;
        assert_eq!(starts(&[first, second, third]), vec![true, false, true]);
    }
    #[test]
    fn author_and_webhook_username_follow_exact_source_predicates() {
        let first = message(1, "2026-11-12T10:00:00Z");
        let mut second = message(2, "2026-11-12T10:00:01Z");
        second.author.id = Snowflake(2);
        assert_eq!(starts(&[first.clone(), second.clone()]), vec![true, true]);
        second.author.id = first.author.id;
        second.author.username = "other webhook".into();
        assert_eq!(starts(&[first.clone(), second.clone()]), vec![true, false]);
        second.extra.insert("webhook_id".into(), json!("900"));
        assert_eq!(starts(&[first.clone(), second.clone()]), vec![true, true]);
        second.author.username = first.author.username.clone();
        assert_eq!(starts(&[first, second]), vec![true, false]);
    }
    #[test]
    fn suppressed_to_regular_breaks_only_when_regular_message_mentions_someone() {
        let mut first = message(1, "2026-11-12T10:00:00Z");
        let mut second = message(2, "2026-11-12T10:00:01Z");
        second.flags = 1 << 12;
        assert_eq!(starts(&[first.clone(), second.clone()]), vec![true, true]);
        first.flags = 1 << 12;
        second.flags = 0;
        assert_eq!(starts(&[first.clone(), second.clone()]), vec![true, false]);
        second.mention_everyone = true;
        assert_eq!(starts(&[first.clone(), second.clone()]), vec![true, true]);
        second.mention_everyone = false;
        second.mention_roles.push(Snowflake(9));
        assert_eq!(starts(&[first, second]), vec![true, true]);
    }
    #[test]
    fn system_author_cutoff_and_half_spacing_match_source() {
        let mut first = message(1, "2026-11-12T10:00:00Z");
        first.kind = 99;
        let mut second = message(2, "2026-11-12T10:00:01Z");
        second.kind = 6;
        second.author.id = Snowflake(2);
        assert_eq!(
            starts(&[first.clone(), second.clone()]),
            vec![true, false],
            "previous type > REPLY bypasses author boundary"
        );
        first.kind = 6;
        let rows = layouts_in_zone(&[first, second], &Utc);
        assert!(rows[1].starts_group);
        assert_eq!(rows[1].space_before, 8);
    }
    #[test]
    fn absent_or_invalid_time_never_merges_and_cozy_dimensions_are_source_values() {
        assert_eq!(
            starts(&[message(1, ""), message(2, "invalid")]),
            vec![true, true]
        );
        assert_eq!(
            (
                COZY_AVATAR_SIZE,
                COZY_GUTTER,
                COZY_LINE_HEIGHT,
                COZY_SPACING_Y
            ),
            (40., 12., 22., 2.)
        );
    }
}
