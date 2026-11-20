// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::controls::{Icon, icon, icon_button, quiet_button};
use fluxer_core::Permissions;
use std::{cell::RefCell, rc::Rc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MessageMenuContext {
    Timeline,
    Preview,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MessageAction {
    React,
    Reply,
    StartThread,
    Forward,
    Copy,
    CopyLink,
    Jump,
    MarkUnread,
    Pin,
    Edit,
    Delete,
}
impl MessageAction {
    pub(super) fn label(self, pinned: bool) -> &'static str {
        match self {
            Self::React => "Add reaction",
            Self::Reply => "Reply",
            Self::StartThread => "Start thread",
            Self::Forward => "Forward",
            Self::Copy => "Copy text",
            Self::CopyLink => "Copy link to message",
            Self::Jump => "Jump to message",
            Self::MarkUnread => "Mark unread",
            Self::Pin => {
                if pinned {
                    "Unpin message"
                } else {
                    "Pin message"
                }
            }
            Self::Edit => "Edit message",
            Self::Delete => "Delete message",
        }
    }
    pub(super) fn icon(self) -> Icon {
        match self {
            Self::React => Icon::React,
            Self::Reply => Icon::Reply,
            Self::StartThread => Icon::Reply,
            Self::Forward => Icon::Forward,
            Self::Copy => Icon::Copy,
            Self::CopyLink => Icon::Copy,
            Self::Jump => Icon::Jump,
            Self::MarkUnread => Icon::MarkUnread,
            Self::Pin => Icon::Pin,
            Self::Edit => Icon::Edit,
            Self::Delete => Icon::Delete,
        }
    }
}

struct OpenMenu {
    message: Message,
    context: MessageMenuContext,
    anchor: Point<Pixels>,
    selected: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ConfirmationChoice {
    Cancel,
    Confirm,
}
#[derive(Default)]
pub(super) struct ConfirmationKeys {
    armed: Option<ConfirmationChoice>,
}
impl ConfirmationKeys {
    pub fn press(&mut self, choice: ConfirmationChoice, is_held: bool) {
        if !is_held {
            self.armed = Some(choice);
        }
    }
    pub fn release(&mut self) -> Option<ConfirmationChoice> {
        self.armed.take()
    }
    pub fn clear(&mut self) {
        self.armed = None;
    }
}
pub(super) struct MessageActions {
    menu: Option<OpenMenu>,
    deleting: Option<Message>,
    focus: FocusHandle,
    delete_cancel_focus: FocusHandle,
    delete_confirm_focus: FocusHandle,
    delete_key_target: ConfirmationKeys,
    previous_focus: Option<FocusHandle>,
    pub row_focus: BTreeMap<Snowflake, FocusHandle>,
    pub row_order: Vec<Snowflake>,
    pub row_bounds: Rc<RefCell<BTreeMap<Snowflake, Bounds<Pixels>>>>,
}
impl MessageActions {
    pub fn new(cx: &mut Context<Fluxer>) -> Self {
        Self {
            menu: None,
            deleting: None,
            focus: cx.focus_handle(),
            delete_cancel_focus: cx.focus_handle().tab_stop(true),
            delete_confirm_focus: cx.focus_handle().tab_stop(true),
            delete_key_target: ConfirmationKeys::default(),
            previous_focus: None,
            row_focus: BTreeMap::new(),
            row_order: Vec::new(),
            row_bounds: Rc::default(),
        }
    }
    pub fn is_open(&self) -> bool {
        self.menu.is_some() || self.deleting.is_some()
    }
    pub fn is_blocking(&self) -> bool {
        self.is_open()
    }
    pub fn reset(&mut self) {
        self.menu = None;
        self.deleting = None;
        self.previous_focus = None;
        self.delete_key_target.clear();
        self.row_focus.clear();
        self.row_order.clear();
        self.row_bounds.borrow_mut().clear();
    }
    pub fn active_message(&self) -> Option<Snowflake> {
        self.menu
            .as_ref()
            .map(|menu| menu.message.id)
            .or_else(|| self.deleting.as_ref().map(|message| message.id))
    }
}

impl Fluxer {
    pub(super) fn move_message_focus(
        &mut self,
        id: Snowflake,
        down: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self
            .message_actions
            .row_order
            .iter()
            .position(|candidate| *candidate == id)
        else {
            return;
        };
        let target = adjacent_row_index(index, self.message_actions.row_order.len(), down);
        if let Some(focus) = self
            .message_actions
            .row_order
            .get(target)
            .and_then(|id| self.message_actions.row_focus.get(id))
        {
            focus.focus(window);
            self.timeline.scroll_to_item(target);
            cx.notify();
        }
    }
    pub(super) fn message_bounds_marker(&self, id: Snowflake) -> AnyElement {
        let bounds = self.message_actions.row_bounds.clone();
        canvas(
            |_, _, _| {},
            move |rect, _, _, _| {
                bounds.borrow_mut().insert(id, rect);
            },
        )
        .absolute()
        .size_full()
        .into_any_element()
    }
    pub(super) fn message_menu_anchor(&self, id: Snowflake) -> Point<Pixels> {
        self.message_actions
            .row_bounds
            .borrow()
            .get(&id)
            .map(|bounds| point(bounds.right() - px(216.), bounds.top() + px(20.)))
            .unwrap_or(point(px(300.), px(120.)))
    }
    fn confirm_message_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(message) = self.message_actions.deleting.as_ref()
            && self.state.selected_channel == Some(message.channel_id)
            && self
                .message_action_options(message, MessageMenuContext::Timeline)
                .contains(&MessageAction::Delete)
        {
            self.command(
                Command::DeleteMessage {
                    message_id: message.id,
                },
                cx,
            );
        }
        self.dismiss_message_actions(window, cx);
    }
    pub(super) fn message_action_options(
        &self,
        message: &Message,
        context: MessageMenuContext,
    ) -> Vec<MessageAction> {
        let mut actions = Vec::new();
        let timeline = context == MessageMenuContext::Timeline
            && self.state.selected_channel == Some(message.channel_id);
        let channel = self.state.channels.get(&message.channel_id);
        let user = self.state.current_user.as_ref();
        let own = user.is_some_and(|user| user.id == message.author.id);
        let permissions = user
            .map(|user| self.state.permissions_for(message.channel_id, user.id))
            .unwrap_or_default();
        let system = channel.is_some_and(|channel| {
            channel.kind == ChannelType::Dm
                && channel
                    .recipients
                    .iter()
                    .any(|user| user.id == Snowflake(0))
        });
        let disabled = channel
            .and_then(|channel| channel.guild_id)
            .and_then(|id| self.state.guilds.get(&id))
            .and_then(|guild| guild.extra.get("disabled_operations"))
            .and_then(|value| value.as_u64().or_else(|| value.as_str()?.parse().ok()))
            .unwrap_or_default();
        let private = channel.is_some_and(|channel| channel.kind.is_private());
        if timeline && !system {
            if disabled & (1 << 5) == 0
                && (private || permissions.contains(Permissions::ADD_REACTIONS))
                && !self
                    .state
                    .relationships
                    .get(&message.author.id)
                    .is_some_and(|relationship| relationship.is_blocked())
            {
                actions.push(MessageAction::React);
            }
            if disabled & (1 << 4) == 0
                && channel.is_some_and(|channel| can_send_message(channel.kind, permissions))
            {
                actions.push(MessageAction::Reply);
            }
            if !self.demo
                && matches!(message.kind, 0 | 19)
                && message.flags & (1 << 5) == 0
                && self.state.selected_channel == Some(message.channel_id)
                && self.state.message_thread_type(message.channel_id).is_some()
            {
                actions.push(MessageAction::StartThread);
            }
            actions.push(MessageAction::Forward);
        }
        actions.push(MessageAction::Copy);
        if permissions.contains(Permissions::VIEW_CHANNEL)
            && self.message_link_for(message).is_some()
        {
            actions.push(MessageAction::CopyLink);
        }
        if permissions.contains(Permissions::VIEW_CHANNEL)
            && self
                .state
                .messages
                .get(&message.channel_id)
                .is_some_and(|messages| messages.contains_key(&message.id))
        {
            actions.push(MessageAction::MarkUnread);
        }
        if !timeline {
            actions.push(MessageAction::Jump);
        }
        if self.can_pin_message(message)
            && !self
                .search_ui
                .mutations
                .contains(&(message.channel_id, message.id))
        {
            actions.push(MessageAction::Pin);
        }
        if timeline && !system && disabled & (1 << 4) == 0 {
            if own && !self.message_pending() && !message.extra.contains_key("message_snapshots") {
                actions.push(MessageAction::Edit);
            }
            if deletable_message_type(message.kind)
                && (own || (!private && permissions.contains(Permissions::MANAGE_MESSAGES)))
            {
                actions.push(MessageAction::Delete);
            }
        }
        actions
    }

    fn message_link_for(&self, message: &Message) -> Option<String> {
        let channel = self.state.channels.get(&message.channel_id)?;
        self.message_links
            .message_link(channel.guild_id, channel.id, message.id)
    }

    pub(super) fn dismiss_message_actions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.message_actions.menu = None;
        self.message_actions.deleting = None;
        self.message_actions.delete_key_target.clear();
        if let Some(focus) = self.message_actions.previous_focus.take() {
            focus.focus(window);
        }
        cx.notify();
    }

    pub(super) fn open_message_menu(
        &mut self,
        message: Message,
        anchor: Point<Pixels>,
        context: MessageMenuContext,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.message_actions.previous_focus = window.focused(cx);
        self.message_actions.menu = Some(OpenMenu {
            message,
            context,
            anchor,
            selected: 0,
        });
        self.message_actions.deleting = None;
        self.message_actions.focus.focus(window);
        cx.notify();
    }

    pub(super) fn perform_message_action(
        &mut self,
        message: Message,
        action: MessageAction,
        anchor: Point<Pixels>,
        shift: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Permission and pending-operation changes are rechecked at activation.
        let context = if self.state.selected_channel == Some(message.channel_id) {
            MessageMenuContext::Timeline
        } else {
            MessageMenuContext::Preview
        };
        if !self
            .message_action_options(&message, context)
            .contains(&action)
        {
            return;
        }
        self.message_actions.menu = None;
        match action {
            MessageAction::React => {
                self.open_emoji(EmojiTarget::Reaction(message.id), anchor, window, cx)
            }
            MessageAction::Reply => {
                self.replies.insert(message.channel_id, (message, false));
                self.cancel_edit(cx);
                self.composer.focus_handle(cx).focus(window);
            }
            MessageAction::StartThread => {
                self.open_thread_form(message.channel_id, window, cx);
                self.thread_private = false;
                self.thread_message = Some(message.id);
            }
            MessageAction::Forward => {
                self.forwarding = Some(message);
                self.forward_search
                    .update(cx, |input, cx| input.set(String::new(), cx));
                self.forward_search.focus_handle(cx).focus(window);
                self.emoji_target = None;
            }
            MessageAction::Copy => {
                cx.write_to_clipboard(ClipboardItem::new_string(chat_helpers::display_text(
                    &message,
                )));
                if let Some(focus) = self.message_actions.previous_focus.take() {
                    focus.focus(window);
                }
            }
            MessageAction::CopyLink => {
                if let Some(link) = self.message_link_for(&message) {
                    cx.write_to_clipboard(ClipboardItem::new_string(link));
                }
                if let Some(focus) = self.message_actions.previous_focus.take() {
                    focus.focus(window);
                }
            }
            MessageAction::MarkUnread => {
                self.mark_message_unread(&message, cx);
                if let Some(focus) = self.message_actions.previous_focus.take() {
                    focus.focus(window);
                }
            }
            MessageAction::Jump => {
                self.jump_message(&message, cx);
                self.focus.focus(window);
            }
            MessageAction::Pin => {
                // Let the pin confirmation capture the originating row/control,
                // rather than this menu's soon-to-be-unmounted focus handle.
                if let Some(focus) = self.message_actions.previous_focus.take() {
                    focus.focus(window);
                }
                self.request_pin(message.clone(), !message.pinned, shift, window, cx)
            }
            MessageAction::Edit => {
                if self.editing.is_none() {
                    self.edit_draft = Some(self.composer.read(cx).content.to_string());
                }
                self.editing = Some(message.id);
                self.composer
                    .update(cx, |input, cx| input.set(message.content, cx));
                self.composer.focus_handle(cx).focus(window);
            }
            MessageAction::Delete => {
                if shift {
                    self.command(
                        Command::DeleteMessage {
                            message_id: message.id,
                        },
                        cx,
                    );
                    self.dismiss_message_actions(window, cx);
                } else {
                    self.message_actions.deleting = Some(message);
                    self.message_actions.delete_key_target.clear();
                    self.message_actions.delete_cancel_focus.focus(window);
                }
            }
        }
        cx.notify();
    }

    pub(super) fn message_toolbar(
        &self,
        message: &Message,
        focused: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let actions = self.message_action_options(message, MessageMenuContext::Timeline);
        let id = message.id;
        let group = SharedString::from(format!("message-{id}"));
        let persistent = focused
            || self.message_actions.active_message() == Some(id)
            || self.emoji_target == Some(EmojiTarget::Reaction(id));
        let mut toolbar = div()
            .id(("message-toolbar", id.0))
            .absolute()
            .top(px(-8.))
            .right(px(8.))
            .h(px(32.))
            .p(px(1.))
            .flex()
            .items_center()
            .rounded_md()
            .border_1()
            .border_color(rgb(0x5a526b))
            .bg(rgb(0x302b3d))
            .shadow_md()
            .invisible()
            .group_hover(group, |style| style.visible())
            .when(persistent, |toolbar| toolbar.visible());
        for action in [MessageAction::React, MessageAction::Reply] {
            if actions.contains(&action) {
                let message = message.clone();
                toolbar = toolbar.child(
                    icon_button(
                        (action.label(false), id.0),
                        action.icon(),
                        action.label(false),
                    )
                    .on_click(cx.listener(
                        move |this, event: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            this.perform_message_action(
                                message.clone(),
                                action,
                                event.position(),
                                event.modifiers().shift,
                                window,
                                cx,
                            );
                        },
                    )),
                );
            }
        }
        let message = message.clone();
        toolbar
            .child(
                icon_button(("message-more", id.0), Icon::More, "More actions").on_click(
                    cx.listener(move |this, event: &ClickEvent, window, cx| {
                        cx.stop_propagation();
                        this.open_message_menu(
                            message.clone(),
                            event.position(),
                            MessageMenuContext::Timeline,
                            window,
                            cx,
                        );
                    }),
                ),
            )
            .into_any_element()
    }

    pub(super) fn message_action_overlay(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if let Some(message) = self.message_actions.deleting.as_ref() {
            let message = message.clone();
            return div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(0x00000080))
                .occlude()
                .child(
                    div()
                        .id("delete-message-confirm")
                        .track_focus(&self.message_actions.focus)
                        .w(px(420.))
                        .max_w(window.viewport_size().width - px(32.))
                        .p_5()
                        .rounded_lg()
                        .bg(rgb(PANEL))
                        .border_1()
                        .border_color(rgb(0x4c4458))
                        .shadow_lg()
                        .capture_key_down(cx.listener(
                            move |this, event: &KeyDownEvent, window, cx| {
                                match event.keystroke.key.as_str() {
                                    "escape" => this.dismiss_message_actions(window, cx),
                                    "tab" => {
                                        this.message_actions.delete_key_target.clear();
                                        if this
                                            .message_actions
                                            .delete_cancel_focus
                                            .is_focused(window)
                                        {
                                            this.message_actions.delete_confirm_focus.focus(window);
                                        } else {
                                            this.message_actions.delete_cancel_focus.focus(window);
                                        }
                                    }
                                    "enter" | "space" if !event.keystroke.modifiers.modified() => {
                                        this.message_actions.delete_key_target.press(
                                            if this
                                                .message_actions
                                                .delete_confirm_focus
                                                .is_focused(window)
                                            {
                                                ConfirmationChoice::Confirm
                                            } else {
                                                ConfirmationChoice::Cancel
                                            },
                                            event.is_held,
                                        );
                                    }
                                    _ => return,
                                }
                                cx.stop_propagation();
                            },
                        ))
                        .capture_key_up(cx.listener(|this, event: &KeyUpEvent, window, cx| {
                            if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                                match this.message_actions.delete_key_target.release() {
                                    Some(ConfirmationChoice::Cancel) => {
                                        this.dismiss_message_actions(window, cx)
                                    }
                                    Some(ConfirmationChoice::Confirm) => {
                                        this.confirm_message_delete(window, cx)
                                    }
                                    None => {}
                                }
                                // The release that opened the dialog is unarmed. It must
                                // not reach GPUI's focusable-button key-up click handler.
                                cx.stop_propagation();
                            }
                        }))
                        .child(heading("Delete message?"))
                        .child(
                            div()
                                .mt_2()
                                .text_sm()
                                .text_color(rgb(MUTED))
                                .child("This message will be permanently deleted."),
                        )
                        .child(
                            div()
                                .my_4()
                                .p_3()
                                .rounded_md()
                                .bg(rgb(BG))
                                .text_sm()
                                .max_h(px(120.))
                                .overflow_hidden()
                                .child(chat_helpers::preview(&message)),
                        )
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .child(
                                    quiet_button("cancel-message-delete", "Cancel")
                                        .track_focus(&self.message_actions.delete_cancel_focus)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.dismiss_message_actions(window, cx)
                                        })),
                                )
                                .child(
                                    quiet_button("confirm-message-delete", "Delete")
                                        .track_focus(&self.message_actions.delete_confirm_focus)
                                        .bg(rgb(0xa43c4b))
                                        .text_color(rgb(0xffffff))
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.confirm_message_delete(window, cx);
                                        })),
                                ),
                        ),
                )
                .into_any_element();
        }
        let Some(menu) = self.message_actions.menu.as_ref() else {
            return div().into_any_element();
        };
        let actions = self.message_action_options(&menu.message, menu.context);
        let origin = menu_origin(menu.anchor, window.viewport_size(), actions.len());
        let anchor = menu.anchor;
        let context = menu.context;
        let rows = actions.iter().copied().enumerate().map(|(index, action)| {
            let message = menu.message.clone();
            div()
                .id(("message-menu-action", index))
                .h(px(32.))
                .px_2()
                .flex()
                .items_center()
                .gap_3()
                .rounded_md()
                .cursor_pointer()
                .text_size(px(13.))
                .text_color(rgb(if action == MessageAction::Delete {
                    0xef9b9f
                } else {
                    TEXT
                }))
                .when(index == menu.selected, |row| row.bg(rgb(0x443953)))
                .hover(|row| row.bg(rgb(0x443953)))
                .child(icon(action.icon(), 16.))
                .child(action.label(message.pinned))
                .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                    if *hovered {
                        if let Some(menu) = this.message_actions.menu.as_mut() {
                            menu.selected = index;
                        }
                        cx.notify();
                    }
                }))
                .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                    cx.stop_propagation();
                    this.perform_message_action(
                        message.clone(),
                        action,
                        anchor,
                        event.modifiers().shift,
                        window,
                        cx,
                    );
                }))
                .into_any_element()
        });
        let popup = div()
            .id("message-context-menu")
            .track_focus(&self.message_actions.focus)
            .occlude()
            .w(px(216.))
            .p_1()
            .rounded_lg()
            .border_1()
            .border_color(rgb(0x4c4458))
            .bg(rgb(PANEL))
            .shadow_lg()
            .on_mouse_down_out(
                cx.listener(|this, _, window, cx| this.dismiss_message_actions(window, cx)),
            )
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    this.dismiss_message_actions(window, cx);
                    cx.stop_propagation();
                    return;
                }
                if event.keystroke.key == "tab" {
                    this.dismiss_message_actions(window, cx);
                    if event.keystroke.modifiers.shift {
                        window.focus_prev();
                    } else {
                        window.focus_next();
                    }
                    cx.stop_propagation();
                    return;
                }
                let Some(menu) = this.message_actions.menu.as_ref() else {
                    return;
                };
                let actions = this.message_action_options(&menu.message, context);
                let count = actions.len();
                if count == 0 {
                    return;
                }
                match event.keystroke.key.as_str() {
                    "up" | "down" | "home" | "end" => {
                        let menu = this.message_actions.menu.as_mut().expect("open menu");
                        menu.selected =
                            menu_selection(menu.selected, count, event.keystroke.key.as_str());
                        cx.notify();
                    }
                    "enter" | "space" => {
                        let action = actions[menu.selected.min(count - 1)];
                        this.perform_message_action(
                            menu.message.clone(),
                            action,
                            menu.anchor,
                            event.keystroke.modifiers.shift,
                            window,
                            cx,
                        );
                    }
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .children(rows);
        deferred(
            anchored()
                .position(origin)
                .anchor(Corner::TopLeft)
                .child(popup),
        )
        .with_priority(3)
        .into_any_element()
    }
}

