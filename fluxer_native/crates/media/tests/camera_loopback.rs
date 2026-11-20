// SPDX-License-Identifier: AGPL-3.0-or-later
//! Disposable LiveKit camera/screen test. Both sources are generated I420;
//! no camera, microphone, screen, account or personal data is accessed.
use futures_util::StreamExt;
use livekit::{
    options::TrackPublishOptions,
    prelude::*,
    webrtc::{
        video_frame::{I420Buffer, VideoFrame, VideoRotation},
        video_source::{RtcVideoSource, VideoResolution, native::NativeVideoSource},
        video_stream::native::NativeVideoStream,
    },
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};
use tokio::time::timeout;

fn source(screen: bool) -> NativeVideoSource {
    NativeVideoSource::new(
        VideoResolution {
            width: 320,
            height: 180,
        },
        screen,
    )
}
fn frame(source: &NativeVideoSource, value: u8, timestamp_us: i64) {
    let mut buffer = I420Buffer::new(320, 180);
    let (y, u, v) = buffer.data_mut();
    y.fill(value);
    u.fill(128);
    v.fill(128);
    source.capture_frame(&VideoFrame {
        rotation: VideoRotation::VideoRotation0,
        timestamp_us,
        frame_metadata: None,
        buffer,
    });
}
async fn pixels(stream: &mut NativeVideoStream, expected: u8) {
    timeout(Duration::from_secs(15), async {
        loop {
            let video = stream.next().await.expect("remote video stream");
            let buffer = video.buffer.to_i420();
            let (y, _, _) = buffer.data();
            if y.iter().any(|value| value.abs_diff(expected) < 15) {
                break;
            }
        }
    })
    .await
    .expect("expected synthetic pixels");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires disposable local LiveKit; run scripts/test_media_loopback.py --camera"]
async fn camera_and_screen_subscriptions_are_independent() {
    let url = std::env::var("FLUXER_MEDIA_TEST_URL").unwrap();
    let a = std::env::var("FLUXER_MEDIA_TEST_TOKEN_A").unwrap();
    let b = std::env::var("FLUXER_MEDIA_TEST_TOKEN_B").unwrap();
    let mut options = RoomOptions::default();
    options.auto_subscribe = false;
    options.dynacast = true;
    options.single_peer_connection = false;
    let (receiver, mut events) = Room::connect(&url, &b, options.clone()).await.unwrap();
    let (publisher, _events) = Room::connect(&url, &a, options).await.unwrap();
    let camera_source = source(false);
    let screen_source = source(true);
    let camera = publisher
        .local_participant()
        .publish_track(
            LocalTrack::Video(LocalVideoTrack::create_video_track(
                "synthetic camera",
                RtcVideoSource::Native(camera_source.clone()),
            )),
            TrackPublishOptions {
                source: TrackSource::Camera,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let screen = publisher
        .local_participant()
        .publish_track(
            LocalTrack::Video(LocalVideoTrack::create_video_track(
                "synthetic screen",
                RtcVideoSource::Native(screen_source.clone()),
            )),
            TrackPublishOptions {
                source: TrackSource::Screenshare,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let (remote_camera, remote_screen) = timeout(Duration::from_secs(20), async {
        let (mut cam, mut scr) = (None, None);
        loop {
            match events.recv().await.expect("room events") {
                RoomEvent::TrackPublished { publication, .. } => {
                    assert!(!publication.is_desired());
                    match publication.source() {
                        TrackSource::Camera => {
                            assert_eq!(publication.sid(), camera.sid());
                            cam = Some(publication);
                        }
                        TrackSource::Screenshare => {
                            assert_eq!(publication.sid(), screen.sid());
                            scr = Some(publication);
                        }
                        other => panic!("unexpected source {other:?}"),
                    }
                }
                RoomEvent::TrackSubscribed { .. } => panic!("unexpected automatic subscription"),
                _ => {}
            }
            if let (Some(cam), Some(scr)) = (&cam, &scr) {
                break (cam.clone(), scr.clone());
            }
        }
    })
    .await
    .expect("publication metadata");
    let screen_luma = Arc::new(AtomicU8::new(170));
    let sending_luma = screen_luma.clone();
    let sending = tokio::spawn(async move {
        for tick in 0..1800_i64 {
            frame(&camera_source, 70, tick * 33_333);
            frame(
                &screen_source,
                sending_luma.load(Ordering::Relaxed),
                tick * 33_333,
            );
            tokio::time::sleep(Duration::from_millis(33)).await;
        }
    });
    remote_camera.set_subscribed(true);
    let remote_video = timeout(Duration::from_secs(20), async {
        loop {
            if let RoomEvent::TrackSubscribed {
                track, publication, ..
            } = events.recv().await.unwrap()
            {
                assert_eq!(publication.sid(), camera.sid());
                if let RemoteTrack::Video(video) = track {
                    break video;
                }
                panic!("camera was not video");
            }
        }
    })
    .await
    .unwrap();
    assert!(!remote_screen.is_desired());
    assert!(remote_screen.track().is_none());
    let mut camera_stream = NativeVideoStream::new(remote_video.rtc_track());
    pixels(&mut camera_stream, 70).await;
    remote_screen.set_subscribed(true);
    let remote_video = timeout(Duration::from_secs(20), async {
        loop {
            if let RoomEvent::TrackSubscribed {
                track, publication, ..
            } = events.recv().await.unwrap()
            {
                assert_eq!(publication.sid(), screen.sid());
                if let RemoteTrack::Video(video) = track {
                    break video;
                }
                panic!("screen was not video");
            }
        }
    })
    .await
    .unwrap();
    let mut screen_stream = NativeVideoStream::new(remote_video.rtc_track());
    pixels(&mut screen_stream, 170).await;
    publisher
        .local_participant()
        .unpublish_track(&camera.sid())
        .await
        .unwrap();
    timeout(Duration::from_secs(10), async {
        loop {
            if let RoomEvent::TrackUnpublished { publication, .. } = events.recv().await.unwrap()
                && publication.sid() == camera.sid()
            {
                break;
            }
        }
    })
    .await
    .expect("camera unpublished");
    assert!(remote_screen.is_desired());
    assert!(remote_screen.is_subscribed());
    screen_luma.store(210, Ordering::Relaxed);
    pixels(&mut screen_stream, 210).await;
    sending.abort();
    camera_stream.close();
    screen_stream.close();
    publisher.close().await.unwrap();
    receiver.close().await.unwrap();
}
