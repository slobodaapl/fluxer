// SPDX-License-Identifier: AGPL-3.0-or-later
//! Opt-in decoded-call playback through two explicitly selected synthetic sinks.
//! The external runner measures both monitor streams and acknowledges phases.
#![cfg(target_os = "linux")]
use fluxer_media::{AudioMixSettings, MediaEngine, MediaEvent, ParticipantAudioSettings};
use livekit::{
    options::{AudioEncoding, TrackPublishOptions},
    prelude::*,
    webrtc::{
        audio_frame::AudioFrame,
        audio_source::{AudioSourceOptions, native::NativeAudioSource},
    },
};
use std::{
    borrow::Cow,
    error::Error,
    io::{BufRead, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::time::timeout;

type TestResult = Result<(), Box<dyn Error + Send + Sync>>;
const DEADLINE: Duration = Duration::from_secs(20);

fn phase(name: &str) -> TestResult {
    println!("FLUXER_OUTPUT_PHASE {name}");
    std::io::stdout().flush()?;
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut line = String::new();
        let result = std::io::stdin().lock().read_line(&mut line).map(|_| line);
        let _ = tx.send(result);
    });
    let line = rx.recv_timeout(Duration::from_secs(15))??;
    if line.trim() != "NEXT" {
        return Err("output monitor did not acknowledge the phase".into());
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires isolated virtual sinks and disposable local LiveKit server; run scripts/test_output_routing.py --media"]
async fn output_pipeline() -> TestResult {
    let a = std::env::var("FLUXER_OUTPUT_TEST_A")?;
    let b = std::env::var("FLUXER_OUTPUT_TEST_B")?;
    for id in [&a, &b] {
        if !id.starts_with("output:pipewire:fluxer-output-test-") {
            return Err("refusing playback to a default or physical output".into());
        }
    }
    if a == b {
        return Err("two distinct synthetic outputs are required".into());
    }
    let endpoint = std::env::var("FLUXER_MEDIA_TEST_URL")?;
    if !["ws://127.0.0.1:", "ws://localhost:", "ws://[::1]:"]
        .iter()
        .any(|prefix| endpoint.starts_with(prefix))
    {
        return Err("output test requires a loopback server".into());
    }
    let publisher_token = std::env::var("FLUXER_MEDIA_TEST_TOKEN_A")?;
    let receiver_token = std::env::var("FLUXER_MEDIA_TEST_TOKEN_B")?;
    let mut media = MediaEngine::new()?;
    media.set_muted(true)?;
    media.set_output_device(&a)?;
    media.configure_mixer(AudioMixSettings::default())?;
    println!("\nFLUXER_OUTPUT_PROCESS {}", std::process::id());
    std::io::stdout().flush()?;

    let mut options = RoomOptions::default();
    options.auto_subscribe = false;
    options.single_peer_connection = false;
    let (publisher, _publisher_events) = timeout(
        DEADLINE,
        Room::connect(&endpoint, &publisher_token, options),
    )
    .await??;
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
    let track = LocalAudioTrack::create_audio_track(
        "synthetic output tone",
        RtcAudioSource::Native(source.clone()),
    );
    publisher
        .local_participant()
        .publish_track(
            LocalTrack::Audio(track),
            TrackPublishOptions {
                source: TrackSource::Microphone,
                audio_encoding: Some(AudioEncoding {
                    max_bitrate: 64_000,
                }),
                dtx: false,
                ..Default::default()
            },
        )
        .await?;
    let sender = tokio::spawn(async move {
        let mut ticks = tokio::time::interval(Duration::from_millis(10));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut offset = 0_u64;
        loop {
            ticks.tick().await;
            let pcm = (0..480)
                .map(|n| {
                    (((offset + n) as f64 * std::f64::consts::TAU * 440. / 48_000.).sin()
                        * (32767. * 0.2)) as i16
                })
                .collect();
            if source
                .capture_frame(&AudioFrame {
                    data: Cow::Owned(pcm),
                    sample_rate: 48_000,
                    num_channels: 1,
                    samples_per_channel: 480,
                })
                .await
                .is_err()
            {
                break;
            }
            offset += 480;
        }
    });
    let failed = Arc::new(AtomicBool::new(false));
    let mut monitor = None;
    let result: TestResult = async {
        let mut events = timeout(DEADLINE, media.connect(&endpoint, &receiver_token)).await??;
        let failure = failed.clone();
        monitor = Some(tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                if matches!(event, MediaEvent::Error(_) | MediaEvent::Disconnected) {
                    failure.store(true, Ordering::Release);
                }
            }
        }));
        phase("A")?;
        media.configure_mixer(AudioMixSettings {
            output_gain: 0.25,
            ..Default::default()
        })?;
        phase("GAIN")?;
        media.configure_mixer(AudioMixSettings::default())?;
        media.set_participant_audio(
            "publisher",
            ParticipantAudioSettings {
                gain: 1.,
                muted: true,
            },
        )?;
        phase("MUTE")?;
        media.set_participant_audio("publisher", ParticipantAudioSettings::default())?;
        media.set_output_device(&b)?;
        phase("B")?;
        if failed.load(Ordering::Acquire) {
            return Err("playback engine reported an error".into());
        }
        timeout(DEADLINE, media.disconnect()).await??;
        phase("STOPPED")?;
        Ok(())
    }
    .await;
    sender.abort();
    if let Some(monitor) = monitor {
        monitor.abort();
    }
    let disconnected = timeout(DEADLINE, media.disconnect()).await;
    let closed = timeout(DEADLINE, publisher.close()).await;
    result?;
    disconnected??;
    closed??;
    Ok(())
}
