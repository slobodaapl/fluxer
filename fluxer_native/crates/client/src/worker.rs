use crate::{Command, Event, VoiceStatus};
use fluxer_core::{
    ChannelType, ClientState, Emoji, Message, MessageReference, Permissions, Snowflake, User,
};
use fluxer_media::{MediaEngine, MediaEvent, MediaEvents, SharedAudioSelection, VideoFrame};
use fluxer_protocol::{
    AuthSession, Client, DesktopHandoffPoll, DesktopHandoffSession, DesktopHandoffStatus, Gateway,
    GatewayCommand, GatewayEvent, Instance, LoginIdentifier, LoginOutcome, SessionToken,
    VoiceServerUpdate, VoiceStateUpdate,
};
use fluxer_session::{SavedSession, SessionStore};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    future::pending,
    sync::{Arc, Mutex, mpsc},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{sync::mpsc as async_mpsc, task::JoinHandle};

#[path = "worker_attachments.rs"]
mod attachment_flow;
#[path = "worker_channels.rs"]
mod channel_flow;
#[path = "worker_community.rs"]
mod community_flow;
#[path = "worker_settings.rs"]
mod display_settings_flow;
#[path = "worker_idle.rs"]
mod idle_flow;
#[path = "worker_invites.rs"]
mod invite_flow;
#[path = "worker_navigation.rs"]
mod navigation_flow;
#[path = "worker_own_profile.rs"]
mod own_profile_flow;
#[path = "worker_people.rs"]
mod people_flow;
#[path = "worker_presence.rs"]
mod presence_flow;
#[path = "worker_profile.rs"]
mod profile_flow;

type Result<T> = std::result::Result<T, String>;
const IP_POLL_INTERVAL: Duration = Duration::from_millis(2000);
const IP_MAX_POLL_ERRORS: u8 = 3;
fn error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct VoiceTarget {
    guild_id: Option<Snowflake>,
    channel_id: Snowflake,
    connection_id: Option<String>,
}

struct VoiceConnectJob {
    generation: u64,
    update: VoiceServerUpdate,
    revoked_share: bool,
    task: JoinHandle<(MediaEngine, Result<MediaEvents>)>,
}

struct PendingShare {
    audio: Option<SharedAudioSelection>,
}

struct ThreadCreation {
    account: Snowflake,
    guild: Snowflake,
    parent: Snowflake,
    kind: ChannelType,
    removed: BTreeSet<Snowflake>,
    task: JoinHandle<Result<Value>>,
}

impl Drop for ThreadCreation {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct PendingIpAuthorization {
    client: Client,
    ticket: String,
    resend_deadline: tokio::time::Instant,
    resend_used: bool,
    consecutive_errors: u8,
    next_poll: Option<tokio::time::Instant>,
}
struct PendingBrowserLogin {
    client: Client,
    session: Option<DesktopHandoffSession>,
    consecutive_errors: u8,
    next_poll: Option<tokio::time::Instant>,
}
type BrowserPollTask = JoinHandle<(
    DesktopHandoffSession,
    fluxer_protocol::Result<DesktopHandoffPoll>,
)>;
type BrowserPollResult = std::result::Result<
    (
        DesktopHandoffSession,
        fluxer_protocol::Result<DesktopHandoffPoll>,
    ),
    tokio::task::JoinError,
>;

impl VoiceTarget {
    fn accepts(&self, update: &VoiceServerUpdate) -> bool {
        if self
            .connection_id
            .as_ref()
            .is_some_and(|id| id != &update.connection_id)
        {
            return false;
        }
        if update
            .channel_id
            .as_deref()
            .is_some_and(|id| id != self.channel_id.to_string())
        {
            return false;
        }
        if update
            .guild_id
            .as_deref()
            .is_some_and(|id| Some(id.to_owned()) != self.guild_id.map(|id| id.to_string()))
        {
            return false;
        }
        update.channel_id.is_some()
            || self.connection_id.as_deref() == Some(update.connection_id.as_str())
    }
    fn payload(
        &self,
        state: &ClientState,
        leave: bool,
        watched: &BTreeSet<String>,
    ) -> VoiceStateUpdate {
        VoiceStateUpdate {
            guild_id: self.guild_id.map(|id| id.to_string()),
            channel_id: (!leave).then(|| self.channel_id.to_string()),
            connection_id: self.connection_id.clone(),
            self_mute: state.voice.muted,
            self_deaf: state.voice.deafened,
            self_stream: state.voice.screen_share,
            viewer_stream_keys: if leave {
                Vec::new()
            } else {
                self.viewer_stream_keys(watched)
            },
            ..VoiceStateUpdate::default()
        }
    }

    fn viewer_stream_keys(&self, watched: &BTreeSet<String>) -> Vec<String> {
        let mut keys = watched
            .iter()
            .filter_map(|identity| stream_key(self, identity))
            .collect::<Vec<_>>();
        keys.sort();
        keys.dedup();
        keys
    }
}

struct StickerEvent {
    token: u64,
    channel: Snowflake,
    result: Result<Message>,
}

struct DmEvent {
    user_id: Snowflake,
    generation: u64,
    result: Result<Value>,
}
struct DmJob {
    generation: u64,
    task: JoinHandle<()>,
}
impl Drop for DmJob {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub(crate) struct Worker {
    sticker_tx: async_mpsc::UnboundedSender<StickerEvent>,
    sticker_rx: async_mpsc::UnboundedReceiver<StickerEvent>,
    sticker_jobs: BTreeMap<u64, JoinHandle<()>>,
    sticker_token: u64,
    dm_tx: async_mpsc::UnboundedSender<DmEvent>,
    dm_rx: async_mpsc::UnboundedReceiver<DmEvent>,
    dm_generation: u64,
    dm_job: Option<DmJob>,
    thread_creation: Option<ThreadCreation>,
    presence: presence_flow::PresenceState,
    display_settings: display_settings_flow::DisplaySettingsState,
    system_idle: Option<(bool, bool)>,
    people: people_flow::PeopleState,
    invites: invite_flow::InviteState,
    navigation: navigation_flow::NavigationState,
    profiles: profile_flow::ProfileState,
    own_profile: own_profile_flow::OwnProfileState,
    account_sessions_task: Option<JoinHandle<()>>,
    community: community_flow::CommunityState,
    channels: channel_flow::ChannelJobs,
    attachments: attachment_flow::AttachmentState,
    session_store: Option<Arc<dyn SessionStore>>,
    audio_key: Option<String>,
    audio_preferences: crate::audio_preferences::AudioPreferences,
    saved_session: Option<SavedSession>,
    restore_task: Option<JoinHandle<Result<Option<SavedSession>>>>,
    restore_channel: Option<Snowflake>,
    restoring_gateway: bool,
    state: ClientState,
    client: Option<Client>,
    pending_mfa: Option<Client>,
    pending_ip: Option<PendingIpAuthorization>,
    ip_poll_task: Option<JoinHandle<fluxer_protocol::Result<Option<AuthSession>>>>,
    pending_browser: Option<PendingBrowserLogin>,
    browser_poll_task: Option<BrowserPollTask>,
    events: mpsc::Sender<Event>,
    frames: Arc<Mutex<BTreeMap<String, VideoFrame>>>,
    gateway_commands: Option<async_mpsc::Sender<GatewayCommand>>,
    gateway_events: Option<async_mpsc::Receiver<GatewayEvent>>,
    gateway_task: Option<JoinHandle<()>>,
    gateway_ready: bool,
    last_sequence: Option<u64>,
    media: Option<MediaEngine>,
    media_events: Option<MediaEvents>,
    voice_target: Option<VoiceTarget>,
    voice_connect: Option<VoiceConnectJob>,
    voice_recovery_started: Option<tokio::time::Instant>,
    voice_recovery_at: Option<tokio::time::Instant>,
    voice_recovery_attempts: u8,
    call_generation: u64,
    call_snapshot: Arc<Mutex<Option<crate::CallControlSnapshot>>>,
    pending_voice_server: BTreeMap<String, VoiceServerUpdate>,
    retired_voice_connections: BTreeSet<String>,
    voice_credentials: Option<VoiceServerUpdate>,
    voice_join_deadline: Option<tokio::time::Instant>,
    pending_voice_leave: Option<VoiceStateUpdate>,
    voice_sync_pending: bool,
    server_muted: bool,
    server_deafened: bool,
    demo: bool,
    locale: String,
    expected_user_id: Option<Snowflake>,
    selected_guild: Option<Snowflake>,
    synced_guilds: BTreeMap<Snowflake, (String, bool)>,
    member_list_subscription: Option<(Snowflake, Snowflake, u32)>,
    member_list_identity: Option<String>,
    member_wire_list_id: Option<String>,
    member_query_nonce: u64,
    member_query: Option<(Snowflake, String)>,
    available_streams: BTreeSet<String>,
    watched_streams: BTreeSet<String>,
    participant_audio_intents: BTreeMap<String, fluxer_media::ParticipantAudioSettings>,
    stream_audio_intents: BTreeMap<String, fluxer_media::ParticipantAudioSettings>,
    /// Media watch epochs fence frames and lifecycle notifications queued before
    /// stopping/restarting a watch on the same publication SID.
    video_generations: BTreeMap<String, u64>,
    /// A selected audio route is armed only after the video source starts;
    /// opening an OS picker alone must never begin audio capture.
    pending_share: Option<PendingShare>,
    active_share_audio: bool,
}

impl Worker {
    pub(crate) fn new(
        events: mpsc::Sender<Event>,
        frames: Arc<Mutex<BTreeMap<String, VideoFrame>>>,
    ) -> Self {
        let (dm_tx, dm_rx) = async_mpsc::unbounded_channel();
        let (sticker_tx, sticker_rx) = async_mpsc::unbounded_channel();
        Self {
            sticker_tx,
            sticker_rx,
            sticker_jobs: BTreeMap::new(),
            sticker_token: 0,
            dm_tx,
            dm_rx,
            dm_generation: 0,
            dm_job: None,
            thread_creation: None,
            presence: presence_flow::PresenceState::new(),
            display_settings: display_settings_flow::DisplaySettingsState::new(),
            system_idle: None,
            people: people_flow::PeopleState::new(),
            invites: invite_flow::InviteState::new(),
            navigation: navigation_flow::NavigationState::new(),
            profiles: profile_flow::ProfileState::new(),
            own_profile: own_profile_flow::OwnProfileState::new(),
            account_sessions_task: None,
            community: community_flow::CommunityState::new(),
            channels: channel_flow::ChannelJobs::new(),
            attachments: attachment_flow::AttachmentState::new(),
            session_store: None,
            audio_key: None,
            audio_preferences: crate::audio_preferences::AudioPreferences::default(),
            saved_session: None,
            restore_task: None,
            restore_channel: None,
            restoring_gateway: false,
            state: ClientState::default(),
            client: None,
            pending_mfa: None,
            pending_ip: None,
            ip_poll_task: None,
            pending_browser: None,
            browser_poll_task: None,
            events,
            frames,
            gateway_commands: None,
            gateway_events: None,
            gateway_task: None,
            gateway_ready: false,
            last_sequence: None,
            media: None,
            media_events: None,
            voice_target: None,
            voice_connect: None,
            voice_recovery_started: None,
            voice_recovery_at: None,
            voice_recovery_attempts: 0,
            call_generation: 0,
            call_snapshot: Arc::new(Mutex::new(None)),
            pending_voice_server: BTreeMap::new(),
            retired_voice_connections: BTreeSet::new(),
            voice_credentials: None,
            voice_join_deadline: None,
            pending_voice_leave: None,
            voice_sync_pending: false,
            server_muted: false,
            server_deafened: false,
            demo: false,
            locale: ClientState::default().settings.locale,
            expected_user_id: None,
            selected_guild: None,
            synced_guilds: BTreeMap::new(),
            member_list_subscription: None,
            member_list_identity: None,
            member_wire_list_id: None,
            member_query_nonce: 0,
            member_query: None,
            available_streams: BTreeSet::new(),
            watched_streams: BTreeSet::new(),
            participant_audio_intents: BTreeMap::new(),
            stream_audio_intents: BTreeMap::new(),
            video_generations: BTreeMap::new(),
            pending_share: None,
            active_share_audio: false,
        }
    }
    pub(crate) fn with_session_store(mut self, store: Arc<dyn SessionStore>) -> Self {
        self.session_store = Some(store);
        self
    }

    fn begin_restore(&mut self) {
        if self.demo || self.client.is_some() || self.restore_task.is_some() {
            return;
        }
        let Some(store) = self.session_store.clone() else {
            return;
        };
        self.emit(Event::SessionRestoring(true));
        self.emit(Event::Status("Restoring your saved session…".into()));
        // Keep vault and discovery I/O cancellable by a new login/demo/cancel.
        // Neither operation sends the stored token to any endpoint.
        self.restore_task = Some(tokio::spawn(async move {
            let saved = tokio::time::timeout(Duration::from_secs(15), store.load())
                .await
                .map_err(|_| "Session restore timed out waiting for the keyring")?
                .map_err(|_| "Could not read your saved session; unlock the keyring and retry")?;
            let Some(mut saved) = saved else {
                return Ok(None);
            };
            if saved.valid {
                saved.instance = saved.instance.refresh_saved().await.map_err(|_| {
                    "Could not safely restore this instance; your saved session has been retained".to_owned()
                })?;
            }
            Ok(Some(saved))
        }));
    }

    async fn finish_restore(&mut self, result: Result<Option<SavedSession>>) -> Result<()> {
        let Some(saved) = result? else {
            self.emit(Event::Status("Sign in to your Fluxer instance".into()));
            return Ok(());
        };
        self.emit(Event::InstancePresentation(Some(
            saved.instance.document.clone(),
        )));
        self.emit(Event::MediaEndpoint(
            saved
                .instance
                .document
                .pointer("/endpoints/media")
                .and_then(Value::as_str)
                .map(str::to_owned),
        ));
        self.emit(Event::SessionContext {
            instance: saved.instance.api.to_string(),
            account_id: Some(saved.user_id),
            selected_guild: saved.selected_guild,
        });
        if !saved.valid {
            self.emit(Event::Status(
                "Your saved session expired; sign in again".into(),
            ));
            return Ok(());
        }
        self.locale = saved.locale.clone();
        self.state.settings.locale = self.locale.clone();
        self.selected_guild = saved.selected_guild;
        self.restore_channel = saved.selected_channel;
        self.restoring_gateway = true;
        let client = Client::new(saved.instance.clone())
            .map_err(error)?
            .with_auth_locale(self.locale.clone())
            .map_err(error)?
            .with_token(saved.token.clone());
        // AuthSessionRestorer's cold path does not GET /users/@me or RESUME
        // without cached state. Bind the stored identity and request full READY.
        self.authenticate(
            client,
            saved.token,
            Some(saved.user_id.to_string()),
            saved.user,
        )
        .await
    }

    async fn persist_active_session(&self) -> Result<()> {
        let (Some(store), Some(session)) = (&self.session_store, &self.saved_session) else {
            return Ok(());
        };
        tokio::time::timeout(Duration::from_secs(15), store.save(session.clone()))
            .await
            .map_err(|_| "Your session could not be saved: the keyring timed out")?
            .map_err(|_| "Your session could not be saved. Unlock your keyring; this sign-in will otherwise be lost on restart".to_owned())
    }

    async fn persist_selection(&mut self) {
        if self.demo {
            return;
        }
        if let Some(session) = &mut self.saved_session {
            session.locale = self.locale.clone();
            session.selected_guild = self.selected_guild;
            session.selected_channel = self.state.selected_channel.or(self.restore_channel);
            if let Some(user) = &self.state.current_user {
                session.user = serde_json::to_value(user).ok();
            }
        }
        if let Err(message) = self.persist_active_session().await {
            self.emit(Event::Error(message));
        }
    }

    fn remember_device_labels(&mut self, devices: &fluxer_media::DeviceInventory) {
        let mut changed = false;
        if let Some(device) = devices
            .inputs
            .iter()
            .find(|d| d.id == self.audio_preferences.input)
            && self.audio_preferences.input_label != device.label
        {
            self.audio_preferences.input_label = device.label.clone();
            changed = true;
        }
        if let Some(device) = devices
            .outputs
            .iter()
            .find(|d| d.id == self.audio_preferences.output)
            && self.audio_preferences.output_label != device.label
        {
            self.audio_preferences.output_label = device.label.clone();
            changed = true;
        }
        if changed {
            self.persist_audio_preferences();
        }
    }

    async fn prepare_audio(&mut self, require_output: bool) -> Result<()> {
        let job = self.media()?.begin_device_recovery();
        let result = job
            .await
            .map_err(|_| "Audio device recovery stopped".to_owned())?
            .map_err(error)?;
        let devices = self
            .media()?
            .finish_device_recovery(result)
            .ok_or("Audio device recovery was cancelled")?;
        let ready = devices.input_selection.effective.is_some()
            && (!require_output || devices.output_selection.effective.is_some());
        self.remember_device_labels(&devices);
        self.emit(Event::Devices(devices));
        if ready {
            Ok(())
        } else {
            Err("No usable audio device is available".into())
        }
    }

    fn persist_audio_preferences(&self) {
        if let Some(key) = &self.audio_key
            && let Err(message) = crate::audio_preferences::save(key, &self.audio_preferences)
        {
            self.emit(Event::Error(message));
        }
    }

