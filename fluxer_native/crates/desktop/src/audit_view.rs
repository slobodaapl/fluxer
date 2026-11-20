// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use serde_json::Value;

pub(super) struct AuditUi {
    pub guild: Option<Snowflake>,
    request: u64,
    rows: Vec<Value>,
    users: Vec<Value>,
    pending: bool,
    error: Option<String>,
    more: bool,
    focus: FocusHandle,
    previous: Option<FocusHandle>,
}
impl AuditUi {
    pub fn new(cx: &mut Context<Fluxer>) -> Self {
        Self {
            guild: None,
            request: 0,
            rows: vec![],
            users: vec![],
            pending: false,
            error: None,
            more: false,
            focus: cx.focus_handle(),
            previous: None,
        }
    }
    pub fn reset(&mut self) {
        self.guild = None;
        self.rows.clear();
        self.users.clear();
        self.pending = false;
        self.error = None;
        self.more = false;
        self.previous = None;
        self.request = self.request.wrapping_add(1);
    }
}
impl Fluxer {
    pub(super) fn can_view_audit(&self, guild: Snowflake) -> bool {
        !self.demo
            && self.state.current_user.as_ref().is_some_and(|user| {
                self.state.members.contains_key(&(guild, user.id))
                    && self
                        .state
                        .guild_permissions_for(guild, user.id)
                        .contains(fluxer_core::Permissions::VIEW_AUDIT_LOG)
            })
    }
    pub(super) fn open_audit(
        &mut self,
        guild: Snowflake,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.can_view_audit(guild) {
            return;
        }
        self.audit_ui.reset();
        self.audit_ui.guild = Some(guild);
        self.audit_ui.previous = window.focused(cx);
        self.audit_ui.focus.focus(window);
        self.fetch_audit(false, cx);
    }
    fn fetch_audit(&mut self, next: bool, cx: &mut Context<Self>) {
        let Some(guild_id) = self.audit_ui.guild else {
            return;
        };
        if self.audit_ui.pending {
            return;
        }
        let before = if next {
            self.audit_ui
                .rows
                .last()
                .and_then(|v| v["id"].as_str())
                .map(str::to_owned)
        } else {
            None
        };
        if next && before.is_none() {
            return;
        }
        self.audit_ui.request = self.audit_ui.request.wrapping_add(1);
        self.audit_ui.pending = true;
        self.audit_ui.error = None;
        let command = Command::FetchAuditLog {
            request_id: self.audit_ui.request,
            guild_id,
            before,
        };
        let result = self
            .controller
            .as_ref()
            .ok_or_else(|| "The native client worker is unavailable".to_owned())
            .and_then(|c| c.send(command));
        if let Err(error) = result {
            self.audit_ui.pending = false;
            self.audit_ui.error = Some(error);
        }
        cx.notify();
    }
    fn close_audit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.command(Command::CancelAuditLog, cx);
        if let Some(focus) = self.audit_ui.previous.take() {
            focus.focus(window);
        }
        self.audit_ui.reset();
        cx.notify();
    }
    pub(super) fn audit_event(
        &mut self,
        request: u64,
        guild: Snowflake,
        result: Result<Value, String>,
        cx: &mut Context<Self>,
    ) {
        if self.audit_ui.guild != Some(guild) || self.audit_ui.request != request {
            return;
        }
        self.audit_ui.pending = false;
        if !self.can_view_audit(guild) {
            self.audit_ui.rows.clear();
            self.audit_ui.users.clear();
            self.audit_ui.more = false;
            self.audit_ui.error = Some("You can no longer view the audit log".into());
            cx.notify();
            return;
        }
        match result {
            Ok(value) => {
                let rows = value["audit_log_entries"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                self.audit_ui.more = rows.len() == 50;
                self.audit_ui.rows.extend(rows);
                self.audit_ui
                    .users
                    .extend(value["users"].as_array().cloned().unwrap_or_default());
                self.audit_ui.error = None;
            }
            Err(error) => self.audit_ui.error = Some(error),
        }
        cx.notify();
    }
    pub(super) fn audit_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.keystroke.key == "escape" {
            self.close_audit(window, cx);
        }
        cx.stop_propagation();
    }
    pub(super) fn audit_dialog(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut list = div()
            .id("audit-list")
            .h(px(360.))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_1();
        for row in &self.audit_ui.rows {
            let actor_id = row["user_id"].as_str().unwrap_or("");
            let actor = self
                .audit_ui
                .users
                .iter()
                .find(|u| u["id"].as_str() == Some(actor_id))
                .and_then(|u| u["global_name"].as_str().or_else(|| u["username"].as_str()))
                .unwrap_or(if actor_id.is_empty() {
                    "System"
                } else {
                    actor_id
                });
            let action = action_name(row["action_type"].as_u64().unwrap_or(0));
            let target_id = row["target_id"].as_str().unwrap_or("—");
            let target = self
                .audit_ui
                .users
                .iter()
                .find(|u| u["id"].as_str() == Some(target_id))
                .and_then(|u| u["global_name"].as_str().or_else(|| u["username"].as_str()))
                .unwrap_or(target_id);
            let reason = row["reason"]
                .as_str()
                .map(|r| format!(" · {r}"))
                .unwrap_or_default();
            list = list.child(
                div()
                    .p_2()
                    .rounded_md()
                    .bg(rgb(0x303138))
                    .child(format!("{actor} · {action} · {target}"))
                    .child(div().text_color(rgb(0xaaaab3)).child(format!(
                        "Entry {}{reason}",
                        row["id"].as_str().unwrap_or("—")
                    ))),
            );
        }
        let mut panel = div()
            .id("audit-dialog")
            .track_focus(&self.audit_ui.focus)
            .w(px(560.))
            .p_5()
            .rounded_lg()
            .bg(rgb(PANEL))
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_size(px(20.))
                    .font_weight(FontWeight::BOLD)
                    .child("Audit log"),
            );
        if self.audit_ui.rows.is_empty() && !self.audit_ui.pending && self.audit_ui.error.is_none()
        {
            panel = panel.child("No audit log entries.");
        } else {
            panel = panel.child(list);
        }
        if self.audit_ui.pending {
            panel = panel.child("Loading audit log…");
        }
        if let Some(error) = &self.audit_ui.error {
            panel = panel.child(div().text_color(rgb(0xe15059)).child(error.clone()));
        }
        let mut buttons = div().flex().justify_end().gap_2().child(
            crate::controls::quiet_button("audit-close", "Close")
                .on_click(cx.listener(|this, _, window, cx| this.close_audit(window, cx))),
        );
        if !self.audit_ui.pending && (self.audit_ui.more || self.audit_ui.error.is_some()) {
            let next = !self.audit_ui.rows.is_empty();
            buttons = buttons.child(
                crate::controls::quiet_button(
                    "audit-more",
                    if self.audit_ui.error.is_some() {
                        "Retry"
                    } else {
                        "Load older"
                    },
                )
                .on_click(cx.listener(move |this, _, _, cx| this.fetch_audit(next, cx))),
            );
        }
        div()
            .absolute()
            .inset_0()
            .bg(rgba(0x00000099))
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .child(panel.child(buttons))
            .into_any_element()
    }
}
fn action_name(n: u64) -> String {
    let name = match n {
        1 => "Updated community",
        10 => "Created channel",
        11 => "Updated channel",
        12 => "Deleted channel",
        13 => "Created channel permission",
        14 => "Updated channel permission",
        15 => "Deleted channel permission",
        20 => "Kicked member",
        21 => "Pruned members",
        22 => "Banned member",
        23 => "Unbanned member",
        24 => "Updated member",
        25 => "Updated member roles",
        26 => "Moved member",
        27 => "Disconnected member",
        28 => "Added bot",
        30 => "Created role",
        31 => "Updated role",
        32 => "Deleted role",
        40 => "Created invite",
        41 => "Updated invite",
        42 => "Deleted invite",
        50 => "Created webhook",
        51 => "Updated webhook",
        52 => "Deleted webhook",
        60 => "Created emoji",
        61 => "Updated emoji",
        62 => "Deleted emoji",
        72 => "Deleted message",
        73 => "Bulk deleted messages",
        74 => "Pinned message",
        75 => "Unpinned message",
        90 => "Created sticker",
        91 => "Updated sticker",
        92 => "Deleted sticker",
        110 => "Created thread",
        111 => "Updated thread",
        112 => "Deleted thread",
        _ => return format!("Action {n}"),
    };
    name.into()
}
