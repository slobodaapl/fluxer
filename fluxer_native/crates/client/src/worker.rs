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
#[path = "worker_navigation.rs"]
mod navigation_flow;

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

struct PendingShare {
    audio: Option<SharedAudioSelection>,
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
        // A channel-bearing response is correlatable during initial join. Older
        // responses without channel data require the matching own voice state.
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

pub(crate) struct Worker {
    navigation: navigation_flow::NavigationState,
    attachments: attachment_flow::AttachmentState,
    session_store: Option<Arc<dyn SessionStore>>,
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
    pending_voice_server: Option<VoiceServerUpdate>,
    voice_credentials: Option<VoiceServerUpdate>,
    voice_join_deadline: Option<tokio::time::Instant>,
    server_muted: bool,
    server_deafened: bool,
    demo: bool,
    locale: String,
    expected_user_id: Option<Snowflake>,
    selected_guild: Option<Snowflake>,
    synced_guilds: BTreeMap<Snowflake, String>,
    available_streams: BTreeSet<String>,
    watched_streams: BTreeSet<String>,
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
        Self {
            navigation: navigation_flow::NavigationState::new(),
            attachments: attachment_flow::AttachmentState::new(),
            session_store: None,
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
            pending_voice_server: None,
            voice_credentials: None,
            voice_join_deadline: None,
            server_muted: false,
            server_deafened: false,
            demo: false,
            locale: ClientState::default().settings.locale,
            expected_user_id: None,
            selected_guild: None,
            synced_guilds: BTreeMap::new(),
            available_streams: BTreeSet::new(),
            watched_streams: BTreeSet::new(),
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
        let _ = self.events.send(event);
    }
    fn snapshot(&self) {
        self.emit(Event::State(Box::new(self.state.clone())));
    }
    fn client(&self) -> Result<Client> {
        self.client.clone().ok_or_else(|| "Sign in first".into())
    }
    fn selected(&self) -> Result<Snowflake> {
        self.state
            .selected_channel
            .ok_or_else(|| "Choose a channel first".into())
    }
    fn media(&mut self) -> Result<&mut MediaEngine> {
        if self.media.is_none() {
            self.media = Some(MediaEngine::new().map_err(error)?);
        }
        self.media
            .as_mut()
            .ok_or_else(|| "Media engine unavailable".into())
    }

