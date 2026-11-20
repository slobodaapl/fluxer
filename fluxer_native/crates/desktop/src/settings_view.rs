// SPDX-License-Identifier: AGPL-3.0-or-later
//! Source-style settings navigation, separate from ordinary channel navigation.
use super::*;
use crate::controls::{Icon, icon_button, quiet_button};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Section {
    Appearance,
    Privacy,
    Accessibility,
    MessagesMedia,
    Keybinds,
    Voice,
    Notifications,
    Desktop,
}

impl Fluxer {
    pub(super) fn open_settings(
        &mut self,
        section: Section,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Recording belongs to the visible Keybinds page, never a hidden page
        // reached through another section or navigation shortcut.
        self.interface.recording = None;
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
        self.interface.recording = None;
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
            Section::Privacy
            | Section::Appearance
            | Section::Accessibility
            | Section::MessagesMedia
            | Section::Keybinds
            | Section::Desktop => self.expanded_settings(cx),
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
                            (Section::Privacy, "settings-privacy", "Privacy & safety"),
                            (Section::Appearance, "settings-appearance", "Appearance"),
                            (
                                Section::Accessibility,
                                "settings-accessibility",
                                "Accessibility",
                            ),
                            (
                                Section::MessagesMedia,
                                "settings-messages-media",
                                "Messages & media",
                            ),
                            (Section::Desktop, "settings-desktop", "Desktop"),
                            (Section::Keybinds, "settings-keybinds", "Keybinds"),
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

fn setting_row(title: &str, description: &str, control: impl IntoElement) -> Div {
    div()
        .w_full()
        .py_4()
        .border_b_1()
        .border_color(rgb(0x403a4c))
        .flex()
        .items_center()
        .gap_4()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title.to_owned()),
                )
                .child(
                    div()
                        .mt_1()
                        .text_size(px(13.))
                        .text_color(rgb(MUTED))
                        .child(description.to_owned()),
                ),
        )
        .child(control)
}
impl Fluxer {
    fn expanded_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        use crate::controls::{slider, toggle};
        use crate::native_preferences::Shortcut;
        use fluxer_core::DisplaySetting;
        let pending = self.interface.server_pending;
        let mut body = div().max_w(px(720.)).flex().flex_col().gap_2();
        match self.settings_section {
            Section::Privacy => {
                body = body.child(settings_heading("Privacy & safety"));
                let settings = &self.state.user_settings;
                if let Some(flags) = settings
                    .get("friend_source_flags")
                    .and_then(serde_json::Value::as_u64)
                {
                    body = body.child(muted("Choose who can send you a friend request."));
                    let everyone = flags & 4 != 0;
                    for (id, title, description, flag) in [
                        (
                            "privacy-everyone",
                            "Everyone",
                            "Anyone who finds you can send a request.",
                            4,
                        ),
                        (
                            "privacy-mutual-friends",
                            "Friends of friends",
                            "People who share a friend with you.",
                            1,
                        ),
                        (
                            "privacy-mutual-guilds",
                            "Community members",
                            "People who share a community with you.",
                            2,
                        ),
                    ] {
                        let enabled = flags & flag != 0;
                        body = body.child(setting_row(
                            title,
                            description,
                            toggle(id, enabled || (flag != 4 && everyone), title).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    if !pending && (flag == 4 || !everyone) {
                                        this.save_display_setting(
                                            DisplaySetting::FriendSource {
                                                flag,
                                                enabled: !enabled,
                                            },
                                            cx,
                                        );
                                    }
                                }),
                            ),
                        ));
                    }
                } else {
                    body = body.child(muted("Friend request settings are loading."));
                }
                for (id, key, title, bot) in [
                    (
                        "privacy-member-dms",
                        "default_guilds_restricted",
                        "Direct messages from community members",
                        false,
                    ),
                    (
                        "privacy-bot-dms",
                        "bot_default_guilds_restricted",
                        "Direct messages from bots",
                        true,
                    ),
                ] {
                    if let Some(restricted) = settings.get(key).and_then(serde_json::Value::as_bool)
                    {
                        body = body.child(setting_row(title, "Allow direct messages in newly joined communities. Existing community settings are unchanged.",
                            toggle(id, !restricted, title).on_click(cx.listener(move |this, _, _, cx| {
                                if !pending {
                                    let setting = if bot { DisplaySetting::BotDefaultGuildsRestricted(!restricted) } else { DisplaySetting::DefaultGuildsRestricted(!restricted) };
                                    this.save_display_setting(setting, cx);
                                }
                            }))));
                    }
                }
            }
            Section::Appearance => {
                let compact = fluxer_core::compact_messages(&self.state.user_settings);
                let size = self
                    .interface
                    .font_draft
                    .unwrap_or_else(|| fluxer_core::message_font_size(&self.state.user_settings));
                body =
                    body.child(settings_heading("Appearance"))
                        .child(muted("Message presentation · saved to your account"))
                        .child(setting_row(
                            "Compact messages",
                            "Hide avatars and reduce spacing between messages.",
                            toggle("settings-compact", compact, "Compact messages").on_click(
                                cx.listener(move |this, _, _, cx| {
                                    if !pending {
                                        this.save_display_setting(
                                            DisplaySetting::Compact(!compact),
                                            cx,
                                        );
                                    }
                                }),
                            ),
                        ))
                        .child(
                            div()
                                .py_4()
                                .child(
                                    div()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(format!("Message text size · {size:.0}px")),
                                )
                                .child(div().mt_2().child(slider(
                                    "settings-font-size",
                                    size,
                                    8.,
                                    48.,
                                    1.,
                                    "Message text size",
                                    cx.listener(|this, size: &f32, _, cx| {
                                        if !this.interface.server_pending {
                                            this.interface.font_draft = Some(*size);
                                            cx.notify();
                                        }
                                    }),
                                )))
                                .child(
                                    div()
                                        .mt_2()
                                        .flex()
                                        .justify_between()
                                        .child(muted("8px"))
                                        .child(muted("48px")),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(
                                    quiet_button(
                                        "settings-apply-size",
                                        if pending {
                                            "Saving…"
                                        } else {
                                            "Apply text size"
                                        },
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.save_display_setting(
                                                DisplaySetting::FontSize(size as f64),
                                                cx,
                                            );
                                        },
                                    )),
                                )
                                .when(self.interface.font_draft.is_some(), |row| {
                                    row.child(
                                        quiet_button("settings-cancel-size", "Reset preview")
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.interface.font_draft = None;
                                                cx.notify();
                                            })),
                                    )
                                }),
                        )
                        .child(
                            div().mt_4().p_4().rounded_md().bg(rgb(0x25222e)).child(
                                div()
                                    .text_size(px(size))
                                    .line_height(px(size * 1.375))
                                    .child(
                                        "The quick brown fox jumps over the lazy dog.
A preview of your message text size.",
                                    ),
                            ),
                        );
            }
            Section::Accessibility => {
                let reduced = self.interface.reduced_motion();
                body=body.child(settings_heading("Accessibility")).child(muted("Motion preferences · saved on this device"))
                    .child(setting_row("Reduce motion", "Pause automatic GIF animation in chat and search. Hover or open an image to play it intentionally.",toggle("settings-reduced-motion",reduced,"Reduce motion").on_click(cx.listener(|this,_,_,cx|{this.interface.toggle_motion();cx.notify();}))))
                    .child(div().mt_4().text_color(rgb(MUTED)).child("Message text size and compact display are available under Appearance."));
            }
            Section::MessagesMedia => {
                let autoplay = media_animation::gif_autoplay(&self.state.user_settings);
                let animate_emoji = crate::emoji_presentation::autoplay(&self.state.user_settings, false);
                body=body.child(settings_heading("Messages & media")).child(muted("Animation preferences · saved to your account"))
                    .child(setting_row("Automatically play GIFs", "Play animated images without hovering. Reduced motion takes priority on this device.",toggle("settings-gif-autoplay",autoplay,"Automatically play GIFs").on_click(cx.listener(move|this,_,_,cx|{if !pending{this.save_display_setting(DisplaySetting::GifAutoplay(!autoplay),cx);}}))))
                    .child(setting_row("Animate custom emoji", "Play animated custom emoji automatically. Reduced motion takes priority on this device.",toggle("settings-animate-emoji",animate_emoji,"Animate custom emoji").on_click(cx.listener(move|this,_,_,cx|{if !pending{this.save_display_setting(DisplaySetting::AnimateEmoji(!animate_emoji),cx);}}))));
            }
            Section::Desktop => {
                use crate::native_preferences::WindowSetting;
                let behavior = self.interface.window_behavior();
                let linux = cfg!(target_os = "linux");
                body = body
                    .child(settings_heading("Desktop"))
                    .child(muted("Window preferences · saved on this device"))
                    .child(
                        div()
                            .mt_2()
                            .p_3()
                            .rounded_md()
                            .bg(rgb(0x25222e))
                            .text_size(px(13.))
                            .text_color(rgb(MUTED))
                            .child(self.desktop_tray_status()),
                    );
                if !linux {
                    body = body.child(muted(
                        "System tray integration is currently available on Linux.",
                    ));
                }
                for (setting, id, title, description, value, available) in [
                    (
                        WindowSetting::ShowTrayIcon,
                        "settings-show-tray",
                        "Show system tray icon",
                        "Keep an icon in the system tray to reopen Fluxer.",
                        behavior.show_tray_icon,
                        linux,
                    ),
                    (
                        WindowSetting::CloseToTray,
                        "settings-close-to-tray",
                        "Close to tray",
                        "When the tray is available, closing the window keeps Fluxer running. Ctrl+Q or Quit exits the app.",
                        behavior.close_to_tray,
                        linux && behavior.show_tray_icon,
                    ),
                    (
                        WindowSetting::StartMinimized,
                        "settings-start-minimized",
                        "Start minimized",
                        "Start in the system tray when a tray host is available.",
                        behavior.start_minimized,
                        linux && behavior.show_tray_icon,
                    ),
                    (
                        WindowSetting::ColoredTrayIcon,
                        "settings-colored-tray",
                        "Use colored tray icon",
                        "Use Fluxer’s colors instead of a monochrome tray icon.",
                        behavior.colored_tray_icon,
                        linux && behavior.show_tray_icon,
                    ),
                ] {
                    let control = toggle(id, value, title)
                        .when(!available, |view| {
                            view.opacity(0.45)
                                .tab_stop(false)
                                .cursor(CursorStyle::Arrow)
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if available {
                                this.interface.set_window_behavior(setting, !value);
                                cx.notify();
                            }
                        }))
                        .on_key_down(move |event, _, cx| {
                            if available
                                && matches!(event.keystroke.key.as_str(), "enter" | "space")
                            {
                                // GPUI synthesizes on_click on key-up. Only
                                // block parent shortcuts here; mutate once in
                                // the click handler for mouse and keyboard.
                                cx.stop_propagation();
                            }
                        });
                    body = body.child(setting_row(title, description, control));
                }
                if linux && !behavior.show_tray_icon {
                    body = body.child(muted(
                        "Enable the tray icon to change the other window preferences.",
                    ));
                }
            }
            Section::Keybinds => {
                body = body.child(settings_heading("Keybinds")).child(muted(
                    "Shortcuts work while the native app is focused and are saved on this device.",
                ));
                for action in Shortcut::ALL {
                    let recording = self.interface.recording == Some(action);
                    let label = if recording {
                        "Press a shortcut…".to_owned()
                    } else {
                        self.interface.binding(action).to_owned()
                    };
                    body = body.child(setting_row(
                        action.label(),
                        if recording {
                            "Escape cancels recording."
                        } else {
                            "Click to record a new shortcut."
                        },
                        quiet_button(
                            ("settings-keybind", action.index()),
                            SharedString::from(label),
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.interface.recording = Some(action);
                                this.interface.error = None;
                                this.focus.focus(window);
                                cx.notify();
                            },
                        )),
                    ));
                }
                body=body.child(div().mt_4().child(quiet_button("settings-reset-keybinds","Reset shortcuts").on_click(cx.listener(|this,_,_,cx|{this.interface.reset();cx.notify();}))))
                    .child(div().mt_2().text_size(px(13.)).text_color(rgb(MUTED)).child("F8 remains reserved for in-app push to talk. These shortcuts do not control the app globally."));
            }
            _ => {}
        }
        if matches!(
            self.settings_section,
            Section::Privacy | Section::Appearance | Section::MessagesMedia
        ) {
            if pending {
                body = body.child(muted("Saving account preference…"));
            }
            if let Some(error) = &self.interface.server_error {
                body = body.child(div().mt_2().text_color(rgb(0xe79a9e)).child(error.clone()));
            }
        }
        if let Some(error) = &self.interface.error {
            body = body.child(div().mt_2().text_color(rgb(0xe79a9e)).child(error.clone()));
        }
        div()
            .id("expanded-settings-body")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .p_6()
            .child(body)
            .into_any_element()
    }
}

fn settings_heading(title: &str) -> Div {
    div()
        .text_size(px(24.))
        .font_weight(FontWeight::BOLD)
        .mb_2()
        .child(title.to_owned())
}
