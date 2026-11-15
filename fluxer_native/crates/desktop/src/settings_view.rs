// SPDX-License-Identifier: AGPL-3.0-or-later
//! Source-style settings navigation, separate from ordinary channel navigation.
use super::*;
use crate::controls::{Icon, icon_button, quiet_button};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Section {
    Voice,
    Notifications,
}

impl Fluxer {
    pub(super) fn open_settings(
        &mut self,
        section: Section,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pane != Pane::Settings {
            self.settings_return = self.pane;
        }
        self.settings_section = section;
        self.pane = Pane::Settings;
        if section == Section::Voice && !self.demo {
            self.command(Command::RefreshDevices, cx);
        }
        self.focus.focus(window);
        cx.notify();
    }

    pub(super) fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pane = self.settings_return;
        if self.pane == Pane::Chat {
            self.composer.focus_handle(cx).focus(window);
        } else {
            self.focus.focus(window);
        }
        cx.notify();
    }

    pub(super) fn settings_shell(&self, cx: &mut Context<Self>) -> AnyElement {
        let content = match self.settings_section {
            Section::Voice => self.voice_settings(cx),
            Section::Notifications => div()
                .id("notification-settings-body")
                .flex_1()
                .min_w_0()
                .h_full()
                .overflow_y_scroll()
                .p_6()
                .child(div().max_w(px(720.)).child(self.notification_settings(cx)))
                .into_any_element(),
        };
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .child(
                div()
                    .w(px(200.))
                    .flex_shrink_0()
                    .h_full()
                    .bg(rgb(0x25222e))
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .px_2()
                            .py_3()
                            .text_size(px(12.))
                            .text_color(rgb(MUTED))
                            .child("USER SETTINGS"),
                    )
                    .children(
                        [
                            (Section::Voice, "settings-voice", "Voice & audio"),
                            (
                                Section::Notifications,
                                "settings-notifications",
                                "Notifications",
                            ),
                        ]
                        .into_iter()
                        .map(|(section, id, label)| {
                            button_base(id)
                                .w_full()
                                .px_2()
                                .py_2()
                                .bg(rgb(if self.settings_section == section {
                                    0x433d52
                                } else {
                                    0x25222e
                                }))
                                .child(label)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.open_settings(section, window, cx)
                                }))
                        }),
                    )
                    .child(div().flex_1())
                    .child(
                        quiet_button("settings-sign-out", "Sign out")
                            .on_click(cx.listener(|this, _, _, cx| this.logout(cx))),
                    ),
            )
            .child(content)
            .child(
                div()
                    .pt_6()
                    .pr_4()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_1()
                    .child(
                        icon_button("close-settings", Icon::Close, "Close settings (Escape)")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.close_settings(window, cx)),
                            ),
                    )
                    .child(muted("ESC")),
            )
            .into_any_element()
    }
}
