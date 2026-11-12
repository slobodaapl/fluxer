// SPDX-License-Identifier: AGPL-3.0-or-later
//! Opt-in test against a disposable local LiveKit server. Uses synthetic sources;
//! never opens a microphone, speaker, display, or window capture session.
use futures_util::StreamExt;
use livekit::{
    options::TrackPublishOptions,
    prelude::*,
    webrtc::{
        audio_frame::AudioFrame,
        audio_source::{AudioSourceOptions, native::NativeAudioSource},
        audio_stream::native::NativeAudioStream,
        video_frame::{I420Buffer, VideoFrame, VideoRotation},
        video_source::{RtcVideoSource, VideoResolution, native::NativeVideoSource},
        video_stream::native::NativeVideoStream,
    },
};
use std::{borrow::Cow, time::Duration};
use tokio::time::timeout;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires local LiveKit server; run scripts/test_media_loopback.py"]
async fn exchanges_synthetic_audio_and_screen_video() {
    let url = std::env::var("FLUXER_MEDIA_TEST_URL").expect("test URL");
    let a = std::env::var("FLUXER_MEDIA_TEST_TOKEN_A").expect("publisher test token");
    let b = std::env::var("FLUXER_MEDIA_TEST_TOKEN_B").expect("receiver test token");
    let (receiver, mut events) = Room::connect(&url, &b, RoomOptions::default())
        .await
        .expect("receiver connect");
    let (publisher, _publisher_events) = Room::connect(&url, &a, RoomOptions::default())
        .await
        .expect("publisher connect");
    let audio_source = NativeAudioSource::new(AudioSourceOptions::default(), 48_000, 1, 100);
    let audio = LocalAudioTrack::create_audio_track(
        "synthetic microphone",
        RtcAudioSource::Native(audio_source.clone()),
    );
    publisher
        .local_participant()
        .publish_track(
            LocalTrack::Audio(audio.clone()),
            TrackPublishOptions {
                source: TrackSource::Microphone,
                ..Default::default()
            },
        )
        .await
        .expect("publish audio");
    let video_source = NativeVideoSource::new(
        VideoResolution {
            width: 320,
            height: 180,
        },
        true,
    );
    let video = LocalVideoTrack::create_video_track(
        "synthetic screen",
        RtcVideoSource::Native(video_source.clone()),
    );
    publisher
        .local_participant()
        .publish_track(
            LocalTrack::Video(video),
            TrackPublishOptions {
                source: TrackSource::Screenshare,
                ..Default::default()
            },
        )
        .await
        .expect("publish screen");
    let (remote_audio, remote_video) = timeout(Duration::from_secs(20), async {
        let (mut a, mut v) = (None, None);
        loop {
            match events.recv().await.expect("room events") {
                RoomEvent::TrackSubscribed {
                    track: RemoteTrack::Audio(track),
                    ..
                } => a = Some(track),
                RoomEvent::TrackSubscribed {
                    track: RemoteTrack::Video(track),
                    ..
                } => v = Some(track),
                RoomEvent::TrackSubscriptionFailed { error, .. } => panic!("subscription: {error}"),
                _ => {}
            }
            if let (Some(audio), Some(video)) = (&a, &v) {
                break (audio.clone(), video.clone());
            }
        }
    })
    .await
    .expect("remote tracks subscribed before deadline");
    let mut audio_stream = NativeAudioStream::new(remote_audio.rtc_track(), 48_000, 1);
    let mut video_stream = NativeVideoStream::new(remote_video.rtc_track());
    let sender = tokio::spawn(async move {
        for tick in 0..300 {
            let pcm: Vec<i16> = (0..480)
                .map(|n| {
                    (((tick * 480 + n) as f64 * std::f64::consts::TAU * 440.0 / 48_000.0).sin()
                        * 8000.0) as i16
                })
                .collect();
            audio_source
                .capture_frame(&AudioFrame {
                    data: Cow::Owned(pcm),
                    sample_rate: 48_000,
                    num_channels: 1,
                    samples_per_channel: 480,
                })
                .await
                .expect("capture audio");
            if tick % 3 == 0 {
                let mut buffer = I420Buffer::new(320, 180);
                let (y, u, v) = buffer.data_mut();
                y.fill(82);
                u.fill(90);
                v.fill(240); // Red in YUV.
                video_source.capture_frame(&VideoFrame {
                    rotation: VideoRotation::VideoRotation0,
                    timestamp_us: tick * 10_000,
                    frame_metadata: None,
                    buffer,
                });
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });
    timeout(Duration::from_secs(10), async {
        loop {
            let frame = audio_stream.next().await.expect("received audio");
            if frame.data.iter().any(|s| s.abs() > 100) {
                break;
            }
        }
    })
    .await
    .expect("non-silent PCM arrived");
    let frame = timeout(Duration::from_secs(10), video_stream.next())
        .await
        .expect("video deadline")
        .expect("video frame");
    assert_eq!((frame.buffer.width(), frame.buffer.height()), (320, 180));
    let yuv = frame.buffer.to_i420();
    let (y, _, _) = yuv.data();
    assert!(
        y.iter().any(|&y| y > 60 && y < 110),
        "received encoded screen pixels"
    );
    audio.mute();
    assert!(audio.is_muted());
    audio.unmute();
    assert!(!audio.is_muted());
    sender.abort();
    audio_stream.close();
    video_stream.close();
    publisher.close().await.expect("publisher close");
    receiver.close().await.expect("receiver close");
}