    pub(crate) async fn run(mut self, mut commands: async_mpsc::UnboundedReceiver<Command>) {
        self.snapshot();
        loop {
            enum Incoming {
                Navigation(Option<navigation_flow::NavigationEvent>),
                Attachment(Option<attachment_flow::AttachmentEvent>),
                Command(Option<Command>),
                Gateway(Option<GatewayEvent>),
                Media(Option<MediaEvent>),
                VoiceTimeout,
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
                event=self.navigation.rx.recv()=>Incoming::Navigation(event),
                event=self.attachments.rx.recv()=>Incoming::Attachment(event),
                command = commands.recv() => Incoming::Command(command),
                event = receive_optional(&mut self.gateway_events) => Incoming::Gateway(event),
                event = receive_media(&mut self.media_events) => Incoming::Media(event),
                _ = voice_timeout(self.voice_join_deadline) => Incoming::VoiceTimeout,
                _ = voice_timeout(self.pending_ip.as_ref().and_then(|pending|pending.next_poll)) => Incoming::BeginIpPoll,
                result=ip_poll_result(&mut self.ip_poll_task)=>Incoming::IpPoll(result),
                _ = voice_timeout(self.pending_browser.as_ref().and_then(|pending|pending.next_poll))=>Incoming::BeginBrowserPoll,
                result=browser_poll_result(&mut self.browser_poll_task)=>Incoming::BrowserPoll(Box::new(result)),
                result=restore_result(&mut self.restore_task)=>Incoming::Restore(Box::new(result)),
            };
            let result = match incoming {
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
                Incoming::VoiceTimeout => {
                    let _ = self.leave_voice(true).await;
                    Err("Voice server did not complete the connection; try joining again".into())
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
        self.clear_navigation();
        self.clear_attachments();
        if let Some(task) = self.restore_task.take() {
            task.abort();
        }
        self.emit(Event::SessionRestoring(false));
        self.saved_session = None;
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
        self.last_sequence = None;
        self.expected_user_id = None;
        self.selected_guild = None;
        self.synced_guilds.clear();
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
        self.state = ClientState::default();
        self.state.settings.locale = self.locale.clone();
        self.demo = false;
        if let Ok(mut frames) = self.frames.lock() {
            frames.clear();
        }
    }

    async fn command(&mut self, command: Command) -> Result<()> {
        match command {
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
                self.emit(Event::Demo(true));
                self.snapshot();
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
            Command::SelectGuild { guild_id } => {
                self.selected_guild = Some(guild_id);
                if !self.demo {
                    self.sync_selected_guild().await?;
                    self.persist_selection().await;
                }
                self.snapshot();
            }
            Command::SelectChannel { channel_id } => self.select_channel(channel_id).await?,
            Command::SearchMessages {
                channel_id,
                query,
                scope,
                scope_override,
                sort,
                page,
            } => self.search_messages(channel_id, query, scope, scope_override, sort, page)?,
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
            Command::SetInputDevice(id) => self.media()?.set_input_device(&id).map_err(error)?,
            Command::SetOutputDevice(id) => self.media()?.set_output_device(&id).map_err(error)?,
            Command::SetDsp(settings) => self.media()?.configure(settings).map_err(error)?,
            Command::SetMixer(settings) => {
                self.media()?.configure_mixer(settings).map_err(error)?
            }
            Command::SetPushToTalkPressed(pressed) => self
                .media()?
                .set_push_to_talk_pressed(pressed)
                .map_err(error)?,
            Command::SetParticipantAudio { identity, settings } => self
                .media()?
                .set_participant_audio(&identity, settings)
                .map_err(error)?,
            Command::SetStreamAudio { identity, settings } => self
                .media()?
                .set_stream_audio(&identity, settings)
                .map_err(error)?,
            Command::WatchStream { identity, watch } => {
                let connected = self.media.as_ref().is_some_and(MediaEngine::is_connected);
                self.set_watch(identity, watch, connected).await?;
            }
            Command::StartMicrophoneTest => {
                if self.voice_target.is_some() {
                    return Err("Leave voice before testing the microphone".into());
                }
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
            selected_guild: self.selected_guild,
        });
        self.snapshot();
        let gateway = Gateway::new(client.instance().gateway.clone(), token)
            // PooledGatewayConnection.ts:474 uses DEFAULT_API_VERSION (1).
            // HTTP routing separately uses discovery's api_code_version.
            .with_api_version(1)
            .with_e2ee_capable(true)
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

    async fn select_channel(&mut self, channel: Snowflake) -> Result<()> {
        if !self.state.channels.contains_key(&channel) {
            return Err("Unknown channel".into());
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
        self.request_history(channel, navigation_flow::HistoryRequest::Latest)?;
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
        if self.synced_guilds.get(&guild) == Some(&session) {
            return Ok(());
        }
        let Some(tx) = &self.gateway_commands else {
            return Ok(());
        };
        // GatewayGuildSubscriptionSynchronizer.ts:43-49. The existing selected
        // guild path leaves previously synced guilds active in this session.
        tx.send(GatewayCommand::Send {
            op: 14,
            data: json!({"subscriptions":{guild.to_string():{"active":true,"sync":true}}}),
        })
        .await
        .map_err(|_| "Gateway is disconnected".to_owned())?;
        self.synced_guilds.insert(guild, session);
        Ok(())
    }

    async fn update_voice(&self) -> Result<()> {
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
        let target = self.voice_target.take();
        self.pending_voice_server = None;
        self.voice_credentials = None;
        self.voice_join_deadline = None;
        self.server_muted = false;
        self.server_deafened = false;
        for identity in std::mem::take(&mut self.available_streams) {
            self.emit(Event::Media(MediaEvent::ScreenStreamAvailable {
                identity,
                available: false,
            }));
        }
        self.watched_streams.clear();
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
            let _ = tx
                .send(GatewayCommand::VoiceState(target.payload(
                    &self.state,
                    true,
                    &self.watched_streams,
                )))
                .await;
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
                self.emit(Event::Status("Connected".into()));
                self.sync_selected_guild().await?;
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
                if name == "VOICE_SERVER_UPDATE" {
                    let update: VoiceServerUpdate = serde_json::from_value(data).map_err(error)?;
                    if self
                        .voice_target
                        .as_ref()
                        .is_some_and(|target| target.accepts(&update))
                    {
                        self.connect_voice(update).await?;
                    } else if update.channel_id.is_none()
                        && self
                            .voice_target
                            .as_ref()
                            .is_some_and(|target| target.connection_id.is_none())
                    {
                        // The server can issue credentials before our own state.
                        // Keep one bounded candidate and await a matching connection ID.
                        self.pending_voice_server = Some(update);
                    }
                } else {
                    let previous_voice = self.state.voice.clone();
                    self.state
                        .apply_gateway(&name, data.clone())
                        .map_err(error)?;
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
                            selected_guild: self.selected_guild,
                        });
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
                        self.synced_guilds.insert(guild, session);
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
                            if channel == Some(target.channel_id.to_string().as_str())
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
                            } else if connection.is_some()
                                && connection == target.connection_id.as_deref()
                            {
                                self.leave_voice(false).await?;
                            }
                        }
                        if let Some(update) = self.pending_voice_server.take()
                            && self
                                .voice_target
                                .as_ref()
                                .is_some_and(|target| target.accepts(&update))
                        {
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
        let muted = self.state.voice.muted || self.server_muted;
        let deafened = self.state.voice.deafened || self.server_deafened;
        self.media_events = None;
        self.state.voice.disconnect();
        // Replace transport handles while preserving explicit watch intent and
        // the focused viewer. Republish gateway availability after reconnect.
        self.available_streams.clear();
        if let Ok(mut frames) = self.frames.lock() {
            frames.clear();
        }
        self.emit(Event::Media(MediaEvent::Reconnecting));
        self.emit(Event::VoiceStatus(VoiceStatus::Connecting {
            channel_id: channel,
        }));
        let result = async {
            let media = self.media()?;
            media.stop_microphone_test().map_err(error)?;
            // Publishing parameters are immutable for an active room. Teardown
            // also releases the old microphone before any replacement attempt.
            media.disconnect().await.map_err(error)?;
            media
                .configure_microphone_publish(Some(bitrate as f64), false)
                .map_err(error)?;
            media.set_muted(muted).map_err(error)?;
            media.set_deafened(deafened).map_err(error)?;
            tokio::time::timeout(
                Duration::from_secs(20),
                media.connect_encrypted(
                    &update.endpoint,
                    update.token.expose_secret(),
                    update.e2ee_key.as_deref(),
                ),
            )
            .await
            .map_err(|_| "Voice media connection timed out".to_owned())?
            .map_err(error)
        }
        .await;
        match result {
            Ok(events) => {
                self.media_events = Some(events);
                self.voice_credentials = Some(update);
                self.voice_join_deadline = None;
                self.state.voice.join(channel);
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
                let _ = self.leave_voice(true).await;
                Err(message)
            }
        }
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
            MediaEvent::Disconnected => {
                self.leave_voice(true).await?;
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
