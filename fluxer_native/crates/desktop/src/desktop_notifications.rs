// SPDX-License-Identifier: AGPL-3.0-or-later
//! Session-bus delivery and output-only sounds, never shell commands or capture.
use super::*;
use fluxer_core::{MessageSound, NotificationContext, notification_sound_volume};
use serde::{Deserialize, Serialize};
use std::{collections::VecDeque, sync::mpsc};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Target {
    account: Snowflake,
    channel: Snowflake,
    message: Snowflake,
}
#[derive(Clone, Debug)]
enum Delivery {
    Alert(Alert),
    Clear {
        account: Snowflake,
        channel: Option<Snowflake>,
    },
}
#[derive(Clone, Debug)]
struct Alert {
    target: Target,
    title: String,
    body: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
struct Preferences {
    desktop: bool,
    sound: bool,
    hide_preview: bool,
    streamer: bool,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            desktop: false,
            sound: true,
            hide_preview: false,
            streamer: false,
        }
    }
}
struct Playing {
    _file: tempfile::NamedTempFile,
    player: fluxer_media::FilePlayer,
}
pub(super) struct DesktopNotifications {
    preferences: Preferences,
    path: Option<std::path::PathBuf>,
    tx: mpsc::SyncSender<Delivery>,
    clicks: mpsc::Receiver<Target>,
    seen: VecDeque<(Snowflake, Snowflake)>,
    playing: Vec<Playing>,
    last_sound: Option<std::time::Instant>,
    focused: bool,
    last_scope: Option<(Snowflake, Option<Snowflake>)>,
}
fn path() -> Option<std::path::PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".config"))
        })
        .map(|root| root.join("fluxer-native/notifications.json"))
}
impl DesktopNotifications {
    pub fn new(demo: bool) -> Self {
        let path = (!demo).then(path).flatten();
        let preferences = path
            .as_ref()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        let (tx, alerts) = mpsc::sync_channel::<Delivery>(32);
        let (clicked, clicks) = mpsc::channel();
        std::thread::spawn(move || delivery(alerts, clicked));
        Self {
            preferences,
            path,
            tx,
            clicks,
            seen: VecDeque::new(),
            playing: vec![],
            last_sound: None,
            focused: true,
            last_scope: None,
        }
    }
    fn save(&self) {
        if let Some(path) = &self.path {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(bytes) = serde_json::to_vec(&self.preferences) {
                let temporary = path.with_extension("json.tmp");
                if std::fs::write(&temporary, bytes).is_ok() {
                    let _ = std::fs::rename(temporary, path);
                }
            }
        }
    }
}
fn escaped(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .take(300)
        .collect::<String>()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
trait NotificationSink {
    fn show(&self, alert: &Alert) -> Result<u32, String>;
}
struct Freedesktop(zbus::blocking::Connection);
impl NotificationSink for Freedesktop {
    fn show(&self, alert: &Alert) -> Result<u32, String> {
        let proxy = zbus::blocking::Proxy::new(
            &self.0,
            "org.freedesktop.Notifications",
            "/org/freedesktop/Notifications",
            "org.freedesktop.Notifications",
        )
        .map_err(|e| e.to_string())?;
        let hints = std::collections::HashMap::from([
            ("suppress-sound", zbus::zvariant::Value::from(true)),
            (
                "desktop-entry",
                zbus::zvariant::Value::from("fluxer-native"),
            ),
        ]);
        proxy
            .call(
                "Notify",
                &(
                    "Fluxer",
                    0u32,
                    "fluxer",
                    &alert.title,
                    &alert.body,
                    vec!["default", "Open message"],
                    hints,
                    -1i32,
                ),
            )
            .map_err(|e| e.to_string())
    }
}
fn delivery(alerts: mpsc::Receiver<Delivery>, clicked: mpsc::Sender<Target>) {
    let mut service = None;
    let targets = Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::<
        u32,
        Target,
    >::new()));
    for request in alerts {
        if let Delivery::Clear { account, channel } = request {
            if let Some(Freedesktop(connection)) = &service
                && let Ok(mut targets) = targets.lock()
            {
                let ids: Vec<_> = targets
                    .iter()
                    .filter(|(_, target)| {
                        target.account == account && channel.is_none_or(|id| id == target.channel)
                    })
                    .map(|(id, _)| *id)
                    .collect();
                if let Ok(proxy) = zbus::blocking::Proxy::new(
                    connection,
                    "org.freedesktop.Notifications",
                    "/org/freedesktop/Notifications",
                    "org.freedesktop.Notifications",
                ) {
                    for id in ids {
                        let _: Result<(), _> = proxy.call("CloseNotification", &(id,));
                        targets.remove(&id);
                    }
                }
            }
            continue;
        }
        let Delivery::Alert(alert) = request else {
            continue;
        };
        if service.is_none() {
            let Ok(connection) = zbus::blocking::Connection::session() else {
                continue;
            };
            let listener = connection.clone();
            let targets = targets.clone();
            let clicked = clicked.clone();
            std::thread::spawn(move || {
                let Ok(proxy) = zbus::blocking::Proxy::new(
                    &listener,
                    "org.freedesktop.Notifications",
                    "/org/freedesktop/Notifications",
                    "org.freedesktop.Notifications",
                ) else {
                    return;
                };
                let Ok(signals) = proxy.receive_signal("ActionInvoked") else {
                    return;
                };
                for signal in signals {
                    if let Ok((id, action)) = signal.body().deserialize::<(u32, String)>() {
                        if action == "default"
                            && let Ok(mut targets) = targets.lock()
                            && let Some(target) = targets.remove(&id)
                        {
                            let _ = clicked.send(target);
                        }
                    }
                }
            });
            service = Some(Freedesktop(connection));
        }
        if let Some(service) = &service
            && let Ok(id) = service.show(&alert)
            && let Ok(mut targets) = targets.lock()
        {
            if targets.len() >= 256 {
                targets.clear();
            }
            targets.insert(id, alert.target);
        }
    }
}
impl Fluxer {
    pub(super) fn notification_focus(&mut self, focused: bool) {
        self.native_notifications.focused = focused;
    }
    pub(super) fn live_message_notification(
        &mut self,
        account: Snowflake,
        message: &Message,
        cx: &mut Context<Self>,
    ) {
        if self.demo || self.state.current_user.as_ref().map(|user| user.id) != Some(account) {
            return;
        }
        let key = (account, message.id);
        if self.native_notifications.seen.contains(&key) {
            return;
        }
        self.native_notifications.seen.push_back(key);
        if self.native_notifications.seen.len() > 2048 {
            self.native_notifications.seen.pop_front();
        }
        let context = NotificationContext {
            focused: self.native_notifications.focused
                && (self.pane == Pane::Chat || (self.pane == Pane::Call && self.viewer.chat_open)),
            modal: self.profile_ui.is_open(cx)
                || self.message_actions.is_blocking()
                || self.image_viewer.is_open()
                || self.playback.modal.is_open(),
            streamer: self.native_notifications.preferences.streamer,
            now_ms: chrono::Utc::now().timestamp_millis(),
        };
        let decision = self.state.live_message_notification(message, context);
        if decision.desktop && self.native_notifications.preferences.desktop {
            let Some(channel) = self.state.channels.get(&message.channel_id) else {
                return;
            };
            let title = if channel.guild_id.is_some() {
                format!("{} (#{})", message.author.display_name(), channel.name)
            } else {
                message.author.display_name().into()
            };
            let body = if self.native_notifications.preferences.hide_preview {
                "New message".into()
            } else {
                let document = crate::message_markup::document(
                    message,
                    &self.state,
                    None,
                    crate::message_markup::MessageContext::InlinePreview,
                );
                let body = document.copy_range(0..document.text.len(), &BTreeSet::new());
                if !body.trim().is_empty() {
                    body
                } else if let Some(attachment) = message.attachments.first() {
                    format!("Attachment: {}", attachment.filename)
                } else {
                    "New message".into()
                }
            };
            let _ = self
                .native_notifications
                .tx
                .try_send(Delivery::Alert(Alert {
                    target: Target {
                        account,
                        channel: message.channel_id,
                        message: message.id,
                    },
                    title: escaped(&title),
                    body: escaped(&body),
                }));
        }
        if let Some(kind) = decision.sound
            && self.native_notifications.preferences.sound
            && let Some(volume) = notification_sound_volume(&self.state.user_settings, kind)
        {
            let now = std::time::Instant::now();
            if self
                .native_notifications
                .last_sound
                .is_some_and(|last| now.duration_since(last) < Duration::from_millis(750))
            {
                return;
            }
            self.native_notifications.last_sound = Some(now);
            self.native_notifications.playing.retain(|playing| {
                !matches!(
                    playing.player.snapshot().status,
                    fluxer_media::FilePlaybackStatus::Ended
                        | fluxer_media::FilePlaybackStatus::Failed
                )
            });
            if self.native_notifications.playing.len() >= 4 {
                return;
            }
            let (bytes, suffix): (&[u8], &str) = if kind == MessageSound::SameChannel {
                (
                    include_bytes!(
                        "../../../../fluxer_app/src/media/sounds/in-channel-notification.ogg"
                    ),
                    ".ogg",
                )
            } else {
                (
                    include_bytes!("../../../../fluxer_app/src/media/sounds/message.mp3"),
                    ".mp3",
                )
            };
            if let Ok(mut file) = tempfile::Builder::new().suffix(suffix).tempfile() {
                use std::io::Write;
                if file.write_all(bytes).is_ok()
                    && let Ok(player) = fluxer_media::FilePlayer::open(
                        file.path().into(),
                        fluxer_media::FilePlaybackOptions {
                            autoplay: true,
                            volume,
                            output_device: self
                                .output_picker
                                .read(cx)
                                .selected()
                                .filter(|id| *id != "output:default")
                                .map(str::to_owned),
                            ..Default::default()
                        },
                    )
                {
                    self.native_notifications.playing.push(Playing {
                        _file: file,
                        player,
                    });
                }
            }
        }
    }
    pub(super) fn poll_notifications(&mut self, cx: &mut Context<Self>) {
        let account = self.state.current_user.as_ref().map(|user| user.id);
        let scope = account.map(|account| {
            (
                account,
                self.state
                    .selected_channel
                    .filter(|_| self.native_notifications.focused),
            )
        });
        if self.native_notifications.last_scope != scope {
            if let Some((old, _)) = self.native_notifications.last_scope
                && account != Some(old)
            {
                let _ = self.native_notifications.tx.try_send(Delivery::Clear {
                    account: old,
                    channel: None,
                });
                self.native_notifications.playing.clear();
            }
            if let Some((account, Some(channel))) = scope {
                let _ = self.native_notifications.tx.try_send(Delivery::Clear {
                    account,
                    channel: Some(channel),
                });
            }
            self.native_notifications.last_scope = scope;
        }
        self.native_notifications.playing.retain(|playing| {
            !matches!(
                playing.player.snapshot().status,
                fluxer_media::FilePlaybackStatus::Ended | fluxer_media::FilePlaybackStatus::Failed
            )
        });
        let targets: Vec<_> = self.native_notifications.clicks.try_iter().collect();
        for target in targets {
            if self.state.current_user.as_ref().map(|user| user.id) != Some(target.account) {
                continue;
            }
            self.select(target.channel, cx);
            self.command(
                Command::JumpToMessage {
                    channel_id: target.channel,
                    message_id: target.message,
                },
                cx,
            );
            cx.defer(|cx| {
                if let Some(handle) = cx.windows().first().copied() {
                    let _ = handle.update(cx, |_, window, _| window.activate_window());
                }
            });
        }
    }
    pub(super) fn notification_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        use crate::controls::toggle;
        div().flex().flex_col().gap_3().child(heading("Notifications"))
            .children([("desktop","Desktop notifications",self.native_notifications.preferences.desktop),("sound","Message sounds",self.native_notifications.preferences.sound),("privacy","Hide notification message previews",self.native_notifications.preferences.hide_preview),("streamer","Streamer mode · suppress notifications and sounds",self.native_notifications.preferences.streamer)].into_iter().map(|(id,label,enabled)|div().flex().items_center().gap_3().child(div().flex_1().child(label)).child(toggle(id,enabled,label).on_click(cx.listener(move|this,_,_,cx|{match id{"desktop"=>this.native_notifications.preferences.desktop=!enabled,"sound"=>this.native_notifications.preferences.sound=!enabled,"privacy"=>this.native_notifications.preferences.hide_preview=!enabled,_=>this.native_notifications.preferences.streamer=!enabled};this.native_notifications.save();cx.notify();})))))
            .child(muted("Only new messages notify. Do Not Disturb, muted channels and message notification settings apply.")).into_any_element()
    }
}
#[cfg(test)]
mod tests {
    use super::{Alert, NotificationSink, Target, escaped};
    use fluxer_core::Snowflake;
    #[test]
    fn private_sink_receives_safe_plaintext_and_exact_click_identity() {
        struct Fake(std::cell::RefCell<Vec<Alert>>);
        impl NotificationSink for Fake {
            fn show(&self, alert: &Alert) -> Result<u32, String> {
                self.0.borrow_mut().push(alert.clone());
                Ok(7)
            }
        }
        let sink = Fake(Default::default());
        let target = Target {
            account: Snowflake(1),
            channel: Snowflake(2),
            message: Snowflake(3),
        };
        let alert = Alert {
            target,
            title: escaped("<b>$(secret)</b>"),
            body: escaped("&\u{1b}body"),
        };
        assert_eq!(sink.show(&alert), Ok(7));
        assert_eq!(sink.0.borrow()[0].target, target);
        assert_eq!(sink.0.borrow()[0].body, "&amp;body");
        assert!(sink.0.borrow()[0].title.contains("&lt;b&gt;"));
    }
}
