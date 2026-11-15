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
    Sound {
        kind: MessageSound,
        volume: f32,
        output: Option<String>,
    },
    Save(Preferences),
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
#[derive(Clone, Debug, Serialize, Deserialize)]
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
enum Feedback {
    Click(Target),
    Loaded(Preferences),
    Failure(String),
}
pub(super) struct DesktopNotifications {
    preferences: Preferences,
    tx: mpsc::SyncSender<Delivery>,
    feedback: mpsc::Receiver<Feedback>,
    status: Option<String>,
    edited: bool,
    seen: VecDeque<(Snowflake, Snowflake)>,
    focused: bool,
    last_scope: Option<(Snowflake, Option<Snowflake>)>,
}
fn config_path(
    config: Option<std::path::PathBuf>,
    home: Option<std::path::PathBuf>,
) -> Option<std::path::PathBuf> {
    config
        .filter(|p| p.is_absolute())
        .or_else(|| {
            home.filter(|p| p.is_absolute())
                .map(|home| home.join(".config"))
        })
        .map(|root| root.join("fluxer-native/notifications.json"))
}
fn path() -> Option<std::path::PathBuf> {
    config_path(
        std::env::var_os("XDG_CONFIG_HOME").map(Into::into),
        std::env::var_os("HOME").map(Into::into),
    )
}

