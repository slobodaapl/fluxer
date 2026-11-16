// SPDX-License-Identifier: AGPL-3.0-or-later
//! Opt-in end-to-end capture test. A separate runner must create a synthetic
//! PipeWire input and feed a steady tone. No microphone/default input fallback,
//! user credentials, recorded PCM, or resubscription is used by this test.
#![cfg(target_os = "linux")]

use fluxer_media::{AudioMixSettings, AudioSettings, InputMode, MediaEngine, MediaEvent};
use futures_util::StreamExt;
use livekit::{prelude::*, webrtc::audio_stream::native::NativeAudioStream};
use std::{error::Error, time::Duration};
use tokio::{
    sync::mpsc,
    task::JoinHandle,
    time::{Instant, timeout, timeout_at},
};

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;
const DEADLINE: Duration = Duration::from_secs(15);
const SETTLE: Duration = Duration::from_millis(800);
const WINDOW: Duration = Duration::from_millis(1000);
const SYNTHETIC_PREFIX: &str = "input:pipewire:fluxer-microphone-test-";

struct Observation {
    received: Instant,
    squares: f64,
    samples: usize,
}

/// Drain the same decoder continuously, so phase measurements never consume
/// old samples retained while the test was changing settings. A full channel
/// drops observations rather than blocking the decoder and accumulating PCM.
struct PcmProbe {
    observations: mpsc::Receiver<Observation>,
    task: JoinHandle<()>,
}
impl PcmProbe {
    fn new(track: &RemoteAudioTrack) -> Self {
        let mut stream = NativeAudioStream::new(track.rtc_track(), 48_000, 1);
        let (tx, observations) = mpsc::channel(128);
        let task = tokio::spawn(async move {
            while let Some(frame) = stream.next().await {
                let observation = Observation {
                    received: Instant::now(),
                    squares: frame
                        .data
                        .iter()
                        .map(|&sample| (f64::from(sample) / 32768.).powi(2))
                        .sum(),
                    samples: frame.data.len(),
                };
                if tx.is_closed() {
                    break;
                }
                let _ = tx.try_send(observation);
            }
            stream.close();
        });
        Self { observations, task }
    }

