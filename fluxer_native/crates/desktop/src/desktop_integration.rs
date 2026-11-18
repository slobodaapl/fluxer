// SPDX-License-Identifier: AGPL-3.0-or-later
//! Keep the original window/client alive when hiding; quit remains an explicit action.
use super::*;
use crate::desktop_tray::{Action, DesktopTray, Snapshot};
use std::time::Instant;

pub(super) struct DesktopState {
    tray: DesktopTray,
    window: AnyWindowHandle,
    hidden: bool,
    startup_decided: bool,
    started: Instant,
    previous_status: String,
}
impl DesktopState {
    pub fn new(demo: bool, window: &Window) -> Self {
        let demo_tray = std::env::args().any(|s| s == "--demo-tray");
        Self {
            tray: DesktopTray::new(!demo || demo_tray, !demo),
            window: window.window_handle(),
            hidden: false,
            startup_decided: false,
            started: Instant::now(),
            previous_status: String::new(),
        }
    }
}
impl Fluxer {
    pub(super) fn desktop_tray_status(&self) -> String {
        if !self.interface.window_behavior().show_tray_icon {
            "The system tray icon is off. Closing the window quits Fluxer Native.".into()
        } else {
            self.desktop.tray.status()
        }
    }
    pub(super) fn request_window_close(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        !(self.interface.window_behavior().close_to_tray && self.hide_to_tray(window, cx))
    }
    fn hide_to_tray(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.interface.window_behavior().show_tray_icon
            && self.desktop.tray.available()
            && window.hide_window()
        {
            self.desktop.hidden = true;
            self.desktop.startup_decided = true;
            cx.notify();
            true
        } else {
            false
        }
    }
    fn show_from_tray(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.desktop.hidden || window.show_window() {
            self.desktop.hidden = false;
            self.desktop.startup_decided = true;
            window.activate_window();
            window.refresh();
            cx.notify();
        }
    }
    pub(super) fn poll_desktop(&mut self, cx: &mut Context<Self>) {
        let preferences = self.interface.window_behavior();
        let mut actions = Vec::new();
        if self.desktop.hidden && (!preferences.show_tray_icon || !self.desktop.tray.available()) {
            actions.push(Action::Show);
        }
        if !self.desktop.startup_decided {
            if !preferences.start_minimized
                || self.desktop.started.elapsed() > Duration::from_secs(3)
            {
                self.desktop.startup_decided = true;
            } else if self.desktop.tray.available() {
                actions.push(Action::Toggle);
                self.desktop.startup_decided = true;
            }
        }
        for _ in 0..16 {
            if let Some(action) = self.desktop.tray.next_action() {
                actions.push(action)
            } else {
                break;
            }
        }
        let call = self
            .controller
            .as_ref()
            .and_then(|c| c.call_control().snapshot())
            .filter(|call| call.status != VoiceStatus::Disconnected);
        let now = chrono::Utc::now().timestamp_millis();
        let unread = self
            .state
            .channels
            .values()
            .any(|c| self.state.unread_inbox_visible(c.id, now));
        let presence = self
            .state
            .user_settings
            .get("status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("online");
        self.desktop.tray.update(Snapshot {
            enabled: preferences.show_tray_icon,
            colored: preferences.colored_tray_icon,
            visible: !self.desktop.hidden,
            can_restart: !self.demo,
            unread,
            account: self.state.current_user.as_ref().map(|u| u.id),
            presence: ["online", "idle", "dnd", "invisible"]
                .iter()
                .position(|s| *s == presence)
                .unwrap_or(0),
            call,
        });
        let status = self.desktop.tray.status();
        if self.desktop.previous_status != status {
            self.desktop.previous_status = status;
            cx.notify();
        }
        if !actions.is_empty() {
            let window = self.desktop.window;
            let entity = cx.entity().downgrade();
            cx.defer(move |cx| {
                let _ = window.update(cx, |_, window, cx| {
                    entity.update(cx, |this, cx| {
                        for action in actions {
                            this.desktop_action(action, window, cx)
                        }
                    })
                });
            });
        }
    }
    fn desktop_action(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        let (action, token) = match action {
            Action::Activate { action, token } => (*action, Some(token)),
            other => (other, None),
        };
        let focus_window = matches!(
            action,
            Action::Show | Action::Toggle | Action::ToggleActive | Action::Settings
        );
        match action {
            Action::ToggleActive => {
                if self.desktop.hidden || !window.is_window_active() {
                    self.show_from_tray(window, cx);
                } else {
                    self.hide_to_tray(window, cx);
                }
            }
            Action::Toggle => {
                if self.desktop.hidden {
                    self.show_from_tray(window, cx)
                } else {
                    self.hide_to_tray(window, cx);
                }
            }
            Action::Show => self.show_from_tray(window, cx),
            Action::Settings => {
                self.show_from_tray(window, cx);
                self.open_settings(settings_view::Section::Desktop, window, cx);
            }
            Action::Presence { account, index } => {
                if self
                    .state
                    .current_user
                    .as_ref()
                    .is_some_and(|u| u.id == account)
                    && let Some(status) = ["online", "idle", "dnd", "invisible"].get(index)
                {
                    self.command(
                        Command::SetPresenceStatus {
                            status: (*status).into(),
                            duration_minutes: None,
                        },
                        cx,
                    );
                }
            }
            Action::Call { expected, action } => {
                if let Some(handle) = self.controller.as_ref().map(|c| c.call_control()) {
                    cx.spawn(async move |entity, cx| {
                        if let Err(error) = handle
                            .control(
                                expected.account_id,
                                expected.generation,
                                action,
                                Instant::now() + Duration::from_secs(3),
                            )
                            .await
                        {
                            let _ = entity.update(cx, |this, cx| {
                                this.error = Some(error);
                                cx.notify();
                            });
                        }
                    })
                    .detach();
                }
            }
            Action::CopyBuild => cx.write_to_clipboard(ClipboardItem::new_string(format!(
                "Fluxer Native {} ({}/{})",
                option_env!("FLUXER_BUILD_VERSION").unwrap_or(env!("CARGO_PKG_VERSION")),
                std::env::consts::OS,
                std::env::consts::ARCH
            ))),
            Action::Restart => {
                if !self.demo {
                    cx.restart();
                }
            }
            Action::Quit => cx.quit(),
            Action::Activate { .. } => {}
        }
        if focus_window
            && !self.desktop.hidden
            && let Some(token) = token
        {
            window.activate_window_with_token(&token);
        }
    }
}