impl DesktopNotifications {
    pub fn new(demo: bool) -> Self {
        let path = (!demo).then(path).flatten();
        let preferences = Preferences::default();
        let (tx, alerts) = mpsc::sync_channel::<Delivery>(32);
        let (feedback_tx, feedback) = mpsc::channel();
        std::thread::spawn(move || delivery(alerts, feedback_tx, path));
        Self {
            preferences,
            tx,
            feedback,
            status: None,
            edited: false,
            seen: VecDeque::new(),
            focused: true,
            last_scope: None,
        }
    }
    fn save(&mut self) {
        self.edited = true;
        if self
            .tx
            .try_send(Delivery::Save(self.preferences.clone()))
            .is_err()
        {
            self.status = Some("Notification settings could not be queued; try again".into());
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
                zbus::zvariant::Value::from("app.fluxer.Native"),
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
fn show_alert(
    sink: &dyn NotificationSink,
    alert: &Alert,
    feedback: &mpsc::Sender<Feedback>,
) -> Option<u32> {
    match sink.show(alert) {
        Ok(id) => Some(id),
        Err(error) => {
            let _ = feedback.send(Feedback::Failure(format!(
                "Desktop notification delivery failed: {error}"
            )));
            None
        }
    }
}
fn load_preferences(path: &std::path::Path) -> Result<Preferences, String> {
    use std::io::Read;
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Preferences::default());
        }
        Err(error) => return Err(format!("Cannot read notification preferences: {error}")),
    };
    let mut bytes = Vec::new();
    file.take(65537)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Cannot read notification preferences: {error}"))?;
    if bytes.len() > 65536 {
        return Err("Notification preferences exceed 64 KiB".into());
    }
    serde_json::from_slice(&bytes).map_err(|_| "Notification preferences are invalid".into())
}
fn save_preferences(path: &std::path::Path, preferences: &Preferences) -> Result<(), String> {
    use std::io::Write;
    let parent = path
        .parent()
        .ok_or("Invalid notification preferences path")?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("Cannot save notification preferences: {error}"))?;
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
    let bytes = serde_json::to_vec(preferences).map_err(|error| error.to_string())?;
    file.write_all(&bytes)
        .and_then(|_| file.as_file().sync_all())
        .map_err(|error| error.to_string())?;
    file.persist(path)
        .map_err(|error| format!("Cannot save notification preferences: {error}"))?;
    Ok(())
}
fn play_sound(kind: MessageSound, volume: f32, output: Option<String>) -> Result<Playing, String> {
    use std::io::Write;
    let (bytes, suffix): (&[u8], &str) = if kind == MessageSound::SameChannel {
        (
            include_bytes!("../../../../fluxer_app/src/media/sounds/in-channel-notification.ogg"),
            ".ogg",
        )
    } else {
        (
            include_bytes!("../../../../fluxer_app/src/media/sounds/message.mp3"),
            ".mp3",
        )
    };
    let mut file = tempfile::Builder::new()
        .suffix(suffix)
        .tempfile()
        .map_err(|error| error.to_string())?;
    file.write_all(bytes).map_err(|error| error.to_string())?;
    let player = fluxer_media::FilePlayer::open(
        file.path().into(),
        fluxer_media::FilePlaybackOptions {
            autoplay: true,
            volume,
            output_device: output,
            ..Default::default()
        },
    )?;
    Ok(Playing {
        _file: file,
        player,
    })
}
fn delivery(
    alerts: mpsc::Receiver<Delivery>,
    feedback: mpsc::Sender<Feedback>,
    path: Option<std::path::PathBuf>,
) {
    let mut preferences = Preferences::default();
    if let Some(path) = &path {
        match load_preferences(path) {
            Ok(loaded) => {
                preferences = loaded;
                let _ = feedback.send(Feedback::Loaded(preferences.clone()));
            }
            Err(error) => {
                let _ = feedback.send(Feedback::Failure(error));
            }
        }
    }
    let mut playing: Vec<Playing> = vec![];
    let mut last_sound: Option<std::time::Instant> = None;
    let mut service = None;
    let targets = Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::<
        u32,
        Target,
    >::new()));
    loop {
        let request = match alerts.recv_timeout(Duration::from_millis(250)) {
            Ok(request) => request,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                playing.retain(|playing| {
                    !matches!(
                        playing.player.snapshot().status,
                        fluxer_media::FilePlaybackStatus::Ended
                            | fluxer_media::FilePlaybackStatus::Failed
                    )
                });
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        playing.retain(|playing| {
            !matches!(
                playing.player.snapshot().status,
                fluxer_media::FilePlaybackStatus::Ended | fluxer_media::FilePlaybackStatus::Failed
            )
        });
        match &request {
            Delivery::Save(saved) => {
                preferences = saved.clone();
                if let Some(path) = &path
                    && let Err(error) = save_preferences(path, saved)
                {
                    let _ = feedback.send(Feedback::Failure(error));
                }
                if !saved.sound || saved.streamer {
                    playing.clear();
                }
                continue;
            }
            Delivery::Sound {
                kind,
                volume,
                output,
            } => {
                if !preferences.sound || preferences.streamer || playing.len() >= 4 {
                    continue;
                }
                let now = std::time::Instant::now();
                if last_sound
                    .is_some_and(|last| now.duration_since(last) < Duration::from_millis(750))
                {
                    continue;
                }
                last_sound = Some(now);
                match play_sound(*kind, *volume, output.clone()) {
                    Ok(sound) => playing.push(sound),
                    Err(error) => {
                        let _ = feedback.send(Feedback::Failure(format!(
                            "Notification sound unavailable: {error}"
                        )));
                    }
                }
                continue;
            }
            _ => {}
        }
        if let Delivery::Clear { account, channel } = request {
            if channel.is_none() {
                playing.clear();
            }
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
        if !preferences.desktop || preferences.streamer {
            continue;
        }
        if service.is_none() {
            let connection = match zbus::blocking::Connection::session() {
                Ok(connection) => connection,
                Err(error) => {
                    let _ = feedback.send(Feedback::Failure(format!(
                        "Desktop notifications unavailable: {error}"
                    )));
                    continue;
                }
            };
            let listener = connection.clone();
            let targets = targets.clone();
            let clicked = feedback.clone();
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
                    if let Ok((id, action)) = signal.body().deserialize::<(u32, String)>()
                        && action == "default"
                        && let Ok(mut targets) = targets.lock()
                        && let Some(target) = targets.remove(&id)
                    {
                        let _ = clicked.send(Feedback::Click(target));
                    }
                }
            });
            service = Some(Freedesktop(connection));
        }
        if let Some(service) = &service
            && let Some(id) = show_alert(service, &alert, &feedback)
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
            modal: self.quick_switcher.open
                || self.profile_ui.is_open(cx)
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
            let name = channel
                .guild_id
                .and_then(|guild| self.state.members.get(&(guild, message.author.id)))
                .and_then(|member| member.nick.as_deref())
                .unwrap_or(message.author.display_name());
            let title = if channel.guild_id.is_some() {
                format!("{} (#{})", message.author.display_name(), channel.name)
            } else {
                name.into()
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
            let output = self
                .output_picker
                .read(cx)
                .selected()
                .filter(|id| *id != "output:default")
                .map(str::to_owned);
            let _ = self.native_notifications.tx.try_send(Delivery::Sound {
                kind,
                volume,
                output,
            });
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
            }
            if let Some((account, Some(channel))) = scope {
                let _ = self.native_notifications.tx.try_send(Delivery::Clear {
                    account,
                    channel: Some(channel),
                });
            }
            self.native_notifications.last_scope = scope;
        }
        let events: Vec<_> = self.native_notifications.feedback.try_iter().collect();
        let mut targets = Vec::new();
        for event in events {
            match event {
                Feedback::Click(target) => targets.push(target),
                Feedback::Loaded(preferences) => {
                    if !self.native_notifications.edited {
                        self.native_notifications.preferences = preferences;
                        cx.notify();
                    }
                }
                Feedback::Failure(error) => {
                    self.native_notifications.status = Some(error);
                    cx.notify();
                }
            }
        }
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
            .children([("desktop","Desktop notifications",self.native_notifications.preferences.desktop),("sound","Message sounds",self.native_notifications.preferences.sound),("privacy","Hide notification message previews",self.native_notifications.preferences.hide_preview),("streamer","Streamer mode · suppress notifications and sounds",self.native_notifications.preferences.streamer)].into_iter().map(|(id,label,enabled)|div().flex().items_center().gap_3().child(div().flex_1().child(label)).child(toggle(id,enabled,label).on_click(cx.listener(move|this,_,_,cx|{match id{"desktop"=>this.native_notifications.preferences.desktop= !enabled,"sound"=>this.native_notifications.preferences.sound= !enabled,"privacy"=>this.native_notifications.preferences.hide_preview= !enabled,_=>this.native_notifications.preferences.streamer= !enabled};this.native_notifications.save();cx.notify();})))))
            .when_some(self.native_notifications.status.clone(),|view,error|view.child(div().text_size(px(12.)).text_color(rgb(0xf0a1b5)).child(error)))
            .child(muted("Only new messages notify. Do Not Disturb, muted channels and message notification settings apply.")).into_any_element()
    }
}
#[cfg(test)]
mod tests {
    use super::{
        Alert, Feedback, NotificationSink, Preferences, Target, config_path, escaped,
        load_preferences, save_preferences, show_alert,
    };
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
    #[test]
    fn service_failure_is_reported_without_a_real_notification() {
        struct Failed;
        impl NotificationSink for Failed {
            fn show(&self, _: &Alert) -> Result<u32, String> {
                Err("private fake service unavailable".into())
            }
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let target = Target {
            account: Snowflake(1),
            channel: Snowflake(2),
            message: Snowflake(3),
        };
        assert!(
            show_alert(
                &Failed,
                &Alert {
                    target,
                    title: "test".into(),
                    body: "test".into()
                },
                &tx
            )
            .is_none()
        );
        assert!(
            matches!(rx.recv().unwrap(),Feedback::Failure(error) if error.contains("private fake service unavailable"))
        );
    }
    #[test]
    fn preferences_are_bounded_atomic_private_and_reject_relative_config_roots() {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            config_path(Some("relative".into()), Some("/home/fixture".into())).unwrap(),
            std::path::PathBuf::from("/home/fixture/.config/fluxer-native/notifications.json")
        );
        assert!(config_path(Some("relative".into()), Some("also-relative".into())).is_none());
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("notifications.json");
        save_preferences(
            &path,
            &Preferences {
                desktop: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(load_preferences(&path).unwrap().desktop);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::write(&path, vec![b' '; 65537]).unwrap();
        assert!(load_preferences(&path).is_err());
        assert!(save_preferences(&path.join("impossible"), &Preferences::default()).is_err());
    }
}
