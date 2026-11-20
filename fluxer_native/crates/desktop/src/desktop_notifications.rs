// SPDX-License-Identifier: AGPL-3.0-or-later
//! Session-bus delivery and output-only sounds, never shell commands or capture.
use super::*;
#[path = "notification_transport.rs"]
mod transport;
use fluxer_core::{MessageSound, NotificationContext, notification_sound_volume};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicU8, Ordering},
        mpsc,
    },
};
const PAUSE_NOTIFICATIONS: u8 = 1;
const MUTE_SOUNDS: u8 = 2;
const MASK_NAMES: u8 = 4;
const MASK_INVITES: u8 = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Target {
    account: Snowflake,
    epoch: u64,
    channel_epoch: u64,
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
    ClearAll,
    Clear {
        account: Snowflake,
        channel: Option<Snowflake>,
    },
}
#[derive(Clone, Debug)]
struct Alert {
    target: Target,
    privacy: u8,
    title: String,
    body: String,
}
impl Alert {
    fn allowed(&self, flags: u8) -> bool {
        flags & PAUSE_NOTIFICATIONS == 0 && self.privacy == flags & (MASK_NAMES | MASK_INVITES)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
struct Preferences {
    desktop: bool,
    sound: bool,
    hide_preview: bool,
    // Retained for migration from the original combined setting.
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
    streaming: Arc<AtomicU8>,
    tx: mpsc::SyncSender<Delivery>,
    feedback: mpsc::Receiver<Feedback>,
    status: Option<String>,
    edited: bool,
    seen: VecDeque<(Snowflake, Snowflake)>,
    focused: bool,
    last_scope: Option<(Snowflake, Option<Snowflake>)>,
    epoch: u64,
    channel_epochs: BTreeMap<Snowflake, u64>,
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

pub(super) fn legacy_streamer_enabled() -> bool {
    path()
        .and_then(|path| load_preferences(&path).ok())
        .is_some_and(|prefs| prefs.streamer)
}

fn streaming_flags(effects: streamer_mode::Effects) -> u8 {
    let mut flags = 0;
    if effects.pause_notifications {
        flags |= PAUSE_NOTIFICATIONS;
    }
    if effects.mute_sounds {
        flags |= MUTE_SOUNDS;
    }
    if effects.hide_names {
        flags |= MASK_NAMES;
    }
    if effects.hide_invites {
        flags |= MASK_INVITES;
    }
    flags
}

impl DesktopNotifications {
    pub fn set_streaming(&self, effects: streamer_mode::Effects) {
        self.streaming
            .store(streaming_flags(effects), Ordering::Release);
    }
    pub fn new(demo: bool) -> Self {
        let path = (!demo).then(path).flatten();
        let preferences = Preferences::default();
        let (tx, alerts) = mpsc::sync_channel::<Delivery>(32);
        let (feedback_tx, feedback) = mpsc::sync_channel(256);
        let streaming = Arc::new(AtomicU8::new(0));
        let worker_streaming = streaming.clone();
        std::thread::spawn(move || delivery(alerts, feedback_tx, path, worker_streaming));
        Self {
            preferences,
            streaming,
            tx,
            feedback,
            status: None,
            edited: false,
            seen: VecDeque::new(),
            focused: true,
            last_scope: None,
            epoch: 0,
            channel_epochs: BTreeMap::new(),
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
#[cfg(test)]
trait NotificationSink {
    fn show(&self, alert: &Alert) -> Result<u32, String>;
}
#[cfg(test)]
fn show_alert(
    sink: &dyn NotificationSink,
    alert: &Alert,
    feedback: &mpsc::SyncSender<Feedback>,
) -> Option<u32> {
    match sink.show(alert) {
        Ok(id) => Some(id),
        Err(error) => {
            let _ = feedback.try_send(Feedback::Failure(format!(
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
    feedback: mpsc::SyncSender<Feedback>,
    path: Option<std::path::PathBuf>,
    streaming: Arc<AtomicU8>,
) {
    let mut preferences = Preferences::default();
    if let Some(path) = &path {
        match load_preferences(path) {
            Ok(loaded) => {
                preferences = loaded;
                let _ = feedback.try_send(Feedback::Loaded(preferences.clone()));
            }
            Err(error) => {
                let _ = feedback.try_send(Feedback::Failure(error));
            }
        }
    }
    let mut playing: Vec<Playing> = vec![];
    let mut last_sound: Option<std::time::Instant> = None;
    let mut service = transport::Transport::new(feedback.clone(), streaming.clone());
    let mut previous_flags = 0;
    loop {
        let request = match alerts.recv_timeout(Duration::from_millis(250)) {
            Ok(request) => Some(request),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        reap_sounds(&mut playing, &feedback);
        let flags = streaming.load(Ordering::Acquire);
        let newly_enabled = flags & !previous_flags;
        if newly_enabled & (PAUSE_NOTIFICATIONS | MASK_NAMES | MASK_INVITES) != 0 {
            service.clear_all();
        }
        if newly_enabled & MUTE_SOUNDS != 0 {
            playing.clear();
        }
        previous_flags = flags;
        let Some(request) = request else { continue };
        match &request {
            Delivery::Save(saved) => {
                if !saved.desktop || saved.hide_preview != preferences.hide_preview {
                    service.clear_all();
                }
                preferences = saved.clone();
                if let Some(path) = &path
                    && let Err(error) = save_preferences(path, saved)
                {
                    let _ = feedback.try_send(Feedback::Failure(error));
                }
                if !saved.sound {
                    playing.clear();
                }
                continue;
            }
            Delivery::Sound {
                kind,
                volume,
                output,
            } => {
                if !preferences.sound || flags & MUTE_SOUNDS != 0 || playing.len() >= 4 {
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
                        let _ = feedback.try_send(Feedback::Failure(format!(
                            "Notification sound unavailable: {error}"
                        )));
                    }
                }
                continue;
            }
            _ => {}
        }
        match request {
            Delivery::ClearAll => {
                playing.clear();
                service.clear_all();
            }
            Delivery::Clear { account, channel } => {
                if channel.is_none() {
                    playing.clear();
                }
                service.clear(account, channel);
            }
            Delivery::Alert(alert) if preferences.desktop && alert.allowed(flags) => {
                service.show(alert)
            }
            _ => {}
        }
    }
}
fn reap_sounds(playing: &mut Vec<Playing>, feedback: &mpsc::SyncSender<Feedback>) {
    playing.retain(|playing| {
        let snapshot = playing.player.snapshot();
        if snapshot.status == fluxer_media::FilePlaybackStatus::Failed {
            let _ = feedback.try_send(Feedback::Failure(format!(
                "Notification sound failed: {}",
                snapshot
                    .error
                    .as_deref()
                    .unwrap_or("audio output unavailable")
            )));
        }
        !matches!(
            snapshot.status,
            fluxer_media::FilePlaybackStatus::Ended | fluxer_media::FilePlaybackStatus::Failed
        )
    });
}
fn click_allowed(state: &ClientState, epoch: u64, channel_epoch: u64, target: Target) -> bool {
    target.epoch == epoch
        && target.channel_epoch == channel_epoch
        && state.current_user.as_ref().map(|user| user.id) == Some(target.account)
        && crate::message_links::can_follow_channel(state, target.channel, None)
        && state
            .channels
            .get(&target.channel)
            .zip(state.current_user.as_ref())
            .is_some_and(|(channel, user)| {
                channel.kind.is_message_channel()
                    && !matches!(channel.kind, ChannelType::Forum | ChannelType::Media)
                    && state
                        .permissions_for(channel.id, user.id)
                        .contains(fluxer_core::Permissions::READ_MESSAGE_HISTORY)
            })
}
impl Fluxer {
    /// Called for every authenticated session/instance transition, including
    /// relogin to the same numeric account on a different instance.
    pub(super) fn begin_notification_session(&mut self) {
        self.native_notifications.epoch = self.native_notifications.epoch.wrapping_add(1);
        self.native_notifications.seen.clear();
        self.native_notifications.channel_epochs.clear();
        self.native_notifications.last_scope = None;
        if self
            .native_notifications
            .tx
            .try_send(Delivery::ClearAll)
            .is_err()
        {
            self.native_notifications.status =
                Some("Could not queue dismissal of old desktop notifications".into());
        }
    }
    pub(super) fn clear_channel_notifications(&mut self, account: Snowflake, channel: Snowflake) {
        if self.state.current_user.as_ref().map(|user| user.id) != Some(account) {
            return;
        }
        let epoch = self
            .native_notifications
            .channel_epochs
            .entry(channel)
            .or_default();
        *epoch = epoch.wrapping_add(1);
        if self.state.current_user.as_ref().map(|user| user.id) == Some(account)
            && self
                .native_notifications
                .tx
                .try_send(Delivery::Clear {
                    account,
                    channel: Some(channel),
                })
                .is_err()
        {
            self.native_notifications.status =
                Some("Could not queue desktop notification dismissal".into());
        }
    }
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
        let streaming = self.streaming();
        let context = NotificationContext {
            focused: self.native_notifications.focused
                && (self.pane == Pane::Chat || (self.pane == Pane::Call && self.viewer.chat_open)),
            modal: self.quick_switcher.open
                || self.profile_ui.is_open(cx)
                || self.message_actions.is_blocking()
                || self.image_viewer.is_open()
                || self.playback.modal.is_open(),
            streamer: streaming.pause_notifications && streaming.mute_sounds,
            now_ms: chrono::Utc::now().timestamp_millis(),
        };
        let decision = self.state.live_message_notification(message, context);
        if decision.desktop
            && !streaming.pause_notifications
            && self.native_notifications.preferences.desktop
        {
            let Some(channel) = self.state.channels.get(&message.channel_id) else {
                return;
            };
            let name = channel
                .guild_id
                .and_then(|guild| self.state.members.get(&(guild, message.author.id)))
                .and_then(|member| member.nick.as_deref())
                .unwrap_or(message.author.display_name());
            let title = if channel.guild_id.is_some() {
                format!(
                    "{} (#{})",
                    streaming.name(message.author.display_name()),
                    channel.name
                )
            } else {
                streaming.name(name).into_owned()
            };
            let body = if self.native_notifications.preferences.hide_preview {
                "New message".into()
            } else {
                let document = self.streamer_document(
                    message,
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
            let queued = self
                .native_notifications
                .tx
                .try_send(Delivery::Alert(Alert {
                    privacy: streaming_flags(streaming) & (MASK_NAMES | MASK_INVITES),
                    target: Target {
                        account,
                        epoch: self.native_notifications.epoch,
                        channel_epoch: self
                            .native_notifications
                            .channel_epochs
                            .get(&message.channel_id)
                            .copied()
                            .unwrap_or_default(),
                        channel: message.channel_id,
                        message: message.id,
                    },
                    title: escaped(&title),
                    body: escaped(&body),
                }));
            if queued.is_err() {
                self.native_notifications.status = Some(
                    "Desktop notification queue is full; the notification was not delivered".into(),
                );
                cx.notify();
            }
        }
        if let Some(kind) = decision.sound
            && !streaming.mute_sounds
            && self.native_notifications.preferences.sound
            && let Some(volume) = notification_sound_volume(&self.state.user_settings, kind)
        {
            let output = self
                .output_picker
                .read(cx)
                .selected()
                .filter(|id| *id != "output:default")
                .map(str::to_owned);
            if self
                .native_notifications
                .tx
                .try_send(Delivery::Sound {
                    kind,
                    volume,
                    output,
                })
                .is_err()
            {
                self.native_notifications.status =
                    Some("Notification sound queue is full; the sound was not played".into());
                cx.notify();
            }
        }
    }
    pub(super) fn poll_notifications(&mut self, cx: &mut Context<Self>) {
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
            if self.streaming().pause_notifications
                || !click_allowed(
                    &self.state,
                    self.native_notifications.epoch,
                    self.native_notifications
                        .channel_epochs
                        .get(&target.channel)
                        .copied()
                        .unwrap_or_default(),
                    target,
                )
            {
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
        // Consume a validated OS click before focus-driven dismissal. Some
        // desktops focus the window as part of the click itself; clearing that
        // channel first would invalidate its still-queued legitimate action.
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
                && self
                    .native_notifications
                    .tx
                    .try_send(Delivery::Clear {
                        account: old,
                        channel: None,
                    })
                    .is_err()
            {
                self.native_notifications.status =
                    Some("Could not queue dismissal of old account notifications".into());
                cx.notify();
            }
            if let Some((account, Some(channel))) = scope {
                self.clear_channel_notifications(account, channel);
            }
            self.native_notifications.last_scope = scope;
        }
    }
    pub(super) fn notification_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        use crate::controls::toggle;
        div().flex().flex_col().gap_3().child(heading("Notifications"))
            .children([("desktop","Desktop notifications",self.native_notifications.preferences.desktop),("sound","Message sounds",self.native_notifications.preferences.sound),("privacy","Hide notification message previews",self.native_notifications.preferences.hide_preview)].into_iter().map(|(id,label,enabled)|div().flex().items_center().gap_3().child(div().flex_1().child(label)).child(toggle(id,enabled,label).on_click(cx.listener(move|this,_,_,cx|{match id{"desktop"=>this.native_notifications.preferences.desktop= !enabled,"sound"=>this.native_notifications.preferences.sound= !enabled ,_=>this.native_notifications.preferences.hide_preview= !enabled};this.native_notifications.save();cx.notify();})))))
            .when_some(self.native_notifications.status.clone(),|view,error|view.child(div().text_size(px(12.)).text_color(rgb(0xf0a1b5)).child(error)))
            .child(muted("Only new messages notify. Do Not Disturb, muted channels and message notification settings apply.")).into_any_element()
    }
}
#[cfg(test)]
mod tests {
    use super::{
        Alert, Feedback, NotificationSink, Preferences, Target, click_allowed, config_path,
        escaped, load_preferences, save_preferences, show_alert,
    };
    use fluxer_core::Snowflake;
    #[test]
    fn streamer_delivery_flags_keep_sound_and_desktop_suppression_independent() {
        use super::{MASK_NAMES, MUTE_SOUNDS, PAUSE_NOTIFICATIONS, streamer_mode, streaming_flags};
        let alert = Alert {
            target: Target {
                account: 1.into(),
                epoch: 0,
                channel_epoch: 0,
                channel: 2.into(),
                message: 3.into(),
            },
            privacy: 0,
            title: String::new(),
            body: String::new(),
        };
        let mute = streaming_flags(streamer_mode::Effects {
            mute_sounds: true,
            ..Default::default()
        });
        assert_eq!(mute, MUTE_SOUNDS);
        assert!(alert.allowed(mute));
        let pause = streaming_flags(streamer_mode::Effects {
            pause_notifications: true,
            ..Default::default()
        });
        assert_eq!(pause, PAUSE_NOTIFICATIONS);
        assert!(!alert.allowed(pause));
        assert!(!alert.allowed(MASK_NAMES));
        assert!(
            Alert {
                privacy: MASK_NAMES,
                ..alert
            }
            .allowed(MASK_NAMES)
        );
    }

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
            epoch: 1,
            channel_epoch: 0,
            channel: Snowflake(2),
            message: Snowflake(3),
        };
        let alert = Alert {
            privacy: 0,
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
        let (tx, rx) = std::sync::mpsc::sync_channel(256);
        let target = Target {
            account: Snowflake(1),
            epoch: 1,
            channel_epoch: 0,
            channel: Snowflake(2),
            message: Snowflake(3),
        };
        assert!(
            show_alert(
                &Failed,
                &Alert {
                    privacy: 0,
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
    fn clicks_require_the_current_session_account_and_accessible_channel() {
        let mut state = fluxer_core::ClientState::demo();
        let target = Target {
            account: Snowflake(100),
            epoch: 7,
            channel_epoch: 0,
            channel: Snowflake(11),
            message: Snowflake(999),
        };
        assert!(click_allowed(&state, 7, 0, target));
        assert!(!click_allowed(&state, 7, 1, target));
        assert!(!click_allowed(&state, 8, 0, target));
        assert!(!click_allowed(
            &state,
            7,
            0,
            Target {
                account: Snowflake(101),
                ..target
            }
        ));
        state.channels.remove(&target.channel);
        assert!(!click_allowed(&state, 7, 0, target));
        state = fluxer_core::ClientState::demo();
        state.guilds.get_mut(&Snowflake(1)).unwrap().roles[0].permissions =
            fluxer_core::Permissions::VIEW_CHANNEL;
        assert!(!click_allowed(&state, 7, 0, target));
        state.guilds.get_mut(&Snowflake(1)).unwrap().roles[0].permissions =
            fluxer_core::Permissions::NONE;
        assert!(!click_allowed(&state, 7, 0, target));
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