    async fn invalidate_saved_session(&mut self) {
        if let Some(session) = &mut self.saved_session {
            session.valid = false;
        }
        if let Err(message) = self.persist_active_session().await {
            self.emit(Event::Error(message));
        }
        self.reset().await;
        self.snapshot();
    }
    fn emit(&self, event: Event) {
        if let Event::VoiceStatus(status) = &event
            && let Ok(mut snapshot) = self.call_snapshot.lock()
            && let Some(snapshot) = snapshot.as_mut()
        {
            snapshot.status = status.clone();
        }
        let _ = self.events.send(event);
    }
    pub(crate) fn with_call_snapshot(
        mut self,
        snapshot: Arc<Mutex<Option<crate::CallControlSnapshot>>>,
    ) -> Self {
        self.call_snapshot = snapshot;
        self
    }
    fn call_control_snapshot(&self) -> Option<crate::CallControlSnapshot> {
        Some(crate::CallControlSnapshot {
            account_id: self.state.current_user.as_ref()?.id,
            generation: self.call_generation,
            channel_id: self.voice_target.as_ref()?.channel_id,
            status: self
                .call_snapshot
                .lock()
                .ok()
                .and_then(|snapshot| {
                    snapshot
                        .as_ref()
                        .filter(|s| s.generation == self.call_generation)
                        .map(|s| s.status.clone())
                })
                .unwrap_or(VoiceStatus::Connecting {
                    channel_id: self.voice_target.as_ref()?.channel_id,
                }),
            muted: self.state.voice.muted,
            deafened: self.state.voice.deafened,
            server_muted: self.server_muted,
            server_deafened: self.server_deafened,
            screen_sharing: self.state.voice.screen_share,
        })
    }
    fn snapshot(&self) {
        let next = self.call_control_snapshot();
        if let Ok(mut snapshot) = self.call_snapshot.lock() {
            *snapshot = next;
        }
        self.emit(Event::State(Box::new(self.state.clone())));
    }
    fn client(&self) -> Result<Client> {
        self.client.clone().ok_or_else(|| "Sign in first".into())
    }
    fn register_gif_share(&self, share_id: String, query: String) {
        let Ok(client) = self.client() else { return };
        let locale = self.locale.clone();
        tokio::spawn(async move {
            let _ = client.register_gif_share(&share_id, &query, &locale).await;
        });
    }
    fn selected(&self) -> Result<Snowflake> {
        self.state
            .selected_channel
            .ok_or_else(|| "Choose a channel first".into())
    }
    fn media(&mut self) -> Result<&mut MediaEngine> {
        if self.voice_connect.is_some() {
            return Err("Audio settings are unavailable while voice is connecting".into());
        }
        if self.media.is_none() {
            let mut media = MediaEngine::new().map_err(error)?;
            media.restore_device_preferences(
                &self.audio_preferences.input,
                &self.audio_preferences.output,
            );
            media
                .configure(self.audio_preferences.dsp.clone())
                .map_err(error)?;
            media
                .configure_mixer(self.audio_preferences.mixer.clone())
                .map_err(error)?;
            self.media = Some(media);
        }
        self.media
            .as_mut()
            .ok_or_else(|| "Media engine unavailable".into())
    }

    pub(crate) async fn run(mut self, mut commands: async_mpsc::UnboundedReceiver<Command>) {
        self.snapshot();
        let (idle_tx, mut idle_rx) = async_mpsc::unbounded_channel();
        let idle_watch = idle_flow::start(idle_tx);
        let mut device_poll = tokio::time::interval(Duration::from_secs(5));
        let mut presence_poll = tokio::time::interval(Duration::from_secs(1));
        let mut last_timed_presence = None;
        let mut attempted_status_expiry = None;
        let mut expiry_account = None;
        let mut last_devices = None;
        let mut last_device_poll = (boot_time(), std::time::Instant::now());
        let mut recovery_requested = false;
        let mut recovery: Option<JoinHandle<fluxer_media::Result<fluxer_media::DeviceRecovery>>> =
            None;
        loop {
            enum Incoming {
                Presence(Option<presence_flow::PresenceEvent>),
                DisplaySettings(Option<display_settings_flow::DisplaySettingsEvent>),
                People(Option<people_flow::PeopleEvent>),
                Invite(Option<invite_flow::InviteEvent>),
                InviteTimeout,
                Kick(Option<people_flow::KickEvent>),
                Dm(Option<DmEvent>),
                Sticker(Option<StickerEvent>),
                ThreadCreated(Result<Value>),
                DevicePoll,
                PresencePoll,
                SystemIdle(Option<Option<(bool, bool)>>),
                DeviceRecovery(
                    std::result::Result<
                        fluxer_media::Result<fluxer_media::DeviceRecovery>,
                        tokio::task::JoinError,
                    >,
                ),
                Navigation(Option<navigation_flow::NavigationEvent>),
                Profile(Option<profile_flow::ProfileEvent>),
                OwnProfile(Option<own_profile_flow::OwnProfileEvent>),
                Community(Option<community_flow::CommunityEvent>),
                Channel(Option<channel_flow::ChannelEvent>),
                Attachment(Option<attachment_flow::AttachmentEvent>),
                Command(Option<Command>),
                Gateway(Option<GatewayEvent>),
                Media(Option<MediaEvent>),
                VoiceTimeout,
                VoiceRetry,
                VoiceConnect(
                    Box<
                        std::result::Result<
                            (MediaEngine, Result<MediaEvents>),
                            tokio::task::JoinError,
                        >,
                    >,
                ),
                BeginIpPoll,
                IpPoll(
                    std::result::Result<
                        fluxer_protocol::Result<Option<AuthSession>>,
                        tokio::task::JoinError,
                    >,
                ),
                BeginBrowserPoll,
                BrowserPoll(Box<BrowserPollResult>),
                Restore(Box<Result<Option<SavedSession>>>),
            }
            let incoming = tokio::select! {
                event=self.presence.rx.recv()=>Incoming::Presence(event),
                event=self.display_settings.rx.recv()=>Incoming::DisplaySettings(event),
                event=self.people.rx.recv()=>Incoming::People(event),
                event=self.invites.rx.recv()=>Incoming::Invite(event),
                _ = voice_timeout(self.invites.deadline)=>Incoming::InviteTimeout,
                event=self.people.kick_rx.recv()=>Incoming::Kick(event),
                event=self.dm_rx.recv()=>Incoming::Dm(event),
                event=self.sticker_rx.recv()=>Incoming::Sticker(event),
                result=thread_creation_result(&mut self.thread_creation)=>Incoming::ThreadCreated(result),
                _ = presence_poll.tick() => Incoming::PresencePoll,
                event = idle_rx.recv() => Incoming::SystemIdle(event),
                _ = device_poll.tick() => Incoming::DevicePoll,
                result = recovery_result(&mut recovery) => Incoming::DeviceRecovery(result),
                event=self.navigation.rx.recv()=>Incoming::Navigation(event),
                event=self.profiles.rx.recv()=>Incoming::Profile(event),
                event=self.own_profile.rx.recv()=>Incoming::OwnProfile(event),
                event=self.community.rx.recv()=>Incoming::Community(event),
                event=self.channels.rx.recv()=>Incoming::Channel(event),
                event=self.attachments.rx.recv()=>Incoming::Attachment(event),
                command = commands.recv() => Incoming::Command(command),
                event = receive_optional(&mut self.gateway_events) => Incoming::Gateway(event),
                event = receive_media(&mut self.media_events) => Incoming::Media(event),
                _ = voice_timeout(self.voice_join_deadline) => Incoming::VoiceTimeout,
                _ = voice_timeout(self.voice_recovery_at) => Incoming::VoiceRetry,
                result = voice_connect_result(&mut self.voice_connect) => Incoming::VoiceConnect(Box::new(result)),
                _ = voice_timeout(self.pending_ip.as_ref().and_then(|pending|pending.next_poll)) => Incoming::BeginIpPoll,
                result=ip_poll_result(&mut self.ip_poll_task)=>Incoming::IpPoll(result),
                _ = voice_timeout(self.pending_browser.as_ref().and_then(|pending|pending.next_poll))=>Incoming::BeginBrowserPoll,
                result=browser_poll_result(&mut self.browser_poll_task)=>Incoming::BrowserPoll(Box::new(result)),
                result=restore_result(&mut self.restore_task)=>Incoming::Restore(Box::new(result)),
            };
            let result = match incoming {
                Incoming::Presence(Some(event)) => {
                    self.presence_event(event).await;
                    Ok(())
                }
                Incoming::Presence(None) => Ok(()),
                Incoming::DisplaySettings(Some(event)) => {
                    self.display_settings_event(event).await;
                    Ok(())
                }
                Incoming::DisplaySettings(None) => Ok(()),
                Incoming::OwnProfile(Some(event)) => {
                    self.own_profile_event(event);
                    Ok(())
                }
                Incoming::OwnProfile(None) => Ok(()),
                Incoming::People(Some(event)) => {
                    self.people_event(event);
                    Ok(())
                }
                Incoming::People(None) => Ok(()),
                Incoming::Invite(event) => {
                    if let Some(event) = event {
                        self.invite_event(event).await;
                    }
                    Ok(())
                }
                Incoming::InviteTimeout => {
                    self.invite_timeout();
                    Ok(())
                }
                Incoming::Kick(Some(event)) => {
                    self.kick_event(event);
                    Ok(())
                }
                Incoming::Kick(None) => Ok(()),
                Incoming::Dm(Some(event)) => self.finish_open_dm(event).await,
                Incoming::Dm(None) => Ok(()),
                Incoming::Sticker(Some(event)) => {
                    if self.sticker_jobs.remove(&event.token).is_some() {
                        match event.result {
                            Ok(message) if message.channel_id == event.channel => {
                                self.state.insert_message(message);
                                self.snapshot();
                            }
                            Ok(_) => self.emit(Event::Error(
                                "Sticker response had an unexpected channel".into(),
                            )),
                            Err(message) => self.emit(Event::Error(message)),
                        }
                    }
                    Ok(())
                }
                Incoming::Sticker(None) => Ok(()),
                Incoming::ThreadCreated(result) => {
                    self.finish_thread_creation(result);
                    Ok(())
                }
                Incoming::SystemIdle(Some(idle)) => {
                    self.system_idle = idle;
                    Ok(())
                }
                Incoming::SystemIdle(None) => Ok(()),
                Incoming::PresencePoll => {
                    idle_watch.timeout.store(
                        self.state
                            .user_settings
                            .get("afk_timeout")
                            .and_then(Value::as_u64)
                            .unwrap_or(600)
                            .saturating_mul(1000)
                            .min(u32::MAX as u64) as u32,
                        std::sync::atomic::Ordering::Relaxed,
                    );
                    let account = self.state.current_user.as_ref().map(|user| user.id);
                    if account != expiry_account {
                        expiry_account = account;
                        attempted_status_expiry = None;
                        last_timed_presence = None;
                    }
                    self.presence_expiry_tick(&mut attempted_status_expiry)
                        .await;
                    let current = self.presence_payload();
                    if last_timed_presence
                        .as_ref()
                        .is_some_and(|previous| Some(previous) != current.as_ref())
                    {
                        self.publish_presence().await;
                        self.snapshot();
                    }
                    last_timed_presence = current;
                    Ok(())
                }
                Incoming::DevicePoll => {
                    let now = (boot_time(), std::time::Instant::now());
                    let slept = suspend_gap(last_device_poll, now);
                    last_device_poll = now;
                    if slept && self.voice_target.is_some() && self.voice_recovery_started.is_none()
                    {
                        self.media_event(MediaEvent::TransportLost).await
                    } else {
                        if recovery.is_none()
                            && let Some(media) = &mut self.media
                        {
                            recovery_requested = false;
                            recovery = Some(media.begin_device_recovery());
                        }
                        Ok(())
                    }
                }
                Incoming::DeviceRecovery(result) => {
                    recovery = None;
                    if recovery_requested
                        && let Err(message) = result
                            .as_ref()
                            .map_err(|e| e.to_string())
                            .and_then(|r| r.as_ref().map(|_| ()).map_err(|e| e.to_string()))
                    {
                        self.emit(Event::Error(message));
                    }
                    if let (Some(media), Ok(Ok(result))) = (&mut self.media, result)
                        && let Some(devices) = media.finish_device_recovery(result)
                    {
                        self.remember_device_labels(&devices);
                        if last_devices.as_ref() != Some(&devices) {
                            last_devices = Some(devices.clone());
                            self.emit(Event::Devices(devices));
                        }
                    }
                    Ok(())
                }
                Incoming::Channel(Some(event)) => {
                    self.channel_event(event);
                    Ok(())
                }
                Incoming::Channel(None) => Ok(()),
                Incoming::Profile(Some(event)) => {
                    self.profile_event(event);
                    Ok(())
                }
                Incoming::Profile(None) => Ok(()),
                Incoming::Community(Some(event)) => {
                    self.community_event(event).await;
                    Ok(())
                }
                Incoming::Community(None) => Ok(()),
                Incoming::Navigation(Some(event)) => {
                    self.navigation_event(event);
                    Ok(())
                }
                Incoming::Navigation(None) => Ok(()),
                Incoming::Attachment(Some(event)) => {
                    self.attachment_event(event);
                    Ok(())
                }
                Incoming::Attachment(None) => Ok(()),
                Incoming::Command(None | Some(Command::Shutdown)) => break,
                Incoming::Command(Some(Command::RefreshDevices)) => {
                    recovery_requested = true;
                    if recovery.is_none() {
                        match self.media() {
                            Ok(media) => {
                                recovery = Some(media.begin_device_recovery());
                                Ok(())
                            }
                            Err(error) => Err(error),
                        }
                    } else {
                        Ok(())
                    }
                }
                Incoming::Command(Some(command)) => self.command(command).await,
                Incoming::Restore(result) => {
                    self.restore_task = None;
                    let restored = self.finish_restore(*result).await;
                    if restored.is_err() {
                        self.restoring_gateway = false;
                        self.emit(Event::SessionRestoreFailed);
                    }
                    if !self.restoring_gateway {
                        self.emit(Event::SessionRestoring(false));
                    }
                    restored
                }
                Incoming::Gateway(Some(event)) => self.gateway(event).await,
                Incoming::Gateway(None) => {
                    self.gateway_events = None;
                    self.gateway_ready = false;
                    if self.restoring_gateway {
                        self.restoring_gateway = false;
                        self.emit(Event::SessionRestoring(false));
                        self.emit(Event::SessionRestoreFailed);
                    }
                    self.leave_voice(false).await
                }
                Incoming::Media(Some(event)) => self.media_event(event).await,
                Incoming::Media(None) => {
                    self.media_events = None;
                    Ok(())
                }
                Incoming::VoiceConnect(result) => self.finish_voice_connect(*result).await,
                Incoming::VoiceRetry => self.retry_voice().await,
                Incoming::VoiceTimeout => {
                    self.voice_join_deadline = None;
                    if self.voice_recovery_started.is_some() {
                        self.schedule_voice_retry().await
                    } else {
                        let _ = self.leave_voice(true).await;
                        Err(
                            "Voice server did not complete the connection; try joining again"
                                .into(),
                        )
                    }
                }
                Incoming::BeginIpPoll => {
                    self.begin_ip_poll();
                    Ok(())
                }
                Incoming::IpPoll(result) => {
                    self.ip_poll_task = None;
                    self.finish_ip_poll(
                        result
                            .map_err(error)
                            .and_then(|result| result.map_err(error)),
                    )
                    .await
                }
                Incoming::BeginBrowserPoll => {
                    self.begin_browser_poll();
                    Ok(())
                }
                Incoming::BrowserPoll(result) => {
                    self.browser_poll_task = None;
                    match *result {
                        Ok((session, result)) => {
                            if let Some(pending) = &mut self.pending_browser {
                                pending.session = Some(session);
                            }
                            self.finish_browser_poll(result.map_err(error)).await
                        }
                        Err(message) => {
                            self.clear_browser_login();
                            Err(message.to_string())
                        }
                    }
                }
            };
            if let Err(message) = result {
                self.emit(Event::Error(message));
            }
        }
        self.reset().await;
    }

    async fn reset(&mut self) {
        if let Some(task) = self.account_sessions_task.take() {
            task.abort();
        }
        self.thread_creation = None;
        self.cancel_open_dm();
        self.clear_presence();
        self.clear_display_settings();
        self.clear_people();
        self.clear_invites();
        self.clear_navigation();
        self.clear_own_profile();
        self.clear_channel_jobs();
        self.clear_profiles();
        self.clear_community();
        self.clear_attachments();
        for (_, job) in std::mem::take(&mut self.sticker_jobs) {
            job.abort();
        }
        if let Some(task) = self.restore_task.take() {
            task.abort();
        }
        self.emit(Event::SessionRestoring(false));
        self.saved_session = None;
        self.audio_key = None;
        self.audio_preferences = crate::audio_preferences::AudioPreferences::default();
        self.restore_channel = None;
        self.restoring_gateway = false;
        let _ = self.leave_voice(true).await;
        if let Some(tx) = self.gateway_commands.take() {
            let _ = tx.try_send(GatewayCommand::Close);
        }
        if let Some(task) = self.gateway_task.take() {
            task.abort();
        }
        self.gateway_events = None;
        self.gateway_ready = false;
        self.pending_voice_leave = None;
        self.voice_sync_pending = false;
        self.retired_voice_connections.clear();
        self.last_sequence = None;
        self.expected_user_id = None;
        self.selected_guild = None;
        self.synced_guilds.clear();
        self.member_query = None;
        self.member_list_subscription = None;
        self.member_list_identity = None;
        self.member_wire_list_id = None;
        self.pending_mfa = None;
        self.pending_ip = None;
        if let Some(task) = self.ip_poll_task.take() {
            task.abort();
        }
        self.emit(Event::IpAuthorizationCleared);
        self.clear_browser_login();
        self.client = None;
        self.media = None;
        self.emit(Event::MediaEndpoint(None));
        self.emit(Event::InstancePresentation(None));
        self.state = ClientState::default();
        self.state.settings.locale = self.locale.clone();
        self.demo = false;
        if let Ok(mut frames) = self.frames.lock() {
            frames.clear();
        }
    }

