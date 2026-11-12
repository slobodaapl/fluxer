// SPDX-License-Identifier: AGPL-3.0-or-later
//! Native media transport. Call credentials come from Fluxer's gateway, never
//! from an embedded LiveKit API secret. Capture begins only when joining/sharing.

mod screen;

use futures_util::StreamExt;
use livekit::webrtc::{video_frame::VideoFormatType, video_stream::native::NativeVideoStream};
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
    Speakers(Vec<String>),
    Video(VideoFrame),
    VideoEnded { track_id: String },
    ScreenShareStarted,
    ScreenShareStopped,
    Error(String),
}

struct RuntimeState {
    deafened: bool,
    remotes: HashMap<String, RemoteAudioTrack>,
}

/// Owned by the asynchronous media worker; all native handles stay off GPUI's
/// rendering thread. Drop aborts capture and tasks even on application shutdown.
pub struct MediaEngine {
    audio: PlatformAudio,
    room: Option<Room>,
    microphone: Option<LocalAudioTrack>,
    muted: bool,
    deafened: bool,
    state: Arc<Mutex<RuntimeState>>,
    events: Option<mpsc::Sender<MediaEvent>>,
    room_task: Option<JoinHandle<()>>,
    screen: Option<screen::ScreenSession>,
    screen_publication: Option<TrackSid>,
    video_tasks: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
    stop: Arc<AtomicBool>,
}

