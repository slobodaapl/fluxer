// SPDX-License-Identifier: AGPL-3.0-or-later
use fluxer_core::{Channel, ChannelType};
use std::collections::BTreeMap;

/// Match classic Fluxer's ChannelOrganization and compareChannelOrdering.
pub(super) fn guild_channel_rows(mut channels: Vec<&Channel>) -> Vec<&Channel> {
    // Categories and siblings each have their own ordering. Within each
    // category (including the root), text-like rows precede voice rows.
    channels.sort_by_cached_key(|channel| {
        (
            channel.kind.is_voice(),
            channel.position,
            channel.id.to_string(),
        )
    });

    let mut categories = Vec::new();
    let mut children = BTreeMap::<_, Vec<_>>::new();
    for channel in channels {
        match channel.kind {
            ChannelType::Category => categories.push(channel),
            ChannelType::Text
            | ChannelType::Announcement
            | ChannelType::Link
            | ChannelType::Forum
            | ChannelType::Media
            | ChannelType::Voice => children.entry(channel.parent_id).or_default().push(channel),
            _ => {}
        }
    }

    let mut rows = children.remove(&None).unwrap_or_default();
    for category in categories {
        rows.push(category);
        if let Some(siblings) = children.remove(&Some(category.id)) {
            rows.extend(siblings);
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories_keep_their_children_when_positions_overlap() {
        use ChannelType::*;

        let channels = [
            (10, Category, None, 0),
            (20, Category, None, 2),
            (30, Category, None, 4),
            (31, Category, None, 6),
            (11, Text, Some(10), 4),
            (12, Text, Some(10), 5),
            (21, Voice, Some(20), 3),
            (22, Voice, Some(20), 7),
            (23, Forum, Some(20), 8),
            (32, Text, Some(30), 9),
            (33, PublicThread, Some(32), 0),
            (34, Text, Some(99), 0),
            (9, Text, None, 10),
            (100, Announcement, None, 10),
            (101, Voice, None, -1),
        ]
        .map(|(id, kind, parent, position)| Channel {
            id: id.into(),
            kind,
            parent_id: parent.map(Into::into),
            position,
            ..Channel::default()
        });

        let rows = guild_channel_rows(channels.iter().rev().collect());
        let ids: Vec<_> = rows.iter().map(|channel| channel.id.0).collect();
        assert_eq!(ids, [100, 9, 101, 10, 11, 12, 20, 23, 21, 22, 30, 32, 31]);
    }
}
