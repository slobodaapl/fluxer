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
