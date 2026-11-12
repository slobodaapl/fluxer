use crate::{Command, Event, VoiceStatus};
use fluxer_core::{
    Channel, ChannelType, ClientState, Emoji, Guild, Message, Permissions, Snowflake, User,
};
use fluxer_media::{MediaEngine, MediaEvent, VideoFrame};
use fluxer_protocol::{
    Client, Gateway, GatewayCommand, GatewayEvent, Instance, LoginIdentifier, LoginOutcome,
    SessionToken, VoiceServerUpdate, VoiceStateUpdate,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    future::pending,
    sync::{Arc, Mutex, mpsc},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{sync::mpsc as async_mpsc, task::JoinHandle};

type Result<T> = std::result::Result<T, String>;
fn error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct VoiceTarget {
    guild_id: Option<Snowflake>,
    channel_id: Snowflake,
    connection_id: Option<String>,
}

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
    fn payload(&self, state: &ClientState, leave: bool) -> VoiceStateUpdate {
        VoiceStateUpdate {
            guild_id: self.guild_id.map(|id| id.to_string()),
            channel_id: (!leave).then(|| self.channel_id.to_string()),
            connection_id: self.connection_id.clone(),
            self_mute: state.voice.muted,
            self_deaf: state.voice.deafened,
            self_stream: state.voice.screen_share,
            ..VoiceStateUpdate::default()
        }
    }
}

pub(crate) struct Worker {
    state: ClientState,
    client: Option<Client>,
    pending_mfa: Option<Client>,
    events: mpsc::Sender<Event>,
    frames: Arc<Mutex<BTreeMap<String, VideoFrame>>>,
    gateway_commands: Option<async_mpsc::Sender<GatewayCommand>>,
    gateway_events: Option<async_mpsc::Receiver<GatewayEvent>>,
    gateway_task: Option<JoinHandle<()>>,
    gateway_ready: bool,
    last_sequence: Option<u64>,
    media: Option<MediaEngine>,
    media_events: Option<async_mpsc::Receiver<MediaEvent>>,
    voice_target: Option<VoiceTarget>,
    pending_voice_server: Option<VoiceServerUpdate>,
    voice_endpoint: Option<String>,
    voice_join_deadline: Option<tokio::time::Instant>,
    server_muted: bool,
    server_deafened: bool,
    demo: bool,
}

