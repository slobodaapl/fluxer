// SPDX-License-Identifier: AGPL-3.0-or-later
//! Native media transport. Call credentials come from Fluxer's gateway, never
//! from an embedded LiveKit API secret. Capture begins only when joining/sharing.

mod audio;
mod file_playback;
pub use file_playback::{
    FilePlaybackFrame, FilePlaybackOptions, FilePlaybackSnapshot, FilePlaybackStatus, FilePlayer,
    PlaybackSource,
};
mod microphone;
mod microphone_device;
mod pipewire_runtime;
mod playback;
mod publish;
mod screen;
mod subscriptions;
mod video_events;
pub use video_events::MediaEvents;

pub use audio::{AudioMixSettings, InputMode, ParticipantAudioSettings};
pub use publish::resolve_voice_channel_bitrate;

use futures_util::StreamExt;
use livekit::prelude::*;
use livekit::webrtc::{
    audio_stream::native::{NativeAudioStream, NativeAudioStreamOptions},
    peer_connection_factory::native::PeerConnectionFactoryExt,
    video_frame::VideoFormatType,
    video_stream::native::NativeVideoStream,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::{sync::mpsc, task::JoinHandle};

pub use fluxer_system_audio::{Application as AudioApplication, Selection as SharedAudioSelection};
pub use screen::{ScreenShareOptions, ScreenSource, ScreenSourceKind};

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("Audio device error: {0}")]
    Audio(String),
    #[error("Call operation failed: {0}")]
    Call(String),
    #[error("Join a voice channel first")]
    NotConnected,
    #[error("Unknown {0} device; refresh the device list")]
    UnknownDevice(&'static str),
    #[error("Screen capture failed: {0}")]
    Screen(String),
    #[error("Invalid media setting: {0}")]
    InvalidSetting(&'static str),
}

pub type Result<T> = std::result::Result<T, MediaError>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioDevice {
    pub id: String,
    pub label: String,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInventory {
    pub inputs: Vec<AudioDevice>,
    pub outputs: Vec<AudioDevice>,
    pub input_selection: DeviceSelection,
    pub output_selection: DeviceSelection,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceSelection {
    pub preferred: String,
    pub effective: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioSettings {
    pub echo_cancellation: bool,
    pub noise_suppression: bool,
    pub auto_gain_control: bool,
}
impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            echo_cancellation: true,
            noise_suppression: true,
            auto_gain_control: true,
        }
    }
}

/// BGRA pixels, directly consumable by GPUI without per-pixel swizzling. The receiving UI should retain only the latest frame per track.
#[derive(Clone, Debug)]
pub struct VideoFrame {
    pub track_id: String,
    pub participant: String,
    pub watch_generation: u64,
    pub width: u32,
    pub height: u32,
    pub bgra: Arc<Vec<u8>>,
}

#[derive(Clone, Debug)]
pub enum MediaEvent {
    Connected,
    Disconnected,
    Reconnecting,
    ParticipantJoined {
        identity: String,
        name: String,
    },
    ParticipantLeft {
        identity: String,
    },
    ScreenStreamAvailable {
        identity: String,
        available: bool,
    },
    Speakers(Vec<String>),
    Video(VideoFrame),
    VideoEnded {
        track_id: String,
        participant: String,
        watch_generation: u64,
    },
    ScreenShareStarted,
    ScreenShareStopped,
    SharedAudioStopped,
    AudioLevels {
        input: f32,
        output: f32,
    },
    Error(String),
    /// The native input stream stopped; reconciliation may reopen it.
    InputDeviceFailed,
}

struct VideoRenderer {
    task: JoinHandle<()>,
    participant: String,
    watch_generation: u64,
}
impl VideoRenderer {
    fn abort(&self) {
        self.task.abort();
    }
}

struct SharedAudioLifecycle {
    running: Arc<AtomicBool>,
    events: Option<mpsc::Sender<MediaEvent>>,
}
fn shared_audio_stopped(events: &Option<mpsc::Sender<MediaEvent>>) {
    if let Some(sender) = events
        && let Err(mpsc::error::TrySendError::Full(event)) =
            sender.try_send(MediaEvent::SharedAudioStopped)
        && let Ok(runtime) = tokio::runtime::Handle::try_current()
    {
        let sender = sender.clone();
        runtime.spawn(async move {
            let _ = sender.send(event).await;
        });
    }
}
impl Drop for SharedAudioLifecycle {
    fn drop(&mut self) {
        if self.running.swap(false, Ordering::AcqRel) {
            shared_audio_stopped(&self.events);
        }
    }
}

struct RuntimeState {
    deafened: bool,
    remotes: HashMap<String, RemoteAudioTrack>,
}

#[derive(Clone, Copy, Debug)]
pub struct SharedAudioSettings {
    pub gain: f32,
    pub muted: bool,
}
impl Default for SharedAudioSettings {
    fn default() -> Self {
        Self {
            gain: 1.0,
            muted: false,
        }
    }
}

/// Owned by the asynchronous media worker; all native handles stay off GPUI's
/// rendering thread. Drop aborts capture and tasks even on application shutdown.
pub struct MediaEngine {
    input_device: Mutex<Option<String>>,
    recovery_cancel: Option<Arc<AtomicBool>>,
    input_retry_at: Option<std::time::Instant>,
    output_retry_at: Option<std::time::Instant>,
    input_selection: DeviceSelection,
    output_selection: DeviceSelection,
    room: Option<Room>,
    microphone: Option<LocalAudioTrack>,
    microphone_publish: publish::MicrophonePublishSettings,
    muted: bool,
    deafened: bool,
    state: Arc<Mutex<RuntimeState>>,
    events: Option<mpsc::Sender<MediaEvent>>,
    room_task: Option<JoinHandle<()>>,
    screen: Option<screen::ScreenSession>,
    screen_publication: Option<TrackSid>,
    video_tasks: Arc<Mutex<HashMap<String, VideoRenderer>>>,
    video_frames: Arc<video_events::VideoMailbox>,
    stop: Arc<AtomicBool>,
    shared_audio_task: Option<JoinHandle<()>>,
    shared_audio_running: Arc<AtomicBool>,
    shared_audio_publication: Option<TrackSid>,
    shared_audio_settings: Arc<Mutex<SharedAudioSettings>>,
    mixer: Arc<Mutex<audio::PlaybackMixer>>,
    output: playback::PlaybackOutput,
    input_controls: Arc<Mutex<microphone::InputRuntime>>,
    input_capture: Option<microphone::MicrophoneCapture>,
    audio_tasks: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
    subscriptions: Arc<Mutex<subscriptions::Registry>>,
}

fn desired_device(
    selection: &DeviceSelection,
    devices: &[AudioDevice],
    default: &str,
) -> Option<String> {
    if devices
        .iter()
        .any(|device| device.id == selection.preferred)
    {
        Some(selection.preferred.clone())
    } else if devices.iter().any(|device| device.id == default) {
        Some(default.into())
    } else {
        None
    }
}

pub struct DeviceRecovery {
    inventory: DeviceInventory,
    cancel: Arc<AtomicBool>,
    input_retry_at: Option<std::time::Instant>,
    output_retry_at: Option<std::time::Instant>,
}

impl MediaEngine {
    pub fn new() -> Result<Self> {
        pipewire_runtime::ensure()?;
        let mixer = Arc::new(Mutex::new(audio::PlaybackMixer::new()));
        let output = playback::PlaybackOutput::new(mixer.clone());
        let subscriptions = Arc::new(Mutex::new(subscriptions::Registry::default()));
        let video_frames = Arc::new(video_events::VideoMailbox::new(subscriptions.clone()));
        Ok(Self {
            input_device: Mutex::new(None),
            recovery_cancel: None,
            input_retry_at: None,
            output_retry_at: None,
            input_selection: DeviceSelection {
                preferred: "input:default".into(),
                effective: Some("input:default".into()),
            },
            output_selection: DeviceSelection {
                preferred: "output:default".into(),
                effective: Some("output:default".into()),
            },
            room: None,
            microphone: None,
            microphone_publish: publish::MicrophonePublishSettings::default(),
            muted: false,
            deafened: false,
            state: Arc::new(Mutex::new(RuntimeState {
                deafened: false,
                remotes: HashMap::new(),
            })),
            events: None,
            room_task: None,
            screen: None,
            screen_publication: None,
            video_tasks: Arc::new(Mutex::new(HashMap::new())),
            video_frames,
            stop: Arc::new(AtomicBool::new(false)),
            shared_audio_task: None,
            shared_audio_running: Arc::new(AtomicBool::new(false)),
            shared_audio_publication: None,
            shared_audio_settings: Arc::new(Mutex::new(SharedAudioSettings::default())),
            mixer,
            output,
            input_controls: Arc::new(Mutex::new(microphone::InputRuntime::default())),
            input_capture: None,
            audio_tasks: Arc::new(Mutex::new(HashMap::new())),
            subscriptions,
        })
    }

    pub fn devices(&self) -> DeviceInventory {
        self.try_devices().unwrap_or_default()
    }

    /// Enumerate current logical devices without hiding backend failures.
    pub fn try_devices(&self) -> Result<DeviceInventory> {
        Ok(DeviceInventory {
            inputs: microphone_device::devices()?,
            outputs: playback::devices()?,
            input_selection: self.input_selection.clone(),
            output_selection: self.output_selection.clone(),
        })
    }

    /// Restores local preferences without enumerating devices or opening capture.
    pub fn restore_device_preferences(&mut self, input: &str, output: &str) {
        self.cancel_device_recovery();
        self.input_selection = DeviceSelection {
            preferred: input.into(),
            effective: None,
        };
        self.output_selection = DeviceSelection {
            preferred: output.into(),
            effective: None,
        };
        if let Ok(mut selected) = self.input_device.lock() {
            *selected = None;
        }
    }

    pub fn set_input_device(&mut self, id: &str) -> Result<()> {
        self.cancel_device_recovery();
        self.input_retry_at = None;
        if !microphone_device::devices()?.iter().any(|d| d.id == id) {
            return Err(MediaError::UnknownDevice("input"));
        }
        self.input_selection.preferred = id.into();
        self.apply_input(id)
    }
    fn apply_input(&mut self, id: &str) -> Result<()> {
        if let Some(capture) = &self.input_capture
            && let Err(error) = capture.set_device(id)
        {
            self.input_selection.effective = None;
            return Err(error);
        }
        *self
            .input_device
            .lock()
            .map_err(|_| MediaError::Audio("Input settings unavailable".into()))? = Some(id.into());
        self.input_selection.effective = Some(id.into());
        Ok(())
    }
    pub fn set_output_device(&mut self, id: &str) -> Result<()> {
        self.cancel_device_recovery();
        self.output_retry_at = None;
        if !playback::devices()?.iter().any(|d| d.id == id) {
            return Err(MediaError::UnknownDevice("output"));
        }
        self.output_selection.preferred = id.into();
        self.apply_output(id)
    }
    fn apply_output(&mut self, id: &str) -> Result<()> {
        if let Err(error) = self.output.set_device(id) {
            self.output_selection.effective = None;
            return Err(error);
        }
        self.output_selection.effective = Some(id.into());
        Ok(())
    }
    pub fn input_device_failed(&mut self) {
        self.cancel_device_recovery();
        self.input_selection.effective = None;
        self.input_retry_at = None;
    }

    fn cancel_device_recovery(&mut self) {
        if let Some(cancel) = self.recovery_cancel.take() {
            cancel.store(true, Ordering::Release);
        }
    }

    /// Enumerate and switch on a blocking worker, never on the client event loop.
    pub fn begin_device_recovery(&mut self) -> JoinHandle<Result<DeviceRecovery>> {
        self.cancel_device_recovery();
        let cancel = Arc::new(AtomicBool::new(false));
        self.recovery_cancel = Some(cancel.clone());
        let input_selection = self.input_selection.clone();
        let output_selection = self.output_selection.clone();
        let input = self
            .input_capture
            .as_ref()
            .map(|capture| capture.controller());
        let output = self.output.controller();
        let input_retry_at = self.input_retry_at;
        let output_retry_at = self.output_retry_at;
        tokio::task::spawn_blocking(move || {
            let inputs = microphone_device::devices()?;
            let outputs = playback::devices()?;
            let target_input = desired_device(&input_selection, &inputs, "input:default");
            let target_output = desired_device(&output_selection, &outputs, "output:default");
            let mut next_input = input_selection.clone();
            let mut next_output = output_selection.clone();
            let now = std::time::Instant::now();
            let mut next_input_retry = input_retry_at;
            let mut next_output_retry = output_retry_at;
            if next_input.effective != target_input
                && input_retry_at.is_none_or(|at| {
                    now >= at || target_input.as_deref() != Some(input_selection.preferred.as_str())
                })
            {
                next_input.effective = match target_input {
                    Some(id) if !cancel.load(Ordering::Acquire) => {
                        let open = |id: &str| {
                            input
                                .as_ref()
                                .is_none_or(|c| c.set_device_guarded(id, cancel.clone()).is_ok())
                        };
                        if open(&id) {
                            next_input_retry = None;
                            Some(id)
                        } else {
                            next_input_retry = Some(now + std::time::Duration::from_secs(30));
                            if id != "input:default"
                                && inputs.iter().any(|d| d.id == "input:default")
                                && !cancel.load(Ordering::Acquire)
                                && open("input:default")
                            {
                                Some("input:default".into())
                            } else {
                                None
                            }
                        }
                    }
                    _ => None,
                };
            }
            if next_output.effective != target_output
                && !cancel.load(Ordering::Acquire)
                && output_retry_at.is_none_or(|at| {
                    now >= at
                        || target_output.as_deref() != Some(output_selection.preferred.as_str())
                })
            {
                next_output.effective = match target_output {
                    Some(id) => {
                        let open = |id: &str| output.set_device_guarded(id, cancel.clone()).is_ok();
                        if open(&id) {
                            next_output_retry = None;
                            Some(id)
                        } else {
                            next_output_retry = Some(now + std::time::Duration::from_secs(30));
                            if id != "output:default"
                                && outputs.iter().any(|d| d.id == "output:default")
                                && !cancel.load(Ordering::Acquire)
                                && open("output:default")
                            {
                                Some("output:default".into())
                            } else {
                                None
                            }
                        }
                    }
                    _ => None,
                };
            }
            Ok(DeviceRecovery {
                inventory: DeviceInventory {
                    inputs,
                    outputs,
                    input_selection: next_input,
                    output_selection: next_output,
                },
                cancel,
                input_retry_at: next_input_retry,
                output_retry_at: next_output_retry,
            })
        })
    }
    pub fn finish_device_recovery(&mut self, result: DeviceRecovery) -> Option<DeviceInventory> {
        if !self
            .recovery_cancel
            .as_ref()
            .is_some_and(|cancel| Arc::ptr_eq(cancel, &result.cancel))
        {
            return None;
        }
        self.recovery_cancel = None;
        self.input_retry_at = result.input_retry_at;
        self.output_retry_at = result.output_retry_at;
        self.input_selection = result.inventory.input_selection.clone();
        self.output_selection = result.inventory.output_selection.clone();
        if let Ok(mut input) = self.input_device.lock() {
            *input = self.input_selection.effective.clone();
        }
        Some(result.inventory)
    }

    pub fn configure(&self, settings: AudioSettings) -> Result<()> {
        self.input_controls
            .lock()
            .map_err(|_| MediaError::Audio("Input processor unavailable".into()))?
            .processing = settings;
        Ok(())
    }

    /// Set the next call's microphone publishing policy. Resolve guild caps
    /// with resolve_voice_channel_bitrate first. Stereo capture is opt-in and
    /// requires at least 64 kbps, matching sendsStereoMicrophone.
    ///
    /// LiveKit Rust 0.10.1 does not expose forceStereo SDP/AddTrack signaling;
    /// this controls the real PCM source channels without claiming that missing
    /// signaling option. Configuration while a call is active is rejected.
    pub fn configure_microphone_publish(
        &mut self,
        channel_bitrate: Option<f64>,
        stereo_capture: bool,
    ) -> Result<()> {
        if self.room.is_some() {
            return Err(MediaError::InvalidSetting(
                "configure microphone publication before joining the call",
            ));
        }
        self.microphone_publish =
            publish::MicrophonePublishSettings::new(channel_bitrate, stereo_capture);
        Ok(())
    }

    pub async fn connect(&mut self, endpoint: &str, token: &str) -> Result<MediaEvents> {
        self.connect_encrypted(endpoint, token, None).await
    }

    /// A supplied shared E2EE passphrase uses LiveKit's PBKDF2 key provider,
    /// matching ExternalE2EEKeyProvider.setKey(string) in the existing client.
    pub async fn connect_encrypted(
        &mut self,
        endpoint: &str,
        token: &str,
        shared_key: Option<&str>,
    ) -> Result<MediaEvents> {
        self.disconnect().await?;
        self.video_frames = Arc::new(video_events::VideoMailbox::new(self.subscriptions.clone()));
        if !(endpoint.starts_with("wss://") || endpoint.starts_with("ws://")) || token.is_empty() {
            return Err(MediaError::InvalidSetting(
                "missing voice endpoint or token",
            ));
        }
        let mut options = RoomOptions::default();
        options.single_peer_connection = false;
        options.auto_subscribe = false;
        options.dynacast = true;
        if let Some(key) = shared_key {
            if key.is_empty() {
                return Err(MediaError::InvalidSetting("empty E2EE key"));
            }
            use livekit::e2ee::{E2eeOptions, EncryptionType, key_provider::KeyProvider};
            options.encryption = Some(E2eeOptions {
                encryption_type: EncryptionType::Gcm,
                key_provider: KeyProvider::with_shared_key(
                    publish::e2ee_key_provider_options(),
                    key.as_bytes().to_vec(),
                ),
            });
        }
        let (room, mut room_events) = Room::connect(endpoint, token, options)
            .await
            .map_err(|_| MediaError::Call("Unable to connect to the voice server".into()))?;
        // Turn off WebRTC's direct speaker path before enabling our gain mixer.
        // Physical microphone input is separate from WebRTC's synthetic ADM.
        livekit::rtc_engine::lk_runtime::LkRuntime::instance()
            .pc_factory()
            .set_adm_playout_enabled(false);
        if let Err(error) = self.output.start() {
            self.output.stop();
            let _ = room.close().await;
            return Err(error);
        }
        let (tx, rx) = mpsc::channel(32);
        if let Ok(mut mixer) = self.mixer.lock() {
            mixer.events = Some(tx.clone());
        }
        if let Ok(mut controls) = self.input_controls.lock() {
            controls.testing = false;
            controls.blocked = self.muted || self.deafened;
        }
        let capture_result = microphone::MicrophoneCapture::start(
            self.input_device
                .lock()
                .map_err(|_| MediaError::Audio("Input settings unavailable".into()))?
                .as_deref(),
            self.input_controls.clone(),
            self.mixer.clone(),
            tx.clone(),
            self.microphone_publish.channels(),
        );
        let (capture, mic) = match capture_result {
            Ok(capture) => capture,
            Err(error) => {
                self.output.stop();
                let _ = room.close().await;
                return Err(error);
            }
        };
        if self.muted || self.deafened {
            mic.mute();
            mic.disable();
        }
        if let Err(error) = room
            .local_participant()
            .publish_track(
                LocalTrack::Audio(mic.clone()),
                self.microphone_publish.options(),
            )
            .await
        {
            let _ = capture.stop();
            drop(capture);
            self.output.stop();
            let _ = room.close().await;
            return Err(MediaError::Call(error.to_string()));
        }
        self.room = Some(room);
        self.microphone = Some(mic);
        self.input_capture = Some(capture);
        // Each call owns its cancellation flag. A previous capture thread must
        // never resume when a new call starts.
        self.stop = Arc::new(AtomicBool::new(false));
        self.events = Some(tx.clone());
        let state = self.state.clone();
        let stop = self.stop.clone();
        let video_tasks = self.video_tasks.clone();
        let video_frames = self.video_frames.clone();
        let capture_control = self
            .input_capture
            .as_ref()
            .map(|capture| capture.controller());
        let microphone = self.microphone.clone();
        let mixer = self.mixer.clone();
        let audio_tasks = self.audio_tasks.clone();
        let subscriptions = self.subscriptions.clone();
        let _ = tx.try_send(MediaEvent::Connected);
        self.room_task = Some(tokio::spawn(async move {
            loop {
                let event = tokio::select! {
                    _ = tx.closed() => break,
                    event = room_events.recv() => match event { Some(event) => event, None => break },
                };
                if stop.load(Ordering::Acquire) {
                    break;
                }
                let outgoing = match event {
                    RoomEvent::Connected {
                        participants_with_tracks,
                    } => {
                        // The initial roster is included in Connected rather
                        // than ParticipantConnected events for existing peers.
                        for (participant, _) in participants_with_tracks {
                            let plan = subscriptions.lock().ok().map(|mut registry| {
                                registry.observe_participant(participant.clone())
                            });
                            if let Some(plan) = plan {
                                apply_subscription_actions(
                                    &plan,
                                    &mixer,
                                    &audio_tasks,
                                    &video_tasks,
                                    &video_frames,
                                );
                                for (identity, available) in plan.availability {
                                    if tx
                                        .send(MediaEvent::ScreenStreamAvailable {
                                            identity,
                                            available,
                                        })
                                        .await
                                        .is_err()
                                    {
                                        break;
                                    }
                                }
                            }
                            if tx
                                .send(MediaEvent::ParticipantJoined {
                                    identity: participant.identity().to_string(),
                                    name: participant.name(),
                                })
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                        None
                    }
                    RoomEvent::ParticipantConnected(p) => {
                        let plan = subscriptions
                            .lock()
                            .ok()
                            .map(|mut registry| registry.observe_participant(p.clone()));
                        if let Some(plan) = plan {
                            apply_subscription_actions(
                                &plan,
                                &mixer,
                                &audio_tasks,
                                &video_tasks,
                                &video_frames,
                            );
                            for (identity, available) in plan.availability {
                                if tx
                                    .send(MediaEvent::ScreenStreamAvailable {
                                        identity,
                                        available,
                                    })
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                        }
                        Some(MediaEvent::ParticipantJoined {
                            identity: p.identity().to_string(),
                            name: p.name(),
                        })
                    }
                    RoomEvent::ParticipantDisconnected(p) => {
                        let plan = subscriptions.lock().ok().map(|mut registry| {
                            registry.remove_participant(&p.identity().to_string())
                        });
                        if let Some(plan) = plan {
                            apply_subscription_actions(
                                &plan,
                                &mixer,
                                &audio_tasks,
                                &video_tasks,
                                &video_frames,
                            );
                            for (identity, available) in plan.availability {
                                if tx
                                    .send(MediaEvent::ScreenStreamAvailable {
                                        identity,
                                        available,
                                    })
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                        }
                        Some(MediaEvent::ParticipantLeft {
                            identity: p.identity().to_string(),
                        })
                    }
                    RoomEvent::TrackPublished {
                        publication,
                        participant,
                    } => {
                        let plan = subscriptions.lock().ok().map(|mut registry| {
                            registry.observe_publication(participant, publication)
                        });
                        if let Some(plan) = plan {
                            apply_subscription_actions(
                                &plan,
                                &mixer,
                                &audio_tasks,
                                &video_tasks,
                                &video_frames,
                            );
                            for (identity, available) in plan.availability {
                                if tx
                                    .send(MediaEvent::ScreenStreamAvailable {
                                        identity,
                                        available,
                                    })
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                        }
                        None
                    }
                    RoomEvent::TrackUnpublished { publication, .. } => {
                        let plan = subscriptions.lock().ok().map(|mut registry| {
                            registry.remove_publication(&publication.sid().to_string())
                        });
                        if let Some(plan) = plan {
                            apply_subscription_actions(
                                &plan,
                                &mixer,
                                &audio_tasks,
                                &video_tasks,
                                &video_frames,
                            );
                            for (identity, available) in plan.availability {
                                if tx
                                    .send(MediaEvent::ScreenStreamAvailable {
                                        identity,
                                        available,
                                    })
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                        }
                        None
                    }
                    RoomEvent::ActiveSpeakersChanged { speakers } => Some(MediaEvent::Speakers(
                        speakers.iter().map(|p| p.identity().to_string()).collect(),
                    )),
                    RoomEvent::Reconnecting => Some(MediaEvent::Reconnecting),
                    RoomEvent::Reconnected => {
                        let plan = subscriptions
                            .lock()
                            .ok()
                            .map(|mut registry| registry.reconcile());
                        if let Some(plan) = plan {
                            apply_subscription_actions(
                                &plan,
                                &mixer,
                                &audio_tasks,
                                &video_tasks,
                                &video_frames,
                            );
                            for (identity, available) in plan.availability {
                                if tx
                                    .send(MediaEvent::ScreenStreamAvailable {
                                        identity,
                                        available,
                                    })
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                        }
                        Some(MediaEvent::Connected)
                    }
                    RoomEvent::Disconnected { .. } => {
                        stop.store(true, Ordering::Release);
                        video_frames.clear();
                        if let Some(mic) = &microphone {
                            mic.mute();
                            mic.disable();
                        }
                        if let Some(capture) = &capture_control {
                            let _ = capture.stop();
                        }
                        if let Ok(mut m) = mixer.lock() {
                            m.clear();
                        }
                        if let Ok(mut tasks) = audio_tasks.lock() {
                            for (_, task) in tasks.drain() {
                                task.abort();
                            }
                        }
                        if let Ok(mut tasks) = video_tasks.lock() {
                            for (_, task) in tasks.drain() {
                                task.abort();
                            }
                        }
                        let availability = subscriptions
                            .lock()
                            .ok()
                            .map(|mut registry| registry.clear_publications().availability)
                            .unwrap_or_default();
                        for (identity, available) in availability {
                            if tx
                                .send(MediaEvent::ScreenStreamAvailable {
                                    identity,
                                    available,
                                })
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                        Some(MediaEvent::Disconnected)
                    }
                    // VoiceRoomEventBinder.ts reports requested screen-track
                    // failures instead of leaving a watch intent silently idle.
                    RoomEvent::TrackSubscriptionFailed {
                        participant,
                        error,
                        track_sid,
                    } => {
                        let desired = subscriptions
                            .lock()
                            .is_ok_and(|registry| registry.desired_track(&track_sid.to_string()));
                        desired.then(|| {
                            MediaEvent::Error(format!(
                                "Unable to subscribe to media from {}: {error}",
                                participant.identity()
                            ))
                        })
                    }
                    RoomEvent::TrackSubscribed {
                        track: RemoteTrack::Audio(track),
                        participant,
                        publication,
                    } => {
                        let (desired, watch_generation) = subscriptions
                            .lock()
                            .map(|registry| {
                                let identity = participant.identity().to_string();
                                (
                                    registry.decision(&identity, &publication),
                                    registry.watch_generation(&identity),
                                )
                            })
                            .unwrap_or((
                                subscriptions::Decision {
                                    subscribe: false,
                                    enabled: false,
                                },
                                0,
                            ));
                        if !desired.subscribe {
                            track.disable();
                            publication.set_subscribed(false);
                            continue;
                        }
                        publication.set_enabled(desired.enabled);
                        let id = track.sid().to_string();
                        let identity = participant.identity().to_string();
                        let mut stream = NativeAudioStream::with_options(
                            track.rtc_track(),
                            48_000,
                            2,
                            NativeAudioStreamOptions {
                                queue_size_frames: Some(4),
                            },
                        );
                        if let Ok(mut m) = mixer.lock() {
                            if publication.source() == TrackSource::ScreenshareAudio
                                || subscriptions::is_screen_audio_label(&publication.name())
                            {
                                m.add_stream_track(id.clone(), identity);
                            } else {
                                m.add_track(id.clone(), identity);
                            }
                        }
                        let frame_id = id.clone();
                        let track_mixer = mixer.clone();
                        let track_subscriptions = subscriptions.clone();
                        let task = tokio::spawn(async move {
                            while let Some(frame) = stream.next().await {
                                if !track_subscriptions.lock().is_ok_and(|registry| {
                                    registry.desired_track_at(&frame_id, watch_generation)
                                }) {
                                    break;
                                }
                                if let Ok(mut m) = track_mixer.lock() {
                                    m.push(&frame_id, &frame.data, frame.num_channels);
                                }
                            }
                            stream.close();
                        });
                        if let Ok(mut tasks) = audio_tasks.lock()
                            && let Some(old) = tasks.insert(id, task)
                        {
                            old.abort();
                        }
                        if let Ok(mut s) = state.lock() {
                            s.remotes.insert(track.sid().to_string(), track);
                        }
                        None
                    }
                    RoomEvent::TrackUnsubscribed {
                        track: RemoteTrack::Audio(track),
                        ..
                    } => {
                        let id = track.sid().to_string();
                        if let Ok(mut tasks) = audio_tasks.lock()
                            && let Some(task) = tasks.remove(&id)
                        {
                            task.abort();
                        }
                        if let Ok(mut m) = mixer.lock() {
                            m.tracks.remove(&id);
                        }
                        if let Ok(mut s) = state.lock() {
                            s.remotes.remove(&track.sid().to_string());
                        }
                        None
                    }
                    RoomEvent::TrackSubscribed {
                        track: RemoteTrack::Video(track),
                        participant,
                        publication,
                    } => {
                        let (desired, watch_generation) = subscriptions
                            .lock()
                            .map(|registry| {
                                let identity = participant.identity().to_string();
                                (
                                    registry.decision(&identity, &publication),
                                    registry.watch_generation(&identity),
                                )
                            })
                            .unwrap_or((
                                subscriptions::Decision {
                                    subscribe: false,
                                    enabled: false,
                                },
                                0,
                            ));
                        if !desired.subscribe {
                            track.disable();
                            publication.set_subscribed(false);
                            continue;
                        }
                        publication.set_enabled(desired.enabled);
                        if publication.simulcasted() {
                            publication.set_video_quality(livekit::track::VideoQuality::High);
                        }
                        let id = track.sid().to_string();
                        let identity = participant.identity().to_string();
                        let mut stream = NativeVideoStream::new(track.rtc_track());
                        let frame_mailbox = video_frames.clone();
                        let frame_id = id.clone();
                        let track_subscriptions = subscriptions.clone();
                        let renderer_identity = identity.clone();
                        let task = tokio::spawn(async move {
                            while let Some(frame) = stream.next().await {
                                if !track_subscriptions.lock().is_ok_and(|registry| {
                                    registry.desired_track_at(&frame_id, watch_generation)
                                }) {
                                    break;
                                }
                                let w = frame.buffer.width();
                                let h = frame.buffer.height();
                                if w == 0 || h == 0 || w > 8192 || h > 8192 {
                                    continue;
                                }
                                let mut bgra = vec![0; w as usize * h as usize * 4];
                                frame.buffer.to_argb(
                                    VideoFormatType::ARGB,
                                    &mut bgra,
                                    w * 4,
                                    w as i32,
                                    h as i32,
                                );
                                // Bounded channel: drop frames when UI is slow rather
                                // than accumulating unbounded video latency/memory.
                                frame_mailbox.publish(VideoFrame {
                                    track_id: frame_id.clone(),
                                    participant: identity.clone(),
                                    watch_generation,
                                    width: w,
                                    height: h,
                                    bgra: Arc::new(bgra),
                                });
                            }
                            stream.close();
                        });
                        if let Ok(mut tasks) = video_tasks.lock()
                            && let Some(old) = tasks.insert(
                                id,
                                VideoRenderer {
                                    task,
                                    participant: renderer_identity,
                                    watch_generation,
                                },
                            )
                        {
                            old.abort();
                        }
                        None
                    }
                    RoomEvent::TrackUnsubscribed {
                        track: RemoteTrack::Video(track),
                        ..
                    } => {
                        end_video_renderer(
                            &track.sid().to_string(),
                            &video_tasks,
                            &mixer,
                            &video_frames,
                        );
                        None
                    }
                    _ => None,
                };
                if let Some(event) = outgoing
                    && tx.send(event).await.is_err()
                {
                    break;
                }
            }
            // Losing the consumer or the room event stream also stops capture,
            // even if the controller cannot deliver an explicit disconnect.
            stop.store(true, Ordering::Release);
            video_frames.clear();
            if let Some(mic) = &microphone {
                mic.mute();
                mic.disable();
            }
            if let Some(capture) = &capture_control {
                let _ = capture.stop();
            }
            if let Ok(mut m) = mixer.lock() {
                m.clear();
            }
            if let Ok(mut tasks) = audio_tasks.lock() {
                for (_, task) in tasks.drain() {
                    task.abort();
                }
            }
            if let Ok(mut tasks) = video_tasks.lock() {
                for (_, task) in tasks.drain() {
                    task.abort();
                }
            }
        }));
        Ok(MediaEvents::new(rx, self.video_frames.clone()))
    }

    pub fn set_muted(&mut self, muted: bool) -> Result<()> {
        self.muted = muted;
        self.apply_microphone_state()
    }
    /// Explicit screen watch intent, retained before joining/reconnecting.
    /// Pass an empty list when leaving a call or switching accounts.
    pub fn set_watched_participants(&mut self, identities: Vec<String>) -> Result<()> {
        let plan = {
            let mut registry = self
                .subscriptions
                .lock()
                .map_err(|_| MediaError::Audio("Subscription registry unavailable".into()))?;
            registry.replace_watched(identities)
        };
        apply_subscription_actions(
            &plan,
            &self.mixer,
            &self.audio_tasks,
            &self.video_tasks,
            &self.video_frames,
        );
        Ok(())
    }
    /// Frames from an earlier watch of the same publication carry an older
    /// epoch, allowing consumers to reject buffered pixels after rapid rewatch.
    pub fn watch_generation(&self, identity: &str) -> u64 {
        self.subscriptions
            .lock()
            .map(|registry| registry.watch_generation(identity))
            .unwrap_or(0)
    }
    pub fn set_deafened(&mut self, deafened: bool) -> Result<()> {
        self.deafened = deafened;
        let plan = {
            let mut registry = self
                .subscriptions
                .lock()
                .map_err(|_| MediaError::Audio("Subscription registry unavailable".into()))?;
            registry.policy.deafened = deafened;
            registry.plan()
        };
        apply_subscription_actions(
            &plan,
            &self.mixer,
            &self.audio_tasks,
            &self.video_tasks,
            &self.video_frames,
        );
        if let Ok(mut state) = self.state.lock() {
            state.deafened = deafened;
        }
        if let Ok(mut mixer) = self.mixer.lock() {
            mixer.deafened = deafened;
        }
        self.apply_microphone_state()
    }
    fn apply_microphone_state(&self) -> Result<()> {
        let blocked = self.muted || self.deafened;
        if let Ok(mut controls) = self.input_controls.lock() {
            controls.blocked = blocked;
        }
        // Close publication before waiting for physical device teardown, so
        // previously queued frames cannot escape during mute/deafen.
        if blocked && let Some(mic) = &self.microphone {
            mic.mute();
            mic.disable();
        }
        if let Some(capture) = &self.input_capture {
            capture.set_enabled(!blocked)?;
        }
        if !blocked && let Some(mic) = &self.microphone {
            mic.enable();
            mic.unmute();
        }
        Ok(())
    }
    pub fn configure_mixer(&self, settings: AudioMixSettings) -> Result<()> {
        if !settings.validate() {
            return Err(MediaError::InvalidSetting(
                "audio gain/activation setting is outside its valid range",
            ));
        }
        self.mixer
            .lock()
            .map_err(|_| MediaError::Audio("Audio mixer unavailable".into()))?
            .master_gain = settings.output_gain;
        self.input_controls
            .lock()
            .map_err(|_| MediaError::Audio("Input processor unavailable".into()))?
            .settings = settings;
        Ok(())
    }
    pub fn set_push_to_talk_pressed(&self, pressed: bool) -> Result<()> {
        self.input_controls
            .lock()
            .map_err(|_| MediaError::Audio("Input processor unavailable".into()))?
            .ptt = pressed;
        Ok(())
    }
    pub fn set_participant_audio(
        &self,
        identity: &str,
        settings: ParticipantAudioSettings,
    ) -> Result<()> {
        if identity.is_empty() || !settings.validate() {
            return Err(MediaError::InvalidSetting(
                "invalid participant audio setting",
            ));
        }
        self.mixer
            .lock()
            .map_err(|_| MediaError::Audio("Audio mixer unavailable".into()))?
            .participants
            .insert(identity.into(), settings);
        Ok(())
    }
    pub fn set_stream_audio(
        &self,
        identity: &str,
        settings: ParticipantAudioSettings,
    ) -> Result<()> {
        if identity.is_empty() || !settings.validate() {
            return Err(MediaError::InvalidSetting("invalid stream audio setting"));
        }
        self.mixer
            .lock()
            .map_err(|_| MediaError::Audio("Audio mixer unavailable".into()))?
            .streams
            .insert(identity.into(), settings);
        Ok(())
    }
    /// Explicit microphone meter test; it does not publish or play microphone
    /// audio, and it stops on joining a call or invoking stop_microphone_test.
    pub fn start_microphone_test(&mut self) -> Result<mpsc::Receiver<MediaEvent>> {
        if self.room.is_some() {
            return Err(MediaError::InvalidSetting(
                "leave the call before starting a microphone test",
            ));
        }
        self.stop_microphone_test()?;
        let (tx, rx) = mpsc::channel(16);
        self.input_controls
            .lock()
            .map_err(|_| MediaError::Audio("Input processor unavailable".into()))?
            .testing = true;
        let capture_result = microphone::MicrophoneCapture::start(
            self.input_device
                .lock()
                .map_err(|_| MediaError::Audio("Input settings unavailable".into()))?
                .as_deref(),
            self.input_controls.clone(),
            self.mixer.clone(),
            tx,
            self.microphone_publish.channels(),
        );
        let (capture, _unpublished) = match capture_result {
            Ok(capture) => capture,
            Err(error) => {
                if let Ok(mut c) = self.input_controls.lock() {
                    c.testing = false;
                }
                return Err(error);
            }
        };
        self.input_capture = Some(capture);
        Ok(rx)
    }
    pub fn stop_microphone_test(&mut self) -> Result<()> {
        self.cancel_device_recovery();
        let testing = {
            let mut controls = self
                .input_controls
                .lock()
                .map_err(|_| MediaError::Audio("Input processor unavailable".into()))?;
            let testing = controls.testing;
            if testing {
                controls.testing = false;
                controls.blocked = true;
            }
            testing
        };
        if testing && let Some(capture) = self.input_capture.take() {
            capture.stop()?;
        }
        Ok(())
    }
    pub fn is_connected(&self) -> bool {
        self.room
            .as_ref()
            .is_some_and(|room| room.connection_state() != ConnectionState::Disconnected)
    }
    pub fn is_sharing(&self) -> bool {
        self.screen
            .as_ref()
            .is_some_and(|screen| !screen.is_stopped())
    }
    pub fn screen_capture_has_frame(&self) -> bool {
        self.screen
            .as_ref()
            .is_some_and(screen::ScreenSession::has_frame)
    }
    pub fn is_sharing_audio(&self) -> bool {
        self.shared_audio_running.load(Ordering::Acquire)
    }
    pub fn shared_audio_available() -> bool {
        if pipewire_runtime::ensure().is_err() {
            return false;
        }
        fluxer_system_audio::SystemAudio::available()
    }
    pub fn audio_applications() -> Result<Vec<AudioApplication>> {
        pipewire_runtime::ensure()?;
        fluxer_system_audio::SystemAudio::applications()
            .map_err(|e| MediaError::Audio(e.to_string()))
    }
    pub fn configure_shared_audio(&self, settings: SharedAudioSettings) -> Result<()> {
        if !settings.gain.is_finite() || !(0.0..=2.0).contains(&settings.gain) {
            return Err(MediaError::InvalidSetting("shared audio gain must be 0–2"));
        }
        *self
            .shared_audio_settings
            .lock()
            .map_err(|_| MediaError::Audio("shared audio state unavailable".into()))? = settings;
        Ok(())
    }
    /// Publishes an independent screen-audio track. Linux capture reuses the
    /// existing graph router, which excludes this process and its own sinks.
    pub async fn start_shared_audio(&mut self, selection: SharedAudioSelection) -> Result<()> {
        self.stop_shared_audio().await?;
        let room = self.room.as_ref().ok_or(MediaError::NotConnected)?;
        let capture = fluxer_system_audio::SystemAudio::start(selection)
            .map_err(|e| MediaError::Audio(e.to_string()))?;
        use livekit::webrtc::audio_source::{AudioSourceOptions, native::NativeAudioSource};
        let source = NativeAudioSource::new(
            AudioSourceOptions {
                echo_cancellation: false,
                noise_suppression: false,
                auto_gain_control: false,
            },
            48_000,
            2,
            100,
        );
        let track = LocalAudioTrack::create_audio_track(
            "Fluxer shared audio",
            RtcAudioSource::Native(source.clone()),
        );
        let publication = room
            .local_participant()
            .publish_track(LocalTrack::Audio(track), publish::screen_audio_options())
            .await
            .map_err(|e| MediaError::Audio(e.to_string()))?;
        self.shared_audio_publication = Some(publication.sid());
        let stop = self.stop.clone();
        let settings = self.shared_audio_settings.clone();
        let events = self.events.clone();
        let running = Arc::new(AtomicBool::new(true));
        self.shared_audio_running = running.clone();
        let lifecycle = SharedAudioLifecycle {
            running,
            events: events.clone(),
        };
        self.shared_audio_task = Some(tokio::spawn(async move {
            let _lifecycle = lifecycle;
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(10));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            while !stop.load(Ordering::Acquire) {
                interval.tick().await;
                let Some(frame) = capture.read() else {
                    continue;
                };
                if frame.sample_rate != 48_000
                    || frame.channels != 2
                    || !frame.samples.len().is_multiple_of(2)
                {
                    if let Some(tx) = &events {
                        let _ = tx.try_send(MediaEvent::Error(
                            "Shared audio capture returned unsupported PCM format".into(),
                        ));
                    }
                    break;
                }
                if frame.samples.is_empty() {
                    continue;
                }
                let controls = settings.lock().map(|s| *s).unwrap_or(SharedAudioSettings {
                    gain: 0.0,
                    muted: true,
                });
                // Existing desktop only applies stream-audio preference here;
                // local microphone mute/deafen must not silence the broadcast.
                let gain = if controls.muted { 0.0 } else { controls.gain };
                let data: Vec<i16> = frame
                    .samples
                    .iter()
                    .map(|s| float_to_pcm(*s, gain))
                    .collect();
                let samples_per_channel = data.len() as u32 / 2;
                let pcm = livekit::webrtc::audio_frame::AudioFrame {
                    data: std::borrow::Cow::Owned(data),
                    sample_rate: 48_000,
                    num_channels: 2,
                    samples_per_channel,
                };
                if source.capture_frame(&pcm).await.is_err() {
                    if let Some(tx) = &events {
                        let _ =
                            tx.try_send(MediaEvent::Error("Shared audio capture stopped".into()));
                    }
                    break;
                }
            }
            capture.stop();
        }));
        Ok(())
    }
    pub async fn stop_shared_audio(&mut self) -> Result<()> {
        let active = self.shared_audio_task.is_some() || self.shared_audio_publication.is_some();
        self.shared_audio_running.store(false, Ordering::Release);
        if let Some(task) = self.shared_audio_task.take() {
            task.abort();
            let _ = task.await;
        }
        let result =
            if let (Some(room), Some(sid)) = (&self.room, self.shared_audio_publication.take()) {
                room.local_participant()
                    .unpublish_track(&sid)
                    .await
                    .map(|_| ())
                    .map_err(|e| MediaError::Audio(e.to_string()))
            } else {
                Ok(())
            };
        if active {
            shared_audio_stopped(&self.events);
        }
        result
    }
    pub fn screen_sources(&self) -> Result<Vec<ScreenSource>> {
        screen::sources()
    }

    pub async fn start_screen_share(&mut self, options: ScreenShareOptions) -> Result<()> {
        options.validate()?;
        self.stop_screen_share().await?;
        let room = self.room.as_ref().ok_or(MediaError::NotConnected)?;
        let tx = self.events.clone().ok_or(MediaError::NotConnected)?;
        let source = screen::video_source(&options);
        let track = LocalVideoTrack::create_video_track(
            "Fluxer screen",
            livekit::webrtc::video_source::RtcVideoSource::Native(source.clone()),
        );
        let publication = room
            .local_participant()
            .publish_track(LocalTrack::Video(track), options.publish_options())
            .await
            .map_err(|e| MediaError::Screen(e.to_string()))?;
        self.screen_publication = Some(publication.sid());
        match screen::start(options, source, tx, self.stop.clone()) {
            Ok(session) => {
                self.screen = Some(session);
                Ok(())
            }
            Err(e) => {
                self.stop_screen_share().await?;
                Err(e)
            }
        }
    }
    pub async fn stop_screen_share(&mut self) -> Result<()> {
        let had_video = self.screen.is_some() || self.screen_publication.is_some();
        // Stop hardware first even when either network unpublish fails.
        if let Some(mut session) = self.screen.take() {
            session.stop();
        }
        let audio_result = self.stop_shared_audio().await;
        let video_result =
            if let (Some(room), Some(sid)) = (&self.room, self.screen_publication.take()) {
                room.local_participant()
                    .unpublish_track(&sid)
                    .await
                    .map(|_| ())
                    .map_err(|e| MediaError::Screen(e.to_string()))
            } else {
                Ok(())
            };
        if had_video && let Some(tx) = &self.events {
            let _ = tx.try_send(MediaEvent::ScreenShareStopped);
        }
        audio_result.and(video_result)
    }
    pub async fn disconnect(&mut self) -> Result<()> {
        self.cancel_device_recovery();
        // Release capture before network operations, including failed teardown.
        if let Ok(mut registry) = self.subscriptions.lock() {
            registry.clear_publications();
        }
        self.stop.store(true, Ordering::Release);
        self.video_frames.clear();
        if let Ok(mut controls) = self.input_controls.lock() {
            controls.blocked = true;
        }
        if let Some(mic) = self.microphone.take() {
            mic.mute();
            mic.disable();
        }
        let audio_result = if let Some(capture) = self.input_capture.take() {
            capture.stop()
        } else {
            Ok(())
        };
        self.output.stop();
        if let Ok(mut controls) = self.input_controls.lock() {
            controls.testing = false;
            controls.ptt = false;
        }
        if let Ok(mut mixer) = self.mixer.lock() {
            mixer.clear();
            mixer.events = None;
        }
        if let Ok(mut tasks) = self.audio_tasks.lock() {
            for (_, task) in tasks.drain() {
                task.abort();
            }
        }
        if let Some(mut session) = self.screen.take() {
            session.stop();
        }
        self.screen_publication = None;
        self.shared_audio_publication = None;
        self.shared_audio_running.store(false, Ordering::Release);
        if let Some(task) = self.shared_audio_task.take() {
            task.abort();
            let _ = task.await;
        }
        if let Some(task) = self.room_task.take() {
            task.abort();
        }
        if let Ok(mut tasks) = self.video_tasks.lock() {
            for (_, task) in tasks.drain() {
                task.abort();
            }
        }
        if let Ok(mut state) = self.state.lock() {
            state.remotes.clear();
            state.deafened = self.deafened;
        }
        let room_result = if let Some(room) = self.room.take() {
            room.close()
                .await
                .map_err(|e| MediaError::Call(e.to_string()))
        } else {
            Ok(())
        };
        if let Some(tx) = self.events.take() {
            let _ = tx.try_send(MediaEvent::Disconnected);
        }
        audio_result.and(room_result)
    }
}

// Never hold the registry mutex while invoking SDK subscription callbacks.
fn apply_subscription_actions(
    plan: &subscriptions::Plan,
    mixer: &Arc<Mutex<audio::PlaybackMixer>>,
    audio_tasks: &Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
    video_tasks: &Arc<Mutex<HashMap<String, VideoRenderer>>>,
    video_frames: &Arc<video_events::VideoMailbox>,
) {
    // Physical local playback/render demand ends before SDK callbacks can
    // synchronously tear down decryptors or wait on native RTC threads.
    for action in plan
        .actions
        .iter()
        .filter(|action| !action.decision.subscribe)
    {
        let sid = action.publication.sid().to_string();
        if let Ok(mut mixer) = mixer.lock() {
            mixer.tracks.remove(&sid);
        }
        if let Ok(mut tasks) = audio_tasks.lock()
            && let Some(task) = tasks.remove(&sid)
        {
            task.abort();
        }
        end_video_renderer(&sid, video_tasks, mixer, video_frames);
    }
    for action in &plan.actions {
        let publication = &action.publication;
        // Ordinary unwatch is unsubscribe only, matching the existing client.
        // Disabling first would leave SFU TrackSettings sticky across rewatch.
        if action.decision.subscribe && !action.decision.enabled && publication.is_subscribed() {
            publication.set_enabled(false);
        }
        if action.force || publication.is_desired() != action.decision.subscribe {
            publication.set_subscribed(action.decision.subscribe);
        }
        if action.decision.subscribe {
            publication.set_enabled(action.decision.enabled);
        }
    }
}

/// Local renderer lifetime ends immediately when watch intent ends. Do not
/// wait for SFU/SDK unsubscribe notification before releasing the displayed
/// frame. The event carries the removed renderer's epoch so a delayed end
/// cannot clear a newer watch of the same publication.
fn end_video_renderer(
    sid: &str,
    tasks: &Arc<Mutex<HashMap<String, VideoRenderer>>>,
    mixer: &Arc<Mutex<audio::PlaybackMixer>>,
    video_frames: &Arc<video_events::VideoMailbox>,
) {
    video_frames.remove(sid);
    let task = tasks.lock().ok().and_then(|mut tasks| tasks.remove(sid));
    let Some(task) = task else { return };
    task.abort();
    let sender = mixer.lock().ok().and_then(|mixer| mixer.events.clone());
    let Some(sender) = sender else { return };
    let event = MediaEvent::VideoEnded {
        track_id: sid.into(),
        participant: task.participant,
        watch_generation: task.watch_generation,
    };
    if let Err(mpsc::error::TrySendError::Full(event)) = sender.try_send(event) {
        // Lifecycle events must survive a channel temporarily filled by video.
        tokio::spawn(async move {
            let _ = sender.send(event).await;
        });
    }
}

impl Drop for MediaEngine {
    fn drop(&mut self) {
        self.cancel_device_recovery();
        self.stop.store(true, Ordering::Release);
        self.video_frames.clear();
        self.input_capture.take();
        self.output.stop();
        if let Ok(mut mixer) = self.mixer.lock() {
            mixer.clear();
            mixer.events = None;
        }
        if let Ok(mut tasks) = self.audio_tasks.lock() {
            for (_, task) in tasks.drain() {
                task.abort();
            }
        }
        self.shared_audio_running.store(false, Ordering::Release);
        if let Some(task) = self.shared_audio_task.take() {
            task.abort();
        }
        if let Some(mut session) = self.screen.take() {
            session.stop();
        }
        if let Some(mic) = &self.microphone {
            mic.mute();
            mic.disable();
        }
        if let Some(task) = self.room_task.take() {
            task.abort();
        }
        if let Ok(mut tasks) = self.video_tasks.lock() {
            for (_, task) in tasks.drain() {
                task.abort();
            }
        }
    }
}

fn float_to_pcm(sample: f32, gain: f32) -> i16 {
    if !sample.is_finite() {
        return 0;
    }
    ((sample * gain).clamp(-1.0, 1.0) * i16::MAX as f32) as i16
}

#[cfg(test)]
mod audio_tests {
    use super::float_to_pcm;
    #[test]
    fn shared_gain_mute_clipping_and_nonfinite_samples() {
        assert_eq!(float_to_pcm(0.5, 0.0), 0);
        assert_eq!(float_to_pcm(0.5, 2.0), i16::MAX);
        assert_eq!(float_to_pcm(-2.0, 2.0), -i16::MAX);
        assert_eq!(float_to_pcm(f32::NAN, 1.0), 0);
    }
}

#[cfg(test)]
mod video_lifecycle_tests {
    use super::*;
    #[tokio::test]
    async fn local_unwatch_aborts_renderer_and_ends_frame_once_without_sdk_event() {
        let (sender, mut events) = mpsc::channel(4);
        let mixer = Arc::new(Mutex::new(audio::PlaybackMixer::new()));
        mixer.lock().unwrap().events = Some(sender);
        let task = tokio::spawn(std::future::pending());
        let abort = task.abort_handle();
        let tasks = Arc::new(Mutex::new(HashMap::from([(
            "screen-sid".into(),
            VideoRenderer {
                task,
                participant: "alice".into(),
                watch_generation: 1,
            },
        )])));
        end_video_renderer(
            "screen-sid",
            &tasks,
            &mixer,
            &Arc::new(video_events::VideoMailbox::new(Arc::new(Mutex::new(
                subscriptions::Registry::default(),
            )))),
        );
        assert!(tasks.lock().unwrap().is_empty());
        assert!(
            matches!(events.recv().await,Some(MediaEvent::VideoEnded {track_id,..}) if track_id=="screen-sid")
        );
        end_video_renderer(
            "screen-sid",
            &tasks,
            &mixer,
            &Arc::new(video_events::VideoMailbox::new(Arc::new(Mutex::new(
                subscriptions::Registry::default(),
            )))),
        );
        assert!(
            events.try_recv().is_err(),
            "late SDK unsubscribe cannot duplicate local frame end"
        );
        tokio::task::yield_now().await;
        assert!(abort.is_finished());
    }
    #[tokio::test]
    async fn video_end_survives_full_frame_channel() {
        let (sender, mut events) = mpsc::channel(1);
        sender.send(MediaEvent::Speakers(vec![])).await.unwrap();
        let mixer = Arc::new(Mutex::new(audio::PlaybackMixer::new()));
        mixer.lock().unwrap().events = Some(sender);
        let task = tokio::spawn(std::future::pending());
        let tasks = Arc::new(Mutex::new(HashMap::from([(
            "screen-sid".into(),
            VideoRenderer {
                task,
                participant: "alice".into(),
                watch_generation: 1,
            },
        )])));
        end_video_renderer(
            "screen-sid",
            &tasks,
            &mixer,
            &Arc::new(video_events::VideoMailbox::new(Arc::new(Mutex::new(
                subscriptions::Registry::default(),
            )))),
        );
        assert!(matches!(events.recv().await, Some(MediaEvent::Speakers(_))));
        assert!(
            matches!(events.recv().await,Some(MediaEvent::VideoEnded {track_id,..}) if track_id=="screen-sid")
        );
    }
}

#[cfg(test)]
mod shared_audio_lifecycle_tests {
    use super::*;
    #[tokio::test]
    async fn unexpected_capture_exit_stops_state_before_notifying_controller() {
        let (sender, mut events) = mpsc::channel(4);
        let running = Arc::new(AtomicBool::new(true));
        let lifecycle = SharedAudioLifecycle {
            running: running.clone(),
            events: Some(sender),
        };
        drop(lifecycle);
        assert!(!running.load(Ordering::Acquire));
        assert!(matches!(
            events.recv().await,
            Some(MediaEvent::SharedAudioStopped)
        ));
    }
    #[tokio::test]
    async fn old_capture_exit_cannot_stop_replacement_or_duplicate_explicit_stop() {
        let (sender, mut events) = mpsc::channel(4);
        let old = Arc::new(AtomicBool::new(true));
        let current = Arc::new(AtomicBool::new(true));
        drop(SharedAudioLifecycle {
            running: old.clone(),
            events: Some(sender.clone()),
        });
        assert!(!old.load(Ordering::Acquire));
        assert!(current.load(Ordering::Acquire));
        assert!(matches!(
            events.recv().await,
            Some(MediaEvent::SharedAudioStopped)
        ));
        current.store(false, Ordering::Release);
        drop(SharedAudioLifecycle {
            running: current,
            events: Some(sender),
        });
        assert!(
            events.try_recv().is_err(),
            "explicit stop owns its single notification"
        );
    }
}

#[cfg(test)]
mod device_recovery_tests {
    use super::*;

    fn device(id: &str) -> AudioDevice {
        AudioDevice {
            id: id.into(),
            label: id.into(),
        }
    }

    #[test]
    fn missing_preference_falls_back_and_returns_when_inventory_recovers() {
        let selected = DeviceSelection {
            preferred: "input:usb".into(),
            effective: Some("input:usb".into()),
        };
        let removed = vec![device("input:default")];
        assert_eq!(
            desired_device(&selected, &removed, "input:default").as_deref(),
            Some("input:default")
        );
        let restored = vec![device("input:default"), device("input:usb")];
        assert_eq!(
            desired_device(&selected, &restored, "input:default").as_deref(),
            Some("input:usb")
        );
        assert_eq!(selected.preferred, "input:usb");
        assert_eq!(desired_device(&selected, &[], "input:default"), None);
    }
}