    async fn command(&mut self, command: Command) -> Result<()> {
        match command {
            Command::ControlCall {
                account_id,
                generation,
                action,
                deadline,
                reply,
            } => {
                let result = if reply.is_closed() || std::time::Instant::now() >= deadline {
                    Err("Call control request expired".into())
                } else if !self
                    .call_control_snapshot()
                    .is_some_and(|s| s.account_id == account_id && s.generation == generation)
                {
                    Err("Call changed or is no longer active".into())
                } else {
                    let command = match action {
                        crate::CallAction::ToggleMute => Command::SetMute(!self.state.voice.muted),
                        crate::CallAction::ToggleDeaf => {
                            Command::SetDeaf(!self.state.voice.deafened)
                        }
                        crate::CallAction::SetMute(value) => Command::SetMute(value),
                        crate::CallAction::SetDeaf(value) => Command::SetDeaf(value),
                        crate::CallAction::Disconnect => Command::LeaveVoice,
                        crate::CallAction::StopSharing => Command::StopShare,
                    };
                    Box::pin(self.command(command))
                        .await
                        .map(|()| self.call_control_snapshot())
                };
                let _ = reply.send(result);
            }
            Command::RestoreSession => self.begin_restore(),
            Command::Login {
                instance,
                identifier,
                password,
            } => {
                self.reset().await;
                self.emit(Event::Demo(false));
                self.snapshot();
                self.emit(Event::Status("Discovering instance…".into()));
                let client = Client::new(Instance::discover(&instance).await.map_err(error)?)
                    .map_err(error)?
                    .with_auth_locale(self.locale.clone())
                    .map_err(error)?;
                let identifier = login_identifier(client.instance(), identifier);
                match client.login(identifier, &password).await.map_err(error)? {
                    LoginOutcome::Authenticated {
                        token,
                        user_id,
                        user,
                    } => {
                        self.authenticate(
                            client.with_token(token.clone()),
                            token,
                            Some(user_id),
                            user,
                        )
                        .await?
                    }
                    LoginOutcome::Mfa {
                        ticket,
                        totp,
                        webauthn,
                        ..
                    } => {
                        self.pending_mfa = Some(client);
                        self.emit(Event::MfaRequired {
                            ticket,
                            totp,
                            webauthn,
                        });
                    }
                    LoginOutcome::IpAuthorization {
                        email,
                        ticket,
                        resend_available_in,
                    } => {
                        self.pending_ip = Some(PendingIpAuthorization {
                            client,
                            ticket,
                            resend_deadline: tokio::time::Instant::now()
                                .checked_add(Duration::from_secs(resend_available_in))
                                .ok_or("Invalid authorization resend cooldown")?,
                            resend_used: false,
                            consecutive_errors: 0,
                            next_poll: Some(tokio::time::Instant::now()),
                        });
                        self.emit(Event::IpAuthorizationRequired {
                            email,
                            resend_available_in,
                        });
                    }
                }
            }
            Command::TokenLogin { instance, token } => {
                self.reset().await;
                self.emit(Event::Demo(false));
                self.snapshot();
                let token = SessionToken::new(token).map_err(error)?;
                let client = Client::new(Instance::discover(&instance).await.map_err(error)?)
                    .map_err(error)?
                    .with_auth_locale(self.locale.clone())
                    .map_err(error)?
                    .with_token(token.clone());
                self.authenticate(client, token, None, None).await?;
            }
            Command::BrowserLogin {
                instance,
                identifier,
            } => {
                self.reset().await;
                self.emit(Event::Demo(false));
                self.snapshot();
                let client = Client::new(Instance::discover(&instance).await.map_err(error)?)
                    .map_err(error)?;
                let session = client.initiate_desktop_handoff().await.map_err(error)?;
                let prefill = (!identifier.trim().is_empty())
                    .then(|| login_identifier(client.instance(), identifier.trim().to_owned()));
                let url = client
                    .desktop_handoff_url(&session.code, prefill.as_ref())
                    .map_err(error)?;
                self.emit(Event::BrowserLoginRequired {
                    code: session.code.clone(),
                    expires_at: session.expires_at.clone(),
                    browser_url: url.to_string(),
                });
                self.pending_browser = Some(PendingBrowserLogin {
                    client,
                    session: Some(session),
                    consecutive_errors: 0,
                    next_poll: Some(tokio::time::Instant::now() + IP_POLL_INTERVAL),
                });
            }
            Command::RetryBrowserLogin => {
                if let Some(pending) = &mut self.pending_browser
                    && pending.consecutive_errors >= IP_MAX_POLL_ERRORS
                {
                    if pending
                        .session
                        .as_ref()
                        .is_some_and(DesktopHandoffSession::is_expired)
                    {
                        self.clear_browser_login();
                        return Err("Browser sign-in code expired; request a new code".into());
                    }
                    pending.consecutive_errors = 0;
                    pending.next_poll = Some(tokio::time::Instant::now() + IP_POLL_INTERVAL);
                    self.emit(Event::BrowserLoginPollingResumed);
                }
            }
            Command::LoginMfa { ticket, code } => {
                let client = self.pending_mfa.clone().ok_or("Start a sign-in first")?;
                let session = client.login_mfa_totp(&ticket, &code).await.map_err(error)?;
                self.pending_mfa = None;
                self.authenticate(
                    client.with_token(session.token.clone()),
                    session.token,
                    Some(session.user_id),
                    session.user,
                )
                .await?;
            }
            Command::ResendIpAuthorization => {
                let Some(pending) = &self.pending_ip else {
                    return Ok(());
                };
                if pending.resend_used || tokio::time::Instant::now() < pending.resend_deadline {
                    return Ok(());
                }
                pending
                    .client
                    .resend_ip_authorization(&pending.ticket)
                    .await
                    .map_err(error)?;
                if let Some(pending) = &mut self.pending_ip {
                    pending.resend_used = true;
                    pending.resend_deadline = tokio::time::Instant::now() + Duration::from_secs(30);
                }
                self.emit(Event::IpAuthorizationResent {
                    resend_available_in: 30,
                });
            }
            Command::RetryIpAuthorization => {
                if let Some(pending) = &mut self.pending_ip
                    && pending.consecutive_errors >= IP_MAX_POLL_ERRORS
                {
                    pending.consecutive_errors = 0;
                    pending.next_poll = Some(tokio::time::Instant::now());
                    self.emit(Event::IpAuthorizationPollingResumed);
                }
            }
            Command::CancelLogin => {
                self.reset().await;
                self.emit(Event::Demo(false));
                self.snapshot();
                self.emit(Event::Status("Sign in to your Fluxer instance".into()));
            }
            Command::Logout => {
                // AuthSessionLifecycle.logout clears durable local credentials
                // before best-effort server notification. A failed clear must
                // not falsely report logout and restore the account next boot.
                if !self.demo
                    && let Some(store) = &self.session_store
                {
                    tokio::time::timeout(Duration::from_secs(15), store.clear())
                            .await
                            .map_err(|_| "Could not clear the saved session: keyring timed out")?
                            .map_err(|_| "Could not clear the saved session; unlock your keyring and retry signing out")?;
                }
                let client = self.client.clone();
                self.reset().await;
                self.emit(Event::Demo(false));
                self.snapshot();
                if let Some(client) = client {
                    let result =
                        tokio::time::timeout(Duration::from_secs(5), client.logout()).await;
                    if !matches!(result, Ok(Ok(()))) {
                        self.emit(Event::Error(
                            "Signed out locally; the server could not be notified".into(),
                        ));
                    }
                }
                self.emit(Event::Status("Signed out".into()));
            }
            Command::Demo => {
                self.reset().await;
                self.demo = true;
                self.state = ClientState::demo();
                self.state.user_settings = json!({"status":"online","custom_status":null});
                self.emit(Event::Demo(true));
                self.snapshot();
            }
            Command::FetchAccountSessions { request_id } => {
                let client = self.client()?;
                let events = self.events.clone();
                if let Some(task) = self.account_sessions_task.take() {
                    task.abort();
                }
                self.account_sessions_task = Some(tokio::spawn(async move {
                    let result =
                        match tokio::time::timeout(Duration::from_secs(15), client.auth_sessions())
                            .await
                        {
                            Ok(result) => result.map_err(|e| e.to_string()),
                            Err(_) => Err("Loading devices timed out".into()),
                        };
                    let _ = events.send(Event::AccountSessions { request_id, result });
                }));
            }
            Command::SetLocale(locale) => {
                if locale.is_empty() || locale.bytes().any(|byte| byte < 32 || byte == 127) {
                    return Err("Invalid client locale".into());
                }
                if let Some(client) = self.client.clone() {
                    self.client = Some(client.with_auth_locale(locale.clone()).map_err(error)?);
                }
                if let Some(client) = self.pending_mfa.clone() {
                    self.pending_mfa =
                        Some(client.with_auth_locale(locale.clone()).map_err(error)?);
                }
                if let Some(pending) = &mut self.pending_ip {
                    pending.client = pending
                        .client
                        .clone()
                        .with_auth_locale(locale.clone())
                        .map_err(error)?;
                }
                self.locale = locale.clone();
                self.state.settings.locale = locale;
                self.persist_selection().await;
                self.snapshot();
            }
            Command::UpdateOwnProfile {
                global_name,
                bio,
                pronouns,
            } => {
                if let Err(message) = self.update_own_profile(global_name, bio, pronouns) {
                    self.emit(Event::OwnProfileMutation(
                        crate::RelationshipMutationStatus::Failed(message),
                    ));
                }
            }
            Command::UpdateDisplaySetting(setting) => {
                if let Err(message) = self.set_display_setting(setting).await {
                    self.emit(Event::DisplaySettingsMutation(
                        crate::RelationshipMutationStatus::Failed(message),
                    ));
                }
            }
            Command::SetPresenceStatus {
                status,
                duration_minutes,
            } => {
                if let Err(message) = self.set_presence_status(status, duration_minutes).await {
                    self.emit(Event::PresenceMutation(
                        crate::RelationshipMutationStatus::Failed(message),
                    ));
                }
            }
            Command::SetCustomStatus {
                text,
                emoji_id,
                emoji_name,
                expires_at,
            } => {
                if let Err(message) = self
                    .set_custom_status(text, emoji_id, emoji_name, expires_at)
                    .await
                {
                    self.emit(Event::PresenceMutation(
                        crate::RelationshipMutationStatus::Failed(message),
                    ));
                }
            }
            Command::ClearCustomStatus => {
                if let Err(message) = self.clear_custom_status().await {
                    self.emit(Event::PresenceMutation(
                        crate::RelationshipMutationStatus::Failed(message),
                    ));
                }
            }
            Command::SetMemberRole {
                guild_id,
                user_id,
                role_id,
                add,
            } => {
                if let Err(message) = self.set_member_role(guild_id, user_id, role_id, add) {
                    self.emit(Event::MemberRoleMutation {
                        guild_id,
                        user_id,
                        role_id,
                        add,
                        error: Some(message),
                    });
                }
            }
            Command::ChangeRelationship { user_id, action } => {
                if let Err(message) = self.change_relationship(user_id, action) {
                    self.emit(Event::RelationshipMutation {
                        user_id,
                        status: crate::RelationshipMutationStatus::Failed(message),
                    });
                }
            }
            Command::KickMember { guild_id, user_id } => {
                if let Err(message) = self.kick_member(guild_id, user_id) {
                    self.emit(Event::Error(message));
                }
            }
            Command::SetMemberList { channel_id, start } => {
                self.set_member_list(channel_id, start).await?
            }
            Command::QueryGuildMembers {
                guild_id,
                query,
                limit,
            } => self.query_guild_members(guild_id, query, limit).await?,
            Command::OpenDm { user_id } => {
                self.emit(Event::DirectMessageOpen {
                    user_id,
                    status: crate::DirectMessageStatus::Pending,
                });
                if let Err(message) = self.open_dm(user_id).await {
                    self.emit(Event::DirectMessageOpen {
                        user_id,
                        status: crate::DirectMessageStatus::Failed(message),
                    });
                } else if self.dm_job.is_none() {
                    self.emit(Event::DirectMessageOpen {
                        user_id,
                        status: crate::DirectMessageStatus::Complete,
                    });
                }
            }
            Command::CreateGroupDm { recipients } => {
                if let Err(message) = self.create_group_dm(recipients).await {
                    self.emit(Event::Status(format!(
                        "Couldn't create group DM: {message}"
                    )));
                }
            }
            Command::LoadUserProfile {
                user_id,
                guild_id,
                force,
            } => self.load_user_profile(user_id, guild_id, force),
            Command::CreateCommunity { name } => self.create_community(name),
            Command::LeaveCommunity {
                guild_id,
                delete_messages,
            } => self.leave_community(guild_id, delete_messages),
            Command::PreviewInvite { request_id, input } => self.preview_invite(request_id, input),
            Command::AcceptInvite { request_id } => self.accept_invite(request_id).await,
            Command::CancelInvite { request_id } => self.cancel_invite(request_id),
            Command::CreateBasicChannel {
                guild_id,
                parent_id,
                kind,
                name,
            } => {
                if let Err(message) = self.create_basic_channel(guild_id, parent_id, kind, name) {
                    self.emit(Event::Error(message));
                }
            }
            Command::RenameBasicChannel { channel_id, name } => {
                if let Err(message) = self.rename_basic_channel(channel_id, name) {
                    self.emit(Event::Error(message));
                }
            }
            Command::SelectGuild { guild_id } => {
                self.supersede_invite_navigation();
                self.cancel_open_dm();
                self.selected_guild = Some(guild_id);
                if !self.demo {
                    self.sync_selected_guild().await?;
                    self.persist_selection().await;
                }
                self.snapshot();
            }
            Command::SelectChannel { channel_id } => {
                self.supersede_invite_navigation();
                self.cancel_open_dm();
                self.select_channel(channel_id).await?;
            }
            Command::CreatePublicThread { parent_id, name } => {
                if let Err(message) = self.begin_thread_creation(parent_id, &name) {
                    self.emit(Event::ThreadCreated {
                        parent_id,
                        result: Err(message),
                    });
                }
            }
            Command::LoadForum {
                channel_id,
                creation_order,
            } => self.load_forum(channel_id, creation_order)?,
            Command::SearchMessages {
                channel_id,
                query,
                scope,
                scope_override,
                sort,
                page,
            } => self.search_messages(channel_id, query, scope, scope_override, sort, page)?,
            Command::FetchMentions { before, filters } => self.fetch_mentions(before, filters),
            Command::CloseMentions => self.navigation_close_mentions(),
            Command::CloseSearch { channel_id } => self.close_search(channel_id),
            Command::LoadPins { channel_id, more } => self.load_pins(channel_id, more)?,
            Command::ClosePins { channel_id } => self.close_pins(channel_id),
            Command::SetMessagePinned {
                channel_id,
                message_id,
                pinned,
            } => self.set_message_pinned(channel_id, message_id, pinned)?,
            Command::JumpToMessage {
                channel_id,
                message_id,
            } => self.jump_to_message(channel_id, message_id).await?,
            Command::LoadOlderMessages { channel_id } => {
                self.load_adjacent_history(channel_id, false)?
            }
            Command::LoadNewerMessages { channel_id } => {
                self.load_adjacent_history(channel_id, true)?
            }
            Command::ReturnToLatest { channel_id } => {
                self.request_history(channel_id, navigation_flow::HistoryRequest::Latest)?
            }
            Command::AcknowledgeMessage {
                channel_id,
                message_id,
            } => {
                if let Err(message) = self.acknowledge_visible_message(channel_id, message_id) {
                    self.emit(Event::MessageAckResult {
                        channel_id,
                        message_id,
                        error: Some(message.clone()),
                    });
                    return Err(message);
                }
            }
            Command::MarkChannelRead { channel_id } => {
                let channel = self
                    .state
                    .channels
                    .get(&channel_id)
                    .ok_or("Unknown channel")?;
                let channels = if channel.kind == ChannelType::Category {
                    self.state
                        .channels
                        .values()
                        .filter(|child| {
                            child.guild_id == channel.guild_id
                                && child.parent_id == Some(channel_id)
                                && child.kind.is_message_channel()
                                && self.state.current_user.as_ref().is_some_and(|me| {
                                    self.state
                                        .permissions_for(child.id, me.id)
                                        .contains(Permissions::VIEW_CHANNEL)
                                })
                        })
                        .map(|child| child.id)
                        .collect()
                } else {
                    vec![channel_id]
                };
                self.mark_read(channels).await?;
            }
            Command::MarkGuildRead { guild_id } => {
                let channels = self
                    .state
                    .channels
                    .values()
                    .filter(|c| {
                        c.guild_id == Some(guild_id)
                            && c.kind.is_message_channel()
                            && self.state.current_user.as_ref().is_some_and(|me| {
                                self.state
                                    .permissions_for(c.id, me.id)
                                    .contains(Permissions::VIEW_CHANNEL)
                            })
                            && if c.kind.is_thread() {
                                self.state.thread_guilds.contains(&guild_id)
                                    && ((c.extra.get("member").is_some_and(|v| !v.is_null())
                                        && self.state.is_unread(c.id))
                                        || self
                                            .state
                                            .read_states
                                            .get(&c.id)
                                            .is_some_and(|r| r.mention_count > 0))
                            } else {
                                self.state.is_unread(c.id)
                                    || self
                                        .state
                                        .read_states
                                        .get(&c.id)
                                        .is_some_and(|r| r.mention_count > 0)
                            }
                    })
                    .map(|c| c.id)
                    .collect();
                self.mark_read(channels).await?;
            }
            Command::MarkMessageUnread {
                channel_id,
                message_id,
            } => {
                if let Err(err) = self.mark_unread(channel_id, message_id).await {
                    self.snapshot();
                    return Err(err);
                }
            }
            Command::SetGuildNotificationSettings {
                guild_id,
                muted,
                mute_seconds,
                message_notifications,
            } => {
                self.set_notifications(guild_id, None, muted, mute_seconds, message_notifications)
                    .await?
            }
            Command::SetChannelNotificationSettings {
                guild_id,
                channel_id,
                muted,
                mute_seconds,
                message_notifications,
            } => {
                self.set_notifications(
                    guild_id,
                    Some(channel_id),
                    muted,
                    mute_seconds,
                    message_notifications,
                )
                .await?
            }
            Command::DemoSearch => self.seed_demo_search()?,
            Command::SetDraft(content) => {
                let channel = self.selected()?;
                self.state.set_draft(channel, content);
            }
            Command::StageFiles { channel_id, paths } => self.stage_files(channel_id, paths)?,
            Command::StageClipboardImage {
                channel_id,
                filename,
                mime_type,
                bytes,
            } => self.stage_clipboard(channel_id, filename, mime_type, bytes)?,
            Command::RemoveAttachment { channel_id, id } => {
                self.cancel_attachment(channel_id, id, true)?
            }
            Command::CancelAttachment { channel_id, id } => {
                self.cancel_attachment(channel_id, id, false)?
            }
            Command::RetryAttachment { channel_id, id } => self.retry_attachment(channel_id, id)?,
            Command::SetAttachmentSpoiler {
                channel_id,
                id,
                spoiler,
            } => self.spoiler_attachment(channel_id, id, spoiler)?,
            Command::CancelAttachmentSend { channel_id } => self.cancel_attachment_send(channel_id),
            Command::RetryPendingMessage { channel_id, id } => {
                self.retry_pending_message(channel_id, id)?
            }
            Command::RemovePendingMessage { channel_id, id } => {
                self.remove_pending_message(channel_id, id)?
            }
            Command::BrowseGifs { request, feed } => {
                let client = self.client();
                let locale = self.locale.clone();
                let events = self.events.clone();
                tokio::spawn(async move {
                    let result = match client {
                        Ok(client) => client.gifs(feed, &locale).await.map_err(error),
                        Err(error) => Err(error),
                    };
                    let _ = events.send(Event::Gifs { request, result });
                });
            }
            Command::RegisterGifShare { share_id, query } => {
                self.register_gif_share(share_id, query);
            }
            Command::SendGif {
                id,
                channel_id,
                share,
                query,
                reply_to,
                mention_author,
            } => {
                let reference = reply_to.map(|message_id| MessageReference {
                    channel_id,
                    message_id: Some(message_id),
                    guild_id: None,
                    kind: 0,
                });
                self.request_pending_submission(
                    id,
                    channel_id,
                    share.url,
                    reference,
                    mention_author,
                    false,
                )?;
                self.register_gif_share(share.id, query);
            }
            Command::SendPendingMessage {
                id,
                channel_id,
                content,
                reply_to,
                mention_author,
            } => {
                let reference = reply_to.map(|message_id| MessageReference {
                    channel_id,
                    message_id: Some(message_id),
                    guild_id: None,
                    kind: 0,
                });
                self.request_pending_send(id, channel_id, content, reference, mention_author)?;
            }
            Command::SendSticker {
                channel_id: channel,
                sticker_id,
            } => {
                let target = self
                    .state
                    .channels
                    .get(&channel)
                    .ok_or("Channel is unavailable")?;
                let user = self
                    .state
                    .current_user
                    .as_ref()
                    .ok_or("No current account")?;
                if !fluxer_core::can_send_message(
                    target.kind,
                    self.state.permissions_for(channel, user.id),
                ) {
                    return Err("You cannot send messages to that channel".into());
                }
                let guild = target
                    .guild_id
                    .and_then(|id| self.state.guilds.get(&id))
                    .ok_or("Stickers are unavailable in this channel")?;
                let sticker = guild
                    .extra
                    .get("stickers")
                    .and_then(Value::as_array)
                    .and_then(|items| {
                        items.iter().find(|item| {
                            item.get("id").and_then(Value::as_str) == Some(&sticker_id.to_string())
                        })
                    })
                    .cloned()
                    .ok_or("Sticker is unavailable")?;
                if self.demo {
                    let mut message = Message::text(
                        self.state.next_local_message_id(),
                        channel,
                        self.state
                            .current_user
                            .clone()
                            .ok_or("No current account")?,
                        "",
                    );
                    message.extra.insert("stickers".into(), json!([sticker]));
                    self.state.insert_message(message);
                    self.snapshot();
                } else {
                    if self.sticker_jobs.len() >= 8 {
                        return Err("Too many sticker sends in progress".into());
                    }
                    let client = self.client()?.clone();
                    self.sticker_token = self.sticker_token.wrapping_add(1);
                    let token = self.sticker_token;
                    let tx = self.sticker_tx.clone();
                    let task = tokio::spawn(async move {
                        let result = match client
                            .send_message_body(
                                &channel.to_string(),
                                json!({"sticker_ids": [sticker_id.to_string()]}),
                            )
                            .await
                        {
                            Ok(value) => serde_json::from_value(value).map_err(error),
                            Err(err) => Err(error(err)),
                        };
                        let _ = tx.send(StickerEvent {
                            token,
                            channel,
                            result,
                        });
                    });
                    self.sticker_jobs.insert(token, task);
                }
            }
            Command::SendMessage {
                content,
                reply_to,
                mention_author,
            } => {
                let channel = self.selected()?;
                let reference = reply_to.map(|message_id| MessageReference {
                    channel_id: channel,
                    message_id: Some(message_id),
                    guild_id: None,
                    kind: 0,
                });
                if let Err(message) = self.request_attachment_send(
                    channel,
                    content.clone(),
                    reference,
                    mention_author,
                ) {
                    self.emit(Event::MessageOperationFailed {
                        channel_id: channel,
                        content,
                        error: message.clone(),
                    });
                    return Err(message);
                }
            }
            Command::ForwardMessage {
                source_channel,
                message_id,
                destination,
            } => {
                let target = self
                    .state
                    .channels
                    .get(&destination)
                    .ok_or("Destination is unavailable")?;
                let user = self
                    .state
                    .current_user
                    .as_ref()
                    .ok_or("No current account")?;
                if !fluxer_core::can_send_message(
                    target.kind,
                    self.state.permissions_for(destination, user.id),
                ) {
                    return Err("You cannot send messages to that destination".into());
                }
                let reference = MessageReference {
                    channel_id: source_channel,
                    message_id: Some(message_id),
                    guild_id: self
                        .state
                        .channels
                        .get(&source_channel)
                        .and_then(|channel| channel.guild_id),
                    kind: 1,
                };
                let message = if self.demo {
                    let source = self
                        .state
                        .channel_messages(source_channel)
                        .find(|message| message.id == message_id)
                        .cloned()
                        .ok_or("Source message is unavailable")?;
                    let mut message = Message::text(
                        self.state.next_local_message_id(),
                        destination,
                        self.state
                            .current_user
                            .clone()
                            .ok_or("No current account")?,
                        "",
                    );
                    message.message_reference = Some(reference);
                    let snapshot = source.extra.get("message_snapshots").and_then(Value::as_array).and_then(|snapshots|snapshots.first()).cloned().unwrap_or_else(||json!({"content":source.content,"timestamp":source.timestamp,"edited_timestamp":source.edited_timestamp,"attachments":source.attachments,"embeds":source.extra.get("embeds").cloned().unwrap_or_else(||json!([])),"type":source.kind,"flags":source.flags}));
                    message
                        .extra
                        .insert("message_snapshots".into(), json!([snapshot]));
                    message
                } else {
                    let nonce = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos()
                        .to_string();
                    serde_json::from_value(
                        self.client()?
                            .send_referenced_message(
                                &destination.to_string(),
                                "",
                                Some(&nonce),
                                Some(&reference),
                                false,
                            )
                            .await
                            .map_err(error)?,
                    )
                    .map_err(error)?
                };
                self.state.insert_message(message);
                self.snapshot();
                self.emit(Event::MessageForwarded { destination });
            }
            Command::EditMessage {
                message_id,
                content,
            } => {
                let channel = self.selected()?;
                if self.demo {
                    self.state
                        .edit_message(channel, message_id, content.clone());
                } else {
                    let result = match self.client() {
                        Ok(client) => client
                            .edit_message(&channel.to_string(), &message_id.to_string(), &content)
                            .await
                            .map_err(error),
                        Err(message) => Err(message),
                    };
                    let message = match result {
                        Ok(message) => message,
                        Err(message) => {
                            self.emit(Event::MessageOperationFailed {
                                channel_id: channel,
                                content: content.clone(),
                                error: message.clone(),
                            });
                            return Err(message);
                        }
                    };
                    self.state
                        .apply_gateway("MESSAGE_UPDATE", message)
                        .map_err(error)?;
                }
                self.snapshot();
                self.emit(Event::MessageEdited {
                    channel_id: channel,
                    message_id,
                    content,
                });
            }
            Command::DeleteMessage { message_id } => {
                let channel = self.selected()?;
                if !self.demo {
                    self.client()?
                        .delete_message(&channel.to_string(), &message_id.to_string())
                        .await
                        .map_err(error)?;
                }
                self.state.delete_message(channel, message_id);
                self.snapshot();
            }
            Command::React { message_id, emoji } => {
                let channel = self.selected()?;
                let reaction = emoji
                    .rsplit_once(':')
                    .and_then(|(name, id)| {
                        id.parse::<Snowflake>().ok().map(|id| Emoji {
                            id: Some(id),
                            name: name.to_owned(),
                            animated: false,
                        })
                    })
                    .unwrap_or_else(|| Emoji::unicode(emoji.clone()));
                let existing = self
                    .state
                    .channel_messages(channel)
                    .find(|m| m.id == message_id)
                    .and_then(|m| m.reactions.iter().find(|r| r.emoji.same_as(&reaction)))
                    .is_some_and(|r| r.me);
                if !self.demo {
                    let client = self.client()?;
                    if existing {
                        client
                            .remove_reaction(&channel.to_string(), &message_id.to_string(), &emoji)
                            .await
                            .map_err(error)?;
                    } else {
                        client
                            .add_reaction(&channel.to_string(), &message_id.to_string(), &emoji)
                            .await
                            .map_err(error)?;
                    }
                }
                if self.demo {
                    self.state.toggle_reaction(channel, message_id, reaction);
                    self.snapshot();
                }
            }
            Command::Typing => {
                if !self.demo {
                    self.client()?
                        .typing(&self.selected()?.to_string())
                        .await
                        .map_err(error)?;
                }
            }
            Command::JoinVoice {
                guild_id,
                channel_id,
            } => {
                if self.demo {
                    return Err("Voice transmission requires signing in to a real instance".into());
                }
                if !self.gateway_ready {
                    return Err("Wait for the gateway to connect".into());
                }
                let channel = self
                    .state
                    .channels
                    .get(&channel_id)
                    .ok_or("Unknown voice channel")?;
                if channel.kind != ChannelType::Voice || channel.guild_id != guild_id {
                    return Err("Choose a voice channel in the current community".into());
                }
                self.leave_voice(true).await?;
                self.call_generation = self.call_generation.wrapping_add(1);
                self.voice_target = Some(VoiceTarget {
                    guild_id,
                    channel_id,
                    connection_id: None,
                });
                self.voice_join_deadline =
                    Some(tokio::time::Instant::now() + Duration::from_secs(15));
                if let Err(message) = self.update_voice().await {
                    let _ = self.leave_voice(false).await;
                    return Err(message);
                }
                self.snapshot();
                self.emit(Event::VoiceStatus(VoiceStatus::Connecting { channel_id }));
            }
            Command::LeaveVoice => self.leave_voice(true).await?,
            Command::SetMute(muted) => {
                let mut next = self.state.voice.clone();
                next.set_muted(muted);
                if let Some(media) = &mut self.media {
                    media
                        .set_muted(next.muted || self.server_muted)
                        .map_err(error)?;
                }
                self.state.voice = next;
                self.snapshot();
                self.update_voice().await?;
            }
            Command::SetDeaf(deafened) => {
                let mut next = self.state.voice.clone();
                next.set_deafened(deafened);
                if let Some(media) = &mut self.media {
                    media
                        .set_deafened(next.deafened || self.server_deafened)
                        .map_err(error)?;
                    media
                        .set_muted(next.muted || self.server_muted)
                        .map_err(error)?;
                }
                self.state.voice = next;
                self.snapshot();
                self.update_voice().await?;
            }
            Command::RefreshDevices => {
                let devices = self.media()?.try_devices().map_err(error)?;
                self.emit(Event::Devices(devices));
            }
            Command::SetInputDevice(id) => {
                let result = self.media()?.set_input_device(&id);
                if !matches!(result, Err(fluxer_media::MediaError::UnknownDevice(_))) {
                    self.audio_preferences.input = id;
                    self.audio_preferences.input_label = "Selected device".into();
                    self.persist_audio_preferences();
                }
                result.map_err(error)?;
            }
            Command::SetOutputDevice(id) => {
                let result = self.media()?.set_output_device(&id);
                if !matches!(result, Err(fluxer_media::MediaError::UnknownDevice(_))) {
                    self.audio_preferences.output = id;
                    self.audio_preferences.output_label = "Selected device".into();
                    self.persist_audio_preferences();
                }
                result.map_err(error)?;
            }
            Command::SetFilePlaybackPreference { kind, preference } => {
                match kind {
                    crate::FilePlaybackKind::Audio => {
                        self.audio_preferences.audio_playback = preference.sanitized()
                    }
                    crate::FilePlaybackKind::Video => {
                        self.audio_preferences.video_playback = preference.sanitized()
                    }
                }
                self.persist_audio_preferences();
            }
            Command::SetUserAudioPreference { user, preference } => {
                if preference
                    .gain
                    .is_some_and(|g| !g.is_finite() || !(0.0..=2.0).contains(&g))
                {
                    return Err("Invalid user audio preference".into());
                }
                self.audio_preferences
                    .users
                    .insert(user.to_string(), preference);
                self.persist_audio_preferences();
            }
            Command::SetDsp(settings) => {
                self.media()?.configure(settings.clone()).map_err(error)?;
                self.audio_preferences.dsp = settings;
                self.persist_audio_preferences();
            }
            Command::SetMixer(settings) => {
                self.media()?
                    .configure_mixer(settings.clone())
                    .map_err(error)?;
                self.audio_preferences.mixer = settings;
                self.persist_audio_preferences();
            }
            Command::SetPushToTalkPressed(pressed) => self
                .media()?
                .set_push_to_talk_pressed(pressed)
                .map_err(error)?,
            Command::SetParticipantAudio { identity, settings } => {
                if identity.is_empty() || !settings.validate() {
                    return Err("Invalid participant audio setting".into());
                }
                if self.voice_connect.is_none() {
                    self.media()?
                        .set_participant_audio(&identity, settings)
                        .map_err(error)?;
                }
                self.participant_audio_intents.insert(identity, settings);
            }
            Command::SetStreamAudio { identity, settings } => {
                if identity.is_empty() || !settings.validate() {
                    return Err("Invalid stream audio setting".into());
                }
                if self.voice_connect.is_none() {
                    self.media()?
                        .set_stream_audio(&identity, settings)
                        .map_err(error)?;
                }
                self.stream_audio_intents.insert(identity, settings);
            }
            Command::WatchStream { identity, watch } => {
                let connected = self.media.as_ref().is_some_and(MediaEngine::is_connected);
                self.set_watch(identity, watch, connected).await?;
            }
            Command::StartMicrophoneTest => {
                if self.voice_target.is_some() {
                    return Err("Leave voice before testing the microphone".into());
                }
                self.prepare_audio(false).await?;
                self.media_events =
                    Some(self.media()?.start_microphone_test().map_err(error)?.into());
            }
            Command::StopMicrophoneTest => {
                if self.voice_target.is_none() {
                    if let Some(media) = &mut self.media {
                        media.stop_microphone_test().map_err(error)?;
                    }
                    self.media_events = None;
                }
            }
            Command::RefreshScreenSources => {
                let sources = self.media()?.screen_sources().map_err(error)?;
                self.emit(Event::ScreenSources(sources));
            }
            Command::StartShare { options, audio } => {
                if self.state.voice.channel_id.is_none() {
                    return Err("Join a voice channel first".into());
                }
                options.validate().map_err(error)?;
                self.pending_share = Some(PendingShare { audio });
                self.active_share_audio = false;
                self.state.voice.screen_share = false;
                self.snapshot();
                self.emit(Event::Media(MediaEvent::ScreenShareStopped));
                self.emit(Event::SharedAudioStatus(false));
                let started = match self.media() {
                    Ok(media) => media.start_screen_share(options).await.map_err(error),
                    Err(message) => Err(message),
                };
                if let Err(message) = started {
                    return self.rollback_share(message).await;
                }
                if let Err(message) = self.update_voice().await {
                    return self.rollback_share(message).await;
                }
                // Publication can precede the OS picker or first captured
                // frame. ScreenShareStarted is the authoritative transition.
            }
            Command::StopShare => {
                self.stop_share().await?;
            }
            Command::RefreshAudioApplications => {
                let applications = MediaEngine::audio_applications().map_err(error)?;
                self.emit(Event::AudioApplications(applications));
            }
            Command::StartSharedAudio(selection) => {
                if !self.state.voice.screen_share {
                    return Err("Start screen sharing before sharing audio".into());
                }
                let started = self
                    .media()?
                    .start_shared_audio(selection)
                    .await
                    .map_err(error);
                self.active_share_audio = started.is_ok();
                self.emit(Event::SharedAudioStatus(self.active_share_audio));
                started?;
            }
            Command::StopSharedAudio => {
                self.active_share_audio = false;
                if let Some(pending) = &mut self.pending_share {
                    pending.audio = None;
                }
                let stopped = if let Some(media) = &mut self.media {
                    media.stop_shared_audio().await.map_err(error)
                } else {
                    Ok(())
                };
                self.emit(Event::SharedAudioStatus(false));
                stopped?;
            }
            Command::SetSharedAudio(settings) => self
                .media()?
                .configure_shared_audio(settings)
                .map_err(error)?,
            Command::Shutdown => {}
        }
        Ok(())
    }

