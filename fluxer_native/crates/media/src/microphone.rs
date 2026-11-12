// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tap WebRTC's processed device source before publication, retaining native
//! routing/AEC/NS/AGC while applying application gain and activation to PCM.
use crate::{
    MediaEvent,
    audio::{AudioMixSettings, InputProcessor, PlaybackMixer},
};
use futures_util::StreamExt;
use livekit::webrtc::{
    audio_source::{AudioSourceOptions, RtcAudioSource, native::NativeAudioSource},
    audio_stream::native::{NativeAudioStream, NativeAudioStreamOptions},
};
use livekit::{PlatformAudio, prelude::*};
use std::sync::{Arc, Mutex};
use tokio::{sync::mpsc, task::JoinHandle};

#[derive(Default)]
pub(crate) struct InputRuntime {
    pub settings: AudioMixSettings,
    pub blocked: bool,
    pub ptt: bool,
    pub testing: bool,
}
pub(crate) struct MicrophoneCapture {
    device_track: LocalAudioTrack,
    task: JoinHandle<()>,
}
impl MicrophoneCapture {
    pub fn start(
        audio: &PlatformAudio,
        controls: Arc<Mutex<InputRuntime>>,
        mixer: Arc<Mutex<PlaybackMixer>>,
        events: mpsc::Sender<MediaEvent>,
    ) -> (Self, LocalAudioTrack) {
        let device_track = LocalAudioTrack::create_audio_track(
            "Fluxer processed device input",
            audio.rtc_source(),
        );
        Self::start_track(device_track, controls, mixer, events)
    }
    fn start_track(
        device_track: LocalAudioTrack,
        controls: Arc<Mutex<InputRuntime>>,
        mixer: Arc<Mutex<PlaybackMixer>>,
        events: mpsc::Sender<MediaEvent>,
    ) -> (Self, LocalAudioTrack) {
        let mut stream = NativeAudioStream::with_options(
            device_track.rtc_track(),
            48_000,
            1,
            NativeAudioStreamOptions {
                queue_size_frames: Some(4),
            },
        );
        let source = NativeAudioSource::new(
            AudioSourceOptions {
                echo_cancellation: false,
                noise_suppression: false,
                auto_gain_control: false,
            },
            48_000,
            1,
            0,
        );
        let published = LocalAudioTrack::create_audio_track(
            "Fluxer microphone",
            RtcAudioSource::Native(source.clone()),
        );
        let task = tokio::spawn(async move {
            let mut processor = InputProcessor::default();
            let mut frames = 0_u32;
            while let Some(mut frame) = stream.next().await {
                let (settings, blocked, ptt, testing) = match controls.lock() {
                    Ok(c) => (c.settings.clone(), c.blocked, c.ptt, c.testing),
                    Err(_) => break,
                };
                let peak = processor.process(
                    frame.data.to_mut(),
                    frame.sample_rate,
                    frame.num_channels,
                    &settings,
                    blocked && !testing,
                    ptt,
                );
                frames += 1;
                if frames.is_multiple_of(10) {
                    let output = mixer
                        .lock()
                        .map(|mut m| {
                            let peak = m.peak;
                            m.peak = 0.0;
                            peak
                        })
                        .unwrap_or(0.0);
                    let _ = events.try_send(MediaEvent::AudioLevels {
                        input: peak,
                        output,
                    });
                }
                if !testing && let Err(error) = source.capture_frame(&frame).await {
                    let _ = events.try_send(MediaEvent::Error(format!(
                        "Microphone processing failed: {error}"
                    )));
                    break;
                }
            }
            stream.close();
        });
        (Self { device_track, task }, published)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InputMode;
    use livekit::webrtc::audio_frame::AudioFrame;
    #[tokio::test]
    async fn rtc_tap_pipeline_changes_forwarded_gain_and_ptt_pcm() {
        let input = NativeAudioSource::new(AudioSourceOptions::default(), 48_000, 1, 0);
        let track = LocalAudioTrack::create_audio_track(
            "synthetic mic",
            RtcAudioSource::Native(input.clone()),
        );
        let controls = Arc::new(Mutex::new(InputRuntime {
            settings: AudioMixSettings {
                input_gain: 0.5,
                ..AudioMixSettings::default()
            },
            ..InputRuntime::default()
        }));
        let (events, _rx) = mpsc::channel(16);
        let (capture, processed) = MicrophoneCapture::start_track(
            track,
            controls.clone(),
            Arc::new(Mutex::new(PlaybackMixer::new())),
            events,
        );
        let mut output = NativeAudioStream::new(processed.rtc_track(), 48_000, 1);
        let frame = AudioFrame {
            data: vec![16_384; 480].into(),
            sample_rate: 48_000,
            num_channels: 1,
            samples_per_channel: 480,
        };
        input.capture_frame(&frame).await.unwrap();
        let actual = tokio::time::timeout(std::time::Duration::from_secs(2), output.next())
            .await
            .unwrap()
            .unwrap();
        assert!(
            actual
                .data
                .iter()
                .all(|sample| (8191..=8192).contains(sample))
        );
        controls.lock().unwrap().settings.input_mode = InputMode::PushToTalk;
        input.capture_frame(&frame).await.unwrap();
        let actual = tokio::time::timeout(std::time::Duration::from_secs(2), output.next())
            .await
            .unwrap()
            .unwrap();
        assert!(actual.data.iter().all(|sample| *sample == 0));
        controls.lock().unwrap().ptt = true;
        input.capture_frame(&frame).await.unwrap();
        let actual = tokio::time::timeout(std::time::Duration::from_secs(2), output.next())
            .await
            .unwrap()
            .unwrap();
        assert!(actual.data.iter().any(|sample| *sample > 0));
        drop(capture);
        output.close();
    }
}
impl Drop for MicrophoneCapture {
    fn drop(&mut self) {
        self.device_track.disable();
        self.task.abort();
    }
}
