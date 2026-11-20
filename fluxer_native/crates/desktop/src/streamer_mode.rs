// SPDX-License-Identifier: AGPL-3.0-or-later
//! Device-local streaming privacy and optional capture process detection.
use super::*;
use crate::controls::{Icon, icon_button, quiet_button, toggle};
use serde::{Deserialize, Serialize};
use std::{borrow::Cow, sync::mpsc, time::Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct Preferences {
    manual_enabled: bool,
    auto_enable: bool,
    hide_personal_information: bool,
    hide_invite_links: bool,
    disable_sounds: bool,
    disable_notifications: bool,
    banner_dismissed: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            manual_enabled: false,
            auto_enable: true,
            hide_personal_information: true,
            hide_invite_links: true,
            disable_sounds: true,
            disable_notifications: true,
            banner_dismissed: false,
        }
    }
}

impl Preferences {
    pub fn from_legacy(enabled: bool) -> Self {
        Self {
            manual_enabled: enabled,
            ..Self::default()
        }
    }

    fn effects(self, detected: bool) -> Effects {
        let enabled = self.manual_enabled || (self.auto_enable && detected);
        Effects {
            enabled,
            hide_names: enabled && self.hide_personal_information,
            hide_invites: enabled && self.hide_invite_links,
            mute_sounds: enabled && self.disable_sounds,
            pause_notifications: enabled && self.disable_notifications,
        }
    }
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, Hash)]
pub(super) struct Effects {
    pub enabled: bool,
    pub hide_names: bool,
    pub hide_invites: bool,
    pub mute_sounds: bool,
    pub pause_notifications: bool,
}

pub(super) const HIDDEN_INVITE: &str = "Invite link hidden while sharing";

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub(super) struct InviteEndpoint {
    pub base: String,
    pub self_hosted: bool,
}

#[derive(Clone, Copy, Default, Hash)]
pub(super) struct Redaction<'a> {
    pub effects: Effects,
    pub invite: Option<&'a InviteEndpoint>,
}

impl Redaction<'_> {
    pub fn hides_link(self, url: &str) -> bool {
        self.effects.hide_invites
            && (matches!(
                desktop_links::DesktopLink::parse(url),
                Some(desktop_links::DesktopLink::Invite(_))
            ) || fluxer_protocol::is_invite_link(
                url,
                self.invite.map_or("", |endpoint| endpoint.base.as_str()),
                self.invite.is_some_and(|endpoint| endpoint.self_hosted),
            ))
    }
}

impl Effects {
    pub fn name(self, name: &str) -> Cow<'_, str> {
        if self.hide_names {
            return match name.trim().chars().next() {
                Some(first) => Cow::Owned(format!("{first}…")),
                None => Cow::Borrowed("…"),
            };
        }
        Cow::Borrowed(name)
    }
}

#[derive(Default)]
pub(super) struct Detection {
    detected: bool,
    next_poll: Option<Instant>,
    pending: Option<mpsc::Receiver<bool>>,
    presented: Effects,
}

impl Detection {
    fn reset(&mut self) {
        self.detected = false;
        self.next_poll = None;
        self.pending = None;
    }

    fn poll(&mut self) -> bool {
        if let Some(receiver) = &self.pending {
            match receiver.try_recv() {
                Ok(detected) => {
                    self.pending = None;
                    return std::mem::replace(&mut self.detected, detected) != detected;
                }
                Err(mpsc::TryRecvError::Disconnected) => self.pending = None,
                Err(mpsc::TryRecvError::Empty) => return false,
            }
        }
        let now = Instant::now();
        if self.next_poll.is_none_or(|next| now >= next) {
            let (sender, receiver) = mpsc::sync_channel(1);
            self.next_poll = Some(now + Duration::from_secs(15));
            self.pending = Some(receiver);
            std::thread::spawn(move || {
                let _ = sender.send(detect_capture_app());
            });
        }
        false
    }
}