    fn begin_ip_poll(&mut self) {
        if self.ip_poll_task.is_some() {
            return;
        }
        let Some(pending) = &mut self.pending_ip else {
            return;
        };
        if pending.consecutive_errors >= IP_MAX_POLL_ERRORS {
            return;
        }
        pending.next_poll = None;
        let client = pending.client.clone();
        let ticket = pending.ticket.clone();
        self.ip_poll_task = Some(tokio::spawn(async move {
            client.poll_ip_authorization(&ticket).await
        }));
    }

    fn clear_browser_login(&mut self) {
        self.pending_browser = None;
        if let Some(task) = self.browser_poll_task.take() {
            task.abort();
        }
        self.emit(Event::BrowserLoginCleared);
    }
    fn begin_browser_poll(&mut self) {
        if self.browser_poll_task.is_some() {
            return;
        }
        let Some(pending) = &mut self.pending_browser else {
            return;
        };
        if pending.consecutive_errors >= IP_MAX_POLL_ERRORS {
            return;
        }
        let Some(mut session) = pending.session.take() else {
            return;
        };
        pending.next_poll = None;
        let client = pending.client.clone();
        self.browser_poll_task = Some(tokio::spawn(async move {
            let result = client.poll_desktop_handoff(&mut session).await;
            (session, result)
        }));
    }
    async fn finish_browser_poll(&mut self, result: Result<DesktopHandoffPoll>) -> Result<()> {
        let Some(pending) = &mut self.pending_browser else {
            return Ok(());
        };
        match result {
            Ok(result) => {
                pending.consecutive_errors = 0;
                match result.status {
                    DesktopHandoffStatus::Pending => {
                        pending.next_poll = Some(tokio::time::Instant::now() + IP_POLL_INTERVAL)
                    }
                    DesktopHandoffStatus::Completed => {
                        let pending = self
                            .pending_browser
                            .take()
                            .ok_or("Browser sign-in was cancelled")?;
                        self.emit(Event::BrowserLoginCleared);
                        let token = result
                            .token
                            .ok_or("Browser sign-in omitted its session token")?;
                        self.authenticate(
                            pending.client.with_token(token.clone()),
                            token,
                            Some(
                                result
                                    .user_id
                                    .ok_or("Browser sign-in omitted its user ID")?,
                            ),
                            result.user,
                        )
                        .await?;
                    }
                    DesktopHandoffStatus::Denied | DesktopHandoffStatus::Expired => {
                        self.clear_browser_login();
                        return Err(if result.status == DesktopHandoffStatus::Denied {
                            "Browser sign-in was denied"
                        } else {
                            "Browser sign-in code expired; request a new code"
                        }
                        .into());
                    }
                }
            }
            Err(_) => {
                pending.consecutive_errors = pending.consecutive_errors.saturating_add(1);
                if pending.consecutive_errors >= IP_MAX_POLL_ERRORS {
                    pending.next_poll = None;
                    self.emit(Event::BrowserLoginPollingFailed);
                } else {
                    pending.next_poll = Some(tokio::time::Instant::now() + IP_POLL_INTERVAL);
                }
            }
        }
        Ok(())
    }