    async fn measure(&mut self, require_frames: bool) -> TestResult<f64> {
        let start = Instant::now() + SETTLE;
        let end = start + WINDOW;
        let mut squares = 0.;
        let mut samples = 0;
        while Instant::now() < end {
            match timeout_at(end, self.observations.recv()).await {
                Ok(Some(frame)) if frame.received >= start => {
                    squares += frame.squares;
                    samples += frame.samples;
                }
                Ok(Some(_)) => (),
                Ok(None) => return Err("remote PCM decoder ended during the test".into()),
                Err(_) => break,
            }
        }
        if require_frames && samples < 480 * 20 {
            return Err("too few decoded frames for an audio measurement".into());
        }
        // A disabled/muted publication may stop delivering frames entirely.
        Ok(if samples == 0 {
            0.
        } else {
            (squares / samples as f64).sqrt()
        })
    }
}
impl Drop for PcmProbe {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn remote_microphone(
    events: &mut mpsc::UnboundedReceiver<RoomEvent>,
) -> TestResult<RemoteAudioTrack> {
    timeout(DEADLINE, async {
        loop {
            match events.recv().await {
                Some(RoomEvent::TrackSubscribed {
                    track, publication, ..
                }) if publication.source() == TrackSource::Microphone => {
                    let RemoteTrack::Audio(track) = track else {
                        return Err("microphone publication was not audio".into());
                    };
                    return Ok(track);
                }
                Some(RoomEvent::TrackSubscriptionFailed { .. }) => {
                    return Err("test receiver subscription failed".into());
                }
                Some(RoomEvent::Disconnected { .. }) | None => {
                    return Err("test receiver disconnected".into());
                }
                _ => (),
            }
        }
    })
    .await
    .map_err(|_| "microphone subscription deadline exceeded")?
}

fn mixer(gain: f32, mode: InputMode) -> AudioMixSettings {
    AudioMixSettings {
        input_gain: gain,
        // This room publishes only the synthetic input; prevent audible output
        // even if a test-server configuration accidentally adds another track.
        output_gain: 0.,
        input_mode: mode,
        ..Default::default()
    }
}

async fn check_meter(media: &mut MediaEngine) -> TestResult {
    let mut events = media.start_microphone_test()?;
    let measured = timeout(DEADLINE, async {
        loop {
            match events.recv().await {
                Some(MediaEvent::AudioLevels { input, .. }) if input > 0.005 => return Ok(()),
                Some(MediaEvent::Error(_)) => {
                    return Err("synthetic input meter reported an error");
                }
                None => return Err("synthetic input meter ended without a tone"),
                _ => (),
            }
        }
    })
    .await
    .map_err(|_| "synthetic input meter deadline exceeded")?;
    // Stop even when the meter failed, before returning that failure.
    media.stop_microphone_test()?;
    measured?;
    timeout(Duration::from_secs(2), async {
        while events.recv().await.is_some() {}
    })
    .await
    .map_err(|_| "stopped microphone meter retained a live producer")?;
    Ok(())
}

async fn check_pipeline(media: &mut MediaEngine, probe: &mut PcmProbe) -> TestResult {
    let baseline = probe.measure(true).await?;
    if baseline < 0.005 {
        return Err("synthetic input was silent after actual microphone publication".into());
    }

    media.configure_mixer(mixer(0.25, InputMode::OpenMic))?;
    let reduced = probe.measure(true).await?;
    let ratio = reduced / baseline;
    println!("decoded microphone gain ratio: {ratio:.3} (requested 0.25)");
    // Opus encoding and packet timing require tolerance; a no-op gain control
    // (ratio near one) or gain-zero bug cannot satisfy this bound.
    if !(0.14..=0.4).contains(&ratio) {
        return Err("input gain did not proportionally change decoded PCM".into());
    }

    media.configure_mixer(mixer(1., InputMode::PushToTalk))?;
    media.set_push_to_talk_pressed(false)?;
    let closed = probe.measure(true).await?;
    let silence_limit = (baseline * 0.03).max(3. / 32768.);
    if closed > silence_limit {
        return Err("closed PTT gate transmitted non-silent PCM".into());
    }
    media.set_push_to_talk_pressed(true)?;
    let pressed = probe.measure(true).await?;
    if pressed < baseline * 0.6 {
        return Err("pressed PTT did not restore microphone audio".into());
    }

    media.set_muted(true)?;
    if probe.measure(false).await? > silence_limit {
        return Err("muted microphone transmitted non-silent PCM".into());
    }
    media.set_muted(false)?;
    if probe.measure(true).await? < baseline * 0.6 {
        return Err("unmuting did not restart microphone capture/publication".into());
    }
    media.set_push_to_talk_pressed(false)?;
    if probe.measure(true).await? > silence_limit {
        return Err("releasing PTT after unmute did not close the gate".into());
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires explicit synthetic PipeWire input and disposable local LiveKit server"]
async fn microphone_pipeline() -> TestResult {
    // Validate all opt-in configuration before constructing/opening any device.
    let input = std::env::var("FLUXER_MICROPHONE_TEST_INPUT")
        .map_err(|_| "FLUXER_MICROPHONE_TEST_INPUT must name the runner's virtual input")?;
    if !input.starts_with(SYNTHETIC_PREFIX) || input.len() <= SYNTHETIC_PREFIX.len() {
        return Err("refusing microphone test: explicit synthetic PipeWire input required".into());
    }
    let endpoint = std::env::var("FLUXER_MEDIA_TEST_URL").map_err(|_| "missing test server URL")?;
    if !["ws://127.0.0.1:", "ws://localhost:", "ws://[::1]:"]
        .iter()
        .any(|prefix| endpoint.starts_with(prefix))
    {
        return Err("microphone integration test requires a loopback LiveKit server".into());
    }
    let publisher_token =
        std::env::var("FLUXER_MEDIA_TEST_TOKEN_A").map_err(|_| "missing publisher test token")?;
    let receiver_token =
        std::env::var("FLUXER_MEDIA_TEST_TOKEN_B").map_err(|_| "missing receiver test token")?;

    let mut media = MediaEngine::new()?;
    if !media
        .devices()
        .inputs
        .iter()
        .any(|device| device.id == input)
    {
        return Err("requested synthetic input was not found; refusing fallback".into());
    }
    media.set_input_device(&input)?;
    media.configure(AudioSettings {
        echo_cancellation: false,
        noise_suppression: false,
        auto_gain_control: false,
        ..AudioSettings::default()
    })?;
    media.configure_mixer(mixer(1., InputMode::OpenMic))?;
    media.configure_microphone_publish(Some(64_000.), false)?;

    let mut receiver = None;
    let mut probe = None;
    let mut monitor = None;
    let result: TestResult = async {
        check_meter(&mut media).await?;
        let mut options = RoomOptions::default();
        // Subscribe exactly once. Muting and PTT must work with the same decoder
        // and publication, without unsubscribe/resubscribe or SDK receiver races.
        options.auto_subscribe = true;
        options.single_peer_connection = false;
        let (room, mut events) = timeout(
            DEADLINE,
            Room::connect(&endpoint, &receiver_token, options),
        )
        .await
        .map_err(|_| "test receiver connect deadline exceeded")?
        .map_err(|_| "test receiver could not connect")?;
        receiver = Some(room);
        let mut media_events = timeout(DEADLINE, media.connect(&endpoint, &publisher_token))
            .await
            .map_err(|_| "microphone publisher connect deadline exceeded")??;
        let (error_tx, mut errors) = mpsc::unbounded_channel();
        monitor = Some(tokio::spawn(async move {
            while let Some(event) = media_events.recv().await {
                if matches!(event, MediaEvent::Error(_) | MediaEvent::Disconnected) {
                    let _ = error_tx.send(());
                }
            }
        }));
        let remote = remote_microphone(&mut events).await?;
        probe = Some(PcmProbe::new(&remote));
        tokio::select! {
            result = check_pipeline(&mut media, probe.as_mut().expect("initialized probe")) => result,
            _ = errors.recv() => Err("microphone publisher reported a failure during the test".into()),
        }
    }.await;

    // Cleanup also executes on failed measurements; both calls are bounded and
    // the engine releases physical capture before awaiting network teardown.
    drop(probe);
    let disconnected = timeout(DEADLINE, media.disconnect()).await;
    if let Some(task) = monitor {
        task.abort();
    }
    let receiver_closed = if let Some(room) = receiver {
        timeout(DEADLINE, room.close())
            .await
            .map_err(|_| "test receiver close deadline exceeded")?
            .map_err(|_| "test receiver could not close")
    } else {
        Ok(())
    };
    result?;
    disconnected.map_err(|_| "microphone disconnect deadline exceeded")??;
    receiver_closed?;
    if media.is_connected() {
        return Err("engine stayed connected after disconnect".into());
    }
    media.stop_microphone_test()?;
    Ok(())
}
