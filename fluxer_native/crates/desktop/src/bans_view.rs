// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

pub(super) struct BansUi {
    pub guild: Option<Snowflake>,
    request: u64,
    rows: Vec<serde_json::Value>,
    selected: usize,
    confirm: bool,
    pending: bool,
    error: Option<String>,
    focus: FocusHandle,
    previous: Option<FocusHandle>,
}
impl BansUi {
    pub fn new(cx: &mut Context<Fluxer>) -> Self {
        Self {
            guild: None,
            request: 0,
            rows: vec![],
            selected: 0,
            confirm: false,
            pending: false,
            error: None,
            focus: cx.focus_handle(),
            previous: None,
        }
    }
    pub fn reset(&mut self) {
        self.guild = None;
        self.rows.clear();
        self.confirm = false;
        self.pending = false;
        self.error = None;
        self.previous = None;
        self.request = self.request.wrapping_add(1);
    }
}
impl Fluxer {
    pub(super) fn can_manage_bans(&self, guild: Snowflake) -> bool {
        !self.demo
            && self.state.current_user.as_ref().is_some_and(|user| {
                self.state.members.contains_key(&(guild, user.id))
                    && self
                        .state
                        .guild_permissions_for(guild, user.id)
                        .contains(fluxer_core::Permissions::BAN_MEMBERS)
            })
    }
    pub(super) fn open_bans(
        &mut self,
        guild: Snowflake,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.can_manage_bans(guild) {
            return;
        }
        self.bans_ui.reset();
        self.bans_ui.guild = Some(guild);
        self.bans_ui.previous = window.focused(cx);
        self.bans_ui.focus.focus(window);
        self.fetch_bans(cx);
    }
    fn send_bans_command(&mut self, command: Command, cx: &mut Context<Self>) {
        let result = self
            .controller
            .as_ref()
            .ok_or_else(|| "The native client worker is unavailable".to_owned())
            .and_then(|controller| controller.send(command));
        if let Err(error) = result {
            self.bans_ui.pending = false;
            self.bans_ui.confirm = false;
            self.bans_ui.rows.clear();
            self.bans_ui.error = Some(error);
        }
        cx.notify();
    }
    fn fetch_bans(&mut self, cx: &mut Context<Self>) {
        let Some(guild_id) = self.bans_ui.guild else {
            return;
        };
        self.bans_ui.request = self.bans_ui.request.wrapping_add(1);
        self.bans_ui.pending = true;
        self.bans_ui.error = None;
        self.send_bans_command(
            Command::FetchBans {
                request_id: self.bans_ui.request,
                guild_id,
            },
            cx,
        );
    }
    fn close_bans(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.bans_ui.pending && self.bans_ui.confirm {
            return;
        }
        self.command(Command::CancelBans, cx);
        if let Some(focus) = self.bans_ui.previous.take() {
            focus.focus(window);
        }
        self.bans_ui.reset();
        cx.notify();
    }
    fn unban_selected(&mut self, cx: &mut Context<Self>) {
        let Some(guild_id) = self.bans_ui.guild else {
            return;
        };
        let Some(user_id) = self
            .bans_ui
            .rows
            .get(self.bans_ui.selected)
            .and_then(|row| row["user"]["id"].as_str())
            .and_then(|id| id.parse::<u64>().ok())
            .map(Snowflake)
        else {
            return;
        };
        if !self.can_manage_bans(guild_id) {
            self.bans_ui.error = Some("You can no longer manage bans".into());
            return;
        }
        self.bans_ui.request = self.bans_ui.request.wrapping_add(1);
        self.bans_ui.pending = true;
        self.bans_ui.error = None;
        self.send_bans_command(
            Command::UnbanMember {
                request_id: self.bans_ui.request,
                guild_id,
                user_id,
            },
            cx,
        );
    }
    pub(super) fn bans_event(
        &mut self,
        request: u64,
        guild: Snowflake,
        result: Result<Vec<serde_json::Value>, String>,
        cx: &mut Context<Self>,
    ) {
        if self.bans_ui.guild != Some(guild) || self.bans_ui.request != request {
            return;
        }
        self.bans_ui.pending = false;
        match result {
            Ok(rows) => {
                self.bans_ui.rows = rows;
                self.bans_ui.selected = 0;
                self.bans_ui.confirm = false;
                self.bans_ui.error = None;
            }
            Err(error) => {
                self.bans_ui.rows.clear();
                self.bans_ui.error = Some(error);
                self.bans_ui.confirm = false;
            }
        }
        cx.notify();
    }
    pub(super) fn bans_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event.keystroke.key.as_str() {
            "escape" if self.bans_ui.confirm && !self.bans_ui.pending => {
                self.bans_ui.confirm = false;
                cx.notify();
            }
            "escape" => self.close_bans(window, cx),
            "up" if !self.bans_ui.confirm => {
                self.bans_ui.selected = self.bans_ui.selected.saturating_sub(1);
                cx.notify();
            }
            "down" if !self.bans_ui.confirm && !self.bans_ui.rows.is_empty() => {
                self.bans_ui.selected =
                    (self.bans_ui.selected + 1).min(self.bans_ui.rows.len() - 1);
                cx.notify();
            }
            "enter" if !event.is_held && !self.bans_ui.pending && self.bans_ui.error.is_some() => {
                self.fetch_bans(cx);
            }
            "enter" if !event.is_held && !self.bans_ui.pending && !self.bans_ui.rows.is_empty() => {
                if self.bans_ui.confirm {
                    self.unban_selected(cx);
                } else {
                    self.bans_ui.confirm = true;
                    cx.notify();
                }
            }
            _ => {}
        }
        cx.stop_propagation();
    }
    pub(super) fn bans_dialog(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut list = div()
            .id("bans-list")
            .h(px(240.))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_1();
        for (index, row) in self.bans_ui.rows.iter().enumerate() {
            let name = row["user"]["global_name"]
                .as_str()
                .or_else(|| row["user"]["username"].as_str())
                .unwrap_or("Unknown user")
                .to_owned();
            let reason = row["reason"]
                .as_str()
                .unwrap_or("No reason provided")
                .to_owned();
            let expires = row["expires_at"]
                .as_str()
                .map(|v| format!("Expires: {v}"))
                .unwrap_or_else(|| "Permanent".into());
            list = list.child(
                div()
                    .id(("ban-row", index))
                    .p_2()
                    .rounded_md()
                    .bg(rgb(if index == self.bans_ui.selected {
                        0x535064
                    } else {
                        0x303138
                    }))
                    .cursor_pointer()
                    .child(name)
                    .child(
                        div()
                            .text_color(rgb(0xaaaab3))
                            .child(format!("{reason} · {expires}")),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.bans_ui.pending && !this.bans_ui.confirm {
                            this.bans_ui.selected = index;
                            cx.notify();
                        }
                    })),
            );
        }
        let selected_name = self
            .bans_ui
            .rows
            .get(self.bans_ui.selected)
            .and_then(|row| {
                row["user"]["global_name"]
                    .as_str()
                    .or_else(|| row["user"]["username"].as_str())
            })
            .unwrap_or("this user");
        let mut panel = div()
            .id("bans-dialog")
            .track_focus(&self.bans_ui.focus)
            .w(px(480.))
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
                    .child("Community bans"),
            );
        if self.bans_ui.confirm {
            panel = panel.child(format!(
                "Revoke the ban for {selected_name}? They will be able to rejoin."
            ));
        } else if self.bans_ui.pending {
            panel = panel.child("Loading bans…");
        } else if self.bans_ui.rows.is_empty() && self.bans_ui.error.is_none() {
            panel = panel.child("No bans to show.");
        } else if !self.bans_ui.rows.is_empty() {
            panel = panel
                .child("Select a user to revoke their ban.")
                .child(list);
        }
        if let Some(error) = &self.bans_ui.error {
            panel = panel.child(div().text_color(rgb(0xe15059)).child(error.clone()));
            if !self.bans_ui.pending {
                panel = panel.child("Press Enter to retry, or Escape to close.");
            }
        }
        let mut buttons = div().flex().justify_end().gap_2().child(
            crate::controls::quiet_button(
                "bans-close",
                if self.bans_ui.confirm {
                    "Cancel"
                } else {
                    "Close"
                },
            )
            .on_click(cx.listener(|this, _, window, cx| {
                if this.bans_ui.confirm && !this.bans_ui.pending {
                    this.bans_ui.confirm = false;
                    cx.notify();
                } else {
                    this.close_bans(window, cx);
                }
            })),
        );
        if !self.bans_ui.pending && !self.bans_ui.rows.is_empty() {
            buttons = buttons.child(
                crate::controls::quiet_button(
                    "bans-revoke",
                    if self.bans_ui.confirm {
                        "Confirm revoke"
                    } else {
                        "Revoke ban"
                    },
                )
                .bg(rgb(0xc43b45))
                .on_click(cx.listener(|this, _, _, cx| {
                    if this.bans_ui.confirm {
                        this.unban_selected(cx);
                    } else {
                        this.bans_ui.confirm = true;
                        cx.notify();
                    }
                })),
            );
        } else if !self.bans_ui.pending && self.bans_ui.error.is_some() {
            buttons = buttons.child(
                crate::controls::quiet_button("bans-retry", "Retry")
                    .on_click(cx.listener(|this, _, _, cx| this.fetch_bans(cx))),
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