    async fn finish_ip_poll(&mut self, result: Result<Option<AuthSession>>) -> Result<()> {
        let Some(pending) = &mut self.pending_ip else {
            return Ok(());
        };
        match result {
            Ok(Some(session)) => {
                let pending = self
                    .pending_ip
                    .take()
                    .ok_or("Authorization challenge was cancelled")?;
                self.emit(Event::IpAuthorizationCleared);
                self.authenticate(
                    pending.client.with_token(session.token.clone()),
                    session.token,
                    Some(session.user_id),
                    session.user,
                )
                .await?;
            }
            Ok(None) => {
                pending.consecutive_errors = 0;
                pending.next_poll = Some(tokio::time::Instant::now() + IP_POLL_INTERVAL);
            }
            Err(_) => {
                pending.consecutive_errors = pending.consecutive_errors.saturating_add(1);
                if pending.consecutive_errors >= IP_MAX_POLL_ERRORS {
                    pending.next_poll = None;
                    self.emit(Event::IpAuthorizationPollingFailed);
                } else {
                    pending.next_poll = Some(tokio::time::Instant::now() + IP_POLL_INTERVAL);
                }
            }
        }
        Ok(())
    }

    async fn authenticate(
        &mut self,
        client: Client,
        token: SessionToken,
        user_id: Option<String>,
        user: Option<Value>,
    ) -> Result<()> {
        // Fresh password/MFA login already authenticates the token. As in
        // AuthSessionLifecycle.login, READY supplies the account/guild/channel
        // snapshot. Explicit token entry follows AuthSession.validateToken's
        // GET /users/@me path; it does not list guilds or private channels.
        let user = if user_id.is_none() {
            Some(client.current_user().await.map_err(error)?)
        } else {
            user
        };
        let user = authentication_user(user, user_id.as_deref())?;
        self.expected_user_id = user_id
            .as_deref()
            .map(str::parse)
            .transpose()
            .map_err(error)?
            .or_else(|| user.as_ref().map(|user| user.id));
        if let Some(user) = user {
            if self.expected_user_id != Some(user.id) {
                return Err("Authentication user differs from the expected account".into());
            }
            self.state.current_user = Some(user.clone());
            self.state.users.insert(user.id, user);
        }
        let identity = self
            .expected_user_id
            .ok_or("Authentication omitted the account identity")?;
        let key = format!(
            "{}::{}",
            client.instance().api.origin().ascii_serialization(),
            identity
        );
        self.media = None;
        self.audio_preferences = match crate::audio_preferences::load(&key) {
            Ok(preferences) => preferences,
            Err(message) => {
                self.emit(Event::Error(message));
                crate::audio_preferences::AudioPreferences::default()
            }
        };
        self.audio_key = Some(key);
        self.emit(Event::AudioPreferences {
            dsp: self.audio_preferences.dsp.clone(),
            mixer: self.audio_preferences.mixer.clone(),
            input: (
                self.audio_preferences.input.clone(),
                self.audio_preferences.input_label.clone(),
            ),
            output: (
                self.audio_preferences.output.clone(),
                self.audio_preferences.output_label.clone(),
            ),
            users: self.audio_preferences.users.clone(),
            audio_playback: self.audio_preferences.audio_playback,
            video_playback: self.audio_preferences.video_playback,
        });
        self.saved_session = Some(SavedSession {
            instance: client.instance().clone(),
            token: token.clone(),
            user_id: identity,
            user: self
                .state
                .current_user
                .as_ref()
                .and_then(|user| serde_json::to_value(user).ok()),
            locale: self.locale.clone(),
            selected_guild: self.selected_guild,
            selected_channel: self.state.selected_channel.or(self.restore_channel),
            valid: true,
        });
        if let Err(message) = self.persist_active_session().await {
            self.emit(Event::Error(message));
        }
        self.client = Some(client.clone());
        self.emit(Event::InstancePresentation(Some(
            client.instance().document.clone(),
        )));
        self.emit(Event::MediaEndpoint(
            client
                .instance()
                .document
                .pointer("/endpoints/media")
                .and_then(Value::as_str)
                .map(str::to_owned),
        ));
        self.emit(Event::SessionContext {
            instance: client.instance().api.to_string(),
            account_id: self.state.current_user.as_ref().map(|user| user.id),
            selected_guild: self.selected_guild,
        });
        self.snapshot();
        let gateway = Gateway::new(client.instance().gateway.clone(), token)
            // PooledGatewayConnection.ts:474 uses DEFAULT_API_VERSION (1).
            // HTTP routing separately uses discovery's api_code_version.
            .with_api_version(1)
            .with_e2ee_capable(true)
            .with_presence(Value::Null)
            .with_initial_guild(self.selected_guild.map(|guild| guild.to_string()))
            .with_locale(self.locale.clone());
        let (tx, rx) = async_mpsc::channel(128);
        let (commands, command_rx) = async_mpsc::channel(64);
        self.gateway_events = Some(rx);
        self.gateway_commands = Some(commands);
        self.gateway_task = Some(tokio::spawn(async move {
            let errors = tx.clone();
            if let Err(message) = gateway.run(tx, command_rx).await {
                let _ = errors.send(GatewayEvent::Error(message.to_string())).await;
            }
        }));
        self.emit(Event::Status("Signed in".into()));
        Ok(())
    }

    async fn set_member_list(&mut self, channel_id: Option<Snowflake>, start: u32) -> Result<()> {
        if start > 100_000 {
            return Err("Invalid member list range".into());
        }
        let next = if let Some(channel) = channel_id {
            let target = self.state.channels.get(&channel).ok_or("Unknown channel")?;
            let guild = target
                .guild_id
                .ok_or("This channel has no community members")?;
            let user = self.state.current_user.as_ref().ok_or("Sign in first")?;
            if self
                .state
                .guilds
                .get(&guild)
                .and_then(|g| g.extra.get("disabled_operations"))
                .and_then(Value::as_u64)
                .is_some_and(|flags| flags & (1 << 6) != 0)
            {
                return Err("Member list updates are disabled in this community".into());
            }
            let permissions = self.state.permissions_for(channel, user.id);
            if self.state.selected_channel != Some(channel)
                || !permissions
                    .contains(Permissions::VIEW_CHANNEL | Permissions::VIEW_CHANNEL_MEMBERS)
            {
                return Err("You cannot view members in this channel".into());
            }
            Some((guild, channel, start))
        } else {
            None
        };
        let identity = next.and_then(|(guild, channel, _)| {
            self.state
                .channels
                .get(&channel)
                .zip(self.state.guilds.get(&guild))
                .map(|(channel, guild)| fluxer_core::member_list_identity(guild, channel))
        });
        if next.map(|(g, c, _)| (g, c)) != self.member_list_subscription.map(|(g, c, _)| (g, c))
            || identity != self.member_list_identity
        {
            self.member_wire_list_id = None;
            if let Some((guild, channel, _)) = next {
                self.state.member_lists.remove(&(guild, channel));
            }
        }
        self.member_list_identity = identity;
        if self.demo {
            self.member_list_subscription = next;
            if let Some((guild, channel, start)) = next {
                // Large fixture is opt-in and exists only in the isolated demo account.
                let members: Vec<Value> = if std::env::var("FLUXER_MEMBER_DEMO").as_deref()
                    == Ok("large")
                {
                    (0..600).map(|n|json!({"member":{"user":{"id":(900000u64+n).to_string(),"username":format!("member-{n:03}")},"roles":[],"presence":{"status":"online"}}})).collect()
                } else {
                    self.state
                        .members
                        .iter()
                        .filter(|((id, _), _)| *id == guild)
                        .map(|(_, member)| json!({"member":member}))
                        .collect()
                };
                let count = members.len();
                let mut rows = vec![json!({"group":{"id":"online","count":count}})];
                rows.extend(members);
                let items: Vec<_> = rows.into_iter().skip(start as usize).take(100).collect();
                self.state.apply_gateway("GUILD_MEMBER_LIST_UPDATE", json!({"guild_id":guild.to_string(),"channel_id":channel.to_string(),"id":"demo","member_count":count,"online_count":count,"groups":[{"id":"online","count":count}],"ops":[{"op":"SYNC","range":[start,(start+99).min(100_000)],"items":items}]})).map_err(error)?;
                if let Some(list) = self.state.member_lists.get_mut(&(guild, channel)) {
                    list.identity = self.member_list_identity.clone().unwrap_or_default();
                }
                self.snapshot();
            }
            return Ok(());
        }
        if !self.gateway_ready {
            self.member_list_subscription = next;
            return if next.is_none() {
                Ok(())
            } else {
                Err("Gateway is disconnected".into())
            };
        }
        let tx = self
            .gateway_commands
            .as_ref()
            .ok_or("Gateway is disconnected")?;
        if let Some((guild, channel, _)) = self.member_list_subscription
            && next.is_none_or(|(g, c, _)| (g, c) != (guild, channel))
        {
            tx.send(GatewayCommand::Send { op: 14, data: json!({"subscriptions":{guild.to_string():{"member_list_channels":{channel.to_string():[]}}}}) }).await.map_err(|_| "Gateway is disconnected".to_owned())?;
        }
        if let Some((guild, channel, start)) = next {
            tx.send(GatewayCommand::Send { op: 14, data: json!({"subscriptions":{guild.to_string():{"active":true,"member_list_channels":{channel.to_string():[[start,(start+99).min(100_000)]]}}}}) }).await.map_err(|_| "Gateway is disconnected".to_owned())?;
        }
        self.member_list_subscription = next;
        if let Some((guild, channel, start)) = next
            && let Some(list) = self.state.member_lists.get_mut(&(guild, channel))
        {
            list.rows
                .retain(|index, _| *index >= start && *index <= start.saturating_add(99));
        }
        Ok(())
    }

    async fn query_guild_members(
        &mut self,
        guild_id: Snowflake,
        query: String,
        limit: u32,
    ) -> Result<()> {
        if self.demo {
            return Ok(());
        }
        if !self.gateway_ready
            || self.selected_guild != Some(guild_id)
            || !self.state.guilds.contains_key(&guild_id)
            || query.is_empty()
            || !(1..=100).contains(&limit)
        {
            return Err("Cannot search members in this community".into());
        }
        let tx = self
            .gateway_commands
            .as_ref()
            .ok_or("Gateway is disconnected")?;
        self.member_query_nonce = self.member_query_nonce.wrapping_add(1);
        let nonce = format!("native-member-{}", self.member_query_nonce);
        tx.send(GatewayCommand::Send { op: 8, data: json!({"guild_id":guild_id.to_string(), "query":query, "limit":limit, "presences":true, "nonce":nonce}) }).await.map_err(|_| "Gateway is disconnected".to_owned())?;
        self.member_query = Some((guild_id, nonce));
        Ok(())
    }

    fn cancel_open_dm(&mut self) {
        self.dm_generation = self.dm_generation.wrapping_add(1);
        self.dm_job = None;
    }
    async fn open_dm(&mut self, user_id: Snowflake) -> Result<()> {
        self.cancel_open_dm();
        if self
            .state
            .current_user
            .as_ref()
            .is_none_or(|user| user.id == user_id)
        {
            return Err("Cannot open a direct message with this user".into());
        }
        if let Some(channel) = self
            .state
            .channels
            .values()
            .find(|channel| {
                channel.kind == fluxer_core::ChannelType::Dm
                    && channel.recipients.iter().any(|user| user.id == user_id)
            })
            .map(|channel| channel.id)
        {
            return self.select_channel(channel).await;
        }
        if self.demo {
            return Err("This demo has no direct message with that user".into());
        }
        let client = self.client.clone().ok_or("Not signed in")?;
        let generation = self.dm_generation;
        let tx = self.dm_tx.clone();
        let task = tokio::spawn(async move {
            let result = match tokio::time::timeout(
                Duration::from_secs(15),
                client.create_dm(&user_id.to_string()),
            )
            .await
            {
                Ok(result) => result.map_err(error),
                Err(_) => Err("Opening the direct message timed out".into()),
            };
            let _ = tx.send(DmEvent {
                user_id,
                generation,
                result,
            });
        });
        self.dm_job = Some(DmJob { generation, task });
        self.emit(Event::Status("Opening direct message…".into()));
        Ok(())
    }
    async fn create_group_dm(&mut self, recipients: Vec<Snowflake>) -> Result<()> {
        self.cancel_open_dm();
        if self.demo {
            return Err("Unavailable in demo mode".into());
        }
        let current = self.state.current_user.as_ref().ok_or("Not signed in")?.id;
        let mut unique = std::collections::BTreeSet::new();
        for id in recipients {
            if id == current
                || !self
                    .state
                    .relationships
                    .get(&id)
                    .is_some_and(|r| r.kind == 1)
                || !unique.insert(id)
            {
                return Err("Select distinct friends to create a group DM".into());
            }
        }
        let client = self.client.clone().ok_or("Not signed in")?;
        let recipients: Vec<String> = unique.into_iter().map(|id| id.to_string()).collect();
        let generation = self.dm_generation;
        let tx = self.dm_tx.clone();
        let task = tokio::spawn(async move {
            let result = match tokio::time::timeout(
                Duration::from_secs(15),
                client.create_group_dm(&recipients),
            )
            .await
            {
                Ok(result) => result.map_err(error),
                Err(_) => Err("Creating the group DM timed out".into()),
            };
            let _ = tx.send(DmEvent {
                user_id: 0.into(),
                generation,
                result,
            });
        });
        self.dm_job = Some(DmJob { generation, task });
        self.emit(Event::Status("Creating group DM…".into()));
        Ok(())
    }
    async fn finish_open_dm(&mut self, event: DmEvent) -> Result<()> {
        if self
            .dm_job
            .as_ref()
            .is_none_or(|job| job.generation != event.generation)
        {
            return Ok(());
        }
        self.dm_job = None;
        let result: Result<()> = async {
            let channel: fluxer_core::Channel =
                self.state.decode_api(event.result?).map_err(error)?;
            let expected = if event.user_id == 0.into() {
                fluxer_core::ChannelType::GroupDm
            } else {
                fluxer_core::ChannelType::Dm
            };
            if channel.kind != expected || channel.guild_id.is_some() {
                return Err("Server returned an invalid direct message channel".into());
            }
            let id = channel.id;
            self.state.channels.insert(id, channel);
            self.select_channel(id).await
        }
        .await;
        let status = match result {
            Ok(()) => crate::DirectMessageStatus::Complete,
            Err(message) => crate::DirectMessageStatus::Failed(message),
        };
        if event.user_id == 0.into() {
            match status {
                crate::DirectMessageStatus::Complete => {
                    self.emit(Event::Status("Group DM created".into()))
                }
                crate::DirectMessageStatus::Failed(message) => self.emit(Event::Status(format!(
                    "Couldn't create group DM: {message}"
                ))),
                _ => {}
            }
        } else {
            self.emit(Event::DirectMessageOpen {
                user_id: event.user_id,
                status,
            });
        }
        Ok(())
    }

