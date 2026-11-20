//! Serialized native client controller. Network and media handles stay on its
//! Tokio worker; GPUI sends commands and polls coalesced presentation events.
mod attachments;
mod audio_preferences;
pub use audio_preferences::{FilePlaybackKind, FilePlaybackPreference, UserAudioPreference};
mod navigation;
mod worker;

pub use attachments::{AttachmentId, AttachmentPreview, AttachmentStatus, StagedAttachment};
pub use navigation::{
    HistorySnapshot, MentionFilters, MentionsSnapshot, PinnedMessage, PinsSnapshot, SearchScope,
    SearchSnapshot, SearchSort, SearchStatus,
};

pub use fluxer_core::{ClientState, Snowflake};

/// A locally accepted message, retained until the server confirms it or the
/// user removes it. IDs are scoped to the current client session.
#[derive(Clone, Debug)]
pub struct PendingMessage {
    pub id: u64,
    pub content: String,
    pub attachments: Vec<StagedAttachment>,
    pub status: PendingMessageStatus,
    pub reply: Option<fluxer_core::MessageReference>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PendingMessageStatus {
    Sending,
    Failed(String),
}

/// Reserve a session-unique local ID before submitting, so the UI can display
/// the pending row synchronously with the user's action.
pub fn next_pending_message_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static LAST: AtomicU64 = AtomicU64::new(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .min(u64::MAX as u128) as u64;
    LAST.fetch_max(now.saturating_sub(1), Ordering::Relaxed);
    LAST.fetch_add(1, Ordering::Relaxed).saturating_add(1)
}
pub use fluxer_media::{
    AudioMixSettings, AudioSettings, DeviceInventory, InputMode, MediaEvent,
    ParticipantAudioSettings, ScreenShareOptions, ScreenSource, SharedAudioSelection,
    SharedAudioSettings, VideoFrame,
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, mpsc},
    thread,
};

