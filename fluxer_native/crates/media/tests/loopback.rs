// SPDX-License-Identifier: AGPL-3.0-or-later
//! Opt-in test against a disposable local LiveKit server. Uses synthetic sources;
//! never opens a microphone, speaker, display, or window capture session.
use futures_util::StreamExt;
use livekit::{
    options::{AudioEncoding, TrackPublishOptions},
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
use std::{
    borrow::Cow,
    sync::{
        Arc,
        atomic::{AtomicI16, Ordering},
    },
    time::Duration,
};
use tokio::{sync::mpsc, time::timeout};

async fn subscribed(
    events: &mut mpsc::UnboundedReceiver<RoomEvent>,
    sid: &TrackSid,
) -> RemoteTrack {
    timeout(Duration::from_secs(20), async {
        loop {
            match events.recv().await.expect("room events") {
                RoomEvent::TrackSubscribed {
                    track, publication, ..
                } => {
                    assert_eq!(
                        &publication.sid(),
                        sid,
                        "only the explicitly requested track may subscribe"
                    );
                    return track;
                }
                RoomEvent::TrackSubscriptionFailed { error, .. } => panic!("subscription: {error}"),
                _ => {}
            }
        }
    })
    .await
    .expect("explicit remote subscription before deadline")
}
async fn unsubscribed(events: &mut mpsc::UnboundedReceiver<RoomEvent>, sid: &TrackSid) {
    timeout(Duration::from_secs(10), async {
        loop {
            match events.recv().await.expect("room events") {
                RoomEvent::TrackUnsubscribed { publication, .. } if &publication.sid() == sid => {
                    break;
                }
                RoomEvent::TrackSubscriptionFailed { error, .. } => panic!("subscription: {error}"),
                _ => {}
            }
        }
    })
    .await
    .expect("explicit track unsubscription before deadline");
}
async fn audible(stream: &mut NativeAudioStream, threshold: u16) {
    timeout(Duration::from_secs(10), async {
        loop {
            let frame = stream.next().await.expect("received audio");
            if frame
                .data
                .iter()
                .any(|sample| sample.unsigned_abs() > threshold)
            {
                break;
            }
        }
    })
    .await
    .expect("fresh non-silent PCM arrived");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires local LiveKit server; run scripts/test_media_loopback.py"]
async fn exchanges_synthetic_audio_and_screen_video() {
    let url = std::env::var("FLUXER_MEDIA_TEST_URL").expect("test URL");
    let a = std::env::var("FLUXER_MEDIA_TEST_TOKEN_A").expect("publisher test token");
    let b = std::env::var("FLUXER_MEDIA_TEST_TOKEN_B").expect("receiver test token");
    let mut room_options = RoomOptions::default();
    // VoiceEngineV2AppConnectionHostAdapter.ts171-176,202-207: metadata first,
    // microphone subscription on effective-deaf policy, streams on watch intent.
    room_options.auto_subscribe = false;
    room_options.dynacast = true;
    room_options.single_peer_connection = false;
    let (receiver, mut events) = Room::connect(&url, &b, room_options.clone())
        .await
        .expect("receiver connect");
    let (publisher, _publisher_events) = Room::connect(&url, &a, room_options)
        .await
        .expect("publisher connect");
    let identity = publisher.local_participant().identity();
    let audio_source = NativeAudioSource::new(AudioSourceOptions::default(), 48_000, 1, 100);
    let audio = LocalAudioTrack::create_audio_track(
        "synthetic microphone",
        RtcAudioSource::Native(audio_source.clone()),
    );
    let mic = publisher
        .local_participant()
        .publish_track(
            LocalTrack::Audio(audio.clone()),
            TrackPublishOptions {
                source: TrackSource::Microphone,
                audio_encoding: Some(AudioEncoding {
                    max_bitrate: 64_000,
                }),
                dtx: false,
                red: true,
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
    let screen = publisher
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

    let (remote_mic, remote_screen) = timeout(Duration::from_secs(20), async {
        let (mut microphone, mut video) = (None, None);
        loop {
            match events.recv().await.expect("room events") {
                RoomEvent::TrackPublished {
                    publication,
                    participant,
                } => {
                    assert_eq!(participant.identity(), identity);
                    assert!(!publication.is_desired());
                    assert!(!publication.is_subscribed());
                    assert!(publication.track().is_none());
                    match publication.source() {
                        TrackSource::Microphone => {
                            assert_eq!(publication.sid(), mic.sid());
                            microphone = Some(publication);
                        }
                        TrackSource::Screenshare => {
                            assert_eq!(publication.sid(), screen.sid());
                            video = Some(publication);
                        }
                        other => panic!("unexpected fixture source {other:?}"),
                    }
                }
                RoomEvent::TrackSubscribed { .. } => panic!(
                    "auto_subscribe:false must expose publications without auto-delivering tracks"
                ),
                RoomEvent::TrackSubscriptionFailed { error, .. } => {
                    panic!("unexpected automatic subscription failure: {error}")
                }
                _ => {}
            }
            if let (Some(microphone), Some(video)) = (&microphone, &video) {
                break (microphone.clone(), video.clone());
            }
        }
    })
    .await
    .expect("both publications observed before subscription");

    // Feed real encoded media while there is still no subscription. The server
    // must not auto-deliver a screen merely because its publisher is active.
    let amplitude = Arc::new(AtomicI16::new(4_000));
    let sending_amplitude = amplitude.clone();
    let sender = tokio::spawn(async move {
        for tick in 0_i64..18_000 {
            let gain = sending_amplitude.load(Ordering::Relaxed) as f64;
            let pcm = (0..480)
                .map(|n| {
                    (((tick * 480 + n) as f64 * std::f64::consts::TAU * 440. / 48_000.).sin()
                        * gain) as i16
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
                v.fill(240);
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
    let no_automatic_tracks = timeout(Duration::from_millis(300), async {
        while let Some(event) = events.recv().await {
            if matches!(event, RoomEvent::TrackSubscribed { .. }) {
                panic!("active publisher caused an unrequested subscription");
            }
        }
    })
    .await;
    assert!(
        no_automatic_tracks.is_err(),
        "room event stream stayed open during the no-subscription observation"
    );
    assert!(!remote_mic.is_desired());
    assert!(!remote_screen.is_desired());

    // Undeafened policy subscribes only the microphone; screen metadata remains
    // available, without receiving its video until explicit watch intent below.
    remote_mic.set_subscribed(true);
    let RemoteTrack::Audio(remote_audio) = subscribed(&mut events, &mic.sid()).await else {
        panic!("microphone subscribed as audio")
    };
    assert!(!remote_screen.is_desired());
    assert!(remote_screen.track().is_none());
    let mut audio_stream = NativeAudioStream::new(remote_audio.rtc_track(), 48_000, 1);
    audible(&mut audio_stream, 100).await;

    // Choosing this participant's screen changes the actual SFU subscription.
    remote_screen.set_subscribed(true);
    let RemoteTrack::Video(remote_video) = subscribed(&mut events, &screen.sid()).await else {
        panic!("screen subscribed as video")
    };
    let mut video_stream = NativeVideoStream::new(remote_video.rtc_track());
    let frame = timeout(Duration::from_secs(10), video_stream.next())
        .await
        .expect("video deadline")
        .expect("video frame");
    assert_eq!((frame.buffer.width(), frame.buffer.height()), (320, 180));
    let yuv = frame.buffer.to_i420();
    let (y, _, _) = yuv.data();
    assert!(
        y.iter().any(|&value| value > 60 && value < 110),
        "received encoded synthetic screen pixels"
    );

    // Stopping the watch drops video only. Raising the generated PCM amplitude
    // proves the microphone continues carrying *new* packets after unwatch,
    // rather than satisfying this check from audio queued before the change.
    remote_screen.set_subscribed(false);
    unsubscribed(&mut events, &screen.sid()).await;
    assert!(!remote_screen.is_desired());
    assert!(remote_screen.track().is_none());
    assert!(remote_mic.is_desired());
    assert!(remote_mic.is_subscribed());
    amplitude.store(12_000, Ordering::Relaxed);
    audible(&mut audio_stream, 8_000).await;

    // Effective deafen explicitly removes microphone demand, and undeafen
    // restores it without re-subscribing the screen that was stopped above.
    remote_mic.set_subscribed(false);
    unsubscribed(&mut events, &mic.sid()).await;
    assert!(!remote_mic.is_desired());
    audio_stream.close();
    remote_mic.set_subscribed(true);
    let RemoteTrack::Audio(restored) = subscribed(&mut events, &mic.sid()).await else {
        panic!("restored microphone")
    };
    let mut restored_audio = NativeAudioStream::new(restored.rtc_track(), 48_000, 1);
    audible(&mut restored_audio, 8_000).await;
    assert!(!remote_screen.is_desired());

    audio.mute();
    assert!(audio.is_muted());
    audio.unmute();
    assert!(!audio.is_muted());
    sender.abort();
    restored_audio.close();
    video_stream.close();
    publisher.close().await.expect("publisher close");
    receiver.close().await.expect("receiver close");
}