    fn begin_thread_creation(&mut self, parent: Snowflake, name: &str) -> Result<()> {
        if self.thread_creation.is_some() {
            return Err("A thread is already being created".into());
        }
        let kind = self
            .state
            .public_thread_type(parent)
            .ok_or("You cannot create a public thread in this channel")?;
        let account = self.state.current_user.as_ref().ok_or("Sign in first")?.id;
        let guild = self.state.channels[&parent]
            .guild_id
            .ok_or("Unknown community")?;
        let name = name.trim().to_owned();
        if !(1..=100).contains(&name.encode_utf16().count()) {
            return Err("Use a thread name between 1 and 100 characters".into());
        }
        let client = self.client()?;
        let task = tokio::spawn(async move {
            tokio::time::timeout(
                Duration::from_secs(15),
                client.create_public_thread(
                    &parent.to_string(),
                    &name,
                    kind == ChannelType::AnnouncementThread,
                ),
            )
            .await
            .map_err(|_| "Thread creation timed out".to_owned())?
            .map_err(error)
        });
        self.thread_creation = Some(ThreadCreation {
            account,
            guild,
            parent,
            kind,
            removed: BTreeSet::new(),
            task,
        });
        Ok(())
    }

    fn finish_thread_creation(&mut self, result: Result<Value>) {
        let Some(job) = self.thread_creation.take() else {
            return;
        };
        if self.state.current_user.as_ref().map(|user| user.id) != Some(job.account) {
            return;
        }
        let result = result.and_then(|value| {
            let thread: fluxer_core::Channel = self.state.decode_api(value).map_err(error)?;
            if thread.parent_id != Some(job.parent)
                || thread.kind != job.kind
                || thread.guild_id != Some(job.guild)
            {
                return Err("The server returned an unexpected thread".into());
            }
            if job.removed.contains(&thread.id)
                || self
                    .state
                    .channels
                    .get(&job.parent)
                    .and_then(|channel| channel.guild_id)
                    != Some(job.guild)
            {
                return Err("The thread is no longer available".into());
            }
            let id = thread.id;
            // The gateway may already have newer name, membership or lifecycle data.
            self.state.channels.entry(id).or_insert(thread);
            Ok(id)
        });
        if result.is_ok() {
            self.snapshot();
        }
        self.emit(Event::ThreadCreated {
            parent_id: job.parent,
            result,
        });
    }

    async fn select_channel(&mut self, channel: Snowflake) -> Result<()> {
        let forum = self
            .state
            .channels
            .get(&channel)
            .is_some_and(|c| matches!(c.kind, ChannelType::Forum | ChannelType::Media));
        if forum {
            let user = self.state.current_user.as_ref().ok_or("Sign in first")?;
            if !self
                .state
                .permissions_for(channel, user.id)
                .contains(Permissions::VIEW_CHANNEL | Permissions::READ_MESSAGE_HISTORY)
            {
                return Err("You cannot view this forum".into());
            }
        } else {
            self.require_history(channel)?;
        }
        if !self.state.channels.contains_key(&channel) {
            return Err("Unknown channel".into());
        }
        if self
            .member_list_subscription
            .is_some_and(|(_, current, _)| current != channel)
        {
            self.set_member_list(None, 0).await?;
        }
        self.navigation_context(channel);
        self.selected_guild = self
            .state
            .channels
            .get(&channel)
            .and_then(|channel| channel.guild_id);
        // Channel selection is navigation, not evidence a message was visible.
        // The UI explicitly acknowledges only rows painted in its viewport.
        self.state.selected_channel = Some(channel);
        if !forum {
            self.request_channel_slowmode(channel);
            self.request_history(channel, navigation_flow::HistoryRequest::Latest)?;
        }
        if !self.demo {
            self.sync_selected_guild().await?;
        }
        self.attachment_snapshot(channel);
        self.restore_channel = None;
        self.persist_selection().await;
        self.snapshot();
        Ok(())
    }

    async fn sync_selected_guild(&mut self) -> Result<()> {
        if !self.gateway_ready {
            return Ok(());
        }
        let Some(guild) = self.selected_guild else {
            return Ok(());
        };
        let Some(session) = self.state.session_id.clone() else {
            return Ok(());
        };
        let threads = self.state.thread_guilds.contains(&guild);
        if self
            .synced_guilds
            .get(&guild)
            .is_some_and(|(current_session, current_threads)| {
                *current_session == session && *current_threads == threads
            })
        {
            return Ok(());
        }
        let Some(tx) = &self.gateway_commands else {
            return Ok(());
        };
        // GatewayGuildSubscriptionSynchronizer.ts:43-49. The existing selected
        // guild path leaves previously synced guilds active in this session.
        let mut options = json!({"active": true, "sync": true});
        if threads {
            options["threads"] = Value::Bool(true);
        }
        tx.send(GatewayCommand::Send {
            op: 14,
            data: json!({"subscriptions":{guild.to_string():options}}),
        })
        .await
        .map_err(|_| "Gateway is disconnected".to_owned())?;
        self.synced_guilds.insert(guild, (session, threads));
        Ok(())
    }

    async fn update_voice(&mut self) -> Result<()> {
        if !self.gateway_ready && self.voice_target.is_some() {
            self.voice_sync_pending = true;
        }
        if let Some(target) = &self.voice_target {
            self.gateway_commands
                .as_ref()
                .ok_or("Gateway is disconnected")?
                .send(GatewayCommand::VoiceState(target.payload(
                    &self.state,
                    false,
                    &self.watched_streams,
                )))
                .await
                .map_err(|_| "Gateway is disconnected".to_owned())?;
        }
        Ok(())
    }

    async fn leave_voice(&mut self, notify_gateway: bool) -> Result<()> {
        self.pending_share = None;
        self.active_share_audio = false;
        if let Some(job) = self.voice_connect.take() {
            job.task.abort();
            let _ = job.task.await;
        }
        let target = self.voice_target.take();
        if let Some(id) = target
            .as_ref()
            .and_then(|target| target.connection_id.clone())
        {
            self.retired_voice_connections.insert(id);
        }
        self.retired_voice_connections
            .extend(std::mem::take(&mut self.pending_voice_server).into_keys());
        self.voice_credentials = None;
        self.voice_join_deadline = None;
        self.voice_recovery_started = None;
        self.voice_recovery_at = None;
        self.voice_recovery_attempts = 0;
        self.server_muted = false;
        self.server_deafened = false;
        for identity in std::mem::take(&mut self.available_streams) {
            self.emit(Event::Media(MediaEvent::ScreenStreamAvailable {
                identity,
                available: false,
            }));
        }
        self.watched_streams.clear();
        self.participant_audio_intents.clear();
        self.stream_audio_intents.clear();
        self.video_generations.clear();
        self.emit(Event::WatchedStreams(Vec::new()));
        self.media_events = None;
        // Stop capture before awaiting any network writes, including failures.
        let disconnected = if let Some(media) = &mut self.media {
            let watch = media.set_watched_participants(Vec::new()).map_err(error);
            let disconnect = media.disconnect().await.map_err(error);
            watch.and(disconnect)
        } else {
            Ok(())
        };
        self.state.voice.disconnect();
        if let Ok(mut frames) = self.frames.lock() {
            frames.clear();
        }
        self.emit(Event::VoiceStatus(VoiceStatus::Disconnected));
        self.emit(Event::SharedAudioStatus(false));
        if notify_gateway && let (Some(target), Some(tx)) = (target, &self.gateway_commands) {
            let payload = target.payload(&self.state, true, &self.watched_streams);
            if !self.gateway_ready
                && (target.connection_id.is_some() || self.pending_voice_leave.is_none())
            {
                self.pending_voice_leave = Some(payload.clone());
            }
            let _ = tx.send(GatewayCommand::VoiceState(payload)).await;
        }
        self.snapshot();
        disconnected
    }

    async fn gateway(&mut self, event: GatewayEvent) -> Result<()> {
        match event {
            GatewayEvent::Connecting => self.emit(Event::Status("Connecting gateway…".into())),
            GatewayEvent::Reconnecting => {
                self.gateway_ready = false;
                self.emit(Event::Status("Reconnecting gateway…".into()));
            }
            GatewayEvent::Connected => {
                self.gateway_ready = true;
                // The socket discards voice updates before authentication. Replay
                // the latest local intent, including a leave, after RESUME/READY.
                if let Some(leave) = self.pending_voice_leave.take()
                    && let Some(tx) = &self.gateway_commands
                {
                    tx.send(GatewayCommand::VoiceState(leave))
                        .await
                        .map_err(|_| "Gateway is disconnected".to_owned())?;
                }
                if std::mem::take(&mut self.voice_sync_pending)
                    && self.voice_recovery_started.is_none()
                {
                    self.update_voice().await?;
                }
                self.emit(Event::Status("Connected".into()));
                self.sync_selected_guild().await?;
                if let Some((_, channel, start)) = self.member_list_subscription {
                    self.set_member_list(Some(channel), start).await?;
                }
                if let Some(channel) = self.restore_channel.take() {
                    self.select_channel(channel).await?;
                }
            }
            GatewayEvent::Error(message) => self.emit(Event::Error(message)),
            GatewayEvent::AuthenticationFailed => {
                self.invalidate_saved_session().await;
                self.emit(Event::Status(
                    "Your saved session expired; sign in again".into(),
                ));
            }
            GatewayEvent::Disconnected => {
                if self.restoring_gateway {
                    self.restoring_gateway = false;
                    self.emit(Event::SessionRestoring(false));
                    self.emit(Event::SessionRestoreFailed);
                }
                self.gateway_ready = false;
                self.synced_guilds.clear();
                self.pending_voice_leave = None;
                self.voice_sync_pending = false;
                self.leave_voice(false).await?;
                self.emit(Event::Status("Gateway disconnected".into()));
            }
            GatewayEvent::Dispatch {
                name,
                data,
                sequence,
            } => {
                if name == "READY"
                    && self.expected_user_id.is_some_and(|expected| {
                        data.get("user")
                            .and_then(|user| user.get("id"))
                            .and_then(Value::as_str)
                            != Some(expected.to_string().as_str())
                    })
                {
                    self.invalidate_saved_session().await;
                    return Err("Gateway READY user differs from the authenticated account".into());
                }
                if sequence
                    .is_some_and(|sequence| self.last_sequence.is_some_and(|last| sequence <= last))
                    && name != "READY"
                {
                    return Ok(());
                }
                let mut data = data;
                if name == "GUILD_MEMBER_LIST_UPDATE" {
                    let Some((guild, channel, _)) = self.member_list_subscription else {
                        return Ok(());
                    };
                    if self.state.current_user.as_ref().is_none_or(|user| {
                        !self
                            .state
                            .permissions_for(channel, user.id)
                            .contains(Permissions::VIEW_CHANNEL | Permissions::VIEW_CHANNEL_MEMBERS)
                    }) {
                        return Ok(());
                    }
                    if self.state.selected_channel != Some(channel)
                        || data.get("guild_id").and_then(Value::as_str)
                            != Some(guild.to_string().as_str())
                    {
                        return Ok(());
                    }
                    let identity = self
                        .state
                        .channels
                        .get(&channel)
                        .zip(self.state.guilds.get(&guild))
                        .map(|(channel, guild)| fluxer_core::member_list_identity(guild, channel));
                    if identity != self.member_list_identity {
                        return Ok(());
                    }
                    let Some(wire_id) = data.get("id").and_then(Value::as_str).map(str::to_owned)
                    else {
                        return Ok(());
                    };
                    let tagged = data.get("channel_id").and_then(Value::as_str).is_some();
                    if let Some(id) = data.get("channel_id").and_then(Value::as_str) {
                        if id != channel.to_string() {
                            return Ok(());
                        }
                    } else {
                        if self.member_wire_list_id.as_deref() != Some(wire_id.as_str()) {
                            return Ok(());
                        }
                        data["channel_id"] = json!(channel.to_string());
                    }
                    let (_, _, start) = self.member_list_subscription.unwrap();
                    if let Some(ops) = data.get_mut("ops").and_then(Value::as_array_mut) {
                        let had_ops = !ops.is_empty();
                        ops.retain(|op| {
                            op.get("range")
                                .and_then(Value::as_array)
                                .and_then(|r| {
                                    r.first()
                                        .and_then(Value::as_u64)
                                        .zip(r.get(1).and_then(Value::as_u64))
                                })
                                .is_some_and(|(a, b)| {
                                    a >= u64::from(start)
                                        && b <= u64::from(start.saturating_add(99))
                                })
                        });
                        if had_ops && ops.is_empty() {
                            return Ok(());
                        }
                    }
                    if tagged {
                        self.member_wire_list_id = Some(wire_id);
                    }
                }
                if name == "GUILD_MEMBERS_CHUNK" {
                    let matches = self.member_query.as_ref().is_some_and(|(guild, nonce)| {
                        self.selected_guild == Some(*guild)
                            && data.get("guild_id").and_then(Value::as_str)
                                == Some(guild.to_string().as_str())
                            && data.get("nonce").and_then(Value::as_str) == Some(nonce.as_str())
                    });
                    if !matches {
                        return Ok(());
                    }
                }
                if matches!(
                    name.as_str(),
                    "READY"
                        | "USER_UPDATE"
                        | "GUILD_MEMBER_UPDATE"
                        | "GUILD_MEMBER_REMOVE"
                        | "GUILD_DELETE"
                ) {
                    self.invalidate_profiles();
                }
                if name == "VOICE_SERVER_UPDATE" {
                    let update: VoiceServerUpdate = serde_json::from_value(data).map_err(error)?;
                    if self
                        .retired_voice_connections
                        .contains(&update.connection_id)
                    {
                        // A cancelled connection may still deliver credentials.
                    } else if self
                        .voice_target
                        .as_ref()
                        .is_some_and(|target| target.accepts(&update))
                    {
                        self.connect_voice(update).await?;
                    } else if self.voice_target.as_ref().is_some_and(|target| {
                        target.connection_id.is_none()
                            && update
                                .channel_id
                                .as_deref()
                                .is_none_or(|id| id == target.channel_id.to_string())
                            && update.guild_id.as_deref().is_none_or(|id| {
                                Some(id.to_owned()) == target.guild_id.map(|id| id.to_string())
                            })
                    }) {
                        // Credentials can arrive before the matching own voice
                        // state. Keep bounded candidates keyed by connection ID.
                        if self.pending_voice_server.len() < 16
                            || self
                                .pending_voice_server
                                .contains_key(&update.connection_id)
                        {
                            self.pending_voice_server
                                .insert(update.connection_id.clone(), update);
                        }
                    }
                } else {
                    let new_live_message = name == "MESSAGE_CREATE"
                        && data
                            .get("channel_id")
                            .and_then(Value::as_str)
                            .and_then(|id| id.parse::<Snowflake>().ok())
                            .zip(
                                data.get("id")
                                    .and_then(Value::as_str)
                                    .and_then(|id| id.parse::<Snowflake>().ok()),
                            )
                            .is_some_and(|(channel, id)| {
                                !self
                                    .state
                                    .messages
                                    .get(&channel)
                                    .is_some_and(|messages| messages.contains_key(&id))
                            });
                    let previous_voice = self.state.voice.clone();
                    let previous_presence = self.presence_payload();
                    if matches!(name.as_str(), "THREAD_UPDATE" | "THREAD_DELETE") {
                        self.navigation_forum_thread_event(&name, &data);
                    }
                    let outcome = self
                        .state
                        .apply_gateway(&name, data.clone())
                        .map_err(error)?;
                    if name == "THREAD_DELETE"
                        && let Some(job) = &mut self.thread_creation
                        && data
                            .get("parent_id")
                            .and_then(Value::as_str)
                            .and_then(|id| id.parse::<Snowflake>().ok())
                            == Some(job.parent)
                        && let Some(id) = data
                            .get("id")
                            .and_then(Value::as_str)
                            .and_then(|id| id.parse::<Snowflake>().ok())
                    {
                        job.removed.insert(id);
                    }
                    if outcome == fluxer_core::ApplyOutcome::Applied
                        && matches!(
                            name.as_str(),
                            "MESSAGE_ACK"
                                | "MESSAGE_DELETE"
                                | "MESSAGE_DELETE_BULK"
                                | "CHANNEL_DELETE"
                                | "THREAD_DELETE"
                        )
                        && let Some(account) = self.state.current_user.as_ref()
                        && let Some(channel) = data
                            .get(
                                if matches!(name.as_str(), "CHANNEL_DELETE" | "THREAD_DELETE") {
                                    "id"
                                } else {
                                    "channel_id"
                                },
                            )
                            .and_then(Value::as_str)
                            .and_then(|id| id.parse::<Snowflake>().ok())
                    {
                        self.emit(Event::ClearChannelNotifications {
                            account_id: account.id,
                            channel_id: channel,
                        });
                    }
                    if name == "GUILD_MEMBER_LIST_UPDATE"
                        && let Some((guild, channel, _)) = self.member_list_subscription
                        && let Some(list) = self.state.member_lists.get_mut(&(guild, channel))
                    {
                        list.identity = self.member_list_identity.clone().unwrap_or_default();
                    }
                    if matches!(name.as_str(), "READY" | "USER_SETTINGS_UPDATE")
                        && self.presence_payload() != previous_presence
                    {
                        self.publish_presence().await;
                    }
                    self.navigation_gateway(&name, &data);
                    if name == "MESSAGE_CREATE"
                        && let (Some(channel), Some(id)) = (
                            data.get("channel_id")
                                .and_then(Value::as_str)
                                .and_then(|id| id.parse::<Snowflake>().ok()),
                            data.get("id")
                                .and_then(Value::as_str)
                                .and_then(|id| id.parse::<Snowflake>().ok()),
                        )
                        && let Some(message) = self
                            .state
                            .messages
                            .get(&channel)
                            .and_then(|messages| messages.get(&id))
                            .cloned()
                    {
                        self.reconcile_attachment_message(&message);
                        if new_live_message {
                            self.navigation_mention_message(&message);
                        }
                        if !self.demo
                            && new_live_message
                            && let Some(account) = self.state.current_user.as_ref()
                        {
                            self.emit(Event::LiveMessageCreated {
                                account_id: account.id,
                                message: Box::new(message),
                            });
                        }
                    }
                    // Publish the identity boundary before account-scoped presentation
                    // events (including attachment policies), and retain ordinary READY.
                    if name == "READY" {
                        self.state.voice = previous_voice;
                        if self
                            .selected_guild
                            .is_none_or(|guild| !self.state.guilds.contains_key(&guild))
                        {
                            // The current native UI initially selects the first
                            // listed guild. Subscribe to that same selection.
                            self.selected_guild = self.state.guilds.keys().next().copied();
                        }
                        self.emit(Event::SessionContext {
                            instance: self
                                .client
                                .as_ref()
                                .map(|client| client.instance().api.to_string())
                                .unwrap_or_default(),
                            account_id: self.state.current_user.as_ref().map(|user| user.id),
                            selected_guild: self.selected_guild,
                        });
                    }
                    if matches!(
                        name.as_str(),
                        "READY"
                            | "USER_SETTINGS_UPDATE"
                            | "USER_UPDATE"
                            | "GUILD_UPDATE"
                            | "GUILD_MEMBER_UPDATE"
                            | "GUILD_ROLE_UPDATE"
                            | "GUILD_ROLE_UPDATE_BULK"
                            | "CHANNEL_UPDATE"
                            | "CHANNEL_UPDATE_BULK"
                    ) {
                        self.refresh_attachment_policies();
                    }
                    if name == "READY" {
                        if let Some(channel) = self.restore_channel.take()
                            && self
                                .state
                                .channels
                                .get(&channel)
                                .is_some_and(|channel| channel.kind.is_message_channel())
                        {
                            self.state.selected_channel = Some(channel);
                            // Load history through the normal selected-channel
                            // path once Identify/READY has completed.
                            self.restore_channel = Some(channel);
                        }
                        self.persist_selection().await;
                    }
                    if name == "GUILD_SYNC"
                        && let (Some(guild), Some(session)) = (
                            data.get("id")
                                .and_then(Value::as_str)
                                .and_then(|id| id.parse::<Snowflake>().ok()),
                            self.state.session_id.clone(),
                        )
                    {
                        // A guild projection can arrive before selection. It does
                        // not establish a thread subscription for the session.
                        if !self.state.thread_guilds.contains(&guild) {
                            self.synced_guilds.insert(guild, (session, false));
                        }
                    }
                    if name == "GUILD_CREATE"
                        && self.selected_guild.is_some_and(|guild| {
                            data.get("id")
                                .and_then(Value::as_str)
                                .and_then(|id| id.parse::<Snowflake>().ok())
                                == Some(guild)
                                && self.state.thread_guilds.contains(&guild)
                        })
                    {
                        self.sync_selected_guild().await?;
                    }
                    if name == "VOICE_STATE_UPDATE"
                        && data.get("user_id").and_then(Value::as_str)
                            == self
                                .state
                                .current_user
                                .as_ref()
                                .map(|u| u.id.to_string())
                                .as_deref()
                    {
                        if let Some(target) = &mut self.voice_target {
                            let channel = data.get("channel_id").and_then(Value::as_str);
                            let connection = data.get("connection_id").and_then(Value::as_str);
                            let own_session = data
                                .get("session_id")
                                .and_then(Value::as_str)
                                .zip(self.state.session_id.as_deref())
                                .is_none_or(|(event, current)| event == current);
                            let current_connection = connection
                                .is_some_and(|id| !self.retired_voice_connections.contains(id));
                            let same_guild = data
                                .get("guild_id")
                                .and_then(Value::as_str)
                                .is_none_or(|id| {
                                    Some(id.to_owned()) == target.guild_id.map(|id| id.to_string())
                                });
                            if own_session
                                && same_guild
                                && current_connection
                                && channel == Some(target.channel_id.to_string().as_str())
                                && target
                                    .connection_id
                                    .as_deref()
                                    .is_none_or(|local| connection == Some(local))
                            {
                                target.connection_id = connection.map(str::to_owned);
                                self.server_muted = data
                                    .get("mute")
                                    .and_then(Value::as_bool)
                                    .unwrap_or(self.server_muted);
                                self.server_deafened = data
                                    .get("deaf")
                                    .and_then(Value::as_bool)
                                    .unwrap_or(self.server_deafened);
                                if let Some(media) = &mut self.media {
                                    media
                                        .set_deafened(
                                            self.state.voice.deafened || self.server_deafened,
                                        )
                                        .map_err(error)?;
                                    media
                                        .set_muted(self.state.voice.muted || self.server_muted)
                                        .map_err(error)?;
                                }
                            } else if own_session
                                && same_guild
                                && current_connection
                                && connection == target.connection_id.as_deref()
                            {
                                self.leave_voice(false).await?;
                            }
                        }
                        if let Some(id) = self
                            .voice_target
                            .as_ref()
                            .and_then(|target| target.connection_id.clone())
                            && let Some(update) = self.pending_voice_server.remove(&id)
                            && self
                                .voice_target
                                .as_ref()
                                .is_some_and(|target| target.accepts(&update))
                        {
                            self.retired_voice_connections
                                .extend(std::mem::take(&mut self.pending_voice_server).into_keys());
                            self.connect_voice(update).await?;
                        }
                    }
                    if let Some(target) = &self.voice_target {
                        let removed = !self.state.channels.contains_key(&target.channel_id);
                        let lost = target
                            .guild_id
                            .zip(self.state.current_user.as_ref().map(|u| u.id))
                            .is_some_and(|(guild, user)| {
                                self.state.members.contains_key(&(guild, user))
                                    && !self
                                        .state
                                        .permissions_for(target.channel_id, user)
                                        .contains(Permissions::VIEW_CHANNEL | Permissions::CONNECT)
                            });
                        if removed || lost {
                            self.leave_voice(true).await?;
                            self.emit(Event::Error("Voice channel is no longer available".into()));
                        }
                    }
                    self.sync_stream_availability().await?;
                    self.snapshot();
                    if matches!(
                        name.as_str(),
                        "READY" | "GUILD_CREATE" | "GUILD_SYNC" | "CHANNEL_CREATE"
                    ) {
                        self.open_accepted_invite().await;
                    }
                    if name == "READY" {
                        self.emit(Event::AudioPreferences {
                            dsp: self.audio_preferences.dsp.clone(),
                            mixer: self.audio_preferences.mixer.clone(),
                            input: (
                                self.audio_preferences.input.clone(),
                                self.audio_preferences.input_label.clone(),
                            ),
                            output: (
                                self.audio_preferences.output.clone(),
                                self.audio_preferences.output_label.clone(),
                            ),
                            users: self.audio_preferences.users.clone(),
                            audio_playback: self.audio_preferences.audio_playback,
                            video_playback: self.audio_preferences.video_playback,
                        });
                    }
                    if name == "READY" && self.restoring_gateway {
                        self.restoring_gateway = false;
                        self.emit(Event::SessionRestoring(false));
                    }
                }
                if let Some(sequence) = sequence {
                    self.last_sequence = Some(sequence);
                }
            }
        }
        Ok(())
    }