fn menu_selection(current: usize, count: usize, key: &str) -> usize {
    if count == 0 {
        return 0;
    }
    match key {
        "up" => (current + count - 1) % count,
        "down" => (current + 1) % count,
        "home" => 0,
        "end" => count - 1,
        _ => current.min(count - 1),
    }
}
fn adjacent_row_index(current: usize, count: usize, down: bool) -> usize {
    if down {
        current.saturating_add(1).min(count.saturating_sub(1))
    } else {
        current.saturating_sub(1)
    }
}
// ChannelConstants.ts MESSAGE_TYPE_DELETABLE: service/system events which the
// original client disallows cannot acquire Delete through the overflow menu.
fn deletable_message_type(kind: u16) -> bool {
    matches!(kind, 0 | 6 | 7 | 12 | 18 | 19)
}
fn menu_origin(anchor: Point<Pixels>, viewport: Size<Pixels>, rows: usize) -> Point<Pixels> {
    point(
        anchor
            .x
            .max(px(8.))
            .min((viewport.width - px(224.)).max(px(8.))),
        anchor
            .y
            .max(px(8.))
            .min((viewport.height - px(rows as f32 * 32. + 16.)).max(px(8.))),
    )
}

pub(super) fn message_time(timestamp: &str) -> String {
    use chrono::{DateTime, Local};
    let Ok(date) = DateTime::parse_from_rfc3339(timestamp) else {
        return timestamp.to_string();
    };
    let date = date.with_timezone(&Local);
    let today = Local::now().date_naive();
    let prefix = if date.date_naive() == today {
        "Today".into()
    } else if today.pred_opt() == Some(date.date_naive()) {
        "Yesterday".into()
    } else {
        date.format("%b %-d, %Y").to_string()
    };
    format!("{prefix} at {}", date.format("%H:%M"))
}

