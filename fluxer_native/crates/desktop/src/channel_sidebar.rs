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
    fn move_payload_clamps_text_and_voice_and_keeps_category_blocks() {
        use ChannelType::*;
        let channels = [
            (10, Category, None, 0),
            (11, Text, Some(10), 0),
            (12, Voice, Some(10), 1),
            (20, Category, None, 1),
            (21, Text, Some(20), 0),
            (22, Voice, Some(20), 1),
        ]
        .map(|(id, kind, parent, position)| Channel {
            id: id.into(),
            kind,
            parent_id: parent.map(Into::into),
            position,
            ..Channel::default()
        });
        let rows: Vec<_> = channels.iter().collect();
        // Appending text to a category places it before the first voice.
        assert_eq!(
            move_operation(&rows, 11.into(), None, Some(20.into()), false),
            Some((Some(20.into()), Some(21.into()), 1))
        );
        // Dropping voice before text clamps it after text.
        assert_eq!(
            move_operation(&rows, 12.into(), Some(21.into()), None, false),
            Some((Some(20.into()), Some(21.into()), 1))
        );
        // Moving a category carries its children as a block.
        assert_eq!(
            move_operation(&rows, 10.into(), None, None, true),
            Some((None, Some(20.into()), 1))
        );
        assert_eq!(
            move_operation(&rows, 20.into(), Some(10.into()), None, false),
            Some((None, None, 0))
        );
        assert_eq!(
            move_operation(&rows, 11.into(), Some(12.into()), None, false),
            None
        );
    }

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

/// Build the classic sibling-based move payload for a compact native menu.
/// `before` targets a row; `inside` appends to a category; neither means root start.
pub(super) fn move_operation(
    channels: &[&Channel],
    id: fluxer_core::Snowflake,
    before: Option<fluxer_core::Snowflake>,
    inside: Option<fluxer_core::Snowflake>,
    end: bool,
) -> Option<(
    Option<fluxer_core::Snowflake>,
    Option<fluxer_core::Snowflake>,
    usize,
)> {
    let dragged = *channels.iter().find(|c| c.id == id)?;
    let category = dragged.kind == ChannelType::Category;
    let mut roots: Vec<_> = channels
        .iter()
        .copied()
        .filter(|c| {
            c.parent_id
                .is_none_or(|parent| !channels.iter().any(|candidate| candidate.id == parent))
        })
        .collect();
    roots.sort_by_key(|c| (c.position, c.id.to_string()));
    let mut ordered = Vec::new();
    for root in roots {
        ordered.push(root);
        if root.kind == ChannelType::Category {
            let mut children: Vec<_> = channels
                .iter()
                .copied()
                .filter(|c| c.parent_id == Some(root.id))
                .collect();
            children.sort_by_key(|c| (c.kind.is_voice(), c.position, c.id.to_string()));
            ordered.extend(children);
        }
    }
    let mut remaining: Vec<_> = channels
        .iter()
        .copied()
        .filter(|c| !ordered.iter().any(|o| o.id == c.id))
        .collect();
    remaining.sort_by_key(|c| (c.position, c.id.to_string()));
    ordered.extend(remaining);
    let current_index = ordered.iter().position(|c| c.id == id)?;
    let current_preceding = ordered[..current_index]
        .iter()
        .rev()
        .find(|c| c.parent_id == dragged.parent_id)
        .map(|c| c.id);
    let base: Vec<_> = ordered
        .iter()
        .copied()
        .filter(|c| c.id != id && !(category && c.parent_id == Some(id)))
        .collect();
    let mut parent = if category {
        None
    } else if let Some(parent) = inside {
        Some(parent)
    } else if let Some(target) = before {
        base.iter().find(|c| c.id == target)?.parent_id
    } else {
        None
    };
    let mut index = if let Some(target) = before {
        base.iter().position(|c| c.id == target)?
    } else if let Some(target) = inside {
        let start = base
            .iter()
            .position(|c| c.id == target && c.kind == ChannelType::Category)?;
        let mut end = start + 1;
        while end < base.len() && base[end].parent_id == Some(target) {
            end += 1;
        }
        end
    } else {
        parent = None;
        if end { base.len() } else { 0 }
    };
    if let Some(parent_id) = parent {
        if dragged.kind.is_voice() {
            let min = base
                .iter()
                .enumerate()
                .filter(|(_, c)| c.parent_id == Some(parent_id) && !c.kind.is_voice())
                .map(|(i, _)| i + 1)
                .max()
                .or_else(|| base.iter().position(|c| c.id == parent_id).map(|i| i + 1))?;
            index = index.max(min);
        } else if let Some(first_voice) = base
            .iter()
            .position(|c| c.parent_id == Some(parent_id) && c.kind.is_voice())
        {
            index = index.min(first_voice);
        }
    }
    let preceding = base[..index]
        .iter()
        .rev()
        .find(|c| c.parent_id == parent)
        .map(|c| c.id);
    if dragged.parent_id == parent && current_preceding == preceding {
        return None;
    }
    let position = match preceding {
        None => 0,
        Some(preceding) => {
            base.iter()
                .filter(|c| c.parent_id == parent)
                .position(|c| c.id == preceding)?
                + 1
        }
    };
    Some((parent, preceding, position))
}