    /// Revoke old share intent before a replacement room tears down capture.
    /// MediaEngineFacade.stopActiveScreenShareForTeardown and
    /// VoiceEngineV2AppScreenShareStateSync drive the matching self_stream state;
    /// native does not retain or silently reacquire OS capture across replacement.
    fn revoke_share_for_replacement(&mut self) -> bool {
        let was_sharing = self.active_share_audio || self.state.voice.screen_share;
        let had_share = self.pending_share.is_some() || was_sharing;
        self.pending_share = None;
        self.active_share_audio = false;
        self.state.voice.screen_share = false;
        if had_share {
            if was_sharing {
                self.emit(Event::ShareReshareRequired);
            }
            self.emit(Event::Media(MediaEvent::ScreenShareStopped));
            self.emit(Event::SharedAudioStatus(false));
        }
        had_share
    }

    async fn connect_voice(&mut self, update: VoiceServerUpdate) -> Result<()> {
        // The existing HostAdapter selects mesh for this payload. Native only
        // implements SFU rooms; do not send mesh credentials into LiveKit or
        // begin capture for a backend this client cannot execute yet.
        if !update.ice_servers.is_empty() {
            if let Some(target) = &mut self.voice_target {
                target.connection_id = Some(update.connection_id.clone());
            }
            let _ = self.leave_voice(true).await;
            return Err(
                "This call requires mesh voice, which the native client does not support yet"
                    .into(),
            );
        }
        if self
            .voice_connect
            .as_ref()
            .is_some_and(|job| same_voice_credentials(&job.update, &update))
        {
            return Ok(());
        }
        let replacing = self.state.voice.channel_id.is_some() || self.voice_connect.is_some();
        let target = self
            .voice_target
            .as_mut()
            .ok_or("Voice join was cancelled")?;
        if self
            .voice_credentials
            .as_ref()
            .is_some_and(|previous| same_voice_credentials(previous, &update))
            && self.state.voice.channel_id == Some(target.channel_id)
        {
            return Ok(());
        }
        target.connection_id = Some(update.connection_id.clone());
        let channel = target.channel_id;
        let bitrate = voice_channel_bitrate(&self.state, channel);
        if let Some(job) = self.voice_connect.take() {
            job.task.abort();
            let _ = job.task.await;
        }
        if replacing {
            self.call_generation = self.call_generation.wrapping_add(1);
        }
        if let Some(media) = &mut self.media {
            media.stop_capture_for_recovery();
        }
        let revoked_share = self.revoke_share_for_replacement();
        self.media_events = None;
        self.state.voice.disconnect();
        self.available_streams.clear();
        if let Ok(mut frames) = self.frames.lock() {
            frames.clear();
        }
        self.snapshot();
        self.emit(Event::Media(MediaEvent::Reconnecting));
        self.emit(Event::VoiceStatus(VoiceStatus::Connecting {
            channel_id: channel,
        }));
        self.voice_join_deadline = None;
        self.media()?;
        let mut media = self.media.take().ok_or("Media engine unavailable")?;
        let task_update = update.clone();
        // Start blocked so a mute/deafen arriving during connection cannot
        // briefly expose audio before the actor applies the latest intent.
        let task = tokio::spawn(async move {
            let result = async {
                media.stop_microphone_test().map_err(error)?;
                media.disconnect().await.map_err(error)?;
                let recovery = media
                    .begin_device_recovery()
                    .await
                    .map_err(|_| "Audio device recovery stopped".to_owned())?
                    .map_err(error)?;
                let devices = media
                    .finish_device_recovery(recovery)
                    .ok_or("Audio device recovery was cancelled")?;
                if devices.input_selection.effective.is_none()
                    || devices.output_selection.effective.is_none()
                {
                    return Err("No usable audio device is available".into());
                }
                media
                    .configure_microphone_publish(Some(bitrate as f64), false)
                    .map_err(error)?;
                media.set_muted(true).map_err(error)?;
                media.set_deafened(true).map_err(error)?;
                tokio::time::timeout(
                    Duration::from_secs(20),
                    media.connect_encrypted(
                        &task_update.endpoint,
                        task_update.token.expose_secret(),
                        task_update.e2ee_key.as_deref(),
                    ),
                )
                .await
                .map_err(|_| "Voice media connection timed out".to_owned())?
                .map_err(error)
            }
            .await;
            (media, result)
        });
        self.voice_connect = Some(VoiceConnectJob {
            generation: self.call_generation,
            update,
            revoked_share,
            task,
        });
        if revoked_share && let Err(message) = self.update_voice().await {
            self.emit(Event::Error(message));
        }
        Ok(())
    }

    async fn finish_voice_connect(
        &mut self,
        result: std::result::Result<(MediaEngine, Result<MediaEvents>), tokio::task::JoinError>,
    ) -> Result<()> {
        let Some(job) = self.voice_connect.take() else {
            return Ok(());
        };
        let (mut media, result) = match result {
            Ok(result) => result,
            Err(failure) => {
                if self.voice_recovery_started.is_some() {
                    self.emit(Event::Error(error(failure)));
                    return self.schedule_voice_retry().await;
                }
                self.leave_voice(true).await?;
                return Err(error(failure));
            }
        };
        if job.generation != self.call_generation || self.voice_target.is_none() {
            let _ = media.disconnect().await;
            return Ok(());
        }
        match result {
            Ok(events) => {
                let channel = self
                    .voice_target
                    .as_ref()
                    .ok_or("Voice join was cancelled")?
                    .channel_id;
                let configure = self
                    .participant_audio_intents
                    .iter()
                    .try_for_each(|(identity, settings)| {
                        media.set_participant_audio(identity, *settings)
                    })
                    .and_then(|()| {
                        self.stream_audio_intents
                            .iter()
                            .try_for_each(|(identity, settings)| {
                                media.set_stream_audio(identity, *settings)
                            })
                    })
                    .and_then(|()| {
                        media.set_deafened(self.state.voice.deafened || self.server_deafened)
                    })
                    .and_then(|()| media.set_muted(self.state.voice.muted || self.server_muted))
                    .and_then(|()| {
                        media.set_watched_participants(
                            self.watched_streams.iter().cloned().collect(),
                        )
                    });
                if let Err(failure) = configure {
                    self.media = Some(media);
                    if self.voice_recovery_started.is_some() {
                        self.emit(Event::Error(error(failure)));
                        return self.schedule_voice_retry().await;
                    }
                    self.leave_voice(true).await?;
                    return Err(error(failure));
                }
                self.video_generations = self
                    .watched_streams
                    .iter()
                    .map(|identity| (identity.clone(), media.watch_generation(identity)))
                    .collect();
                self.media = Some(media);
                self.media_events = Some(events);
                self.voice_credentials = Some(job.update);
                self.voice_join_deadline = None;
                self.voice_recovery_started = None;
                self.voice_recovery_at = None;
                self.voice_recovery_attempts = 0;
                self.state.voice.join(channel);
                if job.revoked_share
                    && let Err(message) = self.update_voice().await
                {
                    self.emit(Event::Error(message));
                }
                self.sync_stream_availability().await?;
                self.emit(Event::WatchedStreams(
                    self.watched_streams.iter().cloned().collect(),
                ));
                self.emit(Event::VoiceStatus(VoiceStatus::Connected {
                    channel_id: channel,
                }));
                self.snapshot();
                Ok(())
            }
            Err(message) => {
                self.media = Some(media);
                if self.voice_recovery_started.is_some() {
                    self.emit(Event::Error(message));
                    self.schedule_voice_retry().await
                } else {
                    let _ = self.leave_voice(true).await;
                    Err(message)
                }
            }
        }
    }

    async fn schedule_voice_retry(&mut self) -> Result<()> {
        let Some(started) = self.voice_recovery_started else {
            return Ok(());
        };
        if self.voice_recovery_attempts >= 5 || started.elapsed() >= Duration::from_secs(30) {
            self.leave_voice(true).await?;
            return Err("Voice recovery failed; join the channel again".into());
        }
        self.voice_recovery_at = Some(tokio::time::Instant::now() + Duration::from_millis(1500));
        Ok(())
    }

    async fn retry_voice(&mut self) -> Result<()> {
        self.voice_recovery_at = None;
        if self.voice_target.is_none() || self.voice_recovery_started.is_none() {
            return Ok(());
        }
        if self
            .voice_recovery_started
            .is_some_and(|started| started.elapsed() >= Duration::from_secs(30))
        {
            return self.schedule_voice_retry().await;
        }
        if !self.gateway_ready {
            return self.schedule_voice_retry().await;
        }
        self.voice_recovery_attempts += 1;
        self.voice_join_deadline = Some(tokio::time::Instant::now() + Duration::from_secs(15));
        if let Err(message) = self.update_voice().await {
            // No join was sent, so do not wait for its response timeout. Keep
            // recovery bounded and let a restored gateway accept the next try.
            self.voice_join_deadline = None;
            self.emit(Event::Error(message));
            return self.schedule_voice_retry().await;
        }
        Ok(())
    }

