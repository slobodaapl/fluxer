// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Dialog {
    Create,
    Overview(Snowflake),
    Leave(Snowflake),
}
impl Dialog {
    pub(super) fn edits_name(self) -> bool {
        matches!(self, Self::Create | Self::Overview(_))
    }
}
pub(super) struct CommunityUi {
    pub dialog: Option<Dialog>,
    pub name: Entity<TextInput>,
    pub delete_messages: bool,
    pub pending: bool,
    pub error: Option<String>,
    pub focus: FocusHandle,
    pub previous_focus: Option<FocusHandle>,
    pub window: Option<AnyWindowHandle>,
    pub pending_dialog: Option<Dialog>,
    pub selected: usize,
    pub armed: bool,
}
impl CommunityUi {
    pub fn new(cx: &mut Context<Fluxer>) -> Self {
        Self {
            dialog: None,
            name: cx.new(|cx| TextInput::new("Community name", cx)),
            delete_messages: false,
            pending: false,
            error: None,
            focus: cx.focus_handle(),
            previous_focus: None,
            window: None,
            pending_dialog: None,
            selected: 0,
            armed: false,
        }
    }
    pub fn reset(&mut self, cx: &mut Context<Fluxer>) {
        self.dialog = None;
        self.pending_dialog = None;
        self.pending = false;
        self.delete_messages = false;
        self.error = None;
        self.armed = false;
        self.selected = 0;
        self.window = None;
        self.previous_focus = None;
        self.name
            .update(cx, |input, cx| input.set(String::new(), cx));
    }
    pub fn open_create(&mut self, cx: &mut Context<Fluxer>) {
        self.name.update(cx, |name, cx| name.set(String::new(), cx));
        self.dialog = Some(Dialog::Create);
        self.selected = 0;
        self.armed = false;
        self.error = None;
    }
    pub fn open_overview(&mut self, id: Snowflake, name: String, cx: &mut Context<Fluxer>) {
        self.name.update(cx, |input, cx| input.set(name, cx));
        self.dialog = Some(Dialog::Overview(id));
        self.selected = 0;
        self.armed = false;
        self.error = None;
    }
    pub fn open_leave(&mut self, id: Snowflake) {
        self.dialog = Some(Dialog::Leave(id));
        self.selected = 0;
        self.armed = false;
        self.delete_messages = false;
        self.error = None;
    }
}
impl Fluxer {
    fn close_community_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.community_ui.dialog = None;
        self.community_ui.window = None;
        self.community_ui.armed = false;
        if let Some(focus) = self.community_ui.previous_focus.take() {
            window.focus(&focus);
        }
        cx.notify();
    }
    pub(super) fn submit_community(&mut self, cx: &mut Context<Self>) {
        if self.community_ui.pending {
            return;
        }
        let command = match self.community_ui.dialog {
            Some(Dialog::Create) => Command::CreateCommunity {
                name: self.community_ui.name.read(cx).content.to_string(),
            },
            Some(Dialog::Overview(guild_id)) => Command::RenameCommunity {
                guild_id,
                name: self.community_ui.name.read(cx).content.to_string(),
            },
            Some(Dialog::Leave(guild_id)) => Command::LeaveCommunity {
                guild_id,
                delete_messages: self.community_ui.delete_messages,
            },
            None => return,
        };
        self.community_ui.pending = true;
        self.community_ui.pending_dialog = self.community_ui.dialog;
        self.community_ui.error = None;
        self.command(command, cx);
        cx.notify();
    }
    pub(super) fn community_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(dialog) = self.community_ui.dialog else {
            return;
        };
        if dialog.edits_name() && self.community_ui.name.focus_handle(cx).is_focused(window) {
            self.community_ui.selected = 0;
        }
        match event.keystroke.key.as_str() {
            "escape" if !self.community_ui.pending => self.close_community_dialog(window, cx),
            "tab" if !self.community_ui.pending => {
                self.community_ui.selected = if event.keystroke.modifiers.shift {
                    (self.community_ui.selected + 2) % 3
                } else {
                    (self.community_ui.selected + 1) % 3
                };
                self.community_ui.armed = false;
                if dialog.edits_name() && self.community_ui.selected == 0 {
                    window.focus(&self.community_ui.name.focus_handle(cx));
                } else {
                    window.focus(&self.community_ui.focus);
                }
                cx.notify();
            }
            "enter" | "space" if !event.is_held && !event.keystroke.modifiers.modified() => {
                if dialog.edits_name()
                    && self.community_ui.selected == 0
                    && (event.keystroke.key == "space"
                        || self.community_ui.name.read(cx).is_composing())
                {
                    return;
                }
                self.community_ui.armed = true;
            }
            _ => return,
        }
        cx.stop_propagation();
    }
    pub(super) fn community_key_up(
        &mut self,
        event: &KeyUpEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(dialog) = self.community_ui.dialog else {
            return;
        };
        if !matches!(event.keystroke.key.as_str(), "enter" | "space") {
            return;
        }
        let armed = std::mem::take(&mut self.community_ui.armed);
        if armed && !event.keystroke.modifiers.modified() && !self.community_ui.pending {
            match (dialog, self.community_ui.selected) {
                (_, 2) | (Dialog::Create | Dialog::Overview(_), 0) => self.submit_community(cx),
                (Dialog::Create | Dialog::Overview(_), 1) | (Dialog::Leave(_), 0) => {
                    self.close_community_dialog(window, cx)
                }
                (Dialog::Leave(_), 1) => {
                    self.community_ui.delete_messages = !self.community_ui.delete_messages;
                    cx.notify();
                }
                _ => {}
            }
        }
        cx.stop_propagation();
    }
    pub(super) fn community_dialog(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(dialog) = &self.community_ui.dialog else {
            return div().into_any_element();
        };
        let edits_name = dialog.edits_name();
        let (title, submit) = match dialog {
            Dialog::Create => ("Create a community", "Create"),
            Dialog::Overview(_) => ("Community Overview", "Save changes"),
            Dialog::Leave(_) => ("Leave community", "Leave community"),
        };
        let pending = self.community_ui.pending;
        div().absolute().inset_0().bg(rgba(0x00000099)).occlude().flex().items_center().justify_center().child(
            div().id("community-dialog").track_focus(&self.community_ui.focus).w(px(420.)).p_5().rounded_lg().bg(rgb(PANEL)).shadow_lg().flex().flex_col().gap_4()
                .child(div().text_size(px(20.)).font_weight(FontWeight::BOLD).child(title))
                .when(edits_name, |v| {
                    let label = match dialog {
                        Dialog::Create => "Give your community a name.",
                        _ => "Community name",
                    };
                    v.child(label).child(self.community_ui.name.clone())
                })
                .when(!edits_name, |v| v.child("Leave this community? You won't see its messages anymore.").child(
                    controls::quiet_button("delete-community-messages", if self.community_ui.delete_messages { "☑ Delete my messages" } else { "☐ Delete my messages" }).bg(rgb(if self.community_ui.selected == 1 { 0x535064 } else { 0x2b2d31 })).on_click(cx.listener(|this, _, _, cx| { this.community_ui.delete_messages = !this.community_ui.delete_messages; cx.notify(); }))
                ).child(muted("Wipe every message you've sent here on the way out. Can't be undone.")))
                .when_some(self.community_ui.error.clone(), |v, error| v.child(div().text_color(rgb(0xe15059)).child(error)))
                .child(div().flex().justify_end().gap_2()
                    .child(controls::quiet_button("community-cancel", "Cancel").bg(rgb(if self.community_ui.selected == if edits_name { 1 } else { 0 } { 0x535064 } else { 0x2b2d31 })).on_click(cx.listener(|this, _, window, cx| { if !this.community_ui.pending { this.close_community_dialog(window, cx); } })))
                    .child(controls::quiet_button("community-submit", if pending { "Please wait…" } else { submit }).bg(rgb(if self.community_ui.selected == 2 { 0x535064 } else if edits_name { ACCENT } else { 0xc43b45 })).on_click(cx.listener(|this, _, _, cx| this.submit_community(cx)))))
        ).into_any_element()
    }
}