impl MediaEngine {
    pub fn new() -> Result<Self> {
        let audio = PlatformAudio::new().map_err(|e| MediaError::Audio(e.to_string()))?;
        // Creating/enumerating the device manager does not grant permission to
        // transmit. Recording remains stopped until an unmuted call is joined.
        audio
            .stop_recording()
            .map_err(|e| MediaError::Audio(e.to_string()))?;
        Ok(Self {
            audio,
            room: None,
            microphone: None,
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
            outputs: self
                .audio
                .playout_devices()
                .map(|d| AudioDevice {
                    id: d.id.as_str().into(),
                    label: d.name,
                })
                .collect(),
        }
    }

    pub fn set_input_device(&self, id: &str) -> Result<()> {
        let device = self
            .audio
            .recording_devices()
            .find(|d| d.id.as_str() == id)
            .ok_or(MediaError::UnknownDevice("input"))?;
        self.audio
            .switch_recording_device(&device.id)
            .map_err(|e| MediaError::Audio(e.to_string()))?;
        if self.muted || self.deafened || self.room.is_none() {
            self.audio
                .stop_recording()
                .map_err(|e| MediaError::Audio(e.to_string()))?;
        }
        Ok(())
    }

    pub fn set_output_device(&self, id: &str) -> Result<()> {
        let device = self
            .audio
            .playout_devices()
            .find(|d| d.id.as_str() == id)
            .ok_or(MediaError::UnknownDevice("output"))?;
        self.audio
            .switch_playout_device(&device.id)
            .map_err(|e| MediaError::Audio(e.to_string()))
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
        if let Some(key) = shared_key {
            if key.is_empty() {
                return Err(MediaError::InvalidSetting("empty E2EE key"));
            }
            use livekit::e2ee::{
                E2eeOptions, EncryptionType,
                key_provider::{KeyProvider, KeyProviderOptions},
            };
            options.encryption = Some(E2eeOptions {
                encryption_type: EncryptionType::Gcm,
                key_provider: KeyProvider::with_shared_key(
                    KeyProviderOptions::default(),
                    key.as_bytes().to_vec(),
                ),
            });
        }
        let (room, mut room_events) = Room::connect(endpoint, token, options)
            .await
            .map_err(|_| MediaError::Call("Unable to connect to the voice server".into()))?;
        let mic = LocalAudioTrack::create_audio_track("Fluxer microphone", self.audio.rtc_source());
        if self.muted || self.deafened {
            mic.mute();
            mic.disable();
        }
        if let Err(error) = room
            .local_participant()
            .publish_track(
                LocalTrack::Audio(mic.clone()),
                TrackPublishOptions {
                    source: TrackSource::Microphone,
                    ..Default::default()
                },
            )
            .await
        {
            let _ = self.audio.stop_recording();
            let _ = room.close().await;
            return Err(MediaError::Call(error.to_string()));
        }
        if self.muted || self.deafened {
            self.audio
                .stop_recording()
                .map_err(|e| MediaError::Audio(e.to_string()))?;
        } else if let Err(error) = self.audio.start_recording() {
            let _ = room.close().await;
            return Err(MediaError::Audio(error.to_string()));
        }
        self.room = Some(room);
        self.microphone = Some(mic);
        self.stop.store(false, Ordering::Release);
        let (tx, rx) = mpsc::channel(32);
        self.events = Some(tx.clone());
        let state = self.state.clone();
        let stop = self.stop.clone();
        let video_tasks = self.video_tasks.clone();
        let audio = self.audio.clone();
        let _ = tx.try_send(MediaEvent::Connected);
        self.room_task = Some(tokio::spawn(async move {
            while let Some(event) = room_events.recv().await {
                if stop.load(Ordering::Acquire) {
                    break;
                }
                let outgoing = match event {
                    RoomEvent::ParticipantConnected(p) => Some(MediaEvent::ParticipantJoined {
                        identity: p.identity().to_string(),
                        name: p.name(),
                    }),
                    RoomEvent::ParticipantDisconnected(p) => Some(MediaEvent::ParticipantLeft {
                        identity: p.identity().to_string(),
                    }),
                    RoomEvent::ActiveSpeakersChanged { speakers } => Some(MediaEvent::Speakers(
                        speakers.iter().map(|p| p.identity().to_string()).collect(),
                    )),
                    RoomEvent::Reconnecting => Some(MediaEvent::Reconnecting),
                    RoomEvent::Reconnected => Some(MediaEvent::Connected),
                    RoomEvent::Disconnected { .. } => {
                        let _ = audio.stop_recording();
                        Some(MediaEvent::Disconnected)
                    }
                    RoomEvent::TrackSubscribed {
                        track: RemoteTrack::Audio(track),
                        ..
                    } => {
                        if let Ok(mut s) = state.lock() {
                            if s.deafened {
                                track.disable();
                            }
                            s.remotes.insert(track.sid().to_string(), track);
                        }
                        None
                    }
                    RoomEvent::TrackUnsubscribed {
                        track: RemoteTrack::Audio(track),
                        ..
                    } => {
                        if let Ok(mut s) = state.lock() {
                            s.remotes.remove(&track.sid().to_string());
                        }
                        None
                    }
                    RoomEvent::TrackSubscribed {
                        track: RemoteTrack::Video(track),
                        participant,
                        ..
                    } => {
                        let id = track.sid().to_string();
                        let identity = participant.identity().to_string();
                        let mut stream = NativeVideoStream::new(track.rtc_track());
                        let sender = tx.clone();
                        let frame_id = id.clone();
                        let task = tokio::spawn(async move {
                            while let Some(frame) = stream.next().await {
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
                        if let Ok(mut tasks) = video_tasks.lock() {
                            if let Some(old) = tasks.insert(id, task) {
                                old.abort();
                            }
                        }
                        None
                    }
                    RoomEvent::TrackUnsubscribed {
                        track: RemoteTrack::Video(track),
                        ..
                    } => {
                        let id = track.sid().to_string();
                        if let Ok(mut tasks) = video_tasks.lock() {
                            if let Some(task) = tasks.remove(&id) {
                                task.abort();
                            }
                        }
                        Some(MediaEvent::VideoEnded { track_id: id })
                    }
                    _ => None,
                };
                if let Some(event) = outgoing {
                    if tx.send(event).await.is_err() {
                        break;
                    }
                }
            }
        }));
        Ok(rx)
    }

    pub fn set_muted(&mut self, muted: bool) -> Result<()> {
        self.muted = muted;
        self.apply_microphone_state()
    }
    pub fn set_deafened(&mut self, deafened: bool) -> Result<()> {
        self.deafened = deafened;
        if let Ok(mut state) = self.state.lock() {
            state.deafened = deafened;
            for track in state.remotes.values() {
                if deafened {
                    track.disable();
                } else {
                    track.enable();
                }
            }
        }
        self.apply_microphone_state()
    }
    fn apply_microphone_state(&self) -> Result<()> {
        if let Some(mic) = &self.microphone {
            if self.muted || self.deafened {
                mic.mute();
                mic.disable();
                self.audio
                    .stop_recording()
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
    pub fn is_connected(&self) -> bool {
        self.room.is_some()
    }
    pub fn is_sharing(&self) -> bool {
        self.screen.is_some()
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
        match screen::start(options, source, tx) {
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
        if let Some(mut session) = self.screen.take() {
            session.stop();
        }
        if let (Some(room), Some(sid)) = (&self.room, self.screen_publication.take()) {
            room.local_participant()
                .unpublish_track(&sid)
                .await
                .map_err(|e| MediaError::Screen(e.to_string()))?;
        }
        if let Some(tx) = &self.events {
            let _ = tx.try_send(MediaEvent::ScreenShareStopped);
        }
        Ok(())
    }
    pub async fn disconnect(&mut self) -> Result<()> {
        // Release capture before network operations, including failed teardown.
        self.stop.store(true, Ordering::Release);
        if let Some(mut session) = self.screen.take() {
            session.stop();
        }
        self.screen_publication = None;
        if let Some(mic) = self.microphone.take() {
            mic.mute();
            mic.disable();
        }
        let audio_result = self
            .audio
            .stop_recording()
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

impl Drop for MediaEngine {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(mut session) = self.screen.take() {
            session.stop();
        }
        if let Some(mic) = &self.microphone {
            mic.mute();
            mic.disable();
        }
        let _ = self.audio.stop_recording();
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