    async fn media_event(&mut self, event: MediaEvent) -> Result<()> {
        match event {
            MediaEvent::ScreenStreamAvailable {
                identity,
                available: _,
            } => {
                if parse_participant_identity(&identity).is_none() {
                    return Ok(());
                }
                // Publication metadata is supplemental. Fluxer's gateway can
                // announce self_stream before the SFU advertises a track, and a
                // publication can disappear temporarily during migration.
                self.sync_stream_availability().await?;
            }
            MediaEvent::ParticipantLeft { identity } => {
                self.sync_stream_availability().await?;
                self.emit(Event::Media(MediaEvent::ParticipantLeft { identity }));
            }
            MediaEvent::Video(frame) => {
                if !self.watched_streams.contains(&frame.participant)
                    || self.video_generations.get(&frame.participant)
                        != Some(&frame.watch_generation)
                {
                    return Ok(());
                }
                if let Ok(mut frames) = self.frames.lock() {
                    frames.insert(frame.track_id.clone(), frame);
                }
            }
            MediaEvent::VideoEnded {
                ref track_id,
                ref participant,
                watch_generation,
            } => {
                if self.video_generations.get(participant) != Some(&watch_generation) {
                    return Ok(());
                }
                if let Ok(mut frames) = self.frames.lock() {
                    frames.remove(track_id);
                }
                self.emit(Event::Media(event));
            }
            MediaEvent::InputDeviceFailed => {
                if let Some(media) = &mut self.media {
                    media.input_device_failed();
                    if let Ok(devices) = media.try_devices() {
                        self.emit(Event::Devices(devices));
                    }
                }
            }
            MediaEvent::Disconnected => {
                self.leave_voice(true).await?;
            }
            MediaEvent::TransportLost => {
                if self
                    .voice_target
                    .as_ref()
                    .is_some_and(|target| target.guild_id.is_some())
                {
                    self.revoke_share_for_replacement();
                    if let Some(job) = self.voice_connect.take() {
                        job.task.abort();
                        let _ = job.task.await;
                    }
                    self.call_generation = self.call_generation.wrapping_add(1);
                    self.media_events = None;
                    self.media.take(); // Drop releases all capture without waiting for the network.
                    self.state.voice.disconnect();
                    self.voice_credentials = None;
                    self.retired_voice_connections
                        .extend(std::mem::take(&mut self.pending_voice_server).into_keys());
                    if let Some(id) = self
                        .voice_target
                        .as_ref()
                        .and_then(|target| target.connection_id.clone())
                    {
                        self.retired_voice_connections.insert(id);
                    }
                    // Classic disconnects the abandoned connection before a
                    // fresh join, so a retry cannot leave a ghost participant.
                    if let Some(target) = &self.voice_target
                        && target.connection_id.is_some()
                    {
                        let leave = target.payload(&self.state, true, &self.watched_streams);
                        if !self.gateway_ready {
                            self.pending_voice_leave = Some(leave.clone());
                        }
                        if let Some(tx) = &self.gateway_commands {
                            let _ = tx.send(GatewayCommand::VoiceState(leave)).await;
                        }
                    }
                    if let Some(target) = &mut self.voice_target {
                        target.connection_id = None;
                    }
                    self.available_streams.clear();
                    if let Ok(mut frames) = self.frames.lock() {
                        frames.clear();
                    }
                    self.voice_recovery_started = Some(tokio::time::Instant::now());
                    self.voice_recovery_attempts = 0;
                    self.snapshot();
                    self.emit(Event::VoiceStatus(VoiceStatus::Reconnecting));
                    self.snapshot();
                    self.schedule_voice_retry().await?;
                } else {
                    self.leave_voice(true).await?;
                }
            }
            MediaEvent::Reconnecting => self.emit(Event::VoiceStatus(VoiceStatus::Reconnecting)),
            MediaEvent::Connected => {
                if let Some(target) = &self.voice_target {
                    self.emit(Event::VoiceStatus(VoiceStatus::Connected {
                        channel_id: target.channel_id,
                    }));
                }
            }
            MediaEvent::ScreenShareStarted => {
                let running = self
                    .media
                    .as_ref()
                    .is_some_and(MediaEngine::screen_capture_has_frame);
                self.screen_event(true, running).await?;
            }
            MediaEvent::ScreenShareStopped => {
                let running = self.media.as_ref().is_some_and(MediaEngine::is_sharing);
                self.screen_event(false, running).await?;
            }
            MediaEvent::SharedAudioStopped => {
                let running = self
                    .media
                    .as_ref()
                    .is_some_and(MediaEngine::is_sharing_audio);
                self.shared_audio_event(running).await?;
            }
            event => self.emit(Event::Media(event)),
        }
        Ok(())
    }

    /// useVoiceCallTracksAndLayout.ts:234-240,263-290,440-447 derives live
    /// streams and pending watches from gateway self_stream, not from receiving
    /// an SFU publication first. An explicit gateway stop/leave/move revokes the
    /// intent even if stale SDK metadata still exists.
    async fn sync_stream_availability(&mut self) -> Result<()> {
        // Do not prune watches merely because a replacement room has not joined yet.
        if self.voice_target.is_some() && self.state.voice.channel_id.is_none() {
            return Ok(());
        }
        let state = &self.state;
        let next: BTreeSet<_> = self
            .voice_target
            .as_ref()
            .filter(|target| self.state.voice.channel_id == Some(target.channel_id))
            .into_iter()
            .flat_map(|target| {
                state.voice_states.values().filter_map(move |voice| {
                    let guild = voice.guild_id.or_else(|| {
                        state
                            .channels
                            .get(&target.channel_id)
                            .and_then(|c| c.guild_id)
                    });
                    let connection = voice.connection_id.as_deref().filter(|id| !id.is_empty())?;
                    let is_local = state.current_user.as_ref().is_some_and(|me| {
                        me.id == voice.user_id
                            && target.connection_id.as_deref() == Some(connection)
                    });
                    if !voice.self_stream
                        || voice.channel_id != Some(target.channel_id)
                        || guild != target.guild_id
                        || is_local
                    {
                        return None;
                    }
                    let identity = format!("user_{}_{connection}", voice.user_id);
                    parse_participant_identity(&identity).map(|_| identity.clone())
                })
            })
            .collect();
        let previous = std::mem::replace(&mut self.available_streams, next);
        for identity in previous.difference(&self.available_streams) {
            self.emit(Event::Media(MediaEvent::ScreenStreamAvailable {
                identity: identity.clone(),
                available: false,
            }));
        }
        for identity in self.available_streams.difference(&previous) {
            self.emit(Event::Media(MediaEvent::ScreenStreamAvailable {
                identity: identity.clone(),
                available: true,
            }));
        }
        let stopped: Vec<_> = self
            .watched_streams
            .difference(&self.available_streams)
            .cloned()
            .collect();
        for identity in stopped {
            self.set_watch(identity, false, false).await?;
        }
        Ok(())
    }

    async fn set_watch(&mut self, identity: String, watch: bool, connected: bool) -> Result<()> {
        if parse_participant_identity(&identity).is_none() {
            return Err("Invalid voice participant identity".into());
        }
        if watch
            && (!connected
                || self
                    .voice_target
                    .as_ref()
                    .is_none_or(|target| self.state.voice.channel_id != Some(target.channel_id)))
        {
            return Err("Join a voice channel before watching a stream".into());
        }
        if watch && !self.available_streams.contains(&identity) {
            return Err("This participant is not sharing a screen".into());
        }
        let mut next = self.watched_streams.clone();
        if watch {
            next.insert(identity.clone());
        } else {
            next.remove(&identity);
            // Stop is a local presentation operation first. Neither synchronous
            // native/SDK teardown nor a full gateway send queue may delay it.
            if let Ok(mut frames) = self.frames.lock() {
                frames.retain(|_, frame| frame.participant != identity);
            }
        }
        if next == self.watched_streams {
            return Ok(());
        }
        if !watch {
            self.watched_streams = next.clone();
            self.emit(Event::WatchedStreams(next.iter().cloned().collect()));
        }
        let media_result = if let Some(media) = &mut self.media {
            media
                .set_watched_participants(next.iter().cloned().collect())
                .map_err(error)
        } else {
            Ok(())
        };
        if watch {
            // A failed start retains the previous intent; a failed stop remains
            // stopped locally and still removes server viewer demand below.
            media_result.as_ref().map_err(Clone::clone)?;
            self.watched_streams = next;
        }
        let generation = self.media.as_ref().map_or_else(
            || {
                self.video_generations
                    .get(&identity)
                    .copied()
                    .unwrap_or(0)
                    .saturating_add(1)
            },
            |media| media.watch_generation(&identity),
        );
        self.video_generations.insert(identity, generation);
        if watch {
            self.emit(Event::WatchedStreams(
                self.watched_streams.iter().cloned().collect(),
            ));
        }
        let gateway_result = self.update_voice().await;
        media_result.and(gateway_result)
    }

    // Capture-session liveness is supplied separately so lifecycle races can be
    // verified without creating OS capture handles in tests.
    async fn screen_event(&mut self, started: bool, running: bool) -> Result<()> {
        if started {
            if !running
                || self
                    .voice_target
                    .as_ref()
                    .is_none_or(|target| self.state.voice.channel_id != Some(target.channel_id))
            {
                return Ok(());
            }
            // Consume consent once, after the source's first frame. A queued
            // Started after StopShare or a duplicate start cannot open audio.
            let Some(request) = self.pending_share.take() else {
                return Ok(());
            };
            let has_audio = request.audio.is_some();
            let audio_started = if let Some(selection) = request.audio {
                match self.media.as_mut() {
                    Some(media) => media.start_shared_audio(selection).await.map_err(error),
                    None => Err("Screen-share audio transport is unavailable".into()),
                }
            } else {
                Ok(())
            };
            self.complete_share_start(has_audio, audio_started).await?;
        } else {
            // A queued stop from the previous source must not stop its live replacement.
            if running {
                return Ok(());
            }
            if self.pending_share.is_some()
                || self.state.voice.screen_share
                || self.active_share_audio
            {
                self.stop_share().await?;
            }
        }
        Ok(())
    }

    // The result is supplied separately so publication failure/rollback can be
    // tested without opening a picker or an audio device. Production supplies
    // the actual start_shared_audio result above.
    async fn complete_share_start(
        &mut self,
        has_audio: bool,
        audio_started: Result<()>,
    ) -> Result<()> {
        if let Err(message) = audio_started {
            return self.rollback_share(message).await;
        }
        self.pending_share = None;
        self.active_share_audio = has_audio;
        self.state.voice.screen_share = true;
        self.snapshot();
        self.emit(Event::Media(MediaEvent::ScreenShareStarted));
        self.emit(Event::SharedAudioStatus(has_audio));
        match self.update_voice().await {
            Ok(()) => Ok(()),
            Err(message) => self.rollback_share(message).await,
        }
    }

    async fn shared_audio_event(&mut self, running: bool) -> Result<()> {
        // A stopped old route may be queued ahead of its live replacement.
        // Explicit audio-only stop revokes this intent before invoking media.
        if running || !self.active_share_audio {
            return Ok(());
        }
        self.rollback_share("Screen-share audio stopped unexpectedly; sharing was stopped".into())
            .await
    }

    /// ScreenShareStartFlow fails requested audio capture instead of silently
    /// switching to video-only. Revoke both source lifetimes and server state.
    async fn rollback_share(&mut self, message: String) -> Result<()> {
        match self.stop_share().await {
            Ok(()) => Err(message),
            Err(cleanup) => Err(format!("{message}; screen-share cleanup failed: {cleanup}")),
        }
    }

    async fn stop_share(&mut self) -> Result<()> {
        self.pending_share = None;
        self.active_share_audio = false;
        self.state.voice.screen_share = false;
        self.snapshot();
        self.emit(Event::Media(MediaEvent::ScreenShareStopped));
        self.emit(Event::SharedAudioStatus(false));
        // MediaEngine stops physical capture before awaiting unpublish, and its
        // screen teardown releases the coupled screen-audio publication too.
        let stopped = if let Some(media) = &mut self.media {
            media.stop_screen_share().await.map_err(error)
        } else {
            Ok(())
        };
        let gateway = self.update_voice().await;
        stopped.and(gateway)
    }
}

async fn receive_optional<T>(receiver: &mut Option<async_mpsc::Receiver<T>>) -> Option<T> {
    match receiver {
        Some(receiver) => receiver.recv().await,
        None => pending().await,
    }
}
async fn receive_media(receiver: &mut Option<MediaEvents>) -> Option<MediaEvent> {
    match receiver {
        Some(receiver) => receiver.recv().await,
        None => pending().await,
    }
}
async fn thread_creation_result(task: &mut Option<ThreadCreation>) -> Result<Value> {
    match task {
        Some(job) => (&mut job.task)
            .await
            .map_err(|_| "Thread creation stopped".to_owned())?,
        None => pending().await,
    }
}
async fn restore_result(
    task: &mut Option<JoinHandle<Result<Option<SavedSession>>>>,
) -> Result<Option<SavedSession>> {
    match task {
        Some(task) => task
            .await
            .map_err(|_| "Session restore task stopped".to_owned())?,
        None => pending().await,
    }
}
async fn ip_poll_result(
    task: &mut Option<JoinHandle<fluxer_protocol::Result<Option<AuthSession>>>>,
) -> std::result::Result<fluxer_protocol::Result<Option<AuthSession>>, tokio::task::JoinError> {
    match task {
        Some(task) => task.await,
        None => pending().await,
    }
}
async fn browser_poll_result(
    task: &mut Option<BrowserPollTask>,
) -> std::result::Result<
    (
        DesktopHandoffSession,
        fluxer_protocol::Result<DesktopHandoffPoll>,
    ),
    tokio::task::JoinError,
> {
    match task {
        Some(task) => task.await,
        None => pending().await,
    }
}
async fn voice_timeout(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => pending().await,
    }
}
// AccountSignInIdentifier.ts:10-11 and AccountIdentityFeatures.ts:13-18 select
// the request field from discovery, independently of the entered text.
fn login_identifier(instance: &Instance, identifier: String) -> LoginIdentifier {
    if instance
        .features
        .get("account_identity")
        .and_then(Value::as_str)
        == Some("username")
    {
        LoginIdentifier::Login(identifier)
    } else {
        LoginIdentifier::Email(identifier)
    }
}

fn authentication_user(user: Option<Value>, user_id: Option<&str>) -> Result<Option<User>> {
    // AuthenticationCommands.authResponseUserToUserData and the desktop IPC
    // handoff omit id from profile data; user_id is authoritative separately.
    user.map(|mut user| {
        if let (Some(fields), Some(id)) = (user.as_object_mut(), user_id) {
            fields
                .entry("id")
                .or_insert_with(|| Value::String(id.to_owned()));
        }
        serde_json::from_value(user).map_err(error)
    })
    .transpose()
}

fn same_voice_credentials(previous: &VoiceServerUpdate, next: &VoiceServerUpdate) -> bool {
    previous.endpoint == next.endpoint
        && previous.connection_id == next.connection_id
        && previous.guild_id == next.guild_id
        && previous.channel_id == next.channel_id
        && previous.token.expose_secret() == next.token.expose_secret()
        && previous.e2ee_key == next.e2ee_key
        && previous.ice_servers == next.ice_servers
}

fn voice_channel_bitrate(state: &ClientState, id: Snowflake) -> u64 {
    let Some(channel) = state.channels.get(&id) else {
        return 64_000;
    };
    if channel.guild_id.is_none() {
        return 64_000;
    }
    let features = channel
        .guild_id
        .and_then(|id| state.guilds.get(&id))
        .and_then(|guild| guild.extra.get("features"))
        .and_then(Value::as_array)
        .map(|features| {
            features
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    fluxer_media::resolve_voice_channel_bitrate(
        channel.extra.get("bitrate").and_then(Value::as_f64),
        &features,
    )
}

// VoiceParticipantIdentity.ts:9. Connection IDs may themselves contain underscores.
fn parse_participant_identity(identity: &str) -> Option<(&str, &str)> {
    let (user, connection) = identity.strip_prefix("user_")?.split_once('_')?;
    if user.is_empty()
        || !user.bytes().all(|byte| byte.is_ascii_digit())
        || connection.is_empty()
        || connection
            .chars()
            .any(|character| matches!(character, '\r' | '\n' | '\u{2028}' | '\u{2029}'))
    {
        None
    } else {
        Some((user, connection))
    }
}
fn stream_key(target: &VoiceTarget, identity: &str) -> Option<String> {
    let (_, connection) = parse_participant_identity(identity)?;
    Some(match target.guild_id {
        Some(guild) => format!("{guild}:{}:{connection}", target.channel_id),
        None => format!("dm:{}:{connection}", target.channel_id),
    })
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;

// Linux CLOCK_BOOTTIME includes suspend but CLOCK_MONOTONIC does not.
// Other platforms do not infer suspend from wall-clock changes.
#[cfg(target_os = "linux")]
fn boot_time() -> Option<Duration> {
    let mut value = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `value` is a valid writable timespec and CLOCK_BOOTTIME is a valid clock ID.
    if unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut value) } != 0
        || value.tv_sec < 0
        || value.tv_nsec < 0
    {
        return None;
    }
    Some(Duration::new(value.tv_sec as u64, value.tv_nsec as u32))
}
#[cfg(not(target_os = "linux"))]
fn boot_time() -> Option<Duration> {
    None
}

fn suspend_gap(
    previous: (Option<Duration>, std::time::Instant),
    now: (Option<Duration>, std::time::Instant),
) -> bool {
    let Some(boot_elapsed) = now
        .0
        .zip(previous.0)
        .and_then(|(now, previous)| now.checked_sub(previous))
    else {
        return false;
    };
    let monotonic = now.1.saturating_duration_since(previous.1);
    boot_elapsed.saturating_sub(monotonic) > Duration::from_secs(10)
}

async fn voice_connect_result(
    job: &mut Option<VoiceConnectJob>,
) -> std::result::Result<(MediaEngine, Result<MediaEvents>), tokio::task::JoinError> {
    match job {
        Some(job) => (&mut job.task).await,
        None => pending().await,
    }
}

async fn recovery_result(
    task: &mut Option<JoinHandle<fluxer_media::Result<fluxer_media::DeviceRecovery>>>,
) -> std::result::Result<fluxer_media::Result<fluxer_media::DeviceRecovery>, tokio::task::JoinError>
{
    match task {
        Some(task) => task.await,
        None => std::future::pending().await,
    }
}