#[derive(Clone, Debug, PartialEq)]
pub struct ProfileSnapshot {
    pub user_id: Snowflake,
    pub guild_id: Option<Snowflake>,
    pub status: ProfileStatus,
    pub profile: Option<Box<fluxer_core::UserProfile>>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProfileStatus {
    Loading,
    Ready,
    Failed(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelationshipAction {
    Send,
    Accept,
    Remove,
    Block,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RelationshipMutationStatus {
    Pending,
    Complete,
    Failed(String),
}

/// A call identity must be carried from the observed snapshot into each remote action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallControlSnapshot {
    pub account_id: Snowflake,
    pub generation: u64,
    pub channel_id: Snowflake,
    pub status: VoiceStatus,
    pub muted: bool,
    pub deafened: bool,
    pub server_muted: bool,
    pub server_deafened: bool,
    pub screen_sharing: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallAction {
    ToggleMute,
    ToggleDeaf,
    SetMute(bool),
    SetDeaf(bool),
    Disconnect,
    StopSharing,
}

#[derive(Clone)]
pub struct CallControlHandle {
    commands: tokio::sync::mpsc::UnboundedSender<Command>,
    snapshot: Arc<Mutex<Option<CallControlSnapshot>>>,
}
impl CallControlHandle {
    pub fn snapshot(&self) -> Option<CallControlSnapshot> {
        self.snapshot.lock().ok().and_then(|value| value.clone())
    }
    pub async fn control(
        &self,
        account_id: Snowflake,
        generation: u64,
        action: CallAction,
        deadline: std::time::Instant,
    ) -> Result<Option<CallControlSnapshot>, String> {
        let (reply, receiver) = tokio::sync::oneshot::channel();
        self.commands
            .send(Command::ControlCall {
                account_id,
                generation,
                action,
                deadline,
                reply,
            })
            .map_err(|_| "Client worker stopped".to_owned())?;
        receiver
            .await
            .map_err(|_| "Client worker stopped".to_owned())?
    }
}

pub enum Command {
    PreviewInvite {
        request_id: u64,
        input: String,
    },
    AcceptInvite {
        request_id: u64,
    },
    CancelInvite {
        request_id: u64,
    },
    ControlCall {
        account_id: Snowflake,
        generation: u64,
        action: CallAction,
        deadline: std::time::Instant,
        reply: tokio::sync::oneshot::Sender<Result<Option<CallControlSnapshot>, String>>,
    },
    UpdateDisplaySetting(fluxer_core::DisplaySetting),
    UpdateOwnProfile {
        global_name: String,
        bio: String,
        pronouns: String,
    },
    SetPresenceStatus {
        status: String,
        duration_minutes: Option<i64>,
    },
    SetCustomStatus {
        text: String,
        emoji_id: Option<String>,
        emoji_name: Option<String>,
        expires_at: Option<String>,
    },
    ClearCustomStatus,
    ChangeRelationship {
        user_id: Snowflake,
        action: RelationshipAction,
    },
    SetMemberList {
        channel_id: Option<Snowflake>,
        start: u32,
    },
    QueryGuildMembers {
        guild_id: Snowflake,
        query: String,
        limit: u32,
    },
    OpenDm {
        user_id: Snowflake,
    },
    CreateGroupDm {
        recipients: Vec<Snowflake>,
    },
    LoadUserProfile {
        user_id: Snowflake,
        guild_id: Option<Snowflake>,
        force: bool,
    },
    RestoreSession,
    Login {
        instance: String,
        identifier: String,
        password: String,
    },
    TokenLogin {
        instance: String,
        token: String,
    },
    BrowserLogin {
        instance: String,
        identifier: String,
    },
    RetryBrowserLogin,
    LoginMfa {
        ticket: String,
        code: String,
    },
    ResendIpAuthorization,
    RetryIpAuthorization,
    CancelLogin,
    Logout,
    Demo,
    DemoSearch,
    SetLocale(String),
    CreateCommunity {
        name: String,
    },
    LeaveCommunity {
        guild_id: Snowflake,
        delete_messages: bool,
    },
    SelectGuild {
        guild_id: Snowflake,
    },
    SelectChannel {
        channel_id: Snowflake,
    },
    FetchMentions {
        before: Option<Snowflake>,
        filters: MentionFilters,
    },
    CloseMentions,
    SearchMessages {
        channel_id: Snowflake,
        query: String,
        scope: SearchScope,
        scope_override: Option<SearchScope>,
        sort: SearchSort,
        page: u32,
    },
    CloseSearch {
        channel_id: Snowflake,
    },
    LoadPins {
        channel_id: Snowflake,
        more: bool,
    },
    ClosePins {
        channel_id: Snowflake,
    },
    SetMessagePinned {
        channel_id: Snowflake,
        message_id: Snowflake,
        pinned: bool,
    },
    JumpToMessage {
        channel_id: Snowflake,
        message_id: Snowflake,
    },
    LoadOlderMessages {
        channel_id: Snowflake,
    },
    LoadNewerMessages {
        channel_id: Snowflake,
    },
    ReturnToLatest {
        channel_id: Snowflake,
    },
    AcknowledgeMessage {
        channel_id: Snowflake,
        message_id: Snowflake,
    },
    MarkChannelRead {
        channel_id: Snowflake,
    },
    MarkGuildRead {
        guild_id: Snowflake,
    },
    MarkMessageUnread {
        channel_id: Snowflake,
        message_id: Snowflake,
    },
    SetGuildNotificationSettings {
        guild_id: Option<Snowflake>,
        muted: Option<bool>,
        mute_seconds: Option<u64>,
        message_notifications: Option<u8>,
    },
    SetChannelNotificationSettings {
        guild_id: Option<Snowflake>,
        channel_id: Snowflake,
        muted: Option<bool>,
        mute_seconds: Option<u64>,
        message_notifications: Option<u8>,
    },
    StageFiles {
        channel_id: Snowflake,
        paths: Vec<std::path::PathBuf>,
    },
    StageClipboardImage {
        channel_id: Snowflake,
        filename: String,
        mime_type: String,
        bytes: Arc<Vec<u8>>,
    },
    RemoveAttachment {
        channel_id: Snowflake,
        id: AttachmentId,
    },
    CancelAttachment {
        channel_id: Snowflake,
        id: AttachmentId,
    },
    RetryAttachment {
        channel_id: Snowflake,
        id: AttachmentId,
    },
    SetAttachmentSpoiler {
        channel_id: Snowflake,
        id: AttachmentId,
        spoiler: bool,
    },
    CancelAttachmentSend {
        channel_id: Snowflake,
    },
    RetryPendingMessage {
        channel_id: Snowflake,
        id: u64,
    },
    RemovePendingMessage {
        channel_id: Snowflake,
        id: u64,
    },
    SendSticker {
        sticker_id: Snowflake,
    },
    SendMessage {
        content: String,
        reply_to: Option<Snowflake>,
        mention_author: bool,
    },
    SendPendingMessage {
        id: u64,
        channel_id: Snowflake,
        content: String,
        reply_to: Option<Snowflake>,
        mention_author: bool,
    },
    ForwardMessage {
        source_channel: Snowflake,
        message_id: Snowflake,
        destination: Snowflake,
    },
    EditMessage {
        message_id: Snowflake,
        content: String,
    },
    DeleteMessage {
        message_id: Snowflake,
    },
    React {
        message_id: Snowflake,
        emoji: String,
    },
    Typing,
    SetDraft(String),
    JoinVoice {
        guild_id: Option<Snowflake>,
        channel_id: Snowflake,
    },
    LeaveVoice,
    SetMute(bool),
    SetDeaf(bool),
    RefreshDevices,
    SetInputDevice(String),
    SetOutputDevice(String),
    SetFilePlaybackPreference {
        kind: FilePlaybackKind,
        preference: FilePlaybackPreference,
    },
    SetUserAudioPreference {
        user: Snowflake,
        preference: UserAudioPreference,
    },
    SetDsp(AudioSettings),
    SetMixer(AudioMixSettings),
    SetPushToTalkPressed(bool),
    SetParticipantAudio {
        identity: String,
        settings: ParticipantAudioSettings,
    },
    SetStreamAudio {
        identity: String,
        settings: ParticipantAudioSettings,
    },
    WatchStream {
        identity: String,
        watch: bool,
    },
    StartMicrophoneTest,
    StopMicrophoneTest,
    RefreshScreenSources,
    StartShare {
        options: ScreenShareOptions,
        audio: Option<SharedAudioSelection>,
    },
    StopShare,
    RefreshAudioApplications,
    StartSharedAudio(SharedAudioSelection),
    StopSharedAudio,
    SetSharedAudio(SharedAudioSettings),
    Shutdown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VoiceStatus {
    Disconnected,
    Connecting { channel_id: Snowflake },
    Connected { channel_id: Snowflake },
    Reconnecting,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DirectMessageStatus {
    Pending,
    Complete,
    Failed(String),
}

#[derive(Clone, Debug)]
pub enum InviteStatus {
    Preview(Box<fluxer_protocol::InvitePreview>),
    Opening,
    Opened { guild_id: Option<Snowflake> },
    Failed(String),
}

pub enum Event {
    CommunityMutation {
        created: Option<Snowflake>,
        left: Option<Snowflake>,
        error: Option<String>,
    },
    Invite {
        request_id: u64,
        status: InviteStatus,
    },
    /// The previous screen capture stopped and needs an explicit new share.
    ShareReshareRequired,
    Mentions(MentionsSnapshot),
    LiveMessageCreated {
        account_id: Snowflake,
        message: Box<fluxer_core::Message>,
    },
    PresenceMutation(RelationshipMutationStatus),
    DisplaySettingsMutation(RelationshipMutationStatus),
    OwnProfileMutation(RelationshipMutationStatus),
    DirectMessageOpen {
        user_id: Snowflake,
        status: DirectMessageStatus,
    },
    RelationshipMutation {
        user_id: Snowflake,
        status: RelationshipMutationStatus,
    },
    UserProfile(ProfileSnapshot),
    /// Source Notification.handleMessageAck/Delete; bound to the active account.
    ClearChannelNotifications {
        account_id: Snowflake,
        channel_id: Snowflake,
    },
    MessageAckResult {
        channel_id: Snowflake,
        message_id: Snowflake,
        error: Option<String>,
    },
    Search(SearchSnapshot),
    Pins(PinsSnapshot),
    History(HistorySnapshot),
    PinMutation {
        channel_id: Snowflake,
        message_id: Snowflake,
        pinned: bool,
        pending: bool,
        error: Option<String>,
    },
    Attachments {
        channel_id: Snowflake,
        attachments: Vec<StagedAttachment>,
    },
    Outbox {
        channel_id: Snowflake,
        messages: Vec<PendingMessage>,
    },
    AttachmentsCleared,
    AttachmentLimits {
        channel_id: Snowflake,
        max_files: usize,
        max_file_bytes: u64,
        preupload: bool,
    },
    MediaEndpoint(Option<String>),
    /// Existing public discovery data for presentation/link routing; no new request.
    InstancePresentation(Option<serde_json::Value>),
    MessageForwarded {
        destination: Snowflake,
    },
    SessionRestoring(bool),
    SessionRestoreFailed,
    SessionContext {
        instance: String,
        account_id: Option<Snowflake>,
        selected_guild: Option<Snowflake>,
    },
    State(Box<ClientState>),
    ChannelSlowmode {
        channel_id: Snowflake,
        rate_limit_per_user: u32,
        retry_after: std::time::Duration,
        can_bypass: bool,
    },
    MessageRateLimited {
        channel_id: Snowflake,
        retry_after: std::time::Duration,
    },
    MessageSent {
        channel_id: Snowflake,
        content: String,
    },
    MessageEdited {
        channel_id: Snowflake,
        message_id: Snowflake,
        content: String,
    },
    MessageOperationFailed {
        channel_id: Snowflake,
        content: String,
        error: String,
    },
    Error(String),
    Status(String),
    MfaRequired {
        ticket: String,
        totp: bool,
        webauthn: bool,
    },
    IpAuthorizationRequired {
        email: String,
        resend_available_in: u64,
    },
    IpAuthorizationPollingFailed,
    IpAuthorizationPollingResumed,
    IpAuthorizationResent {
        resend_available_in: u64,
    },
    IpAuthorizationCleared,
    BrowserLoginRequired {
        code: String,
        expires_at: String,
        browser_url: String,
    },
    BrowserLoginPollingFailed,
    BrowserLoginPollingResumed,
    BrowserLoginCleared,
    Devices(DeviceInventory),
    AudioPreferences {
        dsp: AudioSettings,
        mixer: AudioMixSettings,
        input: (String, String),
        output: (String, String),
        users: std::collections::BTreeMap<String, UserAudioPreference>,
        audio_playback: FilePlaybackPreference,
        video_playback: FilePlaybackPreference,
    },
    ScreenSources(Vec<ScreenSource>),
    AudioApplications(Vec<fluxer_media::AudioApplication>),
    SharedAudioStatus(bool),
    VoiceStatus(VoiceStatus),
    WatchedStreams(Vec<String>),
    Media(MediaEvent),
    Video(VideoFrame),
    Demo(bool),
}

/// One UI owns this controller. Video frames are coalesced by track, preventing
/// a paused rendering thread from accumulating an unbounded pixel queue.
pub struct Controller {
    commands: tokio::sync::mpsc::UnboundedSender<Command>,
    events: mpsc::Receiver<Event>,
    frames: Arc<Mutex<BTreeMap<String, VideoFrame>>>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    call_snapshot: Arc<Mutex<Option<CallControlSnapshot>>>,
}

impl Controller {
    pub fn spawn() -> Result<Self, String> {
        Self::spawn_with_store(Arc::new(fluxer_session::OsSessionStore::default()))
    }

    /// Inject isolated persistence for tests or an alternate OS vault backend.
    pub fn spawn_with_store(store: Arc<dyn fluxer_session::SessionStore>) -> Result<Self, String> {
        let (commands, rx) = tokio::sync::mpsc::unbounded_channel();
        let (tx, events) = mpsc::channel();
        let frames = Arc::new(Mutex::new(BTreeMap::new()));
        let worker_frames = frames.clone();
        let call_snapshot = Arc::new(Mutex::new(None));
        let worker_call_snapshot = call_snapshot.clone();
        let (shutdown, shutdown_rx) = tokio::sync::oneshot::channel();
        thread::Builder::new()
            .name("fluxer-client".into())
            .spawn(move || {
                match tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime.block_on(async move {
                        tokio::select! {
                            _ = worker::Worker::new(tx,worker_frames).with_call_snapshot(worker_call_snapshot).with_session_store(store).run(rx) => {},
                            _ = shutdown_rx => {},
                        }
                    }),
                    Err(error) => {
                        let _ = tx.send(Event::Error(format!("Unable to start client: {error}")));
                    }
                }
            })
            .map_err(|error| error.to_string())?;
        Ok(Self {
            commands,
            events,
            frames,
            shutdown: Some(shutdown),
            call_snapshot,
        })
    }

    pub fn call_control(&self) -> CallControlHandle {
        CallControlHandle {
            commands: self.commands.clone(),
            snapshot: self.call_snapshot.clone(),
        }
    }

    pub fn send(&self, command: Command) -> Result<(), String> {
        self.commands
            .send(command)
            .map_err(|_| "Client worker stopped".into())
    }

    /// Poll control/state events independently from the live video producer.
    /// A UI should drain a bounded number, then call `drain_frames` once per
    /// presentation tick. Repeated `try_recv` while converting video can keep
    /// consuming newly produced frames without yielding back to user input.
    pub fn try_recv_control(&self) -> Option<Event> {
        if let Ok(event) = self.events.try_recv() {
            if let Event::WatchedStreams(watched) = &event
                && let Ok(mut frames) = self.frames.lock()
            {
                frames.retain(|_, frame| watched.contains(&frame.participant));
            }
            return Some(event);
        }
        None
    }

    /// Atomically take the latest frame for each track. Frames arriving during
    /// conversion/rendering remain in the next tick's snapshot. In particular,
    /// a continuously replenished track cannot extend the current UI tick.
    pub fn drain_frames(&self) -> Vec<VideoFrame> {
        let snapshot = self
            .frames
            .lock()
            .map(|mut frames| std::mem::take(&mut *frames))
            .unwrap_or_default();
        snapshot.into_values().collect()
    }

    /// Compatibility polling for consumers which do not convert/render video.
    /// Presentation loops should use `try_recv_control` plus `drain_frames`.
    pub fn try_recv(&self) -> Option<Event> {
        if let Some(event) = self.try_recv_control() {
            return Some(event);
        }
        self.frames
            .lock()
            .ok()?
            .pop_first()
            .map(|(_, frame)| Event::Video(frame))
    }
}

impl Drop for Controller {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
        // Cancellation drops native handles immediately even while an HTTP
        // request is waiting, rather than queueing shutdown behind that request.
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

#[cfg(test)]
mod controller_tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn lifecycle_event_precedes_pixels_and_stop_evicts_coalesced_frames() {
        let (commands, _rx) = tokio::sync::mpsc::unbounded_channel();
        let (events_tx, events) = mpsc::channel();
        let frames = Arc::new(Mutex::new(BTreeMap::from([(
            "sid".into(),
            VideoFrame {
                track_id: "sid".into(),
                participant: "user_1_stream".into(),
                watch_generation: 1,
                width: 1,
                height: 1,
                bgra: Arc::new(vec![0, 0, 0, 255]),
            },
        )])));
        let controller = Controller {
            commands,
            events,
            frames: frames.clone(),
            shutdown: None,
            call_snapshot: Arc::new(Mutex::new(None)),
        };
        events_tx.send(Event::WatchedStreams(Vec::new())).unwrap();
        assert!(
            matches!(controller.try_recv(),Some(Event::WatchedStreams(streams)) if streams.is_empty())
        );
        assert!(frames.lock().unwrap().is_empty());
        assert!(controller.try_recv().is_none());
    }

    #[test]
    fn sustained_producer_cannot_extend_a_frame_snapshot_or_starve_stop_controls() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        let (commands, _rx) = tokio::sync::mpsc::unbounded_channel();
        let (events_tx, events) = mpsc::channel();
        let frames = Arc::new(Mutex::new(BTreeMap::new()));
        let controller = Controller {
            commands,
            events,
            frames: frames.clone(),
            shutdown: None,
            call_snapshot: Arc::new(Mutex::new(None)),
        };
        let producing = Arc::new(AtomicBool::new(true));
        let produced = Arc::new(AtomicUsize::new(0));
        let pending_input = Arc::new(AtomicBool::new(false));
        let thread_frames = frames.clone();
        let thread_producing = producing.clone();
        let thread_produced = produced.clone();
        let thread_pending_input = pending_input.clone();
        // Synthetic full-HD pixels are allocated once: this tests orchestration
        // with an actual concurrent source, not codec or GPU throughput.
        let pixels = Arc::new(vec![0; 1920 * 1080 * 4]);
        let producer = thread::spawn(move || {
            let started = Instant::now();
            while started.elapsed() < Duration::from_secs(2) {
                if started.elapsed() >= Duration::from_millis(250) {
                    // Like a pending window input event, this cannot execute a
                    // UI handler until the current presentation tick returns.
                    thread_pending_input.store(true, Ordering::Release);
                }
                if let Ok(mut frames) = thread_frames.lock()
                    && thread_producing.load(Ordering::Acquire)
                {
                    for track in ["a", "b"] {
                        frames.insert(
                            track.into(),
                            VideoFrame {
                                track_id: track.into(),
                                participant: "user_1_stream".into(),
                                watch_generation: 1,
                                width: 1920,
                                height: 1080,
                                bgra: pixels.clone(),
                            },
                        );
                        thread_produced.fetch_add(1, Ordering::Relaxed);
                    }
                    assert!(
                        frames.len() <= 2,
                        "producer frame storage remains bounded by tracks"
                    );
                }
                if !thread_producing.load(Ordering::Acquire) {
                    break;
                }
                thread::sleep(Duration::from_millis(2));
            }
        });

        let mut rendered = 0;
        let mut saw_stop = false;
        let mut max_batch = 0;
        let mut frames_after_pending_input = 0;
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && !saw_stop {
            if pending_input.load(Ordering::Acquire) {
                producing.store(false, Ordering::Release);
                events_tx.send(Event::WatchedStreams(Vec::new())).unwrap();
            }
            // The desktop uses a fixed control budget, then exactly one frame
            // snapshot. Video arrival cannot replenish this control loop.
            for _ in 0..16 {
                match controller.try_recv_control() {
                    Some(Event::WatchedStreams(streams)) if streams.is_empty() => {
                        saw_stop = true;
                        break;
                    }
                    Some(Event::Video(_)) => panic!("video leaked into the control event queue"),
                    Some(_) => (),
                    None => break,
                }
            }
            let snapshot = controller.drain_frames();
            max_batch = max_batch.max(snapshot.len());
            assert!(snapshot.len() <= 2);
            if saw_stop {
                assert!(
                    snapshot.is_empty(),
                    "stop clears queued pixels before rendering another batch"
                );
                break;
            }
            if snapshot.is_empty() {
                thread::sleep(Duration::from_millis(2));
            }
            for _ in snapshot {
                // Model a conversion/render slower than this source's frame
                // interval. The old repeated try_recv loop could convert 128
                // replenished frames here before returning to input handling.
                if pending_input.load(Ordering::Acquire) {
                    frames_after_pending_input += 1;
                }
                thread::sleep(Duration::from_millis(40));
                rendered += 1;
            }
        }
        producing.store(false, Ordering::Release);
        producer.join().unwrap();
        assert!(
            saw_stop,
            "sustained video must not prevent stop control delivery"
        );
        assert_eq!(max_batch, 2);
        assert!(
            frames_after_pending_input <= 2,
            "a pending Stop input waits at most the already-taken frame batch"
        );
        assert!(
            produced.load(Ordering::Relaxed) > rendered,
            "source replenished frames throughout slow rendering"
        );
        println!(
            "sustained source: {} frames produced, {rendered} rendered, max {max_batch} frames per presentation tick",
            produced.load(Ordering::Relaxed)
        );
    }

    #[test]
    fn public_controller_demo_roundtrip_delivers_populated_state() {
        let controller =
            Controller::spawn_with_store(Arc::new(fluxer_session::MemorySessionStore::default()))
                .expect("spawn client worker");
        controller.send(Command::Demo).expect("send demo command");
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut saw_demo = false;
        while Instant::now() < deadline {
            match controller.try_recv() {
                Some(Event::Demo(true)) => saw_demo = true,
                Some(Event::State(state)) if state.current_user.is_some() => {
                    assert!(saw_demo, "demo event precedes populated state");
                    assert!(!state.guilds.is_empty());
                    assert!(state.selected_channel.is_some());
                    return;
                }
                Some(Event::Error(message)) => panic!("controller error: {message}"),
                Some(_) => {}
                None => thread::sleep(Duration::from_millis(5)),
            }
        }
        panic!(
            "public controller failed to deliver populated demo state (saw Demo event: {saw_demo})"
        );
    }
}
