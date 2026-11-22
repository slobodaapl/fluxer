// SPDX-License-Identifier: AGPL-3.0-or-later
//! The chat viewport, with scroll positions anchored to message IDs.
use fluxer_core::Snowflake;
use gpui::{FocusHandle, ListAlignment, ListOffset, ListState, Pixels, px};
use std::collections::BTreeMap;

pub(super) struct Timeline {
    pub list: ListState,
    channel: Option<Snowflake>,
    rows: Vec<Option<Snowflake>>,
    initial_top: Option<usize>,
}

impl Timeline {
    pub fn new() -> Self {
        Self {
            list: ListState::with_estimated_item_height(
                0,
                ListAlignment::Bottom,
                px(180.),
                px(80.),
            ),
            channel: None,
            rows: Vec::new(),
            initial_top: None,
        }
    }

    pub fn sync(
        &mut self,
        channel: Option<Snowflake>,
        messages: &[Snowflake],
        focus: &BTreeMap<Snowflake, FocusHandle>,
        footer: bool,
    ) {
        if self.channel != channel {
            self.channel = channel;
            self.rows.clear();
            self.list.reset(0);
        }
        let rows = messages
            .iter()
            .copied()
            .map(Some)
            .chain(footer.then_some(None))
            .collect::<Vec<_>>();
        if self.rows == rows {
            return;
        }
        let old_top = self.list.logical_scroll_top();
        let anchor = self.rows.get(old_top.item_ix).copied().flatten();
        let prefix = self
            .rows
            .iter()
            .zip(&rows)
            .take_while(|(a, b)| a == b)
            .count();
        let suffix = self.rows[prefix..]
            .iter()
            .rev()
            .zip(rows[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        self.list.splice_focusable(
            prefix..self.rows.len() - suffix,
            rows[prefix..rows.len() - suffix]
                .iter()
                .map(|id| id.and_then(|id| focus.get(&id).cloned())),
        );
        // When fetching or trimming a page, keep the message the reader was on.
        if let Some(anchor) = anchor
            && let Ok(item_ix) = messages.binary_search(&anchor)
        {
            self.list.scroll_to(ListOffset { item_ix, ..old_top });
        }
        self.rows = rows;
        if let Some(index) = self.initial_top.take() {
            self.scroll_to_top_of_item(index);
        }
    }

    pub fn near_bottom(&self, distance: Pixels) -> bool {
        self.list.logical_scroll_top().item_ix >= self.list.item_count()
            || self.list.max_offset_for_scrollbar().height
                + self.list.scroll_px_offset_for_scrollbar().y
                <= distance
    }

    pub fn scroll_to_bottom(&mut self) {
        self.initial_top = None;
        self.list.scroll_to(ListOffset {
            item_ix: self.list.item_count(),
            offset_in_item: px(0.),
        });
    }

    pub fn scroll_to_top_of_item(&mut self, index: usize) {
        if self.list.item_count() == 0 {
            self.initial_top = Some(index);
        } else {
            self.list.scroll_to(ListOffset {
                item_ix: index,
                offset_in_item: px(0.),
            });
        }
    }

    pub fn center_row(&self, row: gpui::Bounds<Pixels>) {
        let viewport = self.list.viewport_bounds();
        let delta = row.center().y - viewport.center().y;
        let max = self.list.max_offset_for_scrollbar().height;
        let offset = self
            .list
            .scroll_px_offset_for_scrollbar()
            .y
            .clamp(-max, px(0.));
        self.list
            .set_offset_from_scrollbar(gpui::point(px(0.), (offset - delta).clamp(-max, px(0.))));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paginating_preserves_the_visible_message_and_offset() {
        let mut timeline = Timeline::new();
        let channel = Some(Snowflake(1));
        let focus = BTreeMap::new();
        let page = (51..=100).map(Snowflake).collect::<Vec<_>>();
        timeline.sync(channel, &page, &focus, false);
        timeline.list.scroll_to(ListOffset {
            item_ix: 20,
            offset_in_item: px(17.),
        });

        let older = (1..=95).map(Snowflake).collect::<Vec<_>>();
        timeline.sync(channel, &older, &focus, false);
        let top = timeline.list.logical_scroll_top();
        assert_eq!(older[top.item_ix], Snowflake(71));
        assert_eq!(top.offset_in_item, px(17.));
    }

    #[test]
    fn new_conversations_and_messages_keep_bottom_alignment() {
        let mut timeline = Timeline::new();
        let focus = BTreeMap::new();
        let channel = Some(Snowflake(1));
        let messages = (1..=50).map(Snowflake).collect::<Vec<_>>();
        timeline.sync(channel, &messages, &focus, false);
        assert!(timeline.near_bottom(px(0.)));
        timeline.scroll_to_top_of_item(10);

        let other = Some(Snowflake(2));
        timeline.sync(other, &messages, &focus, false);
        assert!(timeline.near_bottom(px(0.)));
        timeline.scroll_to_bottom();
        timeline.sync(other, &messages, &focus, true);
        assert!(timeline.near_bottom(px(0.)));
        assert_eq!(timeline.list.logical_scroll_top().item_ix, 51);
    }

    #[test]
    fn unmeasured_messages_have_scrollable_estimated_height() {
        let mut timeline = Timeline::new();
        let messages = (1..=100).map(Snowflake).collect::<Vec<_>>();
        timeline.sync(Some(Snowflake(1)), &messages, &BTreeMap::new(), false);
        timeline.scroll_to_top_of_item(0);
        timeline.list.scroll_by(px(800.));
        assert_eq!(timeline.list.logical_scroll_top().item_ix, 10);
        assert!(!timeline.near_bottom(px(0.)));
    }
}