fn detect_capture_app() -> bool {
    cfg!(target_os = "linux") && detect_proc(std::path::Path::new("/proc"))
}

fn detect_proc(root: &std::path::Path) -> bool {
    let Ok(entries) = std::fs::read_dir(root) else {
        return false;
    };
    entries.flatten().any(|entry| {
        entry
            .file_name()
            .as_encoded_bytes()
            .iter()
            .all(u8::is_ascii_digit)
            && std::fs::read(entry.path().join("comm")).is_ok_and(|command| {
                let command = command.strip_suffix(b"\n").unwrap_or(&command);
                [b"obs".as_slice(), b"obs-studio", b"xsplit"]
                    .iter()
                    .any(|name| command.eq_ignore_ascii_case(name))
            })
    })
}

impl Fluxer {
    pub(super) fn streaming(&self) -> Effects {
        self.interface
            .streamer()
            .effects(self.streamer_detection.detected)
    }

    pub(super) fn streamer_redaction(&self) -> Redaction<'_> {
        Redaction {
            effects: self.streaming(),
            invite: self.message_links.invite_endpoint(),
        }
    }

    pub(super) fn display_channel_name(&self, channel: &fluxer_core::Channel) -> String {
        let streaming = self.streaming();
        if !streaming.hide_names || !channel.name.is_empty() || channel.recipients.is_empty() {
            return channel.display_name();
        }
        channel
            .recipients
            .iter()
            .map(|user| streaming.name(user.display_name()).into_owned())
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub(super) fn streamer_document(
        &self,
        message: &Message,
        context: message_markup::MessageContext,
    ) -> message_markup::MessageDocument {
        message_markup::document_with_redaction(
            message,
            &self.state,
            None,
            context,
            self.streamer_redaction(),
        )
    }

    pub(super) fn streamer_preview(&self, message: &Message) -> String {
        let streaming = self.streaming();
        if !streaming.hide_invites && !streaming.hide_names {
            return chat_helpers::preview(message);
        }
        let mut rendering = message.clone();
        rendering.content = chat_helpers::display_text(message);
        let document =
            self.streamer_document(&rendering, message_markup::MessageContext::InlinePreview);
        chat_helpers::preview_text(message, &document)
    }

    pub(super) fn poll_streamer(&mut self, cx: &mut Context<Self>) {
        let preferences = self.interface.streamer();
        if self.demo || !cfg!(target_os = "linux") || !preferences.auto_enable {
            self.streamer_detection.reset();
            return;
        }
        if self.streamer_detection.poll() {
            if self.streamer_detection.detected && preferences.banner_dismissed {
                self.interface
                    .update_streamer(|prefs| prefs.banner_dismissed = false);
            }
            self.refresh_streaming(cx);
        }
    }

    fn update_streamer(&mut self, update: impl FnOnce(&mut Preferences), cx: &mut Context<Self>) {
        self.interface.update_streamer(update);
        if !self.interface.streamer().auto_enable {
            self.streamer_detection.reset();
        }
        self.refresh_streaming(cx);
    }

    pub(super) fn refresh_streaming(&mut self, cx: &mut Context<Self>) {
        let effects = self.streaming();
        self.native_notifications.set_streaming(effects);
        let previous = std::mem::replace(&mut self.streamer_detection.presented, effects);
        if effects.hide_invites != previous.hide_invites
            || effects.hide_names != previous.hide_names
        {
            self.reset_context_menu(cx);
            self.refresh_composer_completion(cx);
        }
        self.message_links.hide_pending_invite(effects);
        self.invite_ui.set_hide_invites(effects.hide_invites, cx);
        let invite = self.message_links.invite_endpoint().cloned();
        self.profile_ui
            .popup
            .update(cx, |popup, cx| popup.set_streaming(effects, invite, cx));
        cx.notify();
    }

    pub(super) fn streamer_banner(&self, cx: &mut Context<Self>) -> AnyElement {
        let streaming = self.streaming();
        if !streaming.enabled || self.interface.streamer().banner_dismissed {
            return div().into_any_element();
        }
        div()
            .px_4()
            .py_1()
            .bg(rgb(0x423759))
            .flex()
            .items_center()
            .gap_3()
            .child(div().flex_1().child(if streaming.hide_names {
                "Streaming privacy is on. Names are shortened."
            } else {
                "Streaming privacy is on."
            }))
            .child(
                quiet_button("turn-off-streamer", "Turn off").on_click(cx.listener(
                    |this, _, _, cx| {
                        this.update_streamer(
                            |prefs| {
                                prefs.manual_enabled = false;
                                prefs.auto_enable = false;
                                prefs.banner_dismissed = false;
                            },
                            cx,
                        )
                    },
                )),
            )
            .child(
                icon_button(
                    "dismiss-streamer-banner",
                    Icon::Close,
                    "Dismiss streaming privacy banner",
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.update_streamer(|prefs| prefs.banner_dismissed = true, cx);
                })),
            )
            .into_any_element()
    }

    pub(super) fn streamer_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        use crate::settings_view::setting_row;
        let prefs = self.interface.streamer();
        type Change = fn(&mut Preferences, bool);
        let row = |id, title, description, value, change: Change, cx: &mut Context<Self>| {
            setting_row(
                title,
                description,
                toggle(id, value, title).on_click(cx.listener(move |this, _, _, cx| {
                    this.update_streamer(|prefs| change(prefs, !value), cx)
                })),
            )
        };
        let mut content = div()
            .max_w(px(720.))
            .flex()
            .flex_col()
            .gap_2()
            .child(heading("Streamer Mode"))
            .child(row(
                "streamer-manual",
                "Turn on streaming privacy",
                "Mask private details before sharing your screen or stream.",
                prefs.manual_enabled,
                |prefs, value| {
                    prefs.manual_enabled = value;
                    prefs.banner_dismissed = false;
                },
                cx,
            ));
        if cfg!(target_os = "linux") {
            content = content.child(row(
                "streamer-auto",
                "Turn on automatically",
                "Turn on when OBS or XSplit is detected.",
                prefs.auto_enable,
                |prefs, value| {
                    prefs.auto_enable = value;
                    prefs.banner_dismissed = false;
                },
                cx,
            ));
        }
        content = content.child(
            div()
                .mt_4()
                .font_weight(FontWeight::SEMIBOLD)
                .child("When streaming privacy is on"),
        );
        for (id, title, desc, value, change) in [
            (
                "streamer-names",
                "Mask private account details",
                "Shorten user names, nicknames and tags.",
                prefs.hide_personal_information,
                (|p: &mut Preferences, v| p.hide_personal_information = v) as Change,
            ),
            (
                "streamer-invites",
                "Mask community invite links",
                "Hide invite links and codes.",
                prefs.hide_invite_links,
                |p, v| p.hide_invite_links = v,
            ),
            (
                "streamer-sounds",
                "Mute app sound effects",
                "Pause message notification sounds.",
                prefs.disable_sounds,
                |p, v| p.disable_sounds = v,
            ),
            (
                "streamer-notifications",
                "Pause notifications",
                "Pause desktop notifications.",
                prefs.disable_notifications,
                |p, v| p.disable_notifications = v,
            ),
        ] {
            content = content.child(row(id, title, desc, value, change, cx));
        }
        div()
            .id("streamer-settings-body")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .p_6()
            .child(
                content.when_some(self.interface.error.clone(), |body, error| {
                    body.child(muted(error))
                }),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BTreeSet, ClientState, Effects, HIDDEN_INVITE, InviteEndpoint, Message, Preferences,
        Redaction, detect_proc,
    };

    #[test]
    fn classic_defaults_and_independent_effects() {
        let defaults: Preferences = serde_json::from_str("{}").unwrap();
        assert_eq!(defaults.effects(false), Effects::default());
        assert_eq!(
            defaults.effects(true),
            Effects {
                enabled: true,
                hide_names: true,
                hide_invites: true,
                mute_sounds: true,
                pause_notifications: true,
            }
        );
        let manual = Preferences {
            manual_enabled: true,
            auto_enable: false,
            disable_sounds: false,
            hide_invite_links: false,
            ..defaults
        };
        assert_eq!(
            manual.effects(false),
            Effects {
                enabled: true,
                hide_names: true,
                hide_invites: false,
                mute_sounds: false,
                pause_notifications: true,
            }
        );
        assert_eq!(
            Preferences {
                auto_enable: false,
                ..defaults
            }
            .effects(true),
            Effects::default()
        );
        assert_eq!(manual.effects(false).name("  🐱cat "), "🐱…");
        assert_eq!(manual.effects(false).name(""), "…");
    }

    #[test]
    fn message_redaction_retains_targets_for_names_and_masks_invite_destinations() {
        use crate::message_markup::{MessageCache, MessageContext, MessageTarget};
        let message = Message {
            content: "<@42> [Join now](https://fluxer.gg/ab123) https://fluxer.gg/other-code https://example.net/invite/public-code".to_owned(),
            mentions: vec![fluxer_core::User {
                id: 42.into(),
                username: "Alice".to_owned(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let state = ClientState::default();
        let mut cache = MessageCache::default();
        let render = |cache: &mut MessageCache, redaction| {
            cache.get_with_redaction(&message, &state, None, MessageContext::Standard, redaction)
        };
        assert!(
            render(&mut cache, Redaction::default())
                .text
                .contains("@Alice")
        );
        let masked = render(
            &mut cache,
            Redaction {
                effects: Preferences::from_legacy(true).effects(false),
                invite: None,
            },
        );
        let copied = masked.copy_range(0..masked.text.len(), &BTreeSet::new());
        assert!(copied.contains("@A…"), "{copied}");
        assert_eq!(copied.matches(HIDDEN_INVITE).count(), 2, "{copied}");
        assert!(
            !copied.contains("ab123") && !copied.contains("other-code"),
            "{copied}"
        );
        assert!(copied.contains("https://example.net/invite/public-code"));
        assert!(
            masked.spans.iter().any(
                |span| matches!(span.target, Some(MessageTarget::User(id)) if id == 42.into())
            )
        );
        assert!(masked.spans.iter().all(|span| !matches!(&span.target, Some(MessageTarget::Link(url)) if url.contains("fluxer.gg"))));
        assert!(
            render(&mut cache, Redaction::default())
                .text
                .contains("@Alice")
        );
    }

    #[test]
    fn hides_configured_self_hosted_invites_and_application_links() {
        let endpoint = InviteEndpoint {
            base: "https://chat.example.net/join".into(),
            self_hosted: true,
        };
        let redaction = Redaction {
            effects: Preferences::from_legacy(true).effects(false),
            invite: Some(&endpoint),
        };
        assert!(redaction.hides_link("https://chat.example.net/join/code123"));
        assert!(redaction.hides_link("fluxer://invite/code123"));
        assert!(!redaction.hides_link("https://chat.example.net/community/code123"));
        assert!(!redaction.hides_link("code123"));
    }

    #[test]
    fn detection_checks_exact_process_names() {
        let root = tempfile::tempdir().unwrap();
        let put = |entry: &str, name: &[u8]| {
            let path = root.path().join(entry);
            std::fs::create_dir(&path).unwrap();
            std::fs::write(path.join("comm"), name).unwrap();
        };
        put("24", b"obs-helper\n");
        put("not-a-pid", b"obs\n");
        assert!(!detect_proc(root.path()));
        put("25", b"OBS-Studio\n");
        assert!(detect_proc(root.path()));
    }
}
