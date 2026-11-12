// SPDX-License-Identifier: AGPL-3.0-or-later
//! Hardware smoke checks, run explicitly on the development desktop. The idle
//! case never starts capture/playback. The local call case exercises device
//! setup with the microphone muted; personal audio is never published or saved.
use fluxer_media::MediaEngine;

#[tokio::test]
#[ignore = "requires access to the desktop audio service; no capture is started"]
async fn idle_setup_and_repeated_teardown_do_not_require_recording() {
    let mut media = MediaEngine::new().expect("create idle device manager");
    for _ in 0..3 {
        // Worker.connect_voice's pre-connect operations, including the second
        // disconnect inside connect_encrypted, must work without a prior call.
        media.stop_microphone_test().unwrap();
        media.disconnect().await.expect("idle disconnect");
        media
            .configure_microphone_publish(Some(64_000.), false)
            .unwrap();
        media.set_muted(true).unwrap();
        media.set_deafened(true).unwrap();
        media.set_muted(false).unwrap();
        media.set_deafened(false).unwrap();
        media.disconnect().await.expect("repeated idle disconnect");
    }
    if let Some(input) = media.devices().inputs.first() {
        media
            .set_input_device(&input.id)
            .expect("select idle input");
    }
    media
        .disconnect()
        .await
        .expect("disconnect after selection");
}

#[tokio::test]
#[ignore = "requires desktop audio and a local LiveKit server; microphone stays muted"]
async fn muted_call_joins_leaves_and_rejoins_with_real_device_lifecycle() {
    let endpoint = std::env::var("FLUXER_MEDIA_TEST_URL").expect("local server URL");
    let token = std::env::var("FLUXER_MEDIA_TEST_TOKEN_A").expect("local test token");
    let mut media = MediaEngine::new().expect("create idle engine");
    media.set_muted(true).unwrap();
    for _ in 0..2 {
        media.stop_microphone_test().unwrap();
        media.disconnect().await.expect("pre-join teardown");
        media
            .configure_microphone_publish(Some(64_000.), false)
            .unwrap();
        media.set_muted(true).unwrap();
        media.set_deafened(false).unwrap();
        let _events = media
            .connect(&endpoint, &token)
            .await
            .expect("muted voice join");
        assert!(media.is_connected());
        media.set_deafened(true).expect("deafen while muted");
        media.set_deafened(false).expect("undeafen while muted");
        media.disconnect().await.expect("leave muted call");
        media.disconnect().await.expect("repeated leave");
    }
}
