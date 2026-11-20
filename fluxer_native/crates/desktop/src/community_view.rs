// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

pub(super) enum Dialog {
    Create,
    Leave(Snowflake),
}
pub(super) struct CommunityUi {
    pub dialog: Option<Dialog>,
    pub name: Entity<TextInput>,
    pub delete_messages: bool,
    pub pending: bool,
    pub error: Option<String>,
    pub focus: FocusHandle,
    pub previous_focus: Option<FocusHandle>,
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
        }
    }
    pub fn open_create(&mut self, cx: &mut Context<Fluxer>) {
        self.name.update(cx, |name, cx| name.set(String::new(), cx));
        self.dialog = Some(Dialog::Create);
        self.error = None;
    }
    pub fn open_leave(&mut self, id: Snowflake) {
        self.dialog = Some(Dialog::Leave(id));
        self.delete_messages = false;
        self.error = None;
    }
}
impl Fluxer {
    fn close_community_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.community_ui.dialog = None;
        if let Some(focus) = self.community_ui.previous_focus.take() {
            window.focus(&focus);
        }
        cx.notify();
    }
    pub(super) fn community_dialog(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(dialog) = &self.community_ui.dialog else {
            return div().into_any_element();
        };
        let create = matches!(dialog, Dialog::Create);
        let title = if create {
            "Create a community"
        } else {
            "Leave community"
        };
        let pending = self.community_ui.pending;
        div().absolute().inset_0().bg(rgba(0x00000099)).occlude().flex().items_center().justify_center().child(
            div().id("community-dialog").track_focus(&self.community_ui.focus).w(px(420.)).p_5().rounded_lg().bg(rgb(PANEL)).shadow_lg().flex().flex_col().gap_4()
                .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| { if event.keystroke.key == "escape" && !this.community_ui.pending { this.close_community_dialog(window, cx); cx.stop_propagation(); } }))
                .child(div().text_size(px(20.)).font_weight(FontWeight::BOLD).child(title))
                .when(create, |v| v.child("Give your community a name.").child(self.community_ui.name.clone()))
                .when(!create, |v| v.child("Leave this community? You won't see its messages anymore.").child(
                    controls::quiet_button("delete-community-messages", if self.community_ui.delete_messages { "☑ Delete my messages" } else { "☐ Delete my messages" }).on_click(cx.listener(|this, _, _, cx| { this.community_ui.delete_messages = !this.community_ui.delete_messages; cx.notify(); }))
                ).child(muted("Wipe every message you've sent here on the way out. Can't be undone.")))
                .when_some(self.community_ui.error.clone(), |v, error| v.child(div().text_color(rgb(0xe15059)).child(error)))
                .child(div().flex().justify_end().gap_2()
                    .child(controls::quiet_button("community-cancel", "Cancel").on_click(cx.listener(|this, _, window, cx| { if !this.community_ui.pending { this.close_community_dialog(window, cx); } })))
                    .child(controls::quiet_button("community-submit", if pending { "Please wait…" } else if create { "Create" } else { "Leave community" }).bg(rgb(if create { ACCENT } else { 0xc43b45 })).on_click(cx.listener(|this, _, _, cx| {
                        if this.community_ui.pending { return; }
                        let command = match this.community_ui.dialog { Some(Dialog::Create) => Command::CreateCommunity { name: this.community_ui.name.read(cx).content.to_string() }, Some(Dialog::Leave(guild_id)) => Command::LeaveCommunity { guild_id, delete_messages: this.community_ui.delete_messages }, None => return };
                        this.community_ui.pending = true; this.community_ui.error = None; this.command(command, cx); cx.notify();
                    }))))
        ).into_any_element()
    }
}
