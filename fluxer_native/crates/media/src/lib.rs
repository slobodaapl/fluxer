// SPDX-License-Identifier: AGPL-3.0-or-later
//! Native media transport. Call credentials come from Fluxer's gateway, never
//! from an embedded LiveKit API secret. Capture begins only when joining/sharing.

mod audio;
mod microphone;
mod playback;
mod publish;
mod screen;
mod subscriptions;

pub use audio::{AudioMixSettings, InputMode, ParticipantAudioSettings};
pub use publish::resolve_voice_channel_bitrate;

use futures_util::StreamExt;
use livekit::webrtc::{
    audio_stream::native::{NativeAudioStream, NativeAudioStreamOptions},
    peer_connection_factory::native::PeerConnectionFactoryExt,
    video_frame::VideoFormatType,
    video_stream::native::NativeVideoStream,
};
use livekit::{AudioProcessingOptions, options::TrackPublishOptions, prelude::*};
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

/// PlatformAudio::stop_recording forwards directly to the ADM. Some native
/// backends reject StopRecording after the room has already released recording.
/// Match PlatformAudio::switch_recording_device's initialized-state guard so
/// fresh/idle setup and repeated teardown are valid. Genuine stop failures for
/// initialized recording still propagate to callers.
fn stop_recording_if_initialized(audio: &PlatformAudio) -> livekit::AudioResult<()> {
    if audio.is_recording_initialized() {
        audio.stop_recording()
    } else {
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioDevice {
    pub id: String,
    pub label: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DeviceInventory {
    pub inputs: Vec<AudioDevice>,
    pub outputs: Vec<AudioDevice>,
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

/// RGBA pixels. The receiving UI should retain only the latest frame per track.
#[derive(Clone, Debug)]
pub struct VideoFrame {
    pub track_id: String,
    pub participant: String,
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<Vec<u8>>,
}

#[derive(Clone, Debug)]
pub enum MediaEvent {
    Connected,
    Disconnected,
    Reconnecting,
    ParticipantJoined { identity: String, name: String },
    ParticipantLeft { identity: String },
    ScreenStreamAvailable { identity: String, available: bool },
    Speakers(Vec<String>),
    Video(VideoFrame),
    VideoEnded { track_id: String },
    ScreenShareStarted,
    ScreenShareStopped,
    AudioLevels { input: f32, output: f32 },
    Error(String),
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
    audio: PlatformAudio,
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
    video_tasks: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
    stop: Arc<AtomicBool>,
    shared_audio_task: Option<JoinHandle<()>>,
    shared_audio_publication: Option<TrackSid>,
    shared_audio_settings: Arc<Mutex<SharedAudioSettings>>,
    mixer: Arc<Mutex<audio::PlaybackMixer>>,
    output: playback::PlaybackOutput,
    input_controls: Arc<Mutex<microphone::InputRuntime>>,
    input_capture: Option<microphone::MicrophoneCapture>,
    audio_tasks: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
    subscriptions: Arc<Mutex<subscriptions::Registry>>,
}

impl MediaEngine {
    pub fn new() -> Result<Self> {
        let audio = PlatformAudio::new().map_err(|e| MediaError::Audio(e.to_string()))?;
        // Creating/enumerating the device manager does not grant permission to
        // transmit. Recording remains stopped until an unmuted call is joined.
        stop_recording_if_initialized(&audio).map_err(|e| MediaError::Audio(e.to_string()))?;
        let mixer = Arc::new(Mutex::new(audio::PlaybackMixer::new()));
        let output = playback::PlaybackOutput::new(mixer.clone());
        Ok(Self {
            audio,
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
            stop: Arc::new(AtomicBool::new(false)),
            shared_audio_task: None,
            shared_audio_publication: None,
            shared_audio_settings: Arc::new(Mutex::new(SharedAudioSettings::default())),
            mixer,
            output,
            input_controls: Arc::new(Mutex::new(microphone::InputRuntime::default())),
            input_capture: None,
            audio_tasks: Arc::new(Mutex::new(HashMap::new())),
            subscriptions: Arc::new(Mutex::new(subscriptions::Registry::default())),
        })
    }

    pub fn devices(&self) -> DeviceInventory {
        DeviceInventory {
            inputs: self
                .audio
                .recording_devices()
                .map(|d| AudioDevice {
                    id: d.id.as_str().into(),
                    label: d.name,
                })
                .collect(),
            outputs: playback::devices(),
        }
    }

    pub fn set_input_device(&self, id: &str) -> Result<()> {
        let device = self
            .audio
            .recording_devices()
            .find(|d| d.id.as_str() == id)
            .ok_or(MediaError::UnknownDevice("input"))?;
        let testing = self
            .input_controls
            .lock()
            .map(|c| c.testing)
            .unwrap_or(false);
        // switch_recording_device restarts any initialized ADM even after mute.
        // Explicit stop/set/start avoids briefly recording in muted/settings-only
        // flows and leaves the device stopped if selecting a replacement fails.
        stop_recording_if_initialized(&self.audio).map_err(|e| MediaError::Audio(e.to_string()))?;
        self.audio
            .set_recording_device(&device.id)
            .map_err(|e| MediaError::Audio(e.to_string()))?;
        if testing || (!self.muted && !self.deafened && self.is_connected()) {
            self.audio
                .start_recording()
                .map_err(|e| MediaError::Audio(e.to_string()))?;
        }
        Ok(())
    }

    pub fn set_output_device(&self, id: &str) -> Result<()> {
        self.output.set_device(id)
    }

    pub fn configure(&self, settings: AudioSettings) -> Result<()> {
        self.audio
            .configure_audio_processing(AudioProcessingOptions {
                echo_cancellation: settings.echo_cancellation,
                noise_suppression: settings.noise_suppression,
                auto_gain_control: settings.auto_gain_control,
                prefer_hardware_processing: false,
            })
            .map_err(|e| MediaError::Audio(e.to_string()))
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

    pub async fn connect(
        &mut self,
        endpoint: &str,
        token: &str,
    ) -> Result<mpsc::Receiver<MediaEvent>> {
        self.connect_encrypted(endpoint, token, None).await
    }

    /// A supplied shared E2EE passphrase uses LiveKit's PBKDF2 key provider,
    /// matching ExternalE2EEKeyProvider.setKey(string) in the existing client.
    pub async fn connect_encrypted(
        &mut self,
        endpoint: &str,
        token: &str,
        shared_key: Option<&str>,
    ) -> Result<mpsc::Receiver<MediaEvent>> {
        self.disconnect().await?;
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
        // The public runtime factory owns the same ADM as PlatformAudio.
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
        let (capture, mic) = microphone::MicrophoneCapture::start(
            &self.audio,
            self.input_controls.clone(),
            self.mixer.clone(),
            tx.clone(),
            self.microphone_publish.channels(),
        );
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
            drop(capture);
            self.output.stop();
            let _ = stop_recording_if_initialized(&self.audio);
            let _ = room.close().await;
            return Err(MediaError::Call(error.to_string()));
        }
        let recording = if self.muted || self.deafened {
            stop_recording_if_initialized(&self.audio)
        } else {
            self.audio.start_recording()
        };
        if let Err(error) = recording {
            drop(capture);
            mic.mute();
            mic.disable();
            let _ = stop_recording_if_initialized(&self.audio);
            self.output.stop();
            let _ = room.close().await;
            return Err(MediaError::Audio(error.to_string()));
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
        let audio = self.audio.clone();
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
                            apply_subscription_actions(&plan, &mixer, &audio_tasks, &video_tasks);
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
                            apply_subscription_actions(&plan, &mixer, &audio_tasks, &video_tasks);
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
                            apply_subscription_actions(&plan, &mixer, &audio_tasks, &video_tasks);
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
                            apply_subscription_actions(&plan, &mixer, &audio_tasks, &video_tasks);
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
                            apply_subscription_actions(&plan, &mixer, &audio_tasks, &video_tasks);
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
                        if let Some(mic) = &microphone {
                            mic.mute();
                            mic.disable();
                        }
                        let _ = stop_recording_if_initialized(&audio);
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
                    RoomEvent::TrackSubscribed {
                        track: RemoteTrack::Audio(track),
                        participant,
                        publication,
                    } => {
                        let desired = subscriptions
                            .lock()
                            .map(|registry| {
                                registry.decision(&participant.identity().to_string(), &publication)
                            })
                            .unwrap_or(subscriptions::Decision {
                                subscribe: false,
                                enabled: false,
                            });
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
                            m.add_track(id.clone(), identity);
                        }
                        let frame_id = id.clone();
                        let track_mixer = mixer.clone();
                        let track_subscriptions = subscriptions.clone();
                        let task = tokio::spawn(async move {
                            while let Some(frame) = stream.next().await {
                                if !track_subscriptions
                                    .lock()
                                    .is_ok_and(|registry| registry.desired_track(&frame_id))
                                {
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
                        let desired = subscriptions
                            .lock()
                            .map(|registry| {
                                registry.decision(&participant.identity().to_string(), &publication)
                            })
                            .unwrap_or(subscriptions::Decision {
                                subscribe: false,
                                enabled: false,
                            });
                        if !desired.subscribe {
                            track.disable();
                            publication.set_subscribed(false);
                            continue;
                        }
                        publication.set_enabled(desired.enabled);
                        let id = track.sid().to_string();
                        let identity = participant.identity().to_string();
                        let mut stream = NativeVideoStream::new(track.rtc_track());
                        let sender = tx.clone();
                        let frame_id = id.clone();
                        let track_subscriptions = subscriptions.clone();
                        let task = tokio::spawn(async move {
                            while let Some(frame) = stream.next().await {
                                if !track_subscriptions
                                    .lock()
                                    .is_ok_and(|registry| registry.desired_track(&frame_id))
                                {
                                    break;
                                }
                                let w = frame.buffer.width();
                                let h = frame.buffer.height();
                                if w == 0 || h == 0 || w > 8192 || h > 8192 {
                                    continue;
                                }
                                let mut rgba = vec![0; w as usize * h as usize * 4];
                                frame.buffer.to_argb(
                                    VideoFormatType::ABGR,
                                    &mut rgba,
                                    w * 4,
                                    w as i32,
                                    h as i32,
                                );
                                // Bounded channel: drop frames when UI is slow rather
                                // than accumulating unbounded video latency/memory.
                                let _ = sender.try_send(MediaEvent::Video(VideoFrame {
                                    track_id: frame_id.clone(),
                                    participant: identity.clone(),
                                    width: w,
                                    height: h,
                                    rgba: Arc::new(rgba),
                                }));
                            }
                            stream.close();
                        });
                        if let Ok(mut tasks) = video_tasks.lock()
                            && let Some(old) = tasks.insert(id, task)
                        {
                            old.abort();
                        }
                        None
                    }
                    RoomEvent::TrackUnsubscribed {
                        track: RemoteTrack::Video(track),
                        ..
                    } => {
                        let id = track.sid().to_string();
                        if let Ok(mut tasks) = video_tasks.lock()
                            && let Some(task) = tasks.remove(&id)
                        {
                            task.abort();
                        }
                        Some(MediaEvent::VideoEnded { track_id: id })
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
            if let Some(mic) = &microphone {
                mic.mute();
                mic.disable();
            }
            let _ = stop_recording_if_initialized(&audio);
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
        Ok(rx)
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
            registry.policy.watched = identities.into_iter().filter(|id| !id.is_empty()).collect();
            registry.plan()
        };
        apply_subscription_actions(&plan, &self.mixer, &self.audio_tasks, &self.video_tasks);
        Ok(())
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
        apply_subscription_actions(&plan, &self.mixer, &self.audio_tasks, &self.video_tasks);
        if let Ok(mut state) = self.state.lock() {
            state.deafened = deafened;
        }
        if let Ok(mut mixer) = self.mixer.lock() {
            mixer.deafened = deafened;
        }
        self.apply_microphone_state()
    }
    fn apply_microphone_state(&self) -> Result<()> {
        if let Ok(mut controls) = self.input_controls.lock() {
            controls.blocked = self.muted || self.deafened;
        }
        if let Some(mic) = &self.microphone {
            if self.muted || self.deafened {
                mic.mute();
                mic.disable();
                stop_recording_if_initialized(&self.audio)
                    .map_err(|e| MediaError::Audio(e.to_string()))?;
            } else {
                self.audio
                    .start_recording()
                    .map_err(|e| MediaError::Audio(e.to_string()))?;
                mic.enable();
                mic.unmute();
            }
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
        let (capture, _unpublished) = microphone::MicrophoneCapture::start(
            &self.audio,
            self.input_controls.clone(),
            self.mixer.clone(),
            tx,
            self.microphone_publish.channels(),
        );
        if let Err(error) = self.audio.start_recording() {
            drop(capture);
            if let Ok(mut c) = self.input_controls.lock() {
                c.testing = false;
            }
            return Err(MediaError::Audio(error.to_string()));
        }
        self.input_capture = Some(capture);
        Ok(rx)
    }
    pub fn stop_microphone_test(&mut self) -> Result<()> {
        let mut controls = self
            .input_controls
            .lock()
            .map_err(|_| MediaError::Audio("Input processor unavailable".into()))?;
        if controls.testing {
            controls.testing = false;
            self.input_capture.take();
            stop_recording_if_initialized(&self.audio)
                .map_err(|e| MediaError::Audio(e.to_string()))?;
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
    pub fn shared_audio_available() -> bool {
        fluxer_system_audio::SystemAudio::available()
    }
    pub fn audio_applications() -> Result<Vec<AudioApplication>> {
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
        let state = self.state.clone();
        let events = self.events.clone();
        self.shared_audio_task = Some(tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(10));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            while !stop.load(Ordering::Acquire) {
                interval.tick().await;
                let Some(frame) = capture.read() else {
                    continue;
                };
                if frame.sample_rate != 48_000
                    || frame.channels != 2
                    || frame.samples.len() % 2 != 0
                {
                    continue;
                }
                let controls = settings.lock().map(|s| *s).unwrap_or(SharedAudioSettings {
                    gain: 0.0,
                    muted: true,
                });
                let deaf = state.lock().map(|s| s.deafened).unwrap_or(true);
                let gain = if controls.muted || deaf {
                    0.0
                } else {
                    controls.gain
                };
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
        if let Some(task) = self.shared_audio_task.take() {
            task.abort();
            let _ = task.await;
        }
        if let (Some(room), Some(sid)) = (&self.room, self.shared_audio_publication.take()) {
            room.local_participant()
                .unpublish_track(&sid)
                .await
                .map_err(|e| MediaError::Audio(e.to_string()))?;
        }
        Ok(())
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
            .publish_track(
                LocalTrack::Video(track),
                TrackPublishOptions {
                    source: TrackSource::Screenshare,
                    video_encoding: Some(livekit::options::VideoEncoding {
                        max_bitrate: options.max_bitrate,
                        max_framerate: options.fps as f64,
                    }),
                    ..Default::default()
                },
            )
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
        // Release capture before network operations, including failed teardown.
        if let Ok(mut registry) = self.subscriptions.lock() {
            registry.clear_publications();
        }
        self.stop.store(true, Ordering::Release);
        self.input_capture.take();
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
        if let Some(task) = self.shared_audio_task.take() {
            task.abort();
            let _ = task.await;
        }
        if let Some(mic) = self.microphone.take() {
            mic.mute();
            mic.disable();
        }
        let audio_result = stop_recording_if_initialized(&self.audio)
            .map_err(|e| MediaError::Audio(e.to_string()));
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
    video_tasks: &Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
) {
    for action in &plan.actions {
        let publication = &action.publication;
        if !action.decision.enabled && publication.is_subscribed() {
            publication.set_enabled(false);
        }
        if action.force || publication.is_desired() != action.decision.subscribe {
            publication.set_subscribed(action.decision.subscribe);
        }
        if action.decision.subscribe {
            publication.set_enabled(action.decision.enabled);
        } else {
            let sid = publication.sid().to_string();
            if let Ok(mut mixer) = mixer.lock() {
                mixer.tracks.remove(&sid);
            }
            if let Ok(mut tasks) = audio_tasks.lock()
                && let Some(task) = tasks.remove(&sid)
            {
                task.abort();
            }
            if let Ok(mut tasks) = video_tasks.lock()
                && let Some(task) = tasks.remove(&sid)
            {
                task.abort();
            }
        }
    }
}

impl Drop for MediaEngine {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
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
        let _ = stop_recording_if_initialized(&self.audio);
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
