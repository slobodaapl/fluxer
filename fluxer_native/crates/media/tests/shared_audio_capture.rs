// SPDX-License-Identifier: AGPL-3.0-or-later
//! Real application capture -> MediaEngine shared audio -> encoded local SFU
//! -> decoded PCM. Only a runner-owned non-Fluxer pacat process is captured;
//! microphone capture stays muted and output targets an isolated null sink.
#![cfg(target_os = "linux")]

use fluxer_media::{
    MediaEngine, MediaEvent, ScreenShareOptions, ScreenSourceKind, SharedAudioSelection,
    SharedAudioSettings,
};
use fluxer_system_audio::{SpeakerOutput, SystemAudio};
use futures_util::StreamExt;
use livekit::{prelude::*, webrtc::audio_stream::native::NativeAudioStream};
use std::{
    io::Write,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::time::timeout;
#[derive(Debug)]
struct Stats {
    rms: f64,
    left440: f64,
    left880: f64,
    right440: f64,
    right880: f64,
}
fn tone(samples: &[i16], channel: usize, frequency: f64) -> f64 {
    let (mut sin, mut cos) = (0.0, 0.0);
    for (position, frame) in samples.as_chunks::<2>().0.iter().enumerate() {
        let phase = position as f64 * std::f64::consts::TAU * frequency / 48_000.;
        let sample = f64::from(frame[channel]) / 32768.;
        sin += sample * phase.sin();
        cos += sample * phase.cos();
    }
    sin.hypot(cos) / (samples.len() / 2).max(1) as f64
}
async fn measure(stream: &mut NativeAudioStream) -> Stats {
    let mut pcm = Vec::with_capacity(28_800);
    timeout(Duration::from_secs(3), async {
        for _ in 0..30 {
            let frame = stream
                .next()
                .await
                .expect("decoded shared audio remains open");
            assert_eq!((frame.sample_rate, frame.num_channels), (48_000, 2));
            pcm.extend_from_slice(&frame.data);
        }
    })
    .await
    .expect("actual shared PCM keeps arriving");
    Stats {
        rms: (pcm
            .iter()
            .map(|s| (f64::from(*s) / 32768.).powi(2))
            .sum::<f64>()
            / pcm.len() as f64)
            .sqrt(),
        left440: tone(&pcm, 0, 440.),
        left880: tone(&pcm, 0, 880.),
        right440: tone(&pcm, 1, 440.),
        right880: tone(&pcm, 1, 880.),
    }
}
async fn wait_stats(
    stream: &mut NativeAudioStream,
    phase: &str,
    predicate: impl Fn(&Stats) -> bool,
) -> Stats {
    timeout(Duration::from_secs(10), async {
        loop {
            let stats = measure(stream).await;
            if predicate(&stats) {
                return stats;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("shared capture phase '{phase}' never reached expected decoded PCM"))
}

async fn verify_pipewire_isolation(output: &str, app_pid: &str, loopback_pid: &str) {
    // Use the same PipeWire player as the real call. The Python runner has
    // already linked its sink monitor to a separate loopback playback node.
    let mut frames_rendered = 0;
    let call_tone = SpeakerOutput::start(
        Some(output),
        move |samples| {
            for frame in samples.as_chunks_mut::<2>().0 {
                let phase = frames_rendered as f32 * std::f32::consts::TAU / 30.0;
                frame.fill(phase.sin() * 0.3);
                frames_rendered = (frames_rendered + 1) % 30;
            }
        },
        |error| panic!("synthetic call output failed: {error}"),
    )
    .expect("synthetic call output on isolated sink");
    let apps = SystemAudio::applications().expect("application picker inventory");
    assert!(apps.iter().any(|app| app.process_id == app_pid));
    let self_pid = std::process::id().to_string();
    assert!(
        apps.iter()
            .all(|app| app.process_id != loopback_pid && app.process_id != self_pid)
    );
    for selection in [
        SharedAudioSelection::Application {
            process_id: app_pid.into(),
        },
        SharedAudioSelection::WindowApplication {
            app_id: "app.fluxer.AudioFixture".into(),
        },
        SharedAudioSelection::WindowApplication {
            app_id: "steam_app_424242".into(),
        },
        SharedAudioSelection::System,
    ] {
        let capture = SystemAudio::start(selection.clone()).expect("private PipeWire tap");
        let mut pcm = Vec::with_capacity(96_000);
        timeout(Duration::from_secs(6), async {
            while pcm.len() < 96_000 {
                if let Some(frame) = capture.read() {
                    assert_eq!((frame.sample_rate, frame.channels), (48_000, 2));
                    pcm.extend(
                        frame
                            .samples
                            .iter()
                            .map(|sample| (sample * 32768.).clamp(-32768., 32767.) as i16),
                    );
                } else {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }
        })
        .await
        .expect("at least one second of captured synthetic PCM");
        let app_level = tone(&pcm, 0, 440.) + tone(&pcm, 1, 880.);
        let echo_level = tone(&pcm, 0, 1600.) + tone(&pcm, 1, 1600.);
        assert!(app_level > 0.08, "{selection:?} lost the application audio");
        assert!(
            echo_level < 0.005,
            "{selection:?} captured the call tone directly or via the loopback: {echo_level:.5}"
        );
        eprintln!(
            "shared_audio_capture: {selection:?} app={app_level:.5} call echo={echo_level:.5}"
        );
    }
    drop(call_tone);
}

async fn fixture(encrypted: bool) {
    println!("FLUXER_SHARED_AUDIO_PROCESS {}", std::process::id());
    std::io::stdout().flush().unwrap();
    let endpoint = std::env::var("FLUXER_MEDIA_TEST_URL").expect("local SFU URL");
    assert!(endpoint.starts_with("ws://127.0.0.1:") || endpoint.starts_with("ws://localhost:"));
    let process_id = std::env::var("FLUXER_SHARED_AUDIO_TEST_PROCESS")
        .expect("runner-owned synthetic pacat PID");
    let pid: u32 = process_id.parse().expect("synthetic PID");
    assert_ne!(pid, std::process::id());
    let command =
        std::fs::read(format!("/proc/{pid}/cmdline")).expect("runner tone process exists");
    let command = String::from_utf8_lossy(&command);
    assert!(
        command.contains("SyntheticAudioTone-") && command.contains("fluxer-output-test-"),
        "refuse capturing any personal application"
    );
    let output = std::env::var("FLUXER_SHARED_AUDIO_TEST_OUTPUT").expect("isolated output sink");
    assert!(output.starts_with("output:pipewire:fluxer-output-test-"));
    let mut publisher = MediaEngine::new().unwrap();
    if !encrypted && let Ok(loopback_pid) = std::env::var("FLUXER_SHARED_AUDIO_TEST_LOOPBACK") {
        verify_pipewire_isolation(&output, &process_id, &loopback_pid).await;
    }
    let publisher_token = std::env::var("FLUXER_MEDIA_TEST_TOKEN_A").unwrap();
    let receiver_token = std::env::var("FLUXER_MEDIA_TEST_TOKEN_B").unwrap();
    const KEY: &str = "synthetic-shared-audio-passphrase";
    let mut options = RoomOptions::default();
    options.auto_subscribe = false;
    options.single_peer_connection = false;
    if encrypted {
        use livekit::e2ee::{
            E2eeOptions, EncryptionType,
            key_provider::{KeyProvider, KeyProviderOptions},
        };
        options.encryption = Some(E2eeOptions {
            encryption_type: EncryptionType::Gcm,
            key_provider: KeyProvider::with_shared_key(
                KeyProviderOptions {
                    ratchet_window_size: 0,
                    ..Default::default()
                },
                KEY.as_bytes().to_vec(),
            ),
        });
    }
    let (receiver, mut receiver_events) = Room::connect(&endpoint, &receiver_token, options)
        .await
        .expect("raw receiver connects without hardware");
    publisher.set_output_device(&output).unwrap();
    publisher.set_muted(true).unwrap();
    publisher.set_deafened(true).unwrap();
    let mut events = publisher
        .connect_encrypted(&endpoint, &publisher_token, encrypted.then_some(KEY))
        .await
        .unwrap();
    let failures = Arc::new(Mutex::new(Vec::new()));
    let errors = failures.clone();
    let stopped = Arc::new(AtomicBool::new(false));
    let stop_event = stopped.clone();
    let monitor = tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            match event {
                MediaEvent::Error(error) => errors.lock().unwrap().push(error),
                MediaEvent::SharedAudioStopped => {
                    stop_event.store(true, Ordering::Release);
                }
                _ => (),
            }
        }
    });
    let applications =
        MediaEngine::audio_applications().expect("synchronized application inventory");
    assert!(
        applications.iter().any(|app| app.process_id == process_id),
        "real synthetic app appears in source picker; apps={applications:?}"
    );
    let selection = match std::env::var("FLUXER_SHARED_AUDIO_TEST_WINDOW") {
        Ok(app_id) if !encrypted => {
            let sources = publisher.screen_sources().expect("unified source picker");
            assert!(
                sources
                    .iter()
                    .any(|source| source.kind == ScreenSourceKind::Screen)
            );
            assert!(
                sources
                    .iter()
                    .all(|source| source.kind != ScreenSourceKind::SystemPicker)
            );
            let source = sources
                .into_iter()
                .find(|source| {
                    source.kind == ScreenSourceKind::Window
                        && source.label.ends_with(&format!(" · {app_id}"))
                })
                .expect("synthetic Steam window in the same picker as displays");
            publisher
                .start_screen_share(ScreenShareOptions {
                    source,
                    follow_window_audio: true,
                    max_width: 640,
                    max_height: 480,
                    ..Default::default()
                })
                .await
                .expect("publish selected Wayland window");
            timeout(Duration::from_secs(7), async {
                while !publisher.screen_capture_has_frame() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("selected window produced video");
            let selection = publisher
                .screen_audio_selection()
                .expect("window selected an audio route");
            assert_eq!(
                selection,
                SharedAudioSelection::WindowApplication { app_id }
            );
            selection
        }
        _ => SharedAudioSelection::Application { process_id },
    };
    publisher
        .start_shared_audio(selection)
        .await
        .expect("real native application capture publishes");
    assert!(publisher.is_sharing_audio());
    let publication = timeout(Duration::from_secs(15), async {
        loop {
            match receiver_events.recv().await.expect("receiver events") {
                RoomEvent::TrackPublished { publication, .. }
                    if publication.source() == TrackSource::ScreenshareAudio =>
                {
                    break publication;
                }
                RoomEvent::TrackSubscriptionFailed { error, .. } => panic!("{error}"),
                _ => (),
            }
        }
    })
    .await
    .expect("shared audio publication metadata");
    assert_eq!(publication.kind(), TrackKind::Audio);
    assert!(!publication.is_desired());
    publication.set_subscribed(true);
    let track = timeout(Duration::from_secs(15), async {
        loop {
            match receiver_events.recv().await.expect("receiver events") {
                RoomEvent::TrackSubscribed {
                    track: RemoteTrack::Audio(track),
                    publication: received,
                    ..
                } if received.sid() == publication.sid() => break track,
                RoomEvent::TrackSubscriptionFailed { error, .. } => panic!("{error}"),
                _ => (),
            }
        }
    })
    .await
    .expect("remote subscription");
    let mut stream = NativeAudioStream::new(track.rtc_track(), 48_000, 2);
    let baseline = wait_stats(
        &mut stream,
        "audible while microphone muted and deafened",
        |s| s.rms > 0.04 && (s.left440 + s.right440) > 0.02 && (s.left880 + s.right880) > 0.02,
    )
    .await;
    eprintln!(
        "shared_audio_capture: encrypted={encrypted} actual captured tone survives deafen, {baseline:?}"
    );
    // Distinct runner-owned L440/R880 tones must survive encoding, transport
    // and decoding as separate channels. Upmixed mono cannot satisfy this.
    assert!(
        baseline.left440 > baseline.left880 * 4. && baseline.right880 > baseline.right440 * 4.,
        "stereo channels collapsed or crossed: {baseline:?}"
    );
    publisher
        .configure_shared_audio(SharedAudioSettings {
            gain: 0.25,
            muted: false,
        })
        .unwrap();
    let quiet = wait_stats(&mut stream, "quarter gain", |s| {
        s.rms > baseline.rms * 0.12 && s.rms < baseline.rms * 0.4
    })
    .await;
    eprintln!(
        "shared_audio_capture: actual decoded gain ratio {:.3}",
        quiet.rms / baseline.rms
    );
    publisher
        .configure_shared_audio(SharedAudioSettings {
            gain: 1.,
            muted: true,
        })
        .unwrap();
    wait_stats(&mut stream, "explicit stream mute", |s| s.rms < 0.003).await;
    let silent = measure(&mut stream).await;
    assert!(silent.rms < 0.003, "mute remains silent: {silent:?}");
    publisher
        .configure_shared_audio(SharedAudioSettings {
            gain: 1.,
            muted: false,
        })
        .unwrap();
    wait_stats(&mut stream, "stream unmute fresh captured tone", |s| {
        s.rms > baseline.rms * 0.8
    })
    .await;
    publisher.stop_shared_audio().await.unwrap();
    assert!(!publisher.is_sharing_audio());
    timeout(Duration::from_secs(2), async {
        while !stopped.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("local SharedAudioStopped event");
    timeout(Duration::from_secs(10),async {loop {if matches!(receiver_events.recv().await,Some(RoomEvent::TrackUnpublished {publication:ended,..}) if ended.sid()==publication.sid()) {break;}}}).await.expect("real SFU unpublication");
    assert!(
        failures.lock().unwrap().is_empty(),
        "publisher media errors: {:?}",
        failures.lock().unwrap()
    );
    stream.close();
    publisher.disconnect().await.unwrap();
    receiver.close().await.unwrap();
    monitor.abort();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires local SFU and isolated synthetic application runner"]
async fn application_capture_reaches_decoded_shared_pcm() {
    fixture(false).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires local SFU and isolated synthetic application runner"]
async fn encrypted_application_capture_reaches_decoded_shared_pcm() {
    fixture(true).await;
}
