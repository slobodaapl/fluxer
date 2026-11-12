// SPDX-License-Identifier: AGPL-3.0-or-later
//! Physical PCM -> WebRTC APM -> application gain/gate -> published native source.
//!
//! A Device audio track cannot be tapped: WebRTC pc/local_audio_source.h's
//! LocalAudioSource::AddSink is explicitly a no-op. NativeAudioStream on that
//! track therefore never produces PCM. Use physical capture, then the SDK's
//! standalone APM, rather than silently publishing an empty derived source.
use crate::{
    AudioSettings, MediaEvent, Result,
    audio::{AudioMixSettings, InputProcessor, PlaybackMixer},
};
use livekit::prelude::*;
use livekit::webrtc::{
    audio_frame::AudioFrame,
    audio_source::{AudioSourceOptions, RtcAudioSource, native::NativeAudioSource},
    native::{apm::AudioProcessingModule, audio_resampler::AudioResampler},
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tokio::{sync::mpsc, task::JoinHandle};

#[derive(Default)]
pub(crate) struct InputRuntime {
    pub settings: AudioMixSettings,
    pub processing: AudioSettings,
    pub blocked: bool,
    pub ptt: bool,
    pub testing: bool,
}
pub(crate) struct Pcm {
    pub data: Vec<i16>,
    pub sample_rate: u32,
    pub channels: u32,
}
pub(crate) struct MicrophoneCapture {
    device: super::microphone_device::CaptureDevice,
    task: JoinHandle<()>,
}
impl MicrophoneCapture {
    pub fn start(
        selected: Option<&str>,
        controls: Arc<Mutex<InputRuntime>>,
        mixer: Arc<Mutex<PlaybackMixer>>,
        events: mpsc::Sender<MediaEvent>,
        channels: u32,
    ) -> Result<(Self, LocalAudioTrack)> {
        let (tx, rx) = mpsc::channel(8);
        let active = Arc::new(AtomicBool::new(false));
        let device = super::microphone_device::CaptureDevice::new(
            selected,
            channels,
            tx,
            events.clone(),
            active.clone(),
        )?;
        let enabled = controls.lock().is_ok_and(|c| c.testing || !c.blocked);
        device.controller().set_enabled(enabled)?;
        let (task, track) = process(rx, controls, mixer, events, channels, active);
        Ok((Self { device, task }, track))
    }
    pub fn stop(&self) -> Result<()> {
        self.device.stop()
    }
    pub fn controller(&self) -> super::microphone_device::CaptureControl {
        self.device.controller()
    }
    pub fn set_enabled(&self, enabled: bool) -> Result<()> {
        self.device.controller().set_enabled(enabled)
    }
    pub fn set_device(&self, id: &str) -> Result<()> {
        self.device.controller().set_device(id)
    }
}
impl Drop for MicrophoneCapture {
    fn drop(&mut self) {
        self.task.abort();
        self.device.shutdown();
    }
}
fn process(
    mut input: mpsc::Receiver<Pcm>,
    controls: Arc<Mutex<InputRuntime>>,
    mixer: Arc<Mutex<PlaybackMixer>>,
    events: mpsc::Sender<MediaEvent>,
    channels: u32,
    active: Arc<AtomicBool>,
) -> (JoinHandle<()>, LocalAudioTrack) {
    let source = NativeAudioSource::new(
        AudioSourceOptions {
            echo_cancellation: false,
            noise_suppression: false,
            auto_gain_control: false,
        },
        48_000,
        channels,
        0,
    );
    let track = LocalAudioTrack::create_audio_track(
        "Fluxer microphone",
        RtcAudioSource::Native(source.clone()),
    );
    let task = tokio::spawn(async move {
        let mut processor = InputProcessor::default();
        let mut resampler = AudioResampler::default();
        let mut apm: Option<AudioProcessingModule> = None;
        let mut previous = None;
        let mut pending = Vec::new();
        let mut format = (0, 0);
        let mut frames = 0_u32;
        let mut stalled = false;
        loop {
            let frame = match tokio::time::timeout(std::time::Duration::from_secs(2), input.recv())
                .await
            {
                Ok(Some(frame)) => {
                    stalled = false;
                    frame
                }
                Ok(None) => break,
                Err(_) => {
                    if active.load(Ordering::Acquire) && !stalled {
                        let _ = events.try_send(MediaEvent::Error("The selected microphone produced no audio frames for 2 seconds; check its permissions and routing or select another input".into()));
                        stalled = true;
                    }
                    continue;
                }
            };
            if frame.channels == 0
                || frame.channels > 2
                || frame.sample_rate < 100
                || !frame.sample_rate.is_multiple_of(100)
            {
                let _ = events.try_send(MediaEvent::Error(
                    "Unsupported microphone PCM format".into(),
                ));
                break;
            }
            if format != (frame.sample_rate, frame.channels) {
                pending.clear();
                format = (frame.sample_rate, frame.channels);
            }
            pending.extend(frame.data);
            let size = (frame.sample_rate / 100 * frame.channels) as usize;
            while pending.len() >= size {
                let (settings, dsp, blocked, ptt, testing) = match controls.lock() {
                    Ok(c) => (
                        c.settings.clone(),
                        c.processing.clone(),
                        c.blocked,
                        c.ptt,
                        c.testing,
                    ),
                    Err(_) => return,
                };
                let key = (
                    dsp.echo_cancellation,
                    dsp.auto_gain_control,
                    dsp.noise_suppression,
                );
                if previous != Some(key) {
                    apm = Some(AudioProcessingModule::new(key.0, key.1, false, key.2));
                    previous = Some(key);
                }
                let mut data = resampler
                    .remix_and_resample(
                        &pending[..size],
                        frame.sample_rate / 100,
                        frame.channels,
                        frame.sample_rate,
                        channels,
                        48_000,
                    )
                    .to_vec();
                pending.drain(..size);
                let Some(apm) = &mut apm else { return };
                // The reference contains the actual post-participant/master gain
                // speaker mix, not the unattenuated WebRTC mixer we bypass.
                let reverse = mixer
                    .lock()
                    .map(|mut m| m.take_render_reference())
                    .unwrap_or_default();
                let mut failed = None;
                for mut chunk in reverse {
                    if let Err(e) = apm.process_reverse_stream(&mut chunk, 48_000, 2) {
                        failed = Some(e);
                        break;
                    }
                }
                if failed.is_none() {
                    failed = apm.process_stream(&mut data, 48_000, channels as i32).err();
                }
                if let Some(error) = failed {
                    let _ = events.try_send(MediaEvent::Error(format!(
                        "Microphone audio processing failed: {error}"
                    )));
                    return;
                }
                let peak = processor.process(
                    &mut data,
                    48_000,
                    channels,
                    &settings,
                    blocked && !testing,
                    ptt,
                );
                frames += 1;
                if frames.is_multiple_of(10) {
                    let output = mixer
                        .lock()
                        .map(|mut m| {
                            let p = m.peak;
                            m.peak = 0.0;
                            p
                        })
                        .unwrap_or(0.0);
                    let _ = events.try_send(MediaEvent::AudioLevels {
                        input: peak,
                        output,
                    });
                }
                if !testing {
                    let frame = AudioFrame {
                        data: data.into(),
                        sample_rate: 48_000,
                        num_channels: channels,
                        samples_per_channel: 480,
                    };
                    if let Err(error) = source.capture_frame(&frame).await {
                        let _ = events.try_send(MediaEvent::Error(format!(
                            "Microphone publication failed: {error}"
                        )));
                        return;
                    }
                }
            }
        }
    });
    (task, track)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InputMode;
    use futures_util::StreamExt;
    use livekit::webrtc::audio_stream::native::NativeAudioStream;
    fn controls() -> Arc<Mutex<InputRuntime>> {
        Arc::new(Mutex::new(InputRuntime {
            processing: AudioSettings {
                echo_cancellation: false,
                noise_suppression: false,
                auto_gain_control: false,
            },
            ..Default::default()
        }))
    }
    async fn next(stream: &mut NativeAudioStream) -> AudioFrame<'static> {
        tokio::time::timeout(std::time::Duration::from_secs(1), stream.next())
            .await
            .unwrap()
            .unwrap()
    }
    #[tokio::test]
    async fn physical_pcm_pipeline_changes_published_gain_and_ptt_without_device_track() {
        let (input, rx) = mpsc::channel(8);
        let (events, _) = mpsc::channel(16);
        let controls = controls();
        controls.lock().unwrap().settings.input_gain = 0.5;
        let (task, track) = process(
            rx,
            controls.clone(),
            Arc::new(Mutex::new(PlaybackMixer::new())),
            events,
            1,
            Arc::new(AtomicBool::new(true)),
        );
        let mut output = NativeAudioStream::new(track.rtc_track(), 48_000, 1);
        input
            .send(Pcm {
                data: vec![16_384; 480],
                sample_rate: 48_000,
                channels: 1,
            })
            .await
            .unwrap();
        assert!(
            next(&mut output)
                .await
                .data
                .iter()
                .all(|s| (8191..=8192).contains(s))
        );
        controls.lock().unwrap().settings.input_mode = InputMode::PushToTalk;
        input
            .send(Pcm {
                data: vec![16_384; 480],
                sample_rate: 48_000,
                channels: 1,
            })
            .await
            .unwrap();
        assert!(next(&mut output).await.data.iter().all(|s| *s == 0));
        controls.lock().unwrap().ptt = true;
        input
            .send(Pcm {
                data: vec![16_384; 480],
                sample_rate: 48_000,
                channels: 1,
            })
            .await
            .unwrap();
        assert!(next(&mut output).await.data.iter().any(|s| *s > 0));
        task.abort();
        output.close();
    }
    #[tokio::test]
    async fn arbitrary_capture_chunks_accumulate_exact_10ms_stereo() {
        let (input, rx) = mpsc::channel(8);
        let (events, _) = mpsc::channel(16);
        let (task, track) = process(
            rx,
            controls(),
            Arc::new(Mutex::new(PlaybackMixer::new())),
            events,
            2,
            Arc::new(AtomicBool::new(true)),
        );
        let mut output = NativeAudioStream::new(track.rtc_track(), 48_000, 2);
        for frames in [173, 42, 265] {
            input
                .send(Pcm {
                    data: [10_000_i16, -5_000].repeat(frames),
                    sample_rate: 48_000,
                    channels: 2,
                })
                .await
                .unwrap();
        }
        let actual = next(&mut output).await;
        assert_eq!(actual.samples_per_channel, 480);
        assert_eq!(actual.num_channels, 2);
        assert!(actual.data.as_chunks::<2>().0.iter().all(
            |pair| (9_999..=10_000).contains(&pair[0]) && (-5_000..=-4_999).contains(&pair[1])
        ));
        task.abort();
        output.close();
    }
    #[tokio::test]
    async fn microphone_test_meters_actual_capture_without_forwarding_pcm() {
        let (input, rx) = mpsc::channel(16);
        let (events, mut meter) = mpsc::channel(16);
        let controls = controls();
        controls.lock().unwrap().testing = true;
        let (task, track) = process(
            rx,
            controls,
            Arc::new(Mutex::new(PlaybackMixer::new())),
            events,
            1,
            Arc::new(AtomicBool::new(true)),
        );
        let mut output = NativeAudioStream::new(track.rtc_track(), 48_000, 1);
        for _ in 0..10 {
            input
                .send(Pcm {
                    data: vec![16_384; 480],
                    sample_rate: 48_000,
                    channels: 1,
                })
                .await
                .unwrap();
        }
        let level = tokio::time::timeout(std::time::Duration::from_secs(1), meter.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(level,MediaEvent::AudioLevels{input,..}if input>0.49));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), output.next())
                .await
                .is_err()
        );
        task.abort();
        output.close();
    }
    #[tokio::test]
    async fn enabled_capture_without_callbacks_reports_stall_but_paused_does_not() {
        let (_input, rx) = mpsc::channel(8);
        let (events, mut errors) = mpsc::channel(16);
        let active = Arc::new(AtomicBool::new(true));
        let (task, _track) = process(
            rx,
            controls(),
            Arc::new(Mutex::new(PlaybackMixer::new())),
            events,
            1,
            active.clone(),
        );
        let event = tokio::time::timeout(std::time::Duration::from_millis(2500), errors.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(event,MediaEvent::Error(message) if message.contains("no audio frames")));
        active.store(false, Ordering::Release);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(2100), errors.recv())
                .await
                .is_err()
        );
        task.abort();
    }
    #[tokio::test]
    async fn default_sdk_audio_processing_forwards_nonzero_pcm_without_remote_audio() {
        let (input, rx) = mpsc::channel(8);
        let (events, mut errors) = mpsc::channel(32);
        let controls = Arc::new(Mutex::new(InputRuntime::default()));
        let (task, track) = process(
            rx,
            controls,
            Arc::new(Mutex::new(PlaybackMixer::new())),
            events,
            1,
            Arc::new(AtomicBool::new(true)),
        );
        let mut output = NativeAudioStream::new(track.rtc_track(), 48_000, 1);
        let mut nonzero = false;
        for frame in 0..30 {
            let data = (0..480)
                .map(|sample| {
                    let phase =
                        (frame * 480 + sample) as f32 * 200.0 * std::f32::consts::TAU / 48_000.0;
                    ((phase.sin() + 0.4 * (phase * 2.0).sin()) * 8000.0) as i16
                })
                .collect();
            input
                .send(Pcm {
                    data,
                    sample_rate: 48_000,
                    channels: 1,
                })
                .await
                .unwrap();
            nonzero |= next(&mut output)
                .await
                .data
                .iter()
                .any(|sample| sample.abs() > 10);
        }
        while let Ok(event) = errors.try_recv() {
            assert!(!matches!(event, MediaEvent::Error(_)), "{event:?}");
        }
        assert!(
            nonzero,
            "default AEC/NS/AGC must deliver actual input without needing a remote speaker stream"
        );
        task.abort();
        output.close();
    }
    #[tokio::test]
    async fn sdk_resamples_44100_capture_before_native_publication() {
        let (input, rx) = mpsc::channel(8);
        let (events, _) = mpsc::channel(16);
        let (task, track) = process(
            rx,
            controls(),
            Arc::new(Mutex::new(PlaybackMixer::new())),
            events,
            1,
            Arc::new(AtomicBool::new(true)),
        );
        let mut output = NativeAudioStream::new(track.rtc_track(), 48_000, 1);
        input
            .send(Pcm {
                data: vec![12_000; 441],
                sample_rate: 44_100,
                channels: 1,
            })
            .await
            .unwrap();
        let actual = next(&mut output).await;
        assert_eq!(actual.data.len(), 480);
        assert_eq!(actual.sample_rate, 48_000);
        assert!(actual.data.iter().any(|s| *s > 10_000));
        task.abort();
        output.close();
    }
}
