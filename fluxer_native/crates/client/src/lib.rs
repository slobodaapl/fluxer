//! Serialized native client controller. Network and media handles stay on its
//! Tokio worker; GPUI sends commands and polls coalesced presentation events.
mod worker;

pub use fluxer_core::{ClientState, Snowflake};
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

pub enum Command {
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
    SetLocale(String),
    SelectGuild {
        guild_id: Snowflake,
    },
    SelectChannel {
        channel_id: Snowflake,
    },
    LoadOlderMessages,
    SendMessage {
        content: String,
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
    StartShare(ScreenShareOptions),
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

pub enum Event {
    SessionRestoring(bool),
    SessionRestoreFailed,
    SessionContext {
        instance: String,
        selected_guild: Option<Snowflake>,
    },
    State(Box<ClientState>),
    MessageSent {
        channel_id: Snowflake,
        content: String,
    },
    MessageEdited {
        channel_id: Snowflake,
        message_id: Snowflake,
        content: String,
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
                            _ = worker::Worker::new(tx,worker_frames).with_session_store(store).run(rx) => {},
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
        })
    }

    pub fn send(&self, command: Command) -> Result<(), String> {
        self.commands
            .send(command)
            .map_err(|_| "Client worker stopped".into())
    }

    pub fn try_recv(&self) -> Option<Event> {
        if let Ok(event) = self.events.try_recv() {
            if let Event::WatchedStreams(watched) = &event
                && let Ok(mut frames) = self.frames.lock()
            {
                frames.retain(|_, frame| watched.contains(&frame.participant));
            }
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
                rgba: Arc::new(vec![0, 0, 0, 255]),
            },
        )])));
        let controller = Controller {
            commands,
            events,
            frames: frames.clone(),
            shutdown: None,
        };
        events_tx.send(Event::WatchedStreams(Vec::new())).unwrap();
        assert!(
            matches!(controller.try_recv(),Some(Event::WatchedStreams(streams)) if streams.is_empty())
        );
        assert!(frames.lock().unwrap().is_empty());
        assert!(controller.try_recv().is_none());
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
