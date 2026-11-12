// SPDX-License-Identifier: AGPL-3.0-or-later
//! Actual MediaEngine receiver, not a direct SDK subscription approximation.
//! Only synthetic video and a deliberately isolated virtual speaker sink are
//! used. The receiver microphone remains muted throughout the fixture.
use fluxer_media::{MediaEngine, MediaEvent, MediaEvents, ScreenShareOptions};
use livekit::{
    prelude::*,
    webrtc::{
        video_frame::{I420Buffer, VideoFrame, VideoRotation},
        video_source::{RtcVideoSource, VideoResolution, native::NativeVideoSource},
    },
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::time::timeout;

const WIDTH: u32 = 1920;
const HEIGHT: u32 = 1080;
const FPS: u32 = 30;
async fn until(
    events: &mut MediaEvents,
    mut matches: impl FnMut(&MediaEvent) -> bool,
) -> MediaEvent {
    timeout(Duration::from_secs(20), async {
        loop {
            let event = events
                .recv()
                .await
                .expect("media event stream remains open");
            if let MediaEvent::Error(error) = &event {
                panic!("media failure: {error}");
            }
            if matches(&event) {
                return event;
            }
        }
    })
    .await
    .expect("expected MediaEngine event before deadline")
}
async fn no_video(events: &mut MediaEvents) {
    assert!(
        timeout(Duration::from_millis(350), async {
            while let Some(event) = events.recv().await {
                match event {
                    MediaEvent::Video(frame) => {
                        panic!("unwatched screen delivered frame {}", frame.track_id)
                    }
                    MediaEvent::Error(error) => panic!("unexpected media error: {error}"),
                    _ => (),
                }
            }
        })
        .await
        .is_err()
    );
}
async fn slow_video_consumer(events: &MediaEvents, latest: &AtomicU64) {
    timeout(Duration::from_secs(2), async {
        let mut poll = tokio::time::interval(Duration::from_millis(10));
        while events.pending_video_frames() == 0 {
            poll.tick().await;
        }
    })
    .await
    .expect("real decoded frame available for backpressure experiment");
    // Deliberately stop consuming for 350ms. Storage must stay one latest frame,
    // not build a FIFO of full-resolution images while a UI is busy.
    let before = latest.load(Ordering::Acquire);
    tokio::time::sleep(Duration::from_millis(350)).await;
    assert_eq!(events.pending_video_frames(), 1);
    assert!(
        latest.load(Ordering::Acquire) > before + 3,
        "animated source continued during backpressure"
    );
}
async fn publish(
    room: &Room,
    name: &str,
) -> (
    TrackSid,
    NativeVideoSource,
    tokio::task::JoinHandle<()>,
    Arc<AtomicBool>,
    Arc<AtomicBool>,
    Arc<AtomicU64>,
) {
    let source = NativeVideoSource::new(
        VideoResolution {
            width: WIDTH,
            height: HEIGHT,
        },
        true,
    );
    let track = LocalVideoTrack::create_video_track(name, RtcVideoSource::Native(source.clone()));
    let publication = room
        .local_participant()
        .publish_track(
            LocalTrack::Video(track),
            ScreenShareOptions {
                fps: FPS,
                max_width: WIDTH,
                max_height: HEIGHT,
                max_bitrate: 4_000_000,
                ..Default::default()
            }
            .publish_options(),
        )
        .await
        .expect("publish synthetic screen");
    let sending = source.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let green = Arc::new(AtomicBool::new(false));
    let source_green = green.clone();
    let latest = Arc::new(AtomicU64::new(0));
    let source_latest = latest.clone();
    let task = tokio::spawn(async move {
        let mut tick = 0_i64;
        while !stopped.load(Ordering::Acquire) {
            let mut buffer = I420Buffer::new(WIDTH, HEIGHT);
            let (y, u, v) = buffer.data_mut();
            if source_green.load(Ordering::Acquire) {
                y.fill(145 + (tick % 8) as u8);
                u.fill(54);
                v.fill(34);
            } else {
                y.fill(82 + (tick % 8) as u8);
                u.fill(90);
                v.fill(240);
            }
            // Keep the majority red/green for freshness assertions while a
            // moving 48px patch and luma modulation defeat static-frame elision.
            let left = (tick as usize * 7) % (WIDTH as usize - 48);
            for row in 0..48 {
                y[row * WIDTH as usize + left..row * WIDTH as usize + left + 48].fill(210);
            }
            // Robust grayscale16x16 cells encode the source counter. Reading
            // decoded pixels measures freshness without trusting sender clocks.
            let counter = tick as u32 + 1;
            for bit in 0..32 {
                for row in 0..16 {
                    y[row * WIDTH as usize + bit * 16..row * WIDTH as usize + bit * 16 + 16]
                        .fill(if counter & (1 << bit) != 0 { 224 } else { 32 });
                }
            }
            for row in 0..8 {
                u[row * (WIDTH / 2) as usize..row * (WIDTH / 2) as usize + 256].fill(128);
                v[row * (WIDTH / 2) as usize..row * (WIDTH / 2) as usize + 256].fill(128);
            }
            source_latest.store(counter as u64, Ordering::Release);
            sending.capture_frame(&VideoFrame {
                rotation: VideoRotation::VideoRotation0,
                timestamp_us: tick * 33_333,
                frame_metadata: None,
                buffer,
            });
            tick += 1;
            tokio::time::sleep(Duration::from_millis(33)).await;
        }
    });
    (publication.sid(), source, task, stop, green, latest)
}
fn has_color(frame: &fluxer_media::VideoFrame, green: bool) -> bool {
    frame.bgra.as_chunks::<4>().0.iter().any(|pixel| {
        if green {
            pixel[1] > 180 && pixel[0] < 100 && pixel[2] < 100
        } else {
            pixel[2] > 180 && pixel[1] < 100 && pixel[0] < 100
        }
    })
}
fn source_counter(frame: &fluxer_media::VideoFrame) -> u64 {
    (0..32).fold(0, |counter, bit| {
        let offset = (8 * frame.width as usize + bit * 16 + 8) * 4;
        counter | (u64::from(frame.bgra[offset] > 128) << bit)
    })
}
async fn sustained_video(events: &mut MediaEvents, latest: &AtomicU64, sid: &TrackSid) {
    let start = std::time::Instant::now();
    let mut count = 0;
    while start.elapsed() < Duration::from_secs(3) {
        let event = timeout(Duration::from_millis(250), events.recv())
            .await
            .expect("full-rate video never stalls250ms")
            .expect("media open");
        match event {
            MediaEvent::Video(frame) if frame.track_id == sid.to_string() => {
                count += 1;
                let gap = latest
                    .load(Ordering::Acquire)
                    .saturating_sub(source_counter(&frame));
                assert!(gap <= 8, "decoded frame stale by {gap} source frames");
            }
            MediaEvent::Error(error) => panic!("throughput media error: {error}"),
            _ => (),
        }
    }
    let fps = count as f64 / start.elapsed().as_secs_f64();
    eprintln!(
        "screen_watch: production noSimulcast{WIDTH}x{HEIGHT}/FPS{FPS} received {fps:.1}FPS ({count} frames), fresh<=8sourceframes"
    );
    assert!(
        fps >= 24.0,
        "selected30fps screen must sustain>=24fps on local fixture, got{fps:.1}"
    );
}
async fn newest_frame_after_pause(events: &mut MediaEvents, latest: &AtomicU64, sid: &TrackSid) {
    let event = until(
        events,
        |event| matches!(event,MediaEvent::Video(frame) if frame.track_id==sid.to_string()),
    )
    .await;
    let MediaEvent::Video(frame) = event else {
        unreachable!()
    };
    let gap = latest
        .load(Ordering::Acquire)
        .saturating_sub(source_counter(&frame));
    eprintln!("screen_watch: after350ms consumerpause newest-frame gap={gap} source frames");
    assert!(
        gap <= 8,
        "slow consumer received historical FIFO frame {gap} ticks behind source"
    );
}
async fn decoded(
    events: &mut MediaEvents,
    sid: &TrackSid,
    identity: &str,
    green: bool,
    generation: u64,
) {
    let event = until(
        events,
        |event| matches!(event,MediaEvent::Video(frame) if frame.track_id==sid.to_string() && frame.watch_generation==generation && has_color(frame,green)),
    )
    .await;
    let MediaEvent::Video(frame) = event else {
        unreachable!()
    };
    assert_eq!(frame.participant, identity);
    assert_eq!((frame.width, frame.height), (WIDTH, HEIGHT));
    assert_eq!(frame.bgra.len(), (WIDTH * HEIGHT * 4) as usize);
    assert!(
        has_color(&frame, green),
        "fresh source color must reach GPUI after actual decoding"
    );
}
async fn watch_lifecycle(encrypted: bool) {
    let endpoint = std::env::var("FLUXER_MEDIA_TEST_URL").expect("local SFU URL");
    assert!(
        endpoint.starts_with("ws://127.0.0.1:") || endpoint.starts_with("ws://localhost:"),
        "synthetic fixture only targets local SFU"
    );
    let output =
        std::env::var("FLUXER_MEDIA_SCREEN_TEST_OUTPUT").expect("explicit isolated speaker sink");
    assert!(
        output.starts_with("output:pipewire:fluxer-output-test-"),
        "never open physical speakers in this fixture"
    );
    let publisher_token =
        std::env::var("FLUXER_MEDIA_TEST_TOKEN_A").expect("synthetic publisher token");
    let receiver_token = std::env::var("FLUXER_MEDIA_TEST_TOKEN_B").expect("receiver token");
    let mut options = RoomOptions::default();
    options.auto_subscribe = false;
    options.single_peer_connection = false;
    const KEY: &str = "synthetic-screen-fixture-passphrase";
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
    let (publisher, _publisher_events) = Room::connect(&endpoint, &publisher_token, options)
        .await
        .expect("publisher room");
    let identity = publisher.local_participant().identity().to_string();
    let (first_sid, _source, first_task, first_stop, first_green, first_latest) =
        publish(&publisher, "fixture initial screen").await;
    let mut receiver = MediaEngine::new().expect("native media engine");
    receiver
        .set_output_device(&output)
        .expect("explicit virtual output exists");
    receiver.set_muted(true).expect("microphone stays muted");
    let mut events = receiver
        .connect_encrypted(&endpoint, &receiver_token, encrypted.then_some(KEY))
        .await
        .expect("actual MediaEngine joins room");
    // Screen already exists before receiver joins: exercise Connected roster,
    // not only TrackPublished events from a later publisher.
    until(&mut events,|event|matches!(event,MediaEvent::ScreenStreamAvailable {identity:id,available:true} if id==&identity)).await;
    no_video(&mut events).await;
    eprintln!("screen_watch: metadata-only phase passed");
    receiver
        .set_watched_participants(vec![identity.clone()])
        .expect("watch first screen");
    decoded(
        &mut events,
        &first_sid,
        &identity,
        false,
        receiver.watch_generation(&identity),
    )
    .await;
    eprintln!("screen_watch: first publication decoded");
    sustained_video(&mut events, &first_latest, &first_sid).await;
    slow_video_consumer(&events, &first_latest).await;
    newest_frame_after_pause(&mut events, &first_latest, &first_sid).await;
    slow_video_consumer(&events, &first_latest).await;
    let first_generation = receiver.watch_generation(&identity);
    let stop_started = std::time::Instant::now();
    eprintln!(
        "screen_watch: unwatch requested while newest video slot pending; awaiting VideoEnded"
    );
    receiver
        .set_watched_participants(vec![])
        .expect("stop watching");
    let ended=until(&mut events,|event|matches!(event,MediaEvent::VideoEnded{track_id,..} if track_id==&first_sid.to_string())).await;
    let local_stop = stop_started.elapsed();
    eprintln!(
        "screen_watch: encrypted={encrypted} stop call and local VideoEnded took {local_stop:?}"
    );
    assert!(
        local_stop < Duration::from_millis(250),
        "local stop must not wait for network or decryptor teardown: {local_stop:?}"
    );
    assert!(
        matches!(ended,MediaEvent::VideoEnded{participant,watch_generation,..} if participant==identity && watch_generation==first_generation)
    );
    eprintln!("screen_watch: VideoEnded received; verifying no more video");
    no_video(&mut events).await;
    eprintln!("screen_watch: no packets after unwatch; requesting same publication rewatch");
    first_green.store(true, Ordering::Release);
    receiver
        .set_watched_participants(vec![identity.clone()])
        .expect("rewatch same publication");
    decoded(
        &mut events,
        &first_sid,
        &identity,
        true,
        receiver.watch_generation(&identity),
    )
    .await;
    eprintln!("screen_watch: same publication rewatch decoded");
    slow_video_consumer(&events, &first_latest).await;
    let old_generation = receiver.watch_generation(&identity);
    receiver
        .set_watched_participants(vec![])
        .expect("rapid stop");
    first_green.store(false, Ordering::Release);
    receiver
        .set_watched_participants(vec![identity.clone()])
        .expect("rapid sameSID rewatch without draining old frames/ends");
    let current = receiver.watch_generation(&identity);
    assert_ne!(old_generation, current);
    decoded(&mut events, &first_sid, &identity, false, current).await;
    eprintln!("screen_watch: rapid rewatch fenced old queued frames and lifecycle ends");
    // Replacement SID must be observed while explicit participant watch intent
    // remains. This is not a blanket auto-subscribe to all room video.
    first_stop.store(true, Ordering::Release);
    first_task.abort();
    publisher
        .local_participant()
        .unpublish_track(&first_sid)
        .await
        .expect("end first screen");
    until(&mut events,|event|matches!(event,MediaEvent::ScreenStreamAvailable {identity:id,available:false} if id==&identity)).await;
    let (second_sid, _second_source, second_task, second_stop, _, _) =
        publish(&publisher, "fixture republished screen").await;
    assert_ne!(first_sid, second_sid);
    until(&mut events,|event|matches!(event,MediaEvent::ScreenStreamAvailable {identity:id,available:true} if id==&identity)).await;
    decoded(
        &mut events,
        &second_sid,
        &identity,
        false,
        receiver.watch_generation(&identity),
    )
    .await;
    eprintln!("screen_watch: replacement SID decoded");
    // Also prove pending watch intent works before any publication metadata.
    second_stop.store(true, Ordering::Release);
    second_task.abort();
    publisher
        .local_participant()
        .unpublish_track(&second_sid)
        .await
        .expect("end second screen");
    until(&mut events,|event|matches!(event,MediaEvent::ScreenStreamAvailable {identity:id,available:false} if id==&identity)).await;
    receiver
        .set_watched_participants(vec![])
        .expect("clear watch");
    receiver
        .set_watched_participants(vec![identity.clone()])
        .expect("watch before metadata exists");
    let (late_sid, _late_source, late_task, late_stop, _, _) =
        publish(&publisher, "fixture late screen").await;
    decoded(
        &mut events,
        &late_sid,
        &identity,
        false,
        receiver.watch_generation(&identity),
    )
    .await;
    eprintln!("screen_watch: pending watch before metadata decoded");
    late_stop.store(true, Ordering::Release);
    late_task.abort();
    receiver.disconnect().await.expect("release receiver");
    publisher.close().await.expect("release publisher");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires disposable local LiveKit and isolated virtual output"]
async fn engine_watch_decodes_late_metadata_unwatch_rewatch_and_republish() {
    watch_lifecycle(false).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires disposable local LiveKit and isolated virtual output"]
async fn encrypted_engine_watch_retains_receiver_and_restores_decryptor_on_rewatch() {
    watch_lifecycle(true).await;
}