impl Worker {
    pub(crate) fn new(
        events: mpsc::Sender<Event>,
        frames: Arc<Mutex<BTreeMap<String, VideoFrame>>>,
    ) -> Self {
        Self {
            state: ClientState::default(),
            client: None,
            pending_mfa: None,
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
            voice_endpoint: None,
            voice_join_deadline: None,
            server_muted: false,
            server_deafened: false,
            demo: false,
        }
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
                Command(Option<Command>),
                Gateway(Option<GatewayEvent>),
                Media(Option<MediaEvent>),
                VoiceTimeout,
            }
            let incoming = tokio::select! {
                command = commands.recv() => Incoming::Command(command),
                event = receive_optional(&mut self.gateway_events) => Incoming::Gateway(event),
                event = receive_optional(&mut self.media_events) => Incoming::Media(event),
                _ = voice_timeout(self.voice_join_deadline) => Incoming::VoiceTimeout,
            };
            let result = match incoming {
                Incoming::Command(None | Some(Command::Shutdown)) => break,
                Incoming::Command(Some(command)) => self.command(command).await,
                Incoming::Gateway(Some(event)) => self.gateway(event).await,
                Incoming::Gateway(None) => {
                    self.gateway_events = None;
                    self.gateway_ready = false;
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
            };
            if let Err(message) = result {
                self.emit(Event::Error(message));
            }
        }
        self.reset().await;
    }

    async fn reset(&mut self) {
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
        self.pending_mfa = None;
        self.client = None;
        self.state = ClientState::default();
        self.demo = false;
        if let Ok(mut frames) = self.frames.lock() {
            frames.clear();
        }
    }

    async fn command(&mut self, command: Command) -> Result<()> {
        match command {
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
                    .map_err(error)?;
                let identifier = if identifier.contains('@') {
                    LoginIdentifier::Email(identifier)
                } else {
                    LoginIdentifier::Login(identifier)
                };
                match client.login(identifier, &password).await.map_err(error)? {
                    LoginOutcome::Authenticated { token, .. } => {
                        self.authenticate(client.with_token(token.clone()), token)
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
                    LoginOutcome::IpAuthorization { email, .. } => {
                        return Err(format!(
                            "Authorize this sign-in using the email sent to {email}"
                        ));
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
                    .with_token(token.clone());
                self.authenticate(client, token).await?;
            }
            Command::LoginMfa { ticket, code } => {
                let client = self.pending_mfa.clone().ok_or("Start a sign-in first")?;
                let session = client.login_mfa_totp(&ticket, &code).await.map_err(error)?;
                self.pending_mfa = None;
                self.authenticate(client.with_token(session.token.clone()), session.token)
                    .await?;
            }
            Command::Logout => {
                let client = self.client.clone();
                self.reset().await;
                self.emit(Event::Demo(false));
                self.snapshot();
                if let Some(client) = client {
                    client.logout().await.map_err(error)?;
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
            Command::SelectGuild { guild_id } => {
                if !self.demo {
                    let channels = self
                        .client()?
                        .guild_channels(&guild_id.to_string())
                        .await
                        .map_err(error)?;
                    for mut channel in values(channels, "channels")? {
                        channel["guild_id"] = json!(guild_id);
                        self.state
                            .apply_gateway("CHANNEL_CREATE", channel)
                            .map_err(error)?;
                    }
                }
                self.snapshot();
            }
            Command::SelectChannel { channel_id } => self.select_channel(channel_id).await?,
            Command::LoadOlderMessages => {
                if !self.demo {
                    let channel = self.selected()?;
                    let before = self
                        .state
                        .channel_messages(channel)
                        .next()
                        .map(|m| m.id.to_string());
                    let history = self
                        .client()?
                        .messages(&channel.to_string(), before.as_deref(), 50)
                        .await
                        .map_err(error)?;
                    let messages = values(history, "messages")?
                        .into_iter()
                        .map(serde_json::from_value)
                        .collect::<std::result::Result<Vec<Message>, _>>()
                        .map_err(error)?;
                    self.state.load_history(channel, messages);
                    self.snapshot();
                }
            }
            Command::SetDraft(content) => {
                let channel = self.selected()?;
                self.state.set_draft(channel, content);
            }
            Command::SendMessage { content } => {
                let channel = self.selected()?;
                if content.trim().is_empty() {
                    return Err("Message cannot be empty".into());
                }
                let message = if self.demo {
                    Message::text(
                        self.state.next_local_message_id(),
                        channel,
                        self.state
                            .current_user
                            .clone()
                            .ok_or("No current account")?,
                        content.clone(),
                    )
                } else {
                    let nonce = format!(
                        "{}",
                        SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_nanos()
                    );
                    serde_json::from_value(
                        self.client()?
                            .send_message(&channel.to_string(), &content, Some(&nonce))
                            .await
                            .map_err(error)?,
                    )
                    .map_err(error)?
                };
                self.state.insert_message(message);
                self.state.set_draft(channel, String::new());
                self.snapshot();
                self.emit(Event::MessageSent {
                    channel_id: channel,
                    content,
                });
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
                    let message = self
                        .client()?
                        .edit_message(&channel.to_string(), &message_id.to_string(), &content)
                        .await
                        .map_err(error)?;
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
                let existing = self
                    .state
                    .channel_messages(channel)
                    .find(|m| m.id == message_id)
                    .and_then(|m| m.reactions.iter().find(|r| r.emoji.name == emoji))
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
                    self.state
                        .toggle_reaction(channel, message_id, Emoji::unicode(emoji));
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
                let devices = self.media()?.devices();
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
            Command::StartMicrophoneTest => {
                if self.voice_target.is_some() {
                    return Err("Leave voice before testing the microphone".into());
                }
                self.media_events = Some(self.media()?.start_microphone_test().map_err(error)?);
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
            Command::StartShare(options) => {
                if self.state.voice.channel_id.is_none() {
                    return Err("Join a voice channel first".into());
                }
                self.media()?
                    .start_screen_share(options)
                    .await
                    .map_err(error)?;
                self.state.voice.screen_share = true;
                self.snapshot();
                self.update_voice().await?;
            }
            Command::StopShare => {
                if let Some(media) = &mut self.media {
                    media.stop_screen_share().await.map_err(error)?;
                }
                self.state.voice.screen_share = false;
                self.snapshot();
                self.update_voice().await?;
            }
            Command::RefreshAudioApplications => {
                let applications = MediaEngine::audio_applications().map_err(error)?;
                self.emit(Event::AudioApplications(applications));
            }
            Command::StartSharedAudio(selection) => {
                if !self.state.voice.screen_share {
                    return Err("Start screen sharing before sharing audio".into());
                }
                self.media()?
                    .start_shared_audio(selection)
                    .await
                    .map_err(error)?;
            }
            Command::StopSharedAudio => {
                if let Some(media) = &mut self.media {
                    media.stop_shared_audio().await.map_err(error)?;
                }
            }
            Command::SetSharedAudio(settings) => self
                .media()?
                .configure_shared_audio(settings)
                .map_err(error)?,
            Command::Shutdown => {}
        }
        Ok(())
    }

    async fn authenticate(&mut self, client: Client, token: SessionToken) -> Result<()> {
        let user: User =
            serde_json::from_value(client.current_user().await.map_err(error)?).map_err(error)?;
        self.state.current_user = Some(user.clone());
        self.state.users.insert(user.id, user);
        for result in [client.guilds().await, client.private_channels().await]
            .into_iter()
            .enumerate()
        {
            match result {
                (0, Ok(value)) => {
                    for guild in values(value, "guilds")? {
                        let guild: Guild = serde_json::from_value(guild).map_err(error)?;
                        self.state.guilds.insert(guild.id, guild);
                    }
                }
                (_, Ok(value)) => {
                    for channel in values(value, "channels")? {
                        let channel: Channel = serde_json::from_value(channel).map_err(error)?;
                        self.state.channels.insert(channel.id, channel);
                    }
                }
                (_, Err(message)) => self.emit(Event::Error(message.to_string())),
            }
        }
        self.client = Some(client.clone());
        self.snapshot();
        let gateway = Gateway::new(client.instance().gateway.clone(), token)
            .with_api_version(client.instance().api_version)
            .with_e2ee_capable(true);
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
        if !self.demo {
            let history = self
                .client()?
                .messages(&channel.to_string(), None, 50)
                .await
                .map_err(error)?;
            let messages = values(history, "messages")?
                .into_iter()
                .map(serde_json::from_value)
                .collect::<std::result::Result<Vec<Message>, _>>()
                .map_err(error)?;
            self.state.load_history(channel, messages);
            if let Some(message) = self.state.channel_messages(channel).next_back() {
                let _ = self
                    .client()?
                    .acknowledge(&channel.to_string(), &message.id.to_string())
                    .await
                    .map_err(|message| self.emit(Event::Error(message.to_string())));
            }
        }
        self.state.select_channel(channel);
        self.snapshot();
        Ok(())
    }

    async fn update_voice(&self) -> Result<()> {
        if let Some(target) = &self.voice_target {
            self.gateway_commands
                .as_ref()
                .ok_or("Gateway is disconnected")?
                .send(GatewayCommand::VoiceState(
                    target.payload(&self.state, false),
                ))
                .await
                .map_err(|_| "Gateway is disconnected".to_owned())?;
        }
        Ok(())
    }

    async fn leave_voice(&mut self, notify_gateway: bool) -> Result<()> {
        let target = self.voice_target.take();
        self.pending_voice_server = None;
        self.voice_endpoint = None;
        self.voice_join_deadline = None;
        self.server_muted = false;
        self.server_deafened = false;
        self.media_events = None;
        // Stop capture before awaiting any network writes, including failures.
        let disconnected = if let Some(media) = &mut self.media {
            media.disconnect().await.map_err(error)
        } else {
            Ok(())
        };
        self.state.voice.disconnect();
        if let Ok(mut frames) = self.frames.lock() {
            frames.clear();
        }
        self.emit(Event::VoiceStatus(VoiceStatus::Disconnected));
        if notify_gateway && let (Some(target), Some(tx)) = (target, &self.gateway_commands) {
            let _ = tx
                .send(GatewayCommand::VoiceState(
                    target.payload(&self.state, true),
                ))
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
            }
            GatewayEvent::Error(message) => self.emit(Event::Error(message)),
            GatewayEvent::Disconnected => {
                self.gateway_ready = false;
                self.leave_voice(false).await?;
                self.emit(Event::Status("Gateway disconnected".into()));
            }
            GatewayEvent::Dispatch {
                name,
                data,
                sequence,
            } => {
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
                    if name == "READY" {
                        self.state.voice = previous_voice;
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
                            if channel == Some(target.channel_id.to_string().as_str()) {
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
                        if let Some(update) = self.pending_voice_server.take() {
                            if self
                                .voice_target
                                .as_ref()
                                .is_some_and(|target| target.accepts(&update))
                            {
                                self.connect_voice(update).await?;
                            }
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
                    self.snapshot();
                }
                if let Some(sequence) = sequence {
                    self.last_sequence = Some(sequence);
                }
            }
        }
        Ok(())
    }

    async fn connect_voice(&mut self, update: VoiceServerUpdate) -> Result<()> {
        let target = self
            .voice_target
            .as_mut()
            .ok_or("Voice join was cancelled")?;
        if self.voice_endpoint.as_deref() == Some(update.endpoint.as_str())
            && target.connection_id.as_deref() == Some(update.connection_id.as_str())
            && self.state.voice.channel_id == Some(target.channel_id)
        {
            return Ok(());
        }
        target.connection_id = Some(update.connection_id.clone());
        let channel = target.channel_id;
        let muted = self.state.voice.muted || self.server_muted;
        let deafened = self.state.voice.deafened || self.server_deafened;
        let result = async {
            let media = self.media()?;
            media.stop_microphone_test().map_err(error)?;
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
                self.voice_endpoint = Some(update.endpoint);
                self.voice_join_deadline = None;
                self.state.voice.join(channel);
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
            MediaEvent::Video(frame) => {
                if let Ok(mut frames) = self.frames.lock() {
                    frames.insert(frame.track_id.clone(), frame);
                }
            }
            MediaEvent::VideoEnded { ref track_id } => {
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
            MediaEvent::ScreenShareStopped => {
                self.state.voice.screen_share = false;
                self.snapshot();
                self.update_voice().await?;
            }
            event => self.emit(Event::Media(event)),
        }
        Ok(())
    }
}

async fn receive_optional<T>(receiver: &mut Option<async_mpsc::Receiver<T>>) -> Option<T> {
    match receiver {
        Some(receiver) => receiver.recv().await,
        None => pending().await,
    }
}
async fn voice_timeout(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => pending().await,
    }
}
fn values(value: Value, key: &str) -> Result<Vec<Value>> {
    value
        .as_array()
        .or_else(|| value.get(key).and_then(Value::as_array))
        .cloned()
        .ok_or_else(|| format!("Server response omitted {key} list"))
}