#[cfg(test)]
mod tests {
    use super::{
        ConfirmationChoice, ConfirmationKeys, adjacent_row_index, deletable_message_type,
        menu_origin, menu_selection,
    };
    use gpui::{point, px, size};
    #[test]
    fn menu_keyboard_wraps_and_handles_empty() {
        assert_eq!(menu_selection(0, 4, "up"), 3);
        assert_eq!(menu_selection(3, 4, "down"), 0);
        assert_eq!(menu_selection(2, 4, "home"), 0);
        assert_eq!(menu_selection(1, 4, "end"), 3);
        assert_eq!(menu_selection(3, 0, "down"), 0);
    }
    #[test]
    fn menu_fits_small_windows_at_bottom_right() {
        let origin = menu_origin(point(px(870.), px(590.)), size(px(880.), px(600.)), 8);
        assert_eq!(origin, point(px(656.), px(328.)));
        let origin = menu_origin(point(px(-40.), px(-1.)), size(px(880.), px(600.)), 3);
        assert_eq!(origin, point(px(8.), px(8.)));
    }
    #[test]
    fn delete_menu_matches_source_message_type_allowlist() {
        for kind in [0, 6, 7, 12, 18, 19] {
            assert!(deletable_message_type(kind));
        }
        for kind in [1, 2, 3, 4, 5, 21, 99, 65535] {
            assert!(!deletable_message_type(kind));
        }
    }
    #[test]
    fn timeline_arrows_keep_edges_and_move_one_row() {
        assert_eq!(adjacent_row_index(0, 4, false), 0);
        assert_eq!(adjacent_row_index(3, 4, true), 3);
        assert_eq!(adjacent_row_index(2, 4, false), 1);
        assert_eq!(adjacent_row_index(1, 4, true), 2);
        assert_eq!(adjacent_row_index(0, 0, true), 0);
    }
    #[test]
    fn confirmation_requires_its_own_press_and_preserves_cancel_choice() {
        let mut keys = ConfirmationKeys::default();
        assert_eq!(
            keys.release(),
            None,
            "opening menu key release must not activate a modal button"
        );
        keys.press(ConfirmationChoice::Cancel, false);
        keys.press(ConfirmationChoice::Confirm, true);
        assert_eq!(
            keys.release(),
            Some(ConfirmationChoice::Cancel),
            "a held key cannot rearm the destructive choice"
        );
        assert_eq!(keys.release(), None);
        keys.press(ConfirmationChoice::Confirm, false);
        keys.clear();
        assert_eq!(
            keys.release(),
            None,
            "changing focus by Tab cancels pending activation"
        );
        keys.press(ConfirmationChoice::Confirm, false);
        assert_eq!(keys.release(), Some(ConfirmationChoice::Confirm));
    }
}
