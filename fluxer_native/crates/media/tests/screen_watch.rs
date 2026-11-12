// SPDX-License-Identifier: AGPL-3.0-or-later
//! Actual MediaEngine receiver, not a direct SDK subscription approximation.
//! Only synthetic video and a deliberately isolated virtual speaker sink are
//! used. The receiver microphone remains muted throughout the fixture.
use fluxer_media::{MediaEngine, MediaEvent};
use livekit::{
    options::TrackPublishOptions,
    prelude::*,
    webrtc::{
        video_frame::{I420Buffer, VideoFrame, VideoRotation},
        video_source::{RtcVideoSource, VideoResolution, native::NativeVideoSource},
    },
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{sync::mpsc, time::timeout};

async fn until(
    events: &mut mpsc::Receiver<MediaEvent>,
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
async fn no_video(events: &mut mpsc::Receiver<MediaEvent>) {
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
async fn publish(
    room: &Room,
    name: &str,
) -> (
    TrackSid,
    NativeVideoSource,
    tokio::task::JoinHandle<()>,
    Arc<AtomicBool>,
    Arc<AtomicBool>,
) {
    let source = NativeVideoSource::new(
        VideoResolution {
            width: 320,
            height: 180,
        },
        true,
    );
    let track = LocalVideoTrack::create_video_track(name, RtcVideoSource::Native(source.clone()));
    let publication = room
        .local_participant()
        .publish_track(
            LocalTrack::Video(track),
            TrackPublishOptions {
                source: TrackSource::Screenshare,
                ..Default::default()
            },
        )
        .await
        .expect("publish synthetic screen");
    let sending = source.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let green = Arc::new(AtomicBool::new(false));
    let source_green = green.clone();
    let task = tokio::spawn(async move {
        let mut tick = 0_i64;
        while !stopped.load(Ordering::Acquire) {
            let mut buffer = I420Buffer::new(320, 180);
            let (y, u, v) = buffer.data_mut();
            if source_green.load(Ordering::Acquire) {
                y.fill(145);
                u.fill(54);
                v.fill(34);
            } else {
                y.fill(82);
                u.fill(90);
                v.fill(240);
            }
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
    (publication.sid(), source, task, stop, green)
}
fn has_color(frame: &fluxer_media::VideoFrame, green: bool) -> bool {
    frame.rgba.as_chunks::<4>().0.iter().any(|pixel| {
        if green {
            pixel[1] > 180 && pixel[0] < 100 && pixel[2] < 100
        } else {
            pixel[0] > 180 && pixel[1] < 100 && pixel[2] < 100
        }
    })
}
async fn decoded(
    events: &mut mpsc::Receiver<MediaEvent>,
    sid: &TrackSid,
    identity: &str,
    green: bool,
) {
    let event = until(
        events,
        |event| matches!(event,MediaEvent::Video(frame) if frame.track_id==sid.to_string() && has_color(frame,green)),
    )
    .await;
    let MediaEvent::Video(frame) = event else {
        unreachable!()
    };
    assert_eq!(frame.participant, identity);
    assert_eq!((frame.width, frame.height), (320, 180));
    assert_eq!(frame.rgba.len(), 320 * 180 * 4);
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
    let (first_sid, _source, first_task, first_stop, first_green) =
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
    decoded(&mut events, &first_sid, &identity, false).await;
    eprintln!("screen_watch: first publication decoded");
    eprintln!("screen_watch: unwatch requested; awaiting VideoEnded");
    receiver
        .set_watched_participants(vec![])
        .expect("stop watching");
    until(&mut events,|event|matches!(event,MediaEvent::VideoEnded{track_id} if track_id==&first_sid.to_string())).await;
    eprintln!("screen_watch: VideoEnded received; verifying no more video");
    no_video(&mut events).await;
    eprintln!("screen_watch: no packets after unwatch; requesting same publication rewatch");
    first_green.store(true, Ordering::Release);
    receiver
        .set_watched_participants(vec![identity.clone()])
        .expect("rewatch same publication");
    decoded(&mut events, &first_sid, &identity, true).await;
    eprintln!("screen_watch: same publication rewatch decoded");
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
    let (second_sid, _second_source, second_task, second_stop, _) =
        publish(&publisher, "fixture republished screen").await;
    assert_ne!(first_sid, second_sid);
    until(&mut events,|event|matches!(event,MediaEvent::ScreenStreamAvailable {identity:id,available:true} if id==&identity)).await;
    decoded(&mut events, &second_sid, &identity, false).await;
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
    let (late_sid, _late_source, late_task, late_stop, _) =
        publish(&publisher, "fixture late screen").await;
    decoded(&mut events, &late_sid, &identity, false).await;
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
