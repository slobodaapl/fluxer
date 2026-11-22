use super::*;
use futures_util::{SinkExt, StreamExt};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    time::timeout,
};
use tokio_tungstenite::{accept_hdr_async, tungstenite::Message as WireMessage};

#[path = "../../../../fluxer_app/rust/libfluxcore/src/zstd_stream.rs"]
mod wire_compression;
use wire_compression::{ZstdStreamDecoder, ZstdStreamEncoder};

fn compressed(value: Value, encoder: &mut ZstdStreamEncoder) -> WireMessage {
    WireMessage::Binary(
        encoder
            .compress_chunk(value.to_string().as_bytes())
            .unwrap_or_else(|error| panic!("{}", error.message()))
            .into(),
    )
}
fn uncompressed(message: WireMessage, decoder: &mut ZstdStreamDecoder) -> Value {
    match message {
        WireMessage::Text(text) => serde_json::from_str(&text).unwrap(),
        WireMessage::Binary(bytes) => serde_json::from_slice(
            &decoder
                .decompress_chunk(&bytes)
                .unwrap_or_else(|error| panic!("{}", error.message())),
        )
        .unwrap(),
        _ => panic!("expected a JSON gateway payload"),
    }
}

fn worker() -> (Worker, mpsc::Receiver<Event>) {
    let (tx, rx) = mpsc::channel();
    (Worker::new(tx, Arc::new(Mutex::new(BTreeMap::new()))), rx)
}

#[cfg(target_os = "macos")]
#[test]
fn explicit_audio_refresh_republishes_the_current_inventory() {
    let controller = crate::Controller::spawn_with_store(Arc::new(
        fluxer_session::MemorySessionStore::default(),
    ))
    .unwrap();
    let refresh = || {
        controller.send(Command::RefreshDevices).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            match controller.try_recv_control() {
                Some(Event::Devices(devices)) => return devices,
                Some(Event::Error(message)) => panic!("{message}"),
                Some(_) => {}
                None => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        panic!("explicit refresh did not publish an audio device inventory");
    };
    for _ in 0..2 {
        let inventory = refresh();
        assert!(
            inventory
                .inputs
                .iter()
                .any(|device| device.id == "input:default")
        );
        assert!(
            inventory
                .outputs
                .iter()
                .any(|device| device.id == "output:default")
        );
    }
}

async fn finish_attachment_send(worker: &mut Worker, channel: Snowflake) {
    timeout(Duration::from_secs(5), async {
        loop {
            let event = worker.attachments.rx.recv().await.expect("attachment task event");
            let finished = matches!(&event, attachment_flow::AttachmentEvent::Sent { channel: target, .. } if *target == channel);
            worker.attachment_event(event);
            if finished { break; }
        }
    }).await.expect("message HTTP task did not complete");
}

async fn finish_history(worker: &mut Worker, channel: Snowflake) {
    timeout(Duration::from_secs(5),async {
        loop {
            let event=worker.navigation.rx.recv().await.expect("navigation task event");
            let finished=matches!(&event,navigation_flow::NavigationEvent::History{channel:target,..}if *target==channel);
            worker.navigation_event(event);
            if finished {break;}
        }
    }).await.expect("history HTTP task did not complete");
}

fn voice_update(channel: Option<&str>, connection: &str) -> VoiceServerUpdate {
    serde_json::from_value(json!({"token":"voice-secret","endpoint":"wss://voice.example","connection_id":connection,"guild_id":"10","channel_id":channel})).unwrap()
}

#[test]
fn camera_voice_state_is_scoped_to_the_active_connection() {
    let target = VoiceTarget {
        guild_id: Some(10.into()),
        channel_id: 100.into(),
        connection_id: Some("local".into()),
    };
    let mut state = ClientState::default();
    state.voice.join(100.into());
    assert!(!target.payload(&state, false, &BTreeSet::new()).self_video);
    state.voice.camera = true;
    let active = target.payload(&state, false, &BTreeSet::new());
    assert!(active.self_video);
    assert_eq!(active.connection_id.as_deref(), Some("local"));
    assert!(!target.payload(&state, true, &BTreeSet::new()).self_video);
    state.voice.disconnect();
    state.voice.join(100.into());
    assert!(!target.payload(&state, false, &BTreeSet::new()).self_video);
}

#[test]
fn discovery_identity_selects_the_login_field_independently_of_input_text() {
    // AccountSignInIdentifier.ts:10-11; AccountIdentityFeatures.ts:13-18.
    let mut instance =
        Instance::from_endpoints("https://api.example/api", "wss://gateway.example").unwrap();
    instance.features = json!({"account_identity":"username"});
    assert!(
        matches!(login_identifier(&instance,"alice@example.com".into()),LoginIdentifier::Login(value) if value=="alice@example.com")
    );
    instance.features = json!({"account_identity":"email"});
    assert!(
        matches!(login_identifier(&instance,"plain-identifier".into()),LoginIdentifier::Email(value) if value=="plain-identifier")
    );
    instance.features = json!({});
    assert!(matches!(
        login_identifier(&instance, "plain-identifier".into()),
        LoginIdentifier::Email(_)
    ));
}

#[test]
fn auth_profile_without_id_uses_separate_authoritative_user_id_and_keeps_private_fields() {
    let user=authentication_user(Some(json!({"username":"alice","discriminator":"0000","email":"alice@example.com","global_name":"Alice"})),Some("1")).unwrap().unwrap();
    assert_eq!(user.id, Snowflake(1));
    assert_eq!(user.extra.get("email"), Some(&json!("alice@example.com")));
}

#[test]
fn watched_stream_keys_follow_existing_identity_and_stream_key_source() {
    let target = VoiceTarget {
        guild_id: Some(10.into()),
        channel_id: 100.into(),
        connection_id: Some("local".into()),
    };
    assert_eq!(
        parse_participant_identity("user_123_connection_with_underscores"),
        Some(("123", "connection_with_underscores"))
    );
    assert_eq!(
        stream_key(&target, "user_123_connection_with_underscores"),
        Some("10:100:connection_with_underscores".into())
    );
    let dm = VoiceTarget {
        guild_id: None,
        ..target.clone()
    };
    assert_eq!(
        stream_key(&dm, "user_123_connection"),
        Some("dm:100:connection".into())
    );
    for invalid in [
        "user_no_connection",
        "user_123_",
        "user__id",
        "user_1_line\nbreak",
        "invalid",
    ] {
        assert!(parse_participant_identity(invalid).is_none());
    }
}

#[tokio::test]
async fn watch_intent_is_explicit_in_every_voice_payload_and_pruned_without_capture() {
    let (mut worker, events) = worker();
    let (tx, mut outgoing) = async_mpsc::channel(16);
    worker.gateway_commands = Some(tx);
    worker.voice_target = Some(VoiceTarget {
        guild_id: Some(10.into()),
        channel_id: 100.into(),
        connection_id: Some("local".into()),
    });
    worker.state.voice.join(100.into());
    let identity = "user_123_remote_connection".to_owned();
    assert!(
        worker
            .command(Command::WatchStream {
                identity: identity.clone(),
                watch: true
            })
            .await
            .is_err()
    );
    assert!(
        worker.media.is_none(),
        "watching cannot create an audio engine"
    );
    worker
        .state
        .apply_gateway("VOICE_STATE_UPDATE", json!({"guild_id":"10","channel_id":"100","user_id":"123","connection_id":"remote_connection","self_stream":true}))
        .unwrap();
    worker.sync_stream_availability().await.unwrap();
    assert!(
        worker.watched_streams.is_empty(),
        "availability does not automatically watch streams"
    );
    worker
        .set_watch(identity.clone(), true, true)
        .await
        .unwrap();
    let GatewayCommand::VoiceState(payload) = outgoing.try_recv().unwrap() else {
        panic!("expected voice state");
    };
    assert_eq!(payload.viewer_stream_keys, ["10:100:remote_connection"]);
    worker.command(Command::SetMute(true)).await.unwrap();
    let GatewayCommand::VoiceState(payload) = outgoing.try_recv().unwrap() else {
        panic!("expected voice state");
    };
    assert!(payload.self_mute);
    assert_eq!(payload.viewer_stream_keys, ["10:100:remote_connection"]);
    worker
        .media_event(MediaEvent::ScreenStreamAvailable {
            identity: identity.clone(),
            available: false,
        })
        .await
        .unwrap();
    assert!(
        worker.watched_streams.contains(&identity),
        "temporary metadata loss preserves a gateway-live watch"
    );
    worker
        .state
        .apply_gateway(
            "VOICE_STATE_UPDATE",
            json!({"user_id":"123","connection_id":"remote_connection","self_stream":false}),
        )
        .unwrap();
    worker.sync_stream_availability().await.unwrap();
    let GatewayCommand::VoiceState(payload) = outgoing.try_recv().unwrap() else {
        panic!("expected voice state");
    };
    assert!(payload.viewer_stream_keys.is_empty());
    assert!(worker.watched_streams.is_empty());
    assert!(events.try_iter().any(|event|matches!(event,Event::WatchedStreams(ref streams) if streams==&vec![identity.clone()])));
    worker
        .state
        .apply_gateway(
            "VOICE_STATE_UPDATE",
            json!({"user_id":"123","connection_id":"remote_connection","self_stream":true}),
        )
        .unwrap();
    worker.sync_stream_availability().await.unwrap();
    worker
        .set_watch(identity.clone(), true, true)
        .await
        .unwrap();
    worker
        .media_event(MediaEvent::ParticipantLeft {
            identity: identity.clone(),
        })
        .await
        .unwrap();
    assert!(worker.watched_streams.contains(&identity));
    worker.leave_voice(true).await.unwrap();
    assert!(worker.watched_streams.is_empty());
    assert!(worker.available_streams.is_empty());
    assert!(worker.media.is_none());
}

fn watch_worker() -> (
    Worker,
    mpsc::Receiver<Event>,
    async_mpsc::Receiver<GatewayCommand>,
) {
    let (mut worker, events) = worker();
    worker.state.apply_gateway("READY",json!({"session_id":"session","user":{"id":"1","username":"me"},
        "guilds":[{"id":"10","properties":{"name":"Guild","owner_id":"1"},
            "channels":[{"id":"100","name":"voice","type":2},{"id":"101","name":"other","type":2}]}]})).unwrap();
    worker.voice_target = Some(VoiceTarget {
        guild_id: Some(10.into()),
        channel_id: 100.into(),
        connection_id: Some("local".into()),
    });
    worker.state.voice.join(100.into());
    let (tx, outgoing) = async_mpsc::channel(32);
    worker.gateway_commands = Some(tx);
    (worker, events, outgoing)
}

async fn gateway_voice(worker: &mut Worker, data: Value) {
    worker
        .gateway(GatewayEvent::Dispatch {
            name: "VOICE_STATE_UPDATE".into(),
            data,
            sequence: None,
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn watch_button_is_advertised_by_gateway_before_sfu_metadata_and_intent_survives_metadata_loss()
 {
    // useVoiceCallTracksAndLayout.ts:263-290 permits pending tracks from gateway
    // self_stream, while VoiceStreamWatchState.ts sends viewer_stream_keys.
    let (mut worker, events, mut outgoing) = watch_worker();
    let identity = "user_123_connection_with_underscores".to_owned();
    gateway_voice(
        &mut worker,
        json!({"guild_id":"10","channel_id":"100","user_id":"123",
        "connection_id":"connection_with_underscores","self_stream":true}),
    )
    .await;
    assert!(worker.available_streams.contains(&identity));
    assert!(events.try_iter().any(|event| matches!(event,
        Event::Media(MediaEvent::ScreenStreamAvailable{identity:ref id,available:true}) if id==&identity)));
    assert!(
        worker.media.is_none(),
        "gateway availability never opens devices"
    );
    assert!(
        worker.watched_streams.is_empty(),
        "a live flag is not automatic watch consent"
    );
    worker
        .set_watch(identity.clone(), true, true)
        .await
        .unwrap();
    let GatewayCommand::VoiceState(payload) = outgoing.try_recv().unwrap() else {
        panic!("voice update");
    };
    assert_eq!(
        payload.viewer_stream_keys,
        ["10:100:connection_with_underscores"]
    );
    for event in [
        MediaEvent::ScreenStreamAvailable {
            identity: identity.clone(),
            available: false,
        },
        MediaEvent::ParticipantLeft {
            identity: identity.clone(),
        },
    ] {
        worker.media_event(event).await.unwrap();
        assert!(worker.available_streams.contains(&identity));
        assert!(worker.watched_streams.contains(&identity));
    }
    assert!(
        outgoing.try_recv().is_err(),
        "transient SFU events must not send watch removal"
    );
    assert!(!events.try_iter().any(|event| matches!(
        event,
        Event::Media(MediaEvent::ScreenStreamAvailable {
            available: false,
            ..
        })
    )));

    gateway_voice(
        &mut worker,
        json!({"user_id":"123","connection_id":"connection_with_underscores","self_stream":false}),
    )
    .await;
    assert!(worker.available_streams.is_empty() && worker.watched_streams.is_empty());
    let GatewayCommand::VoiceState(payload) = outgoing.try_recv().unwrap() else {
        panic!("voice update");
    };
    assert!(payload.viewer_stream_keys.is_empty());
    worker
        .media_event(MediaEvent::ScreenStreamAvailable {
            identity,
            available: true,
        })
        .await
        .unwrap();
    assert!(
        worker.available_streams.is_empty(),
        "late SFU metadata cannot revive a gateway-stopped stream"
    );
}

#[tokio::test]
async fn watch_roster_scopes_channel_and_local_connection_and_prunes_move_leave_disconnect() {
    let (mut worker, events, mut outgoing) = watch_worker();
    for (user, connection, channel) in [
        (1, "local", 100),
        (1, "other_device", 100),
        (123, "remote", 100),
        (124, "elsewhere", 101),
    ] {
        gateway_voice(
            &mut worker,
            json!({"guild_id":"10","channel_id":channel.to_string(),
            "user_id":user.to_string(),"connection_id":connection,"self_stream":true}),
        )
        .await;
    }
    assert_eq!(
        worker.available_streams,
        BTreeSet::from(["user_1_other_device".into(), "user_123_remote".into()])
    );
    assert!(
        worker
            .set_watch("user_124_elsewhere".into(), true, true)
            .await
            .is_err()
    );
    assert!(
        worker
            .set_watch("user_1_local".into(), true, true)
            .await
            .is_err()
    );
    worker
        .set_watch("user_123_remote".into(), true, true)
        .await
        .unwrap();
    outgoing.try_recv().unwrap();
    gateway_voice(
        &mut worker,
        json!({"user_id":"123","connection_id":"remote","channel_id":"101"}),
    )
    .await;
    assert!(!worker.available_streams.contains("user_123_remote"));
    assert!(worker.watched_streams.is_empty());
    let GatewayCommand::VoiceState(payload) = outgoing.try_recv().unwrap() else {
        panic!("voice update");
    };
    assert!(payload.viewer_stream_keys.is_empty());

    worker
        .set_watch("user_1_other_device".into(), true, true)
        .await
        .unwrap();
    outgoing.try_recv().unwrap();
    gateway_voice(
        &mut worker,
        json!({"user_id":"1","connection_id":"other_device","channel_id":null}),
    )
    .await;
    assert!(worker.watched_streams.is_empty());
    assert!(
        worker.voice_target.is_some(),
        "other account connection leaving must not disconnect local call"
    );
    outgoing.try_recv().unwrap();
    gateway_voice(&mut worker,json!({"guild_id":"10","user_id":"123","connection_id":"remote","channel_id":"100","self_stream":true})).await;
    worker
        .set_watch("user_123_remote".into(), true, true)
        .await
        .unwrap();
    outgoing.try_recv().unwrap();
    worker.gateway(GatewayEvent::Disconnected).await.unwrap();
    assert!(worker.available_streams.is_empty() && worker.watched_streams.is_empty());
    assert!(worker.voice_target.is_none());
    assert!(worker.media.is_none());
    assert!(events.try_iter().any(|event| matches!(event,
        Event::Media(MediaEvent::ScreenStreamAvailable{identity:ref id,available:false}) if id=="user_123_remote")));
}

#[tokio::test]
async fn watch_roster_in_ready_is_projected_when_joining_and_republished_after_transport_reset() {
    let (mut worker, events, _) = watch_worker();
    // READY/GUILD_SYNC can hydrate live states before the local media join.
    worker.state.voice.disconnect();
    gateway_voice(&mut worker,json!({"guild_id":"10","channel_id":"100","user_id":"123","connection_id":"live","self_stream":true})).await;
    assert!(worker.available_streams.is_empty());
    worker.state.voice.join(100.into());
    worker.sync_stream_availability().await.unwrap();
    assert!(worker.available_streams.contains("user_123_live"));
    let initial = events
        .try_iter()
        .filter(|event| {
            matches!(
                event,
                Event::Media(MediaEvent::ScreenStreamAvailable {
                    available: true,
                    ..
                })
            )
        })
        .count();
    assert_eq!(initial, 1);
    worker.available_streams.clear(); // connect_voice resets the UI projection.
    worker.sync_stream_availability().await.unwrap();
    assert!(events.try_iter().any(|event| matches!(event,
        Event::Media(MediaEvent::ScreenStreamAvailable{identity:ref id,available:true}) if id=="user_123_live")));
}

fn watch_frame(identity: &str, generation: u64) -> VideoFrame {
    VideoFrame {
        source: fluxer_media::VideoSource::ScreenShare,
        track_id: "same-screen-sid".into(),
        participant: identity.into(),
        watch_generation: generation,
        width: 1,
        height: 1,
        bgra: Arc::new(vec![0, 0, 0, 255]),
    }
}

#[tokio::test]
async fn watch_stop_is_observable_and_frames_evicted_before_a_full_gateway_queue_drains() {
    let (mut worker, events, mut initial_outgoing) = watch_worker();
    let identity = "user_123_live".to_owned();
    gateway_voice(&mut worker,json!({"guild_id":"10","channel_id":"100","user_id":"123","connection_id":"live","self_stream":true})).await;
    worker
        .set_watch(identity.clone(), true, true)
        .await
        .unwrap();
    initial_outgoing.try_recv().unwrap();
    let generation = worker.video_generations[&identity];
    worker
        .media_event(MediaEvent::Video(watch_frame(&identity, generation)))
        .await
        .unwrap();
    let frames = worker.frames.clone();
    assert_eq!(frames.lock().unwrap().len(), 1);
    events.try_iter().for_each(drop);

    let (gateway, mut outgoing) = async_mpsc::channel(1);
    gateway
        .try_send(GatewayCommand::Send {
            op: 42,
            data: json!({"blocked":true}),
        })
        .unwrap();
    worker.gateway_commands = Some(gateway);
    let stopping_identity = identity.clone();
    let started = tokio::time::Instant::now();
    let stopping = tokio::spawn(async move {
        let result = worker.set_watch(stopping_identity, false, true).await;
        (worker, result)
    });
    timeout(Duration::from_secs(2), async {
        loop {
            if matches!(events.try_recv(),Ok(Event::WatchedStreams(streams)) if streams.is_empty())
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("local stop must be visible without draining gateway commands");
    println!(
        "local watch stop published after {:?} with gateway queue full",
        started.elapsed()
    );
    assert!(
        frames.lock().unwrap().is_empty(),
        "coalesced pixels are removed before network teardown"
    );
    assert!(
        !stopping.is_finished(),
        "gateway backpressure is still present during the local acknowledgment"
    );
    assert!(matches!(
        outgoing.recv().await,
        Some(GatewayCommand::Send { op: 42, .. })
    ));
    let GatewayCommand::VoiceState(payload) = outgoing.recv().await.unwrap() else {
        panic!("stop viewer demand");
    };
    assert!(payload.viewer_stream_keys.is_empty());
    let (mut worker, result) = stopping.await.unwrap();
    result.unwrap();
    worker
        .media_event(MediaEvent::Video(watch_frame(&identity, generation)))
        .await
        .unwrap();
    assert!(
        frames.lock().unwrap().is_empty(),
        "a buffered post-stop frame must not reappear"
    );
    assert!(
        worker.media.is_none(),
        "the regression never opens native media devices"
    );
}

#[tokio::test]
async fn watch_stop_rewatch_rejects_old_same_sid_frames_and_end_notifications() {
    let (mut worker, events, mut outgoing) = watch_worker();
    let identity = "user_123_live".to_owned();
    gateway_voice(&mut worker,json!({"guild_id":"10","channel_id":"100","user_id":"123","connection_id":"live","self_stream":true})).await;
    worker
        .set_watch(identity.clone(), true, true)
        .await
        .unwrap();
    outgoing.try_recv().unwrap();
    let old = worker.video_generations[&identity];
    worker
        .set_watch(identity.clone(), false, true)
        .await
        .unwrap();
    outgoing.try_recv().unwrap();
    worker
        .set_watch(identity.clone(), true, true)
        .await
        .unwrap();
    outgoing.try_recv().unwrap();
    let current = worker.video_generations[&identity];
    assert!(current > old);
    events.try_iter().for_each(drop);
    worker
        .media_event(MediaEvent::Video(watch_frame(&identity, old)))
        .await
        .unwrap();
    assert!(worker.frames.lock().unwrap().is_empty());
    worker
        .media_event(MediaEvent::Video(watch_frame(&identity, current)))
        .await
        .unwrap();
    assert_eq!(worker.frames.lock().unwrap().len(), 1);
    worker
        .media_event(MediaEvent::VideoEnded {
            source: fluxer_media::VideoSource::ScreenShare,
            track_id: "same-screen-sid".into(),
            participant: identity.clone(),
            watch_generation: old,
        })
        .await
        .unwrap();
    assert_eq!(
        worker.frames.lock().unwrap().len(),
        1,
        "late stop from previous watch cannot clear the new frame"
    );
    assert!(
        events.try_iter().next().is_none(),
        "old lifecycle notifications cannot leak to the UI"
    );
    worker
        .media_event(MediaEvent::VideoEnded {
            source: fluxer_media::VideoSource::ScreenShare,
            track_id: "same-screen-sid".into(),
            participant: identity,
            watch_generation: current,
        })
        .await
        .unwrap();
    assert!(worker.frames.lock().unwrap().is_empty());
    assert!(matches!(
        events.try_recv(),
        Ok(Event::Media(MediaEvent::VideoEnded { .. }))
    ));
}

#[tokio::test]
async fn selected_guild_sync_matches_desktop_payload_and_session_deduplication() {
    // GatewayGuildSubscriptionSynchronizer.ts:29-51,90-95; GuildSync.ts:8-11.
    let (mut worker, _) = worker();
    let (tx, mut outgoing) = async_mpsc::channel(16);
    worker.gateway_commands = Some(tx);
    worker
        .command(Command::SelectGuild {
            guild_id: 10.into(),
        })
        .await
        .unwrap();
    assert!(outgoing.try_recv().is_err(), "selection waits for READY");
    worker.gateway(GatewayEvent::Dispatch{name:"READY".into(),data:json!({"session_id":"first","user":{"id":"1","username":"alice"},"guilds":[{"id":"10","name":"A","threads":[]},{"id":"20","name":"B"}]}),sequence:Some(1)}).await.unwrap();
    worker.gateway(GatewayEvent::Connected).await.unwrap();
    let GatewayCommand::Send { op, data } = outgoing.try_recv().unwrap() else {
        panic!("expected op14");
    };
    assert_eq!(op, 14);
    assert_eq!(
        data,
        json!({"subscriptions":{"10":{"active":true,"sync":true,"threads":true}}})
    );
    worker
        .command(Command::SelectGuild {
            guild_id: 10.into(),
        })
        .await
        .unwrap();
    assert!(outgoing.try_recv().is_err(), "one guild sync per session");
    worker
        .command(Command::SelectGuild {
            guild_id: 20.into(),
        })
        .await
        .unwrap();
    let GatewayCommand::Send { op, data } = outgoing.try_recv().unwrap() else {
        panic!("expected op14");
    };
    assert_eq!(op, 14);
    assert_eq!(
        data,
        json!({"subscriptions":{"20":{"active":true,"sync":true}}})
    );
    assert!(
        outgoing.try_recv().is_err(),
        "desktop selection does not invent an old-guild deactivation"
    );
    worker.gateway(GatewayEvent::Dispatch{name:"GUILD_SYNC".into(),data:json!({"id":"20","properties":{"name":"Hydrated B"},"channels":[{"id":"200","name":"large-guild-text","type":0}]}),sequence:Some(2)}).await.unwrap();
    assert_eq!(
        worker.state.channels.get(&200.into()).unwrap().guild_id,
        Some(20.into())
    );
    worker.gateway(GatewayEvent::Reconnecting).await.unwrap();
    worker
        .gateway(GatewayEvent::Dispatch {
            name: "RESUMED".into(),
            data: json!({}),
            sequence: Some(3),
        })
        .await
        .unwrap();
    worker.gateway(GatewayEvent::Connected).await.unwrap();
    assert!(
        outgoing.try_recv().is_err(),
        "same-session resume preserves completed syncs"
    );
    worker.gateway(GatewayEvent::Dispatch{name:"READY".into(),data:json!({"session_id":"second","user":{"id":"1","username":"alice"},"guilds":[{"id":"20","name":"B"}]}),sequence:Some(1)}).await.unwrap();
    worker.gateway(GatewayEvent::Connected).await.unwrap();
    let GatewayCommand::Send { op, data } = outgoing.try_recv().unwrap() else {
        panic!("expected op14");
    };
    assert_eq!(op, 14);
    assert_eq!(
        data,
        json!({"subscriptions":{"20":{"active":true,"sync":true}}})
    );
    assert!(worker.media.is_none());
}

#[tokio::test]
async fn locale_survives_account_reset_and_rejects_header_injection() {
    let (mut worker, _) = worker();
    worker
        .command(Command::SetLocale("de".into()))
        .await
        .unwrap();
    worker.reset().await;
    assert_eq!(worker.locale, "de");
    assert_eq!(worker.state.settings.locale, "de");
    assert!(
        worker
            .command(Command::SetLocale("en\r\nInjected:yes".into()))
            .await
            .is_err()
    );
}

fn ip_challenge() -> PendingIpAuthorization {
    PendingIpAuthorization {
        client: Client::new(
            Instance::from_endpoints("https://api.example/api", "wss://gateway.example").unwrap(),
        )
        .unwrap(),
        ticket: "private-ticket".into(),
        resend_deadline: tokio::time::Instant::now() + Duration::from_secs(30),
        resend_used: false,
        consecutive_errors: 0,
        next_poll: Some(tokio::time::Instant::now()),
    }
}

#[tokio::test]
async fn ip_authorization_poll_errors_pause_after_three_and_retry_is_immediate() {
    // IpAuthorizationScreen.tsx:29-30,65-105,126-128.
    let (mut worker, events) = worker();
    worker.pending_ip = Some(ip_challenge());
    assert!(worker.pending_ip.as_ref().unwrap().next_poll.unwrap() <= tokio::time::Instant::now());
    worker.finish_ip_poll(Err("network".into())).await.unwrap();
    assert_eq!(worker.pending_ip.as_ref().unwrap().consecutive_errors, 1);
    let delay = worker
        .pending_ip
        .as_ref()
        .unwrap()
        .next_poll
        .unwrap()
        .duration_since(tokio::time::Instant::now());
    assert!(delay > Duration::from_millis(1900) && delay <= IP_POLL_INTERVAL);
    worker.finish_ip_poll(Ok(None)).await.unwrap();
    assert_eq!(worker.pending_ip.as_ref().unwrap().consecutive_errors, 0);
    for _ in 0..3 {
        worker.finish_ip_poll(Err("network".into())).await.unwrap();
    }
    assert!(worker.pending_ip.as_ref().unwrap().next_poll.is_none());
    assert!(
        events
            .try_iter()
            .any(|event| matches!(event, Event::IpAuthorizationPollingFailed))
    );
    worker.command(Command::RetryIpAuthorization).await.unwrap();
    assert_eq!(worker.pending_ip.as_ref().unwrap().consecutive_errors, 0);
    assert!(worker.pending_ip.as_ref().unwrap().next_poll.unwrap() <= tokio::time::Instant::now());
    assert!(
        events
            .try_iter()
            .any(|event| matches!(event, Event::IpAuthorizationPollingResumed))
    );
    assert!(worker.media.is_none());
}

#[tokio::test]
async fn ip_authorization_cooldown_once_only_and_cancellation_do_not_make_network_calls() {
    // IpAuthorizationScreen.tsx:113-125 disallows a second resend even after cooldown.
    let (mut worker, events) = worker();
    worker.pending_ip = Some(ip_challenge());
    worker
        .command(Command::ResendIpAuthorization)
        .await
        .unwrap();
    worker.pending_ip.as_mut().unwrap().resend_deadline = tokio::time::Instant::now();
    worker.pending_ip.as_mut().unwrap().resend_used = true;
    worker
        .command(Command::ResendIpAuthorization)
        .await
        .unwrap();
    worker.ip_poll_task = Some(tokio::spawn(pending()));
    let abort = worker.ip_poll_task.as_ref().unwrap().abort_handle();
    worker.command(Command::CancelLogin).await.unwrap();
    tokio::task::yield_now().await;
    assert!(abort.is_finished());
    assert!(worker.pending_ip.is_none());
    assert!(worker.ip_poll_task.is_none());
    assert!(
        events
            .try_iter()
            .any(|event| matches!(event, Event::IpAuthorizationCleared))
    );
    assert!(worker.state.current_user.is_none());
    assert!(worker.media.is_none());
}

#[tokio::test]
async fn browser_poll_error_pause_retry_delay_and_cancellation_match_handoff_flow() {
    let (mut worker, events) = worker();
    worker.pending_browser = Some(PendingBrowserLogin {
        client: ip_challenge().client,
        session: None,
        consecutive_errors: 0,
        next_poll: Some(tokio::time::Instant::now() + IP_POLL_INTERVAL),
    });
    for _ in 0..3 {
        worker
            .finish_browser_poll(Err("network".into()))
            .await
            .unwrap();
    }
    assert!(worker.pending_browser.as_ref().unwrap().next_poll.is_none());
    assert!(
        events
            .try_iter()
            .any(|event| matches!(event, Event::BrowserLoginPollingFailed))
    );
    let before = tokio::time::Instant::now();
    worker.command(Command::RetryBrowserLogin).await.unwrap();
    assert!(
        worker.pending_browser.as_ref().unwrap().next_poll.unwrap() >= before + IP_POLL_INTERVAL
    );
    assert!(
        events
            .try_iter()
            .any(|event| matches!(event, Event::BrowserLoginPollingResumed))
    );
    worker.browser_poll_task = Some(tokio::spawn(pending()));
    let abort = worker.browser_poll_task.as_ref().unwrap().abort_handle();
    worker.command(Command::CancelLogin).await.unwrap();
    tokio::task::yield_now().await;
    assert!(abort.is_finished());
    assert!(worker.pending_browser.is_none());
    assert!(worker.media.is_none());
}

#[tokio::test]
async fn denied_and_expired_handoff_clear_private_state_without_authenticating() {
    for status in [DesktopHandoffStatus::Denied, DesktopHandoffStatus::Expired] {
        let (mut worker, events) = worker();
        worker.pending_browser = Some(PendingBrowserLogin {
            client: ip_challenge().client,
            session: None,
            consecutive_errors: 0,
            next_poll: None,
        });
        assert!(
            worker
                .finish_browser_poll(Ok(DesktopHandoffPoll {
                    status,
                    token: None,
                    user_id: None,
                    user: None
                }))
                .await
                .is_err()
        );
        assert!(worker.pending_browser.is_none());
        assert!(worker.state.current_user.is_none());
        assert!(worker.media.is_none());
        assert!(
            events
                .try_iter()
                .any(|event| matches!(event, Event::BrowserLoginCleared))
        );
    }
}

#[test]
fn voice_credentials_require_the_requested_channel_and_current_connection() {
    let mut target = VoiceTarget {
        guild_id: Some(10.into()),
        channel_id: 100.into(),
        connection_id: None,
    };
    assert!(target.accepts(&voice_update(Some("100"), "new")));
    assert!(!target.accepts(&voice_update(Some("200"), "new")));
    assert!(!target.accepts(&voice_update(None, "unknown")));
    target.connection_id = Some("new".into());
    assert!(target.accepts(&voice_update(None, "new")));
    assert!(!target.accepts(&voice_update(Some("100"), "stale")));
}

#[test]
fn only_identical_full_voice_credentials_are_deduplicated() {
    let previous = voice_update(Some("100"), "connection");
    assert!(same_voice_credentials(&previous, &previous.clone()));
    let mut changed = previous.clone();
    changed.token = SessionToken::new("rotated-secret").unwrap();
    assert!(!same_voice_credentials(&previous, &changed));
    let mut changed = previous.clone();
    changed.e2ee_key = Some("rotated-key".into());
    assert!(!same_voice_credentials(&previous, &changed));
    let mut changed = previous.clone();
    changed.ice_servers = vec![json!({"urls":["turn:relay.example"]})];
    assert!(!same_voice_credentials(&previous, &changed));
}

#[tokio::test]
async fn mesh_credentials_leave_the_server_without_initializing_capture_or_livekit() {
    let (mut worker, _) = worker();
    let (tx, mut outgoing) = async_mpsc::channel(8);
    worker.gateway_commands = Some(tx);
    worker.voice_target = Some(VoiceTarget {
        guild_id: Some(10.into()),
        channel_id: 100.into(),
        connection_id: None,
    });
    let mut update = voice_update(Some("100"), "server-mesh-connection");
    update.ice_servers = vec![json!({"urls":["turn:relay.example"]})];
    assert!(
        worker
            .connect_voice(update)
            .await
            .unwrap_err()
            .contains("mesh voice")
    );
    assert!(worker.media.is_none());
    assert!(worker.voice_target.is_none());
    assert!(worker.state.voice.channel_id.is_none());
    let GatewayCommand::VoiceState(payload) = outgoing.try_recv().unwrap() else {
        panic!("expected leave state");
    };
    assert_eq!(payload.guild_id.as_deref(), Some("10"));
    assert!(payload.channel_id.is_none());
    assert_eq!(
        payload.connection_id.as_deref(),
        Some("server-mesh-connection")
    );
}

#[test]
fn channel_bitrate_uses_the_shared_desktop_resolver_and_guild_features() {
    let mut state = ClientState::default();
    state.apply_gateway("GUILD_CREATE",json!({"id":"10","name":"Test","features":["AUDIO_BITRATE_256_KBPS"],"channels":[{"id":"100","name":"voice","type":2,"bitrate":384000}]})).unwrap();
    assert_eq!(voice_channel_bitrate(&state, 100.into()), 256_000);
    state
        .guilds
        .get_mut(&10.into())
        .unwrap()
        .extra
        .remove("features");
    assert_eq!(voice_channel_bitrate(&state, 100.into()), 96_000);
    state
        .apply_gateway(
            "CHANNEL_CREATE",
            json!({"id":"200","type":1,"bitrate":384000}),
        )
        .unwrap();
    assert_eq!(voice_channel_bitrate(&state, 200.into()), 64_000);
}

#[tokio::test]
async fn uncorrelated_early_voice_credentials_are_buffered_and_cleared_on_leave() {
    let (mut worker, _) = worker();
    worker.voice_target = Some(VoiceTarget {
        guild_id: Some(10.into()),
        channel_id: 100.into(),
        connection_id: None,
    });
    worker.gateway(GatewayEvent::Dispatch{name:"VOICE_SERVER_UPDATE".into(),data:json!({"token":"voice-secret","endpoint":"wss://voice.example","connection_id":"early","guild_id":"10"}),sequence:Some(1)}).await.unwrap();
    assert!(!worker.pending_voice_server.is_empty());
    assert!(worker.media.is_none());
    worker.leave_voice(false).await.unwrap();
    assert!(worker.pending_voice_server.is_empty());
    assert!(worker.voice_target.is_none());
    assert!(worker.media.is_none());
}

#[tokio::test]
async fn playback_preferences_are_separate_and_do_not_initialize_media() {
    let (mut worker, _) = worker();
    worker
        .command(Command::SetFilePlaybackPreference {
            kind: crate::FilePlaybackKind::Audio,
            preference: crate::FilePlaybackPreference {
                volume: f32::NAN,
                muted: true,
            },
        })
        .await
        .unwrap();
    worker
        .command(Command::SetFilePlaybackPreference {
            kind: crate::FilePlaybackKind::Video,
            preference: crate::FilePlaybackPreference {
                volume: -2.0,
                muted: false,
            },
        })
        .await
        .unwrap();
    assert_eq!(
        worker.audio_preferences.audio_playback,
        crate::FilePlaybackPreference {
            volume: 1.0,
            muted: true
        }
    );
    assert_eq!(
        worker.audio_preferences.video_playback,
        crate::FilePlaybackPreference {
            volume: 0.0,
            muted: false
        }
    );
    assert!(worker.media.is_none());
}

#[tokio::test]
async fn demo_never_initializes_capture_or_claims_voice_transmission() {
    let (mut worker, _) = worker();
    worker.command(Command::Demo).await.unwrap();
    assert!(
        worker
            .command(Command::JoinVoice {
                guild_id: Some(10.into()),
                channel_id: 100.into()
            })
            .await
            .is_err()
    );
    assert!(worker.media.is_none());
    assert!(worker.state.voice.channel_id.is_none());
}

#[tokio::test]
async fn replacement_revokes_pending_share_and_advertises_no_stream_without_capture() {
    let (mut worker, events) = worker();
    worker.voice_target = Some(VoiceTarget {
        guild_id: Some(10.into()),
        channel_id: 100.into(),
        connection_id: Some("replacement".into()),
    });
    worker.state.voice.join(100.into());
    worker.state.voice.set_muted(true);
    worker.state.voice.screen_share = true;
    worker.pending_share = Some(PendingShare {
        audio: Some(SharedAudioSelection::System),
    });
    worker.active_share_audio = true;
    assert!(worker.revoke_share_for_replacement());
    assert!(worker.pending_share.is_none());
    assert!(!worker.active_share_audio && !worker.state.voice.screen_share);
    assert!(worker.state.voice.muted);
    assert!(worker.media.is_none(), "revocation must not create capture");
    worker.state.voice.disconnect();
    worker.state.voice.join(100.into());
    let (tx, mut gateway) = async_mpsc::channel(1);
    worker.gateway_commands = Some(tx);
    worker.update_voice().await.unwrap();
    match gateway.recv().await.unwrap() {
        GatewayCommand::VoiceState(payload) => {
            assert!(!payload.self_stream);
            assert!(payload.self_mute);
        }
        _ => panic!("expected voice state"),
    }
    assert!(
        events
            .try_iter()
            .any(|event| matches!(event, Event::SharedAudioStatus(false)))
    );
    assert!(!worker.revoke_share_for_replacement());
}

#[tokio::test]
async fn capture_events_drive_sharing_state_gateway_and_ui_without_opening_media() {
    let (mut worker, events) = worker();
    worker.voice_target = Some(VoiceTarget {
        guild_id: Some(10.into()),
        channel_id: 100.into(),
        connection_id: Some("connection".into()),
    });
    worker.state.voice.join(100.into());
    let (tx, mut gateway) = async_mpsc::channel(8);
    worker.gateway_commands = Some(tx);
    // A previous share's stopped notification can precede a replacement start.
    worker.pending_share = Some(PendingShare { audio: None });
    worker.screen_event(false, false).await.unwrap();
    worker.pending_share = Some(PendingShare { audio: None });
    worker.screen_event(true, true).await.unwrap();
    assert!(worker.state.voice.screen_share);
    // A stale stop must not clear a replacement source that is still capturing.
    worker.screen_event(false, true).await.unwrap();
    assert!(worker.state.voice.screen_share);
    worker.screen_event(false, false).await.unwrap();
    assert!(!worker.state.voice.screen_share);
    let mut states = Vec::new();
    while let Ok(command) = gateway.try_recv() {
        let GatewayCommand::VoiceState(state) = command else {
            panic!("expected voice state");
        };
        assert_eq!(state.connection_id.as_deref(), Some("connection"));
        states.push(state.self_stream);
    }
    assert_eq!(states, [false, true, false]);
    let transitions = events
        .try_iter()
        .filter_map(|event| match event {
            Event::Media(MediaEvent::ScreenShareStarted) => Some(true),
            Event::Media(MediaEvent::ScreenShareStopped) => Some(false),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(transitions, [false, true, false]);
    assert!(
        worker.media.is_none(),
        "event-state tests must not create capture handles"
    );
}

#[tokio::test]
async fn late_share_started_after_leaving_does_not_reenable_sharing() {
    let (mut worker, events) = worker();
    worker
        .media_event(MediaEvent::ScreenShareStarted)
        .await
        .unwrap();
    assert!(!worker.state.voice.screen_share);
    assert!(
        !events
            .try_iter()
            .any(|event| matches!(event, Event::Media(MediaEvent::ScreenShareStarted)))
    );
    assert!(worker.media.is_none());
}

#[tokio::test]
async fn unified_share_defers_requested_audio_until_capture_and_cancel_revokes_late_start() {
    let (mut worker, events, mut gateway) = watch_worker();
    worker.pending_share = Some(PendingShare {
        audio: Some(SharedAudioSelection::Application {
            process_id: "456".into(),
        }),
    });
    worker.screen_event(true, false).await.unwrap();
    assert!(!worker.state.voice.screen_share && !worker.active_share_audio);
    assert!(
        matches!(worker.pending_share.as_ref().and_then(|share|share.audio.as_ref()),
        Some(SharedAudioSelection::Application{process_id}) if process_id=="456")
    );
    assert!(gateway.try_recv().is_err());
    assert!(
        worker.media.is_none(),
        "opening a picker must not open audio devices"
    );
    worker.command(Command::StopShare).await.unwrap();
    assert!(worker.pending_share.is_none());
    // Even a misleading late source-liveness signal cannot reinstate revoked
    // consent. No OS or media handle exists in this regression.
    worker.screen_event(true, true).await.unwrap();
    assert!(!worker.state.voice.screen_share && !worker.active_share_audio);
    assert!(worker.media.is_none());
    let GatewayCommand::VoiceState(payload) = gateway.try_recv().unwrap() else {
        panic!("stopped share state");
    };
    assert!(!payload.self_stream);
    assert!(!events.try_iter().any(|event| matches!(
        event,
        Event::SharedAudioStatus(true) | Event::Media(MediaEvent::ScreenShareStarted)
    )));
}

#[tokio::test]
async fn requested_share_audio_failure_rolls_back_pending_video_and_server_state() {
    // ScreenShareStartFlow.ts:308 failRequestedAudioCapture and the capture
    // adapter's rollback/cleanup paths reject silently dropping requested audio.
    let (mut worker, events, mut gateway) = watch_worker();
    worker.pending_share = Some(PendingShare {
        audio: Some(SharedAudioSelection::System),
    });
    let message = worker
        .complete_share_start(true, Err("Selected audio route could not start".into()))
        .await
        .unwrap_err();
    assert!(message.contains("Selected audio route could not start"));
    assert!(worker.pending_share.is_none());
    assert!(!worker.state.voice.screen_share && !worker.active_share_audio);
    let GatewayCommand::VoiceState(payload) = gateway.try_recv().unwrap() else {
        panic!("rollback state");
    };
    assert!(!payload.self_stream);
    let events: Vec<_> = events.try_iter().collect();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::SharedAudioStatus(false)))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::Media(MediaEvent::ScreenShareStopped)))
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Event::Media(MediaEvent::ScreenShareStarted)))
    );
    assert!(worker.media.is_none());
}

#[tokio::test]
async fn unified_share_success_and_stop_keep_audio_video_status_and_self_stream_together() {
    let (mut worker, events, mut gateway) = watch_worker();
    worker.complete_share_start(true, Ok(())).await.unwrap();
    assert!(worker.state.voice.screen_share && worker.active_share_audio);
    let GatewayCommand::VoiceState(started) = gateway.try_recv().unwrap() else {
        panic!("started share");
    };
    assert!(started.self_stream);
    worker.command(Command::StopShare).await.unwrap();
    let GatewayCommand::VoiceState(stopped) = gateway.try_recv().unwrap() else {
        panic!("stopped share");
    };
    assert!(!stopped.self_stream);
    assert!(!worker.state.voice.screen_share && !worker.active_share_audio);
    let audio: Vec<_> = events
        .try_iter()
        .filter_map(|event| match event {
            Event::SharedAudioStatus(active) => Some(active),
            _ => None,
        })
        .collect();
    assert_eq!(audio, [true, false]);
}

#[tokio::test]
async fn disabling_audio_while_picker_is_pending_preserves_video_only_consent() {
    let (mut worker, events, mut gateway) = watch_worker();
    worker.pending_share = Some(PendingShare {
        audio: Some(SharedAudioSelection::System),
    });
    worker.command(Command::StopSharedAudio).await.unwrap();
    assert!(worker.pending_share.as_ref().unwrap().audio.is_none());
    worker.screen_event(true, true).await.unwrap();
    assert!(worker.state.voice.screen_share && !worker.active_share_audio);
    assert!(
        worker.media.is_none(),
        "video-only readiness does not initialize audio capture"
    );
    let GatewayCommand::VoiceState(payload) = gateway.try_recv().unwrap() else {
        panic!("video only share");
    };
    assert!(payload.self_stream);
    assert!(
        !events
            .try_iter()
            .any(|event| matches!(event, Event::SharedAudioStatus(true)))
    );
}

#[tokio::test]
async fn unified_share_gateway_failure_revokes_pending_and_active_sources() {
    let (mut worker, events, gateway) = watch_worker();
    drop(gateway);
    worker.pending_share = Some(PendingShare {
        audio: Some(SharedAudioSelection::System),
    });
    assert!(worker.complete_share_start(true, Ok(())).await.is_err());
    assert!(worker.pending_share.is_none());
    assert!(!worker.active_share_audio && !worker.state.voice.screen_share);
    let audio: Vec<_> = events
        .try_iter()
        .filter_map(|event| match event {
            Event::SharedAudioStatus(active) => Some(active),
            _ => None,
        })
        .collect();
    assert_eq!(audio, [true, false]);
    assert!(worker.media.is_none());
}

#[tokio::test]
async fn shared_audio_runtime_stop_rolls_back_video_but_old_or_manual_stops_do_not() {
    let (mut worker, events, mut gateway) = watch_worker();
    worker.complete_share_start(true, Ok(())).await.unwrap();
    gateway.try_recv().unwrap();
    events.try_iter().for_each(drop);
    worker.shared_audio_event(true).await.unwrap();
    assert!(
        worker.active_share_audio && worker.state.voice.screen_share,
        "old stopped route must not affect live replacement"
    );
    assert!(gateway.try_recv().is_err());
    assert!(
        worker
            .shared_audio_event(false)
            .await
            .unwrap_err()
            .contains("audio stopped unexpectedly")
    );
    assert!(!worker.active_share_audio && !worker.state.voice.screen_share);
    let GatewayCommand::VoiceState(payload) = gateway.try_recv().unwrap() else {
        panic!("audio failure rollback");
    };
    assert!(!payload.self_stream);

    worker.complete_share_start(true, Ok(())).await.unwrap();
    gateway.try_recv().unwrap();
    worker.command(Command::StopSharedAudio).await.unwrap();
    worker.shared_audio_event(false).await.unwrap();
    assert!(
        worker.state.voice.screen_share && !worker.active_share_audio,
        "explicit audio-only stop preserves video"
    );
    assert!(gateway.try_recv().is_err());
    assert!(worker.media.is_none());
}

async fn read_request(socket: &mut tokio::net::TcpStream) -> String {
    let mut request = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let size = socket.read(&mut buffer).await.unwrap();
        if size == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..size]);
        if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
            let header = String::from_utf8_lossy(&request[..end]);
            let length: usize = header
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().unwrap())
                })
                .unwrap_or(0);
            if request.len() >= end + 4 + length {
                break;
            }
        }
    }
    String::from_utf8(request).unwrap()
}

#[tokio::test]
async fn actual_http_and_gateway_send_merge_preserve_failed_outbox_without_capture() {
    actual_text_workflow(0).await;
}

#[tokio::test]
async fn actual_password_login_uses_discovery_identity_locale_then_ready_without_list_rest() {
    actual_text_workflow(1).await;
}

#[tokio::test]
async fn actual_ip_authorization_resend_and_poll_completion_bootstrap_ready_without_capture() {
    actual_text_workflow(2).await;
}

#[tokio::test]
async fn actual_browser_handoff_code_and_poll_bootstrap_without_exposing_secrets_or_capture() {
    actual_text_workflow(3).await;
}

#[tokio::test]
async fn actual_saved_session_cold_restore_identifies_without_user_validation_or_resume() {
    actual_text_workflow(4).await;
}

#[allow(clippy::result_large_err)] // tungstenite's required handshake callback return type
async fn actual_text_workflow(login_mode: u8) {
    let password_login = matches!(login_mode, 1 | 2);
    let ip_authorization = login_mode == 2;
    let browser_login = login_mode == 3;
    let restoring = login_mode == 4;
    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", http.local_addr().unwrap());
    let gateway = format!("ws://{}", ws.local_addr().unwrap());
    let user = json!({"id":"1","username":"alice"});
    let channel =
        json!({"id":"100","guild_id":"10","name":"general","type":0,"last_message_id":"200"});
    let guild = json!({"id":"10","name":"Test","owner_id":"1","channels":[channel.clone()]});
    let initial = json!({"id":"200","channel_id":"100","author":user.clone(),"content":"history"});
    let sent = json!({"id":"201","channel_id":"100","author":user.clone(),"content":"hello"});
    let (dispatch_tx, mut dispatch_rx) = async_mpsc::channel::<Value>(8);
    let ws_user = user.clone();
    let ws_guild = guild.clone();
    let websocket = tokio::spawn(async move {
        let (socket, _) = ws.accept().await.unwrap();
        let mut socket = accept_hdr_async(
            socket,
            |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
             response: tokio_tungstenite::tungstenite::handshake::server::Response| {
                assert!(
                    request
                        .uri()
                        .query()
                        .unwrap_or("")
                        .contains("compress=zstd-stream")
                );
                Ok(response)
            },
        )
        .await
        .unwrap();
        let mut encoder = ZstdStreamEncoder::new(3).unwrap();
        let mut decoder = ZstdStreamDecoder::new().unwrap();
        socket
            .send(compressed(
                json!({"op":10,"d":{"heartbeat_interval":1000}}),
                &mut encoder,
            ))
            .await
            .unwrap();
        let identify = uncompressed(socket.next().await.unwrap().unwrap(), &mut decoder);
        assert_eq!(identify["op"], 2);
        assert_eq!(identify["d"]["properties"]["locale"], "de");
        assert_eq!(identify["d"]["flags"], 14);
        if restoring {
            assert_eq!(identify["d"]["initial_guild_id"], "10");
        }
        socket.send(compressed(json!({"op":0,"t":"READY","s":1,"d":{"session_id":"session","user":ws_user,"guilds":[ws_guild]}}),&mut encoder)).await.unwrap();
        loop {
            tokio::select! {
                value=dispatch_rx.recv()=>match value{Some(value)=>{if socket.send(compressed(value,&mut encoder)).await.is_err(){break;}},None=>break},
                value=socket.next()=>match value{
                    Some(Ok(message@ (WireMessage::Text(_) | WireMessage::Binary(_))))=>{
                        let value=uncompressed(message,&mut decoder);
                        if value["op"]==1 && socket.send(compressed(json!({"op":11}),&mut encoder)).await.is_err(){break;}
                        if value["op"]==14 {assert_eq!(value["d"],json!({"subscriptions":{"10":{"active":true,"sync":true}}}));}
                    },
                    Some(Ok(WireMessage::Close(_)))|None|Some(Err(_))=>break,
                    _=>{},
                }
            }
        }
    });
    let advertised_api = format!("{origin}/api");
    let advertised_webapp = origin.clone();
    let ws_endpoint = gateway.clone();
    let rest_sent = sent.clone();
    let http_server = tokio::spawn(async move {
        let mut poll_count = 0;
        loop {
            let (mut socket, _) = http.accept().await.unwrap();
            let request = read_request(&mut socket).await;
            let first = request.lines().next().unwrap();
            let mut fields = first.split_whitespace();
            let method = fields.next().unwrap();
            let path = fields.next().unwrap();
            let (status, body) = match (method, path) {
                ("GET", "/.well-known/fluxer") => (
                    200,
                    json!({"codename":"tungsten","api_code_version":1,"features":{"account_identity":"username"},"endpoints":{"api":advertised_api,"api_client":advertised_api,"api_public":advertised_api,"media":advertised_api,"static_cdn":advertised_api,"marketing":advertised_api,"admin":advertised_api,"invite":advertised_api,"gift":advertised_api,"webapp":advertised_webapp,"gateway":ws_endpoint}}),
                ),
                ("POST", "/api/v1/auth/login") => {
                    assert!(password_login);
                    let body: Value =
                        serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
                    assert_eq!(
                        body,
                        json!({"login":"alice@example.com","password":"password"})
                    );
                    assert!(
                        request
                            .to_ascii_lowercase()
                            .contains("accept-language: de\r\n")
                    );
                    if ip_authorization {
                        (
                            403,
                            json!({"code":"IP_AUTHORIZATION_REQUIRED","details":{"context":{"ticket":"pending-ticket","email":"alice@example.com","resend_available_in":0}}}),
                        )
                    } else {
                        (
                            200,
                            json!({"token":"secret","user_id":"1","user":{"username":"alice","discriminator":"0000","email":"alice@example.com"}}),
                        )
                    }
                }
                ("POST", "/api/v1/auth/handoff/initiate") => {
                    assert!(browser_login);
                    let headers = request.to_ascii_lowercase();
                    assert!(!headers.contains("accept-language:"));
                    assert!(!headers.contains("authorization:"));
                    assert!(!headers.contains("x-fluxer-features:"));
                    (
                        200,
                        json!({"code":"ABCDEFGH","expires_at":"2100-01-01T00:00:00Z","poll_secret":"private-proof"}),
                    )
                }
                ("POST", "/api/v1/auth/handoff/ABCDEFGH/status") => {
                    assert!(browser_login);
                    poll_count += 1;
                    let body: Value =
                        serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
                    assert_eq!(body, json!({"poll_secret":"private-proof"}));
                    assert!(!request.to_ascii_lowercase().contains("accept-language:"));
                    if poll_count == 1 {
                        (200, json!({"status":"pending"}))
                    } else {
                        (
                            200,
                            json!({"status":"completed","token":"secret","user_id":"1","user":{"username":"alice","discriminator":"0000","email":"alice@example.com"}}),
                        )
                    }
                }
                ("POST", "/api/v1/auth/ip-authorization/resend") => {
                    assert!(ip_authorization);
                    let body: Value =
                        serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
                    assert_eq!(body, json!({"ticket":"pending-ticket"}));
                    assert!(
                        request
                            .to_ascii_lowercase()
                            .contains("accept-language: de\r\n")
                    );
                    (200, json!({}))
                }
                ("GET", "/api/v1/auth/ip-authorization/poll?ticket=pending-ticket") => {
                    assert!(ip_authorization);
                    poll_count += 1;
                    assert!(
                        request
                            .to_ascii_lowercase()
                            .contains("accept-language: de\r\n")
                    );
                    if poll_count == 1 {
                        (200, json!({"completed":false}))
                    } else {
                        (
                            200,
                            json!({"completed":true,"token":"secret","user_id":"1","user":{"username":"alice","discriminator":"0000","email":"alice@example.com"}}),
                        )
                    }
                }
                ("GET", "/api/v1/users/@me") => {
                    assert!(
                        login_mode == 0,
                        "fresh desktop login trusts auth response then READY"
                    );
                    (200, user.clone())
                }
                ("GET", "/api/v1/channels/100/messages?limit=50") => {
                    (200, json!([initial.clone()]))
                }
                ("GET", "/api/v1/channels/100/slowmode") => (
                    200,
                    json!({"rate_limit_per_user":0,"retry_after_ms":0,"can_bypass":false}),
                ),
                ("POST", "/api/v1/read-states/ack") => {
                    let body: Value =
                        serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
                    assert_eq!(
                        body,
                        json!({"read_states":[{"channel_id":"100","message_id":"200"}]})
                    );
                    (200, json!({}))
                }
                ("POST", "/api/v1/channels/100/messages") => {
                    let body: Value =
                        serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
                    assert!(
                        request
                            .to_ascii_lowercase()
                            .contains("authorization: secret")
                    );
                    if body["content"] == "fail" {
                        (403, json!({"code":"MISSING_PERMISSIONS"}))
                    } else {
                        (200, rest_sent.clone())
                    }
                }
                _ => panic!("unexpected request {first}"),
            };
            let body = body.to_string();
            let reason = if status == 200 { "OK" } else { "Forbidden" };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            let _ = socket.shutdown().await;
        }
    });
    let store = Arc::new(fluxer_session::MemorySessionStore::default());
    if restoring {
        // A serialized record from a previous process, with no gateway snapshot
        // or cursor. Refresh uses discovery without forwarding this token.
        let instance = Instance::discover(&origin).await.unwrap();
        store
            .save(SavedSession {
                instance,
                token: SessionToken::new("secret").unwrap(),
                user_id: 1.into(),
                user: None,
                locale: "de".into(),
                selected_guild: Some(10.into()),
                selected_channel: Some(100.into()),
                valid: true,
            })
            .await
            .unwrap();
    }
    let (worker, events) = worker();
    let mut worker = worker.with_session_store(store.clone());
    worker
        .command(Command::SetLocale("de".into()))
        .await
        .unwrap();
    worker
        .command(if restoring {
            Command::RestoreSession
        } else if browser_login {
            Command::BrowserLogin {
                instance: origin,
                identifier: "alice@example.com".into(),
            }
        } else if password_login {
            Command::Login {
                instance: origin,
                identifier: "alice@example.com".into(),
                password: "password".into(),
            }
        } else {
            Command::TokenLogin {
                instance: origin,
                token: "secret".into(),
            }
        })
        .await
        .unwrap();
    if restoring {
        let saved = timeout(
            Duration::from_secs(5),
            restore_result(&mut worker.restore_task),
        )
        .await
        .unwrap();
        worker.restore_task = None;
        worker.finish_restore(saved).await.unwrap();
    }
    if browser_login {
        assert!(worker.pending_browser.is_some());
        assert!(worker.state.current_user.is_none());
        assert!(events.try_iter().any(|event|matches!(event,Event::BrowserLoginRequired{ref code,ref browser_url,..}if code=="ABCDEFGH" && browser_url.contains("login=alice%40example.com") && !browser_url.contains("private-proof"))));
        for _ in 0..2 {
            tokio::time::sleep_until(worker.pending_browser.as_ref().unwrap().next_poll.unwrap())
                .await;
            worker.begin_browser_poll();
            let (session, result) = timeout(
                Duration::from_secs(5),
                browser_poll_result(&mut worker.browser_poll_task),
            )
            .await
            .unwrap()
            .unwrap();
            worker.browser_poll_task = None;
            worker.pending_browser.as_mut().unwrap().session = Some(session);
            worker
                .finish_browser_poll(result.map_err(error))
                .await
                .unwrap();
        }
        assert!(worker.pending_browser.is_none());
        assert!(
            events
                .try_iter()
                .any(|event| matches!(event, Event::BrowserLoginCleared))
        );
    }
    if ip_authorization {
        assert!(worker.pending_ip.is_some());
        assert!(worker.state.current_user.is_none());
        assert!(events.try_iter().any(|event| matches!(
            event,
            Event::IpAuthorizationRequired {
                resend_available_in: 0,
                ..
            }
        )));
        tokio::time::sleep_until(worker.pending_ip.as_ref().unwrap().next_poll.unwrap()).await;
        worker.begin_ip_poll();
        let result = timeout(
            Duration::from_secs(5),
            ip_poll_result(&mut worker.ip_poll_task),
        )
        .await
        .unwrap()
        .unwrap()
        .map_err(error);
        worker.ip_poll_task = None;
        worker.finish_ip_poll(result).await.unwrap();
        assert!(worker.pending_ip.is_some());
        worker
            .command(Command::ResendIpAuthorization)
            .await
            .unwrap();
        assert!(worker.pending_ip.as_ref().unwrap().resend_used);
        assert!(events.try_iter().any(|event| matches!(
            event,
            Event::IpAuthorizationResent {
                resend_available_in: 30
            }
        )));
        tokio::time::sleep_until(worker.pending_ip.as_ref().unwrap().next_poll.unwrap()).await;
        worker.begin_ip_poll();
        let result = timeout(
            Duration::from_secs(5),
            ip_poll_result(&mut worker.ip_poll_task),
        )
        .await
        .unwrap()
        .unwrap()
        .map_err(error);
        worker.ip_poll_task = None;
        worker.finish_ip_poll(result).await.unwrap();
        assert!(worker.pending_ip.is_none());
        assert!(
            events
                .try_iter()
                .any(|event| matches!(event, Event::IpAuthorizationCleared))
        );
    }
    timeout(Duration::from_secs(5), async {
        while !worker.gateway_ready {
            let event = worker
                .gateway_events
                .as_mut()
                .unwrap()
                .recv()
                .await
                .unwrap();
            worker.gateway(event).await.unwrap();
        }
    })
    .await
    .unwrap();
    worker
        .command(Command::SelectGuild {
            guild_id: 10.into(),
        })
        .await
        .unwrap();
    worker
        .command(Command::SelectChannel {
            channel_id: 100.into(),
        })
        .await
        .unwrap();
    finish_history(&mut worker, 100.into()).await;
    worker
        .command(Command::SendMessage {
            content: "hello".into(),
            reply_to: None,
            mention_author: false,
        })
        .await
        .unwrap();
    finish_attachment_send(&mut worker, 100.into()).await;
    dispatch_tx
        .send(json!({"op":0,"t":"MESSAGE_CREATE","s":2,"d":sent}))
        .await
        .unwrap();
    timeout(Duration::from_secs(5), async {
        while worker.last_sequence != Some(2) {
            let event = worker
                .gateway_events
                .as_mut()
                .unwrap()
                .recv()
                .await
                .unwrap();
            worker.gateway(event).await.unwrap();
        }
    })
    .await
    .unwrap();
    assert_eq!(
        worker.state.channel_messages(100.into()).count(),
        2,
        "REST and gateway confirmation must not duplicate a send"
    );
    worker
        .command(Command::SetDraft("fail".into()))
        .await
        .unwrap();
    worker
        .command(Command::SendMessage {
            content: "fail".into(),
            reply_to: None,
            mention_author: false,
        })
        .await
        .unwrap();
    finish_attachment_send(&mut worker, 100.into()).await;
    assert!(!worker.state.drafts.contains_key(&100.into()));
    let events = events.try_iter().collect::<Vec<_>>();
    assert!(events.iter().any(|event| matches!(event, Event::Outbox { channel_id, messages } if *channel_id == Snowflake(100) && messages.iter().any(|message| message.content == "fail" && matches!(&message.status, crate::PendingMessageStatus::Failed(_))))));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, Event::MessageSent { .. }))
            .count(),
        1
    );
    assert!(events.iter().any(|event| matches!(event, Event::MessageOperationFailed { channel_id, content, error } if *channel_id==Snowflake(100) && content=="fail" && error.contains("403"))));
    assert!(
        worker.media.is_none(),
        "text workflow must never initialize physical audio"
    );
    let saved = store
        .load()
        .await
        .unwrap()
        .expect("every completed login is persisted");
    assert_eq!(saved.user_id, Snowflake(1));
    assert_eq!(saved.token.expose_secret(), "secret");
    assert_eq!(saved.selected_guild, Some(10.into()));
    assert_eq!(saved.selected_channel, Some(100.into()));
    worker.reset().await;
    assert!(
        store.load().await.unwrap().is_some(),
        "quitting/resetting must not log out"
    );
    http_server.abort();
    websocket.abort();
}

fn stored_worker_session() -> SavedSession {
    SavedSession {
        instance: Instance::from_endpoints(
            "https://saved.example/api",
            "wss://saved.example/gateway",
        )
        .unwrap(),
        token: SessionToken::new("saved-test-secret").unwrap(),
        user_id: 1.into(),
        user: Some(json!({"id":"1","username":"alice"})),
        locale: "en-US".into(),
        selected_guild: None,
        selected_channel: None,
        valid: true,
    }
}

#[tokio::test]
async fn reset_demo_and_transient_gateway_errors_preserve_saved_credentials() {
    let store = Arc::new(fluxer_session::MemorySessionStore::default());
    store.save(stored_worker_session()).await.unwrap();
    let (worker, _) = worker();
    let mut worker = worker.with_session_store(store.clone());
    worker.saved_session = store.load().await.unwrap();
    worker
        .gateway(GatewayEvent::Error("temporary connection failure".into()))
        .await
        .unwrap();
    worker.gateway(GatewayEvent::Disconnected).await.unwrap();
    assert!(store.load().await.unwrap().unwrap().valid);
    worker.reset().await;
    assert!(store.load().await.unwrap().unwrap().valid);
    worker.command(Command::Demo).await.unwrap();
    worker.command(Command::Logout).await.unwrap();
    assert!(
        store.load().await.unwrap().is_some(),
        "offline demo logout must not erase a real saved account"
    );
}

#[tokio::test]
async fn definitive_gateway_auth_failure_retains_invalid_record_without_auto_login() {
    let store = Arc::new(fluxer_session::MemorySessionStore::default());
    store.save(stored_worker_session()).await.unwrap();
    let (original, _) = worker();
    let mut original = original.with_session_store(store.clone());
    original.saved_session = store.load().await.unwrap();
    original
        .gateway(GatewayEvent::AuthenticationFailed)
        .await
        .unwrap();
    assert!(original.state.current_user.is_none());
    assert!(!store.load().await.unwrap().unwrap().valid);
    let (next, events) = worker();
    let mut next = next.with_session_store(store.clone());
    next.command(Command::RestoreSession).await.unwrap();
    let restored = timeout(
        Duration::from_secs(2),
        restore_result(&mut next.restore_task),
    )
    .await
    .unwrap();
    next.restore_task = None;
    next.finish_restore(restored).await.unwrap();
    assert!(
        next.gateway_task.is_none(),
        "invalid saved records must not open a gateway connection"
    );
    assert!(next.media.is_none());
    assert!(
        events
            .try_iter()
            .any(|event| matches!(event, Event::Status(message) if message.contains("expired")))
    );
}

#[tokio::test]
async fn explicit_logout_clears_saved_credentials_even_when_server_notification_fails() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let request = read_request(&mut socket).await;
        assert!(request.starts_with("POST /api/v1/auth/logout HTTP/1.1"));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: saved-test-secret")
        );
        socket.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}").await.unwrap();
    });
    let store = Arc::new(fluxer_session::MemorySessionStore::default());
    let mut saved = stored_worker_session();
    saved.instance =
        Instance::from_endpoints(&format!("{origin}/api"), "ws://127.0.0.1/gateway").unwrap();
    store.save(saved.clone()).await.unwrap();
    let (worker, events) = worker();
    let mut worker = worker.with_session_store(store.clone());
    worker.client = Some(
        Client::new(saved.instance.clone())
            .unwrap()
            .with_token(saved.token.clone()),
    );
    worker.saved_session = Some(saved);
    worker.command(Command::Logout).await.unwrap();
    assert!(store.load().await.unwrap().is_none());
    assert!(worker.client.is_none());
    assert!(worker.state.current_user.is_none());
    assert!(events.try_iter().any(
        |event| matches!(event,Event::Error(message) if message.contains("Signed out locally"))
    ));
    server.await.unwrap();
}

struct LockedSessionVault;
impl SessionStore for LockedSessionVault {
    fn load(&self) -> fluxer_session::StoreFuture<'_, Option<SavedSession>> {
        Box::pin(async { Err(fluxer_session::StoreError::Unavailable) })
    }
    fn save(&self, _: SavedSession) -> fluxer_session::StoreFuture<'_, ()> {
        Box::pin(async { Err(fluxer_session::StoreError::Unavailable) })
    }
    fn clear(&self) -> fluxer_session::StoreFuture<'_, ()> {
        Box::pin(async { Err(fluxer_session::StoreError::Unavailable) })
    }
}

#[tokio::test]
async fn failed_durable_logout_does_not_falsely_report_account_signed_out() {
    let (worker, _) = worker();
    let mut worker = worker.with_session_store(Arc::new(LockedSessionVault));
    worker.saved_session = Some(stored_worker_session());
    worker.state.current_user =
        Some(serde_json::from_value(json!({"id":"1","username":"alice"})).unwrap());
    assert!(worker.command(Command::Logout).await.is_err());
    assert!(worker.state.current_user.is_some());
    assert!(worker.saved_session.is_some());
}

struct PendingSessionVault;
impl SessionStore for PendingSessionVault {
    fn load(&self) -> fluxer_session::StoreFuture<'_, Option<SavedSession>> {
        Box::pin(pending())
    }
    fn save(&self, _: SavedSession) -> fluxer_session::StoreFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn clear(&self) -> fluxer_session::StoreFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

struct SlowSelectionStore {
    inner: fluxer_session::MemorySessionStore,
    started: async_mpsc::UnboundedSender<Option<Snowflake>>,
    permits: tokio::sync::Semaphore,
}

impl SessionStore for SlowSelectionStore {
    fn load(&self) -> fluxer_session::StoreFuture<'_, Option<SavedSession>> {
        self.inner.load()
    }
    fn save(&self, session: SavedSession) -> fluxer_session::StoreFuture<'_, ()> {
        Box::pin(async move {
            self.started.send(session.selected_channel).unwrap();
            self.permits.acquire().await.unwrap().forget();
            self.inner.save(session).await
        })
    }
    fn clear(&self) -> fluxer_session::StoreFuture<'_, ()> {
        self.inner.clear()
    }
}

#[tokio::test]
async fn selection_saves_coalesce_without_blocking_state_and_finish_before_logout() {
    let (started, mut writes) = async_mpsc::unbounded_channel();
    let store = Arc::new(SlowSelectionStore {
        inner: fluxer_session::MemorySessionStore::default(),
        started,
        permits: tokio::sync::Semaphore::new(0),
    });
    let (worker, events) = worker();
    let mut worker = worker.with_session_store(store.clone());
    worker.saved_session = Some(stored_worker_session());
    worker.state = ClientState::demo();

    worker.state.selected_channel = Some(11.into());
    worker.persist_selection();
    assert_eq!(writes.recv().await, Some(Some(11.into())));
    worker.state.selected_channel = Some(12.into());
    worker.persist_selection();
    worker.state.selected_channel = Some(13.into());
    worker.persist_selection();
    worker.snapshot();
    assert!(events.try_iter().any(|event| {
        matches!(event, Event::State(state) if state.selected_channel == Some(13.into()))
    }));
    assert!(store.load().await.unwrap().is_none());

    let logout = tokio::spawn(async move { worker.command(Command::Logout).await });
    store.permits.add_permits(1);
    assert_eq!(writes.recv().await, Some(Some(13.into())));
    assert!(!logout.is_finished());
    store.permits.add_permits(1);
    tokio::time::timeout(Duration::from_secs(2), logout)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(store.load().await.unwrap().is_none());
    assert!(writes.try_recv().is_err());
}

#[tokio::test]
async fn cancelled_restore_cannot_later_replace_a_new_account_or_open_capture() {
    let (worker, _) = worker();
    let mut worker = worker.with_session_store(Arc::new(PendingSessionVault));
    worker.command(Command::RestoreSession).await.unwrap();
    let pending = worker.restore_task.as_ref().unwrap().abort_handle();
    worker.command(Command::CancelLogin).await.unwrap();
    tokio::task::yield_now().await;
    assert!(pending.is_finished());
    assert!(worker.restore_task.is_none());
    assert!(worker.gateway_task.is_none());
    assert!(worker.media.is_none());
    assert!(worker.state.current_user.is_none());
}

#[tokio::test]
async fn wrong_account_ready_invalidates_saved_session_without_adopting_foreign_state() {
    let store = Arc::new(fluxer_session::MemorySessionStore::default());
    store.save(stored_worker_session()).await.unwrap();
    let (worker, _) = worker();
    let mut worker = worker.with_session_store(store.clone());
    worker.saved_session = store.load().await.unwrap();
    worker.expected_user_id = Some(1.into());
    assert!(worker.gateway(GatewayEvent::Dispatch {
        name: "READY".into(),
        data: json!({"session_id":"wrong-account","user":{"id":"2","username":"other"},"guilds":[]}),
        sequence: Some(1),
    }).await.is_err());
    let retained = store.load().await.unwrap().unwrap();
    assert_eq!(retained.user_id, Snowflake(1));
    assert!(!retained.valid);
    assert!(worker.state.current_user.is_none());
    assert!(worker.media.is_none());
}

#[tokio::test]
async fn actual_reply_and_forward_requests_keep_source_destination_and_failed_outbox() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        for step in 0..3 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let request = read_request(&mut socket).await;
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("authorization: fixture-secret")
            );
            let body: Value =
                serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
            assert!(body["nonce"].is_string());
            let (status, response) = match step {
                0 => {
                    assert!(request.starts_with("POST /api/v1/channels/11/messages "));
                    assert_eq!(body["content"], "reply");
                    assert_eq!(
                        body["message_reference"],
                        json!({"channel_id":"11","message_id":"200","type":0})
                    );
                    assert_eq!(body["allowed_mentions"], json!({"replied_user":false}));
                    (
                        200,
                        json!({"id":"900","channel_id":"11","author":{"id":"100","username":"you"},"content":"reply","message_reference":body["message_reference"],"type":19}),
                    )
                }
                1 => {
                    assert!(request.starts_with("POST /api/v1/channels/13/messages "));
                    assert!(body.get("content").is_none());
                    assert_eq!(
                        body["message_reference"],
                        json!({"channel_id":"11","message_id":"200","guild_id":"1","type":1})
                    );
                    assert!(body.get("allowed_mentions").is_none());
                    (
                        200,
                        json!({"id":"901","channel_id":"13","author":{"id":"100","username":"you"},"content":"","message_reference":body["message_reference"],"message_snapshots":[{"content":"history","timestamp":"2026-01-01T00:00:00Z","type":0,"flags":0}]}),
                    )
                }
                _ => {
                    assert_eq!(body["content"], "failed reply");
                    assert_eq!(body["allowed_mentions"], json!({"replied_user":true}));
                    (403, json!({"code":"MISSING_PERMISSIONS"}))
                }
            };
            let response = response.to_string();
            let reason = if status == 200 { "OK" } else { "Forbidden" };
            socket.write_all(format!("HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",response.len()).as_bytes()).await.unwrap();
            socket.shutdown().await.unwrap();
        }
    });
    let (mut worker, events) = worker();
    worker.state = ClientState::demo();
    worker.state.select_channel(11.into());
    worker.client = Some(
        Client::new(
            Instance::from_endpoints(&format!("{origin}/api"), "ws://127.0.0.1/gateway").unwrap(),
        )
        .unwrap()
        .with_token(SessionToken::new("fixture-secret").unwrap()),
    );
    worker
        .command(Command::SendMessage {
            content: "reply".into(),
            reply_to: Some(200.into()),
            mention_author: false,
        })
        .await
        .unwrap();
    finish_attachment_send(&mut worker, 11.into()).await;
    let reply = worker
        .state
        .channel_messages(11.into())
        .find(|message| message.id == Snowflake(900))
        .unwrap();
    assert_eq!(
        reply.message_reference.as_ref().unwrap().message_id,
        Some(200.into())
    );
    worker
        .command(Command::SetDraft("keep this draft".into()))
        .await
        .unwrap();
    worker
        .command(Command::ForwardMessage {
            source_channel: 11.into(),
            message_id: 200.into(),
            destination: 13.into(),
        })
        .await
        .unwrap();
    assert_eq!(worker.state.selected_channel, Some(11.into()));
    assert_eq!(
        worker.state.drafts.get(&11.into()).map(String::as_str),
        Some("keep this draft")
    );
    assert_eq!(
        worker
            .state
            .channel_messages(13.into())
            .find(|message| message.id == Snowflake(901))
            .unwrap()
            .extra["message_snapshots"][0]["content"],
        "history"
    );
    worker
        .command(Command::SetDraft("failed reply".into()))
        .await
        .unwrap();
    worker
        .command(Command::SendMessage {
            content: "failed reply".into(),
            reply_to: Some(200.into()),
            mention_author: true,
        })
        .await
        .unwrap();
    finish_attachment_send(&mut worker, 11.into()).await;
    assert!(!worker.state.drafts.contains_key(&11.into()));
    let events = events.try_iter().collect::<Vec<_>>();
    assert!(events.iter().any(|event| matches!(event, Event::Outbox { channel_id, messages } if *channel_id == Snowflake(11) && messages.iter().any(|message| message.content == "failed reply" && matches!(&message.status, crate::PendingMessageStatus::Failed(_))))));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, Event::MessageSent { .. }))
            .count(),
        1
    );
    assert!(events.iter().any(|event|matches!(event,Event::MessageOperationFailed{channel_id,content,error} if *channel_id==Snowflake(11) && content=="failed reply" && error.contains("403"))));
    assert!(worker.media.is_none());
    timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn demo_custom_reactions_toggle_by_id_and_forward_does_not_copy_author_identity() {
    let (mut worker, _events) = worker();
    worker.state = ClientState::demo();
    worker.demo = true;
    worker.state.select_channel(11.into());
    let source = Message::text(
        950.into(),
        11.into(),
        User {
            id: 999.into(),
            username: "source".into(),
            ..User::default()
        },
        "Forwarded text",
    );
    worker.state.insert_message(source);
    for name in ["party:42", "renamed:42"] {
        worker
            .command(Command::React {
                message_id: 950.into(),
                emoji: name.into(),
            })
            .await
            .unwrap();
    }
    assert!(
        worker
            .state
            .channel_messages(11.into())
            .find(|message| message.id == Snowflake(950))
            .unwrap()
            .reactions
            .is_empty()
    );
    worker
        .command(Command::ForwardMessage {
            source_channel: 11.into(),
            message_id: 950.into(),
            destination: 13.into(),
        })
        .await
        .unwrap();
    let forward = worker
        .state
        .channel_messages(13.into())
        .find(|message| {
            message
                .message_reference
                .as_ref()
                .is_some_and(|reference| reference.kind == 1)
        })
        .unwrap();
    assert_eq!(
        forward.author.id,
        worker.state.current_user.as_ref().unwrap().id
    );
    assert_eq!(
        forward.extra["message_snapshots"][0]["content"],
        "Forwarded text"
    );
    assert!(
        forward.extra["message_snapshots"][0]
            .get("message")
            .is_none()
    );
    let forwarded_id = forward.id;
    worker
        .command(Command::ForwardMessage {
            source_channel: 13.into(),
            message_id: forwarded_id,
            destination: 11.into(),
        })
        .await
        .unwrap();
    let reforward = worker
        .state
        .channel_messages(11.into())
        .find(|message| {
            message
                .message_reference
                .as_ref()
                .is_some_and(|reference| reference.channel_id == Snowflake(13))
        })
        .unwrap();
    assert_eq!(
        reforward.extra["message_snapshots"][0]["content"],
        "Forwarded text"
    );
    assert!(
        worker
            .command(Command::ForwardMessage {
                source_channel: 11.into(),
                message_id: 950.into(),
                destination: 10.into()
            })
            .await
            .is_err()
    );
}

#[tokio::test]
async fn open_dm_failure_keeps_navigation_and_account_state() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let request = read_request(&mut socket).await;
        assert!(request.starts_with("POST /api/v1/users/@me/channels "));
        assert_eq!(
            serde_json::from_str::<Value>(request.split_once("\r\n\r\n").unwrap().1).unwrap(),
            json!({"recipient_id":"42"})
        );
        let body = json!({"code":"CANNOT_SEND_MESSAGES_TO_USER"}).to_string();
        socket.write_all(format!("HTTP/1.1 403 Forbidden\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    });
    let (mut worker, _) = worker();
    worker.state = ClientState::demo();
    worker.state.selected_channel = Some(11.into());
    worker.client = Some(
        Client::new(
            Instance::from_endpoints(&format!("{origin}/api"), "ws://127.0.0.1/gateway").unwrap(),
        )
        .unwrap()
        .with_token(SessionToken::new("fixture-secret").unwrap()),
    );
    let before = worker.state.channels.clone();
    worker.open_dm(42.into()).await.unwrap();
    let event = timeout(Duration::from_secs(5), worker.dm_rx.recv())
        .await
        .unwrap()
        .unwrap();
    worker.finish_open_dm(event).await.unwrap();
    assert_eq!(worker.state.selected_channel, Some(11.into()));
    assert_eq!(worker.state.channels, before);
    server.await.unwrap();
    worker.reset().await;
    assert!(worker.state.current_user.is_none());
    assert!(worker.open_dm(42.into()).await.is_err());
}

#[tokio::test]
async fn open_existing_dm_reuses_it_without_server_request() {
    let (mut worker, _) = worker();
    worker.state = ClientState::demo();
    worker.demo = true;
    worker.state.channels.insert(
        90.into(),
        serde_json::from_value(
            json!({"id":"90","type":1,"recipients":[{"id":"42","username":"Ada"}]}),
        )
        .unwrap(),
    );
    worker.open_dm(42.into()).await.unwrap();
    assert_eq!(worker.state.selected_channel, Some(90.into()));
}

#[tokio::test]
async fn member_query_sends_source_shape_and_rejects_stale_chunks() {
    let (mut worker, _) = worker();
    worker.state = ClientState::demo();
    worker.selected_guild = Some(1.into());
    worker.gateway_ready = true;
    let (tx, mut rx) = async_mpsc::channel(4);
    worker.gateway_commands = Some(tx);
    worker
        .query_guild_members(1.into(), "al".into(), 25)
        .await
        .unwrap();
    let GatewayCommand::Send { op, data } = rx.recv().await.unwrap() else {
        panic!("expected request")
    };
    assert_eq!(op, 8);
    assert_eq!(data["guild_id"], "1");
    assert_eq!(data["query"], "al");
    assert_eq!(data["limit"], 25);
    assert_eq!(data["presences"], true);
    let nonce = data["nonce"].as_str().unwrap().to_owned();
    let chunk = |nonce: &str| json!({"guild_id":"1","members":[{"user":{"id":"987","username":"Alice"},"roles":[]}],"presences":[],"chunk_index":0,"chunk_count":1,"nonce":nonce});
    worker
        .gateway(GatewayEvent::Dispatch {
            name: "GUILD_MEMBERS_CHUNK".into(),
            data: chunk("stale"),
            sequence: None,
        })
        .await
        .unwrap();
    assert!(!worker.state.members.contains_key(&(1.into(), 987.into())));
    worker
        .gateway(GatewayEvent::Dispatch {
            name: "GUILD_MEMBERS_CHUNK".into(),
            data: chunk(&nonce),
            sequence: None,
        })
        .await
        .unwrap();
    assert_eq!(
        worker.state.members[&(1.into(), 987.into())].user.username,
        "Alice"
    );
    worker.selected_guild = Some(2.into());
    worker
        .gateway(GatewayEvent::Dispatch {
            name: "GUILD_MEMBERS_CHUNK".into(),
            data: chunk(&nonce),
            sequence: None,
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn pending_dm_is_cancelled_by_navigation_and_account_reset() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let _request = read_request(&mut socket).await;
        let _ = accepted_tx.send(());
        tokio::time::sleep(Duration::from_secs(1)).await;
        let body =
            json!({"id":"999","type":1,"recipients":[{"id":"42","username":"Alice"}]}).to_string();
        let _ = socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await;
    });
    let (mut worker, _) = worker();
    worker.state = ClientState::demo();
    worker.state.selected_channel = Some(11.into());
    worker.client = Some(
        Client::new(
            Instance::from_endpoints(&format!("{origin}/api"), "ws://127.0.0.1/gateway").unwrap(),
        )
        .unwrap()
        .with_token(SessionToken::new("fixture-secret").unwrap()),
    );
    worker.open_dm(42.into()).await.unwrap();
    timeout(Duration::from_secs(2), accepted_rx)
        .await
        .unwrap()
        .unwrap();
    let old = worker.dm_generation;
    worker
        .command(Command::SelectChannel {
            channel_id: 12.into(),
        })
        .await
        .unwrap();
    worker
        .finish_open_dm(DmEvent {
            group_add: false,
            user_id: 42.into(),
            generation: old,
            result: Ok(json!({"id":"999","type":1,"recipients":[{"id":"42","username":"Alice"}]})),
        })
        .await
        .unwrap();
    assert_eq!(worker.state.selected_channel, Some(12.into()));
    assert!(!worker.state.channels.contains_key(&999.into()));
    worker.reset().await;
    worker
        .finish_open_dm(DmEvent {
            group_add: false,
            user_id: 42.into(),
            generation: old,
            result: Ok(json!({"id":"999","type":1,"recipients":[{"id":"42","username":"Alice"}]})),
        })
        .await
        .unwrap();
    assert!(worker.state.current_user.is_none());
    assert!(worker.state.channels.is_empty());
    server.abort();
}

#[tokio::test]
async fn pending_group_creation_cannot_override_message_jump_or_reset() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let request = read_request(&mut socket).await;
        assert_eq!(
            serde_json::from_str::<Value>(request.split_once("\r\n\r\n").unwrap().1).unwrap(),
            json!({"recipients":[]})
        );
        let _ = accepted_tx.send(());
        tokio::time::sleep(Duration::from_secs(5)).await;
    });
    let (mut worker, _) = worker();
    worker.state = ClientState::demo();
    worker.state.selected_channel = Some(11.into());
    worker.client = Some(
        Client::new(
            Instance::from_endpoints(&format!("{origin}/api"), "ws://127.0.0.1/gateway").unwrap(),
        )
        .unwrap()
        .with_token(SessionToken::new("fixture-secret").unwrap()),
    );
    timeout(Duration::from_secs(1), worker.create_group_dm(vec![]))
        .await
        .unwrap()
        .unwrap();
    timeout(Duration::from_secs(2), accepted_rx)
        .await
        .unwrap()
        .unwrap();
    let old = worker.dm_generation;
    worker.demo = true;
    worker.jump_to_message(12.into(), 123.into()).await.unwrap();
    worker
        .finish_open_dm(DmEvent {
            group_add: false,
            user_id: 0.into(),
            generation: old,
            result: Ok(json!({"id":"999","type":3,"recipients":[]})),
        })
        .await
        .unwrap();
    assert_eq!(worker.state.selected_channel, Some(12.into()));
    assert!(!worker.state.channels.contains_key(&999.into()));
    worker.reset().await;
    worker
        .finish_open_dm(DmEvent {
            group_add: false,
            user_id: 0.into(),
            generation: old,
            result: Ok(json!({"id":"999","type":3,"recipients":[]})),
        })
        .await
        .unwrap();
    assert!(worker.state.current_user.is_none());
    assert!(worker.state.channels.is_empty());
    server.abort();
}

#[tokio::test]
async fn notification_events_only_follow_new_live_message_dispatches() {
    let (mut worker, events) = worker();
    worker.state = ClientState::demo();
    worker.state.selected_channel = Some(Snowflake(11));
    let body = json!({"id":"999","channel_id":"11","author":{"id":"101","username":"alex"},"content":"new live message","type":0});
    worker
        .gateway(GatewayEvent::Dispatch {
            name: "MESSAGE_CREATE".into(),
            data: body.clone(),
            sequence: Some(1),
        })
        .await
        .unwrap();
    worker
        .gateway(GatewayEvent::Dispatch {
            name: "MESSAGE_CREATE".into(),
            data: body.clone(),
            sequence: Some(1),
        })
        .await
        .unwrap();
    worker
        .gateway(GatewayEvent::Dispatch {
            name: "MESSAGE_UPDATE".into(),
            data: body.clone(),
            sequence: Some(2),
        })
        .await
        .unwrap();
    worker
        .gateway(GatewayEvent::Dispatch {
            name: "MESSAGE_CREATE".into(),
            data: body.clone(),
            sequence: Some(3),
        })
        .await
        .unwrap();
    worker.snapshot();
    let live: Vec<_> = events
        .try_iter()
        .filter_map(|event| {
            if let Event::LiveMessageCreated {
                account_id,
                message,
            } = event
            {
                Some((account_id, message.id))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(live, vec![(Snowflake(100), Snowflake(999))]);
    worker.demo = true;
    worker.gateway(GatewayEvent::Dispatch{name:"MESSAGE_CREATE".into(),data:json!({"id":"1000","channel_id":"11","author":{"id":"101","username":"alex"},"content":"offline fixture"}),sequence:Some(4)}).await.unwrap();
    assert!(
        !events
            .try_iter()
            .any(|event| matches!(event, Event::LiveMessageCreated { .. }))
    );
}

#[tokio::test]
async fn member_list_subscribes_bounded_ranges_and_unsubscribes() {
    let (mut worker, _) = worker();
    worker.state = ClientState::demo();
    worker.state.guilds.get_mut(&1.into()).unwrap().owner_id = 100.into();
    worker.state.selected_channel = Some(11.into());
    worker.gateway_ready = true;
    let (tx, mut rx) = async_mpsc::channel(4);
    worker.gateway_commands = Some(tx);
    worker.set_member_list(Some(11.into()), 100).await.unwrap();
    let GatewayCommand::Send { op, data } = rx.recv().await.unwrap() else {
        panic!()
    };
    assert_eq!(op, 14);
    assert_eq!(
        data,
        json!({"subscriptions":{"1":{"active":true,"member_list_channels":{"11":[[100,199]]}}}})
    );
    assert!(worker.set_member_list(Some(12.into()), 0).await.is_err());
    worker.set_member_list(None, 0).await.unwrap();
    let GatewayCommand::Send { op, data } = rx.recv().await.unwrap() else {
        panic!()
    };
    assert_eq!(op, 14);
    assert_eq!(
        data,
        json!({"subscriptions":{"1":{"member_list_channels":{"11":[]}}}})
    );
    worker.state.guilds.get_mut(&1.into()).unwrap().owner_id = 101.into();
    worker.state.guilds.get_mut(&1.into()).unwrap().roles[0].permissions =
        Permissions::VIEW_CHANNEL;
    assert!(worker.set_member_list(Some(11.into()), 0).await.is_err());
}

#[tokio::test]
async fn member_list_dispatch_is_fenced_by_channel_and_permission() {
    let (mut worker, _) = worker();
    worker.state = ClientState::demo();
    worker.state.guilds.get_mut(&1.into()).unwrap().owner_id = 100.into();
    worker.state.selected_channel = Some(11.into());
    worker.member_list_subscription = Some((1.into(), 11.into(), 0));
    worker.member_list_identity = Some(fluxer_core::member_list_identity(
        &worker.state.guilds[&1.into()],
        &worker.state.channels[&11.into()],
    ));
    let payload = |channel: &str| json!({"guild_id":"1","channel_id":channel,"id":"everyone","member_count":1,"online_count":1,"ops":[{"op":"SYNC","range":[0,0],"items":[{"member":{"user":{"id":"42","username":"Alice"},"roles":[]}}]}]});
    worker
        .gateway(GatewayEvent::Dispatch {
            name: "GUILD_MEMBER_LIST_UPDATE".into(),
            data: payload("12"),
            sequence: None,
        })
        .await
        .unwrap();
    assert!(worker.state.member_lists.is_empty());
    worker
        .gateway(GatewayEvent::Dispatch {
            name: "GUILD_MEMBER_LIST_UPDATE".into(),
            data: payload("11"),
            sequence: None,
        })
        .await
        .unwrap();
    assert_eq!(
        worker.state.member_lists[&(1.into(), 11.into())].rows.len(),
        1
    );
    assert!(worker.state.members.contains_key(&(1.into(), 42.into())));
    worker.state.guilds.get_mut(&1.into()).unwrap().owner_id = 101.into();
    worker.state.guilds.get_mut(&1.into()).unwrap().roles[0].permissions =
        Permissions::VIEW_CHANNEL;
    worker
        .gateway(GatewayEvent::Dispatch {
            name: "GUILD_MEMBER_LIST_UPDATE".into(),
            data: payload("11"),
            sequence: None,
        })
        .await
        .unwrap();
    assert_eq!(
        worker.state.member_lists[&(1.into(), 11.into())].rows.len(),
        1
    );
}

#[tokio::test]
async fn member_list_channel_less_updates_require_registered_wire_id_and_current_range() {
    let (mut worker, _) = worker();
    worker.state = ClientState::demo();
    worker.state.guilds.get_mut(&1.into()).unwrap().owner_id = 100.into();
    worker.state.selected_channel = Some(11.into());
    worker.member_list_subscription = Some((1.into(), 11.into(), 0));
    worker.member_list_identity = Some(fluxer_core::member_list_identity(
        &worker.state.guilds[&1.into()],
        &worker.state.channels[&11.into()],
    ));
    let payload = |wire: &str, channel: Option<&str>, index: u32| {
        let mut data = json!({"guild_id":"1","id":wire,"member_count":200,"online_count":1,"groups":[{"id":"online","count":200}],"ops":[{"op":"SYNC","range":[index,index],"items":[{"group":{"id":"online","count":200}}]}]});
        if let Some(channel) = channel {
            data["channel_id"] = json!(channel);
        }
        data
    };
    worker
        .gateway(GatewayEvent::Dispatch {
            name: "GUILD_MEMBER_LIST_UPDATE".into(),
            data: payload("wire-a", None, 0),
            sequence: None,
        })
        .await
        .unwrap();
    assert!(worker.state.member_lists.is_empty());
    worker
        .gateway(GatewayEvent::Dispatch {
            name: "GUILD_MEMBER_LIST_UPDATE".into(),
            data: payload("wire-a", Some("11"), 0),
            sequence: None,
        })
        .await
        .unwrap();
    worker
        .gateway(GatewayEvent::Dispatch {
            name: "GUILD_MEMBER_LIST_UPDATE".into(),
            data: payload("wire-b", None, 1),
            sequence: None,
        })
        .await
        .unwrap();
    worker
        .gateway(GatewayEvent::Dispatch {
            name: "GUILD_MEMBER_LIST_UPDATE".into(),
            data: payload("wire-a", None, 101),
            sequence: None,
        })
        .await
        .unwrap();
    assert_eq!(
        worker.state.member_lists[&(1.into(), 11.into())].rows.len(),
        1
    );
    worker
        .gateway(GatewayEvent::Dispatch {
            name: "GUILD_MEMBER_LIST_UPDATE".into(),
            data: payload("wire-a", None, 1),
            sequence: None,
        })
        .await
        .unwrap();
    assert_eq!(
        worker.state.member_lists[&(1.into(), 11.into())].rows.len(),
        2
    );
}

#[tokio::test]
async fn relationship_mutations_match_classic_requests_and_remain_gateway_authoritative() {
    use crate::RelationshipAction;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        for (method, body) in [
            ("POST", Some(json!({}))),
            ("PUT", None),
            ("PUT", Some(json!({"type":2}))),
            ("DELETE", None),
        ] {
            let (mut socket, _) = listener.accept().await.unwrap();
            let request = read_request(&mut socket).await;
            assert!(request.starts_with(&format!("{method} /api/v1/users/@me/relationships/42 ")));
            let raw = request.split_once("\r\n\r\n").unwrap().1;
            match body {
                Some(expected) => assert_eq!(serde_json::from_str::<Value>(raw).unwrap(), expected),
                None => assert!(raw.is_empty()),
            };
            socket
                .write_all(
                    b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
        }
    });
    let (mut worker, _) = worker();
    worker.state = ClientState::demo();
    worker.client = Some(
        Client::new(
            Instance::from_endpoints(&format!("{origin}/api"), "ws://127.0.0.1/gateway").unwrap(),
        )
        .unwrap()
        .with_token(SessionToken::new("fixture-secret").unwrap()),
    );
    for (action, kind) in [
        (RelationshipAction::Send, None),
        (RelationshipAction::Accept, Some(3)),
        (RelationshipAction::Block, Some(1)),
        (RelationshipAction::Remove, Some(2)),
    ] {
        worker.state.relationships.remove(&42.into());
        if let Some(kind) = kind {
            worker.state.relationships.insert(
                42.into(),
                fluxer_core::Relationship {
                    id: 42.into(),
                    kind,
                    ..Default::default()
                },
            );
        }
        let before = worker.state.relationships.clone();
        worker.change_relationship(42.into(), action).unwrap();
        let event = timeout(Duration::from_secs(5), worker.people.rx.recv())
            .await
            .unwrap()
            .unwrap();
        worker.people_event(event);
        assert_eq!(worker.state.relationships, before);
    }
    assert!(
        worker
            .change_relationship(100.into(), RelationshipAction::Block)
            .is_err()
    );
    worker.reset().await;
    assert!(
        worker
            .change_relationship(42.into(), RelationshipAction::Send)
            .is_err()
    );
    server.await.unwrap();
}

#[tokio::test]
async fn relationship_failure_is_visible_and_stale_completion_cannot_cross_accounts() {
    use crate::{RelationshipAction, RelationshipMutationStatus};
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        for status in [403, 200] {
            let (mut socket, _) = listener.accept().await.unwrap();
            let _ = read_request(&mut socket).await;
            let body = if status == 403 {
                json!({"code":"FRIEND_REQUEST_BLOCKED"})
            } else {
                json!({})
            }
            .to_string();
            socket.write_all(format!("HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",if status==403{"Forbidden"}else{"OK"},body.len()).as_bytes()).await.unwrap();
        }
    });
    let (mut worker, events) = worker();
    worker.state = ClientState::demo();
    worker.client = Some(
        Client::new(
            Instance::from_endpoints(&format!("{origin}/api"), "ws://127.0.0.1/gateway").unwrap(),
        )
        .unwrap()
        .with_token(SessionToken::new("fixture-secret").unwrap()),
    );
    worker
        .change_relationship(42.into(), RelationshipAction::Send)
        .unwrap();
    let first = timeout(Duration::from_secs(5), worker.people.rx.recv())
        .await
        .unwrap()
        .unwrap();
    worker.people_event(first);
    assert!(events.try_iter().any(|event|matches!(event,Event::RelationshipMutation{user_id,status:RelationshipMutationStatus::Failed(_)} if user_id==42.into())));
    worker
        .change_relationship(42.into(), RelationshipAction::Send)
        .unwrap();
    let stale = timeout(Duration::from_secs(5), worker.people.rx.recv())
        .await
        .unwrap()
        .unwrap();
    worker.reset().await;
    let _ = events.try_iter().count();
    worker.people_event(stale);
    assert!(
        !events
            .try_iter()
            .any(|event| matches!(event, Event::RelationshipMutation { .. }))
    );
    assert!(worker.state.relationships.is_empty());
    server.await.unwrap();
}

#[tokio::test]
async fn presence_settings_match_source_patch_gateway_and_rollback() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        for (index, expected) in [
            json!({"status":"idle","status_resets_at":null,"status_resets_to":null}),
            json!({"custom_status":{"text":"Busy","expires_at":null,"emoji_name":"🌟"}}),
            json!({"custom_status":null}),
        ]
        .into_iter()
        .enumerate()
        {
            let (mut socket, _) = listener.accept().await.unwrap();
            let request = read_request(&mut socket).await;
            assert!(request.starts_with("PATCH /api/v1/users/@me/settings "));
            assert_eq!(
                serde_json::from_str::<Value>(request.split_once("\r\n\r\n").unwrap().1).unwrap(),
                expected
            );
            let (status, body) = if index == 0 {
                ("403 Forbidden", json!({"code":"INVALID_STATUS"}))
            } else {
                ("200 OK", json!({}))
            };
            let body = body.to_string();
            socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        }
    });
    let (mut worker, events) = worker();
    worker.state = ClientState::demo();
    worker.state.user_settings = json!({"status":"online","status_resets_at":null,"status_resets_to":null,"custom_status":{"text":"Old","expires_at":null,"emoji_name":"🌟"}});
    worker.client = Some(
        Client::new(
            Instance::from_endpoints(&format!("{origin}/api"), "ws://127.0.0.1/gateway").unwrap(),
        )
        .unwrap()
        .with_token(SessionToken::new("fixture-secret").unwrap()),
    );
    let (tx, mut rx) = async_mpsc::channel(4);
    worker.gateway_commands = Some(tx);
    worker
        .set_presence_status("idle".into(), None)
        .await
        .unwrap();
    let GatewayCommand::Presence(value) = rx.recv().await.unwrap() else {
        panic!("presence")
    };
    assert_eq!(
        value,
        json!({"status":"idle","afk":false,"mobile":false,"custom_status":{"text":"Old","expires_at":null,"emoji_id":null,"emoji_name":"🌟"}})
    );
    let result = timeout(Duration::from_secs(5), worker.presence.rx.recv())
        .await
        .unwrap()
        .unwrap();
    worker.presence_event(result).await;
    let GatewayCommand::Presence(value) = rx.recv().await.unwrap() else {
        panic!("rollback presence")
    };
    assert_eq!(value["status"], "online");
    assert_eq!(worker.state.user_settings["status"], "online");
    assert!(events.try_iter().any(|event| matches!(
        event,
        Event::PresenceMutation(crate::RelationshipMutationStatus::Failed(_))
    )));
    worker
        .set_custom_status("  Busy  ".into(), None, Some("🌟".into()), None)
        .await
        .unwrap();
    let GatewayCommand::Presence(value) = rx.recv().await.unwrap() else {
        panic!("presence")
    };
    assert_eq!(value["custom_status"]["text"], "Busy");
    let result = timeout(Duration::from_secs(5), worker.presence.rx.recv())
        .await
        .unwrap()
        .unwrap();
    worker.presence_event(result).await;
    assert_eq!(worker.state.user_settings["custom_status"]["text"], "Busy");
    worker.clear_custom_status().await.unwrap();
    let GatewayCommand::Presence(value) = rx.recv().await.unwrap() else {
        panic!("clear presence")
    };
    assert!(value["custom_status"].is_null());
    let result = timeout(Duration::from_secs(5), worker.presence.rx.recv())
        .await
        .unwrap()
        .unwrap();
    worker.presence_event(result).await;
    assert!(worker.state.user_settings["custom_status"].is_null());
    server.await.unwrap();
    worker.reset().await;
    assert!(worker.state.current_user.is_none());
}

#[tokio::test]
async fn presence_failure_preserves_newer_gateway_values_and_reset_fences_completion() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        for status in [403, 200] {
            let (mut socket, _) = listener.accept().await.unwrap();
            let _ = read_request(&mut socket).await;
            let body = json!({}).to_string();
            socket.write_all(format!("HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",if status==403{"Forbidden"}else{"OK"},body.len()).as_bytes()).await.unwrap();
        }
    });
    let (mut worker, events) = worker();
    worker.state = ClientState::demo();
    worker.state.user_settings = json!({"status":"online"});
    worker.client = Some(
        Client::new(
            Instance::from_endpoints(&format!("{origin}/api"), "ws://127.0.0.1/gateway").unwrap(),
        )
        .unwrap()
        .with_token(SessionToken::new("fixture-secret").unwrap()),
    );
    worker
        .set_presence_status("idle".into(), None)
        .await
        .unwrap();
    let event = timeout(Duration::from_secs(5), worker.presence.rx.recv())
        .await
        .unwrap()
        .unwrap();
    worker
        .state
        .apply_gateway("USER_SETTINGS_UPDATE", json!({"status":"dnd"}))
        .unwrap();
    worker.presence_event(event).await;
    assert_eq!(worker.state.user_settings["status"], "dnd");
    worker
        .set_presence_status("online".into(), None)
        .await
        .unwrap();
    let stale = timeout(Duration::from_secs(5), worker.presence.rx.recv())
        .await
        .unwrap()
        .unwrap();
    worker.reset().await;
    let _ = events.try_iter().count();
    worker.presence_event(stale).await;
    assert!(worker.state.current_user.is_none());
    assert!(worker.state.user_settings.is_null());
    assert!(
        !events
            .try_iter()
            .any(|event| matches!(event, Event::PresenceMutation(_)))
    );
    server.await.unwrap();
}

#[tokio::test]
async fn demo_presence_changes_are_local_without_a_client() {
    let (mut worker, _) = worker();
    worker.demo = true;
    worker.state = ClientState::demo();
    worker.state.user_settings = json!({"status":"online","custom_status":null});
    worker
        .set_presence_status("idle".into(), None)
        .await
        .unwrap();
    assert_eq!(worker.state.user_settings["status"], "idle");
    worker
        .set_custom_status("Offline sample".into(), None, None, None)
        .await
        .unwrap();
    assert_eq!(
        worker.state.user_settings["custom_status"]["text"],
        "Offline sample"
    );
    worker.clear_custom_status().await.unwrap();
    assert!(worker.state.user_settings["custom_status"].is_null());
    assert!(worker.client.is_none());
}

#[tokio::test]
async fn demo_empty_custom_status_saves_as_clear_and_completes() {
    let (mut worker, events) = worker();
    worker.demo = true;
    worker.state = ClientState::demo();
    worker.state.user_settings = json!({"status":"online","custom_status":{"text":"Old"}});
    worker
        .set_custom_status("  ".into(), None, None, None)
        .await
        .unwrap();
    assert!(worker.state.user_settings["custom_status"].is_null());
    assert!(events.try_iter().any(|event| matches!(
        event,
        Event::PresenceMutation(crate::RelationshipMutationStatus::Complete)
    )));
}

#[tokio::test]
async fn remote_call_controls_are_serialized_fenced_and_cancelled() {
    let (mut worker, _) = worker();
    worker.state.current_user = Some(User {
        id: 1.into(),
        ..Default::default()
    });
    worker.voice_target = Some(VoiceTarget {
        guild_id: None,
        channel_id: 9.into(),
        connection_id: None,
    });
    worker.call_generation = 5;
    let (gateway, _gateway_rx) = async_mpsc::channel(32);
    worker.gateway_commands = Some(gateway);
    worker.snapshot();
    async fn request(
        worker: &mut Worker,
        account: u64,
        generation: u64,
        action: crate::CallAction,
        deadline: std::time::Instant,
    ) -> Result<Option<crate::CallControlSnapshot>> {
        let (reply, receive) = tokio::sync::oneshot::channel();
        worker
            .command(Command::ControlCall {
                account_id: account.into(),
                generation,
                action,
                deadline,
                reply,
            })
            .await
            .unwrap();
        receive.await.unwrap()
    }
    let deadline = || std::time::Instant::now() + Duration::from_secs(5);
    assert!(
        request(&mut worker, 2, 5, crate::CallAction::ToggleMute, deadline())
            .await
            .is_err()
    );
    assert!(
        request(&mut worker, 1, 4, crate::CallAction::ToggleMute, deadline())
            .await
            .is_err()
    );
    assert!(
        request(
            &mut worker,
            1,
            5,
            crate::CallAction::ToggleMute,
            std::time::Instant::now()
        )
        .await
        .is_err()
    );
    let (reply, receive) = tokio::sync::oneshot::channel();
    drop(receive);
    worker
        .command(Command::ControlCall {
            account_id: 1.into(),
            generation: 5,
            action: crate::CallAction::ToggleMute,
            deadline: deadline(),
            reply,
        })
        .await
        .unwrap();
    assert!(!worker.state.voice.muted);
    assert!(
        request(&mut worker, 1, 5, crate::CallAction::ToggleMute, deadline())
            .await
            .unwrap()
            .unwrap()
            .muted
    );
    assert!(
        !request(&mut worker, 1, 5, crate::CallAction::ToggleMute, deadline())
            .await
            .unwrap()
            .unwrap()
            .muted
    );
    assert!(
        request(
            &mut worker,
            1,
            5,
            crate::CallAction::SetDeaf(true),
            deadline()
        )
        .await
        .unwrap()
        .unwrap()
        .deafened
    );
    assert!(
        request(
            &mut worker,
            1,
            5,
            crate::CallAction::SetDeaf(true),
            deadline()
        )
        .await
        .unwrap()
        .unwrap()
        .deafened
    );
    assert!(
        request(&mut worker, 1, 5, crate::CallAction::Disconnect, deadline())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        request(&mut worker, 1, 5, crate::CallAction::ToggleMute, deadline())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn voice_changes_and_leave_during_gateway_reconnect_are_replayed_in_order() {
    let (mut worker, _) = worker();
    let (tx, mut rx) = async_mpsc::channel(32);
    worker.gateway_commands = Some(tx);
    worker.gateway_ready = true;
    worker.voice_target = Some(VoiceTarget {
        guild_id: Some(10.into()),
        channel_id: 9.into(),
        connection_id: Some("old".into()),
    });
    worker.gateway(GatewayEvent::Reconnecting).await.unwrap();
    worker.command(Command::SetMute(true)).await.unwrap();
    assert!(matches!(
        rx.recv().await,
        Some(GatewayCommand::VoiceState(_))
    ));
    worker.leave_voice(true).await.unwrap();
    assert!(matches!(
        rx.recv().await,
        Some(GatewayCommand::VoiceState(_))
    ));
    worker.gateway(GatewayEvent::Connected).await.unwrap();
    match rx.recv().await.unwrap() {
        GatewayCommand::VoiceState(state) => {
            assert_eq!(state.channel_id, None);
            assert_eq!(state.connection_id.as_deref(), Some("old"));
        }
        _ => panic!("expected replayed voice leave"),
    }
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn display_settings_exact_transport_rollback_and_pending_scope() {
    use fluxer_core::DisplaySetting;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        for (index, expected) in [
            json!({"message_display_compact":true}),
            json!({"gif_auto_play":false}),
            json!({"animate_emoji":false}),
            json!({"trusted_domains":["example.org"]}),
        ]
        .into_iter()
        .enumerate()
        {
            let (mut socket, _) = listener.accept().await.unwrap();
            let request = read_request(&mut socket).await;
            assert!(request.starts_with("PATCH /api/v1/users/@me/settings "));
            assert_eq!(
                serde_json::from_str::<Value>(request.split_once("\r\n\r\n").unwrap().1).unwrap(),
                expected
            );
            let status = if index == 0 {
                "403 Forbidden"
            } else {
                "200 OK"
            };
            let body = "{}";
            socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        }
    });
    let (mut worker, events) = worker();
    worker.state = ClientState::demo();
    worker.state.user_settings =
        json!({"status":"online","message_display_compact":false,"gif_auto_play":true});
    worker.client = Some(
        Client::new(
            Instance::from_endpoints(&format!("{origin}/api"), "ws://127.0.0.1/gateway").unwrap(),
        )
        .unwrap()
        .with_token(SessionToken::new("fixture-secret").unwrap()),
    );
    worker
        .set_display_setting(DisplaySetting::Compact(true))
        .await
        .unwrap();
    assert_eq!(worker.state.user_settings["message_display_compact"], true);
    assert!(
        worker
            .set_display_setting(DisplaySetting::GifAutoplay(false))
            .await
            .is_err()
    );
    let result = timeout(Duration::from_secs(5), worker.display_settings.rx.recv())
        .await
        .unwrap()
        .unwrap();
    worker.display_settings_event(result).await;
    assert_eq!(worker.state.user_settings["message_display_compact"], false);
    assert_eq!(worker.state.user_settings["status"], "online");
    assert!(events.try_iter().any(|event| matches!(
        event,
        Event::DisplaySettingsMutation(crate::RelationshipMutationStatus::Failed(_))
    )));
    worker
        .set_display_setting(DisplaySetting::GifAutoplay(false))
        .await
        .unwrap();
    let result = timeout(Duration::from_secs(5), worker.display_settings.rx.recv())
        .await
        .unwrap()
        .unwrap();
    worker.display_settings_event(result).await;
    assert_eq!(worker.state.user_settings["gif_auto_play"], false);
    worker
        .set_display_setting(DisplaySetting::AnimateEmoji(false))
        .await
        .unwrap();
    let result = timeout(Duration::from_secs(5), worker.display_settings.rx.recv())
        .await
        .unwrap()
        .unwrap();
    worker.display_settings_event(result).await;
    assert_eq!(worker.state.user_settings["animate_emoji"], false);
    worker
        .set_display_setting(DisplaySetting::TrustedDomains(vec!["example.org".into()]))
        .await
        .unwrap();
    let result = timeout(Duration::from_secs(5), worker.display_settings.rx.recv())
        .await
        .unwrap()
        .unwrap();
    worker.display_settings_event(result).await;
    assert_eq!(
        worker.state.user_settings["trusted_domains"],
        json!(["example.org"])
    );
    server.await.unwrap();
}

#[tokio::test]
async fn pending_voice_connect_accepts_controls_and_leave_cancels_job() {
    use std::sync::atomic::{AtomicBool, Ordering};
    struct OnDrop(Arc<AtomicBool>);
    impl Drop for OnDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let (mut worker, _) = worker();
    let (gateway, _rx) = async_mpsc::channel(32);
    worker.gateway_commands = Some(gateway);
    worker.voice_target = Some(VoiceTarget {
        guild_id: Some(10.into()),
        channel_id: 100.into(),
        connection_id: Some("a".into()),
    });
    worker.call_generation = 7;
    let dropped = Arc::new(AtomicBool::new(false));
    let guard = OnDrop(dropped.clone());
    let task = tokio::spawn(async move {
        let _guard = guard;
        pending::<(MediaEngine, Result<MediaEvents>)>().await
    });
    tokio::task::yield_now().await;
    worker.voice_connect = Some(VoiceConnectJob {
        generation: 7,
        update: voice_update(Some("100"), "a"),
        revoked_share: false,
        task,
    });
    worker.command(Command::SetMute(true)).await.unwrap();
    worker.command(Command::SetDeaf(true)).await.unwrap();
    assert!(worker.state.voice.muted && worker.state.voice.deafened);
    worker.leave_voice(true).await.unwrap();
    assert!(dropped.load(Ordering::SeqCst));
    assert!(worker.voice_connect.is_none());
    assert!(worker.voice_target.is_none());
    assert!(worker.state.voice.muted && worker.state.voice.deafened);
}

#[tokio::test]
async fn transport_loss_preserves_intent_and_leave_cancels_rejoin() {
    let (mut worker, events) = worker();
    worker.state.current_user = Some(User {
        id: 1.into(),
        ..Default::default()
    });
    let (gateway, mut rx) = async_mpsc::channel(32);
    worker.gateway_commands = Some(gateway);
    worker.gateway_ready = true;
    worker.voice_target = Some(VoiceTarget {
        guild_id: Some(10.into()),
        channel_id: 100.into(),
        connection_id: Some("old".into()),
    });
    worker.state.voice.join(100.into());
    worker.state.voice.screen_share = true;
    worker.watched_streams.insert("user_2_watch".into());
    worker.media_event(MediaEvent::TransportLost).await.unwrap();
    assert!(worker.voice_recovery_at.is_some());
    assert!(matches!(
        worker.call_control_snapshot().unwrap().status,
        VoiceStatus::Reconnecting
    ));
    assert!(!worker.state.voice.screen_share);
    assert!(worker.watched_streams.contains("user_2_watch"));
    assert!(
        events
            .try_iter()
            .any(|event| matches!(event, Event::ShareReshareRequired))
    );
    worker.sync_stream_availability().await.unwrap();
    assert!(worker.watched_streams.contains("user_2_watch"));
    let GatewayCommand::VoiceState(old_leave) = rx.try_recv().unwrap() else {
        panic!("expected abandoned connection leave");
    };
    assert_eq!(old_leave.connection_id.as_deref(), Some("old"));
    assert!(old_leave.channel_id.is_none());
    worker.retry_voice().await.unwrap();
    let GatewayCommand::VoiceState(join) = rx.try_recv().unwrap() else {
        panic!("expected voice rejoin");
    };
    assert_eq!(join.channel_id.as_deref(), Some("100"));
    assert_eq!(join.connection_id, None);
    assert!(!join.self_stream);
    assert_eq!(join.viewer_stream_keys, vec!["10:100:watch"]);
    worker.leave_voice(true).await.unwrap();
    assert!(worker.voice_recovery_at.is_none());
    assert!(worker.voice_recovery_started.is_none());
    worker.retry_voice().await.unwrap();
    let GatewayCommand::VoiceState(leave) = rx.try_recv().unwrap() else {
        panic!("expected leave");
    };
    assert!(leave.channel_id.is_none());
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn failed_rejoin_send_retries_without_waiting_for_join_timeout() {
    let (mut worker, _events) = worker();
    worker.gateway_ready = true;
    worker.voice_target = Some(VoiceTarget {
        guild_id: Some(10.into()),
        channel_id: 100.into(),
        connection_id: None,
    });
    worker.voice_recovery_started = Some(tokio::time::Instant::now());
    let (gateway, rx) = async_mpsc::channel(1);
    drop(rx);
    worker.gateway_commands = Some(gateway);

    worker.retry_voice().await.unwrap();
    assert_eq!(worker.voice_recovery_attempts, 1);
    assert!(worker.voice_join_deadline.is_none());
    assert!(worker.voice_recovery_at.is_some());

    let (gateway, mut rx) = async_mpsc::channel(1);
    worker.gateway_commands = Some(gateway);
    worker.retry_voice().await.unwrap();
    assert!(matches!(rx.try_recv(), Ok(GatewayCommand::VoiceState(_))));
    assert!(worker.voice_join_deadline.is_some());
    assert!(worker.voice_recovery_at.is_none());
}

#[test]
fn suspend_detection_uses_boot_clock_not_wall_clock() {
    let monotonic = std::time::Instant::now();
    let boot = Duration::from_secs(100);
    assert!(!suspend_gap(
        (Some(boot), monotonic),
        (
            Some(boot + Duration::from_secs(90)),
            monotonic + Duration::from_secs(90)
        )
    ));
    assert!(suspend_gap(
        (Some(boot), monotonic),
        (
            Some(boot + Duration::from_secs(90)),
            monotonic + Duration::from_secs(5)
        )
    ));
    // No wall-clock value is consulted, including after a forward clock correction.
    assert!(!suspend_gap(
        (None, monotonic),
        (None, monotonic + Duration::from_secs(5))
    ));
}

#[tokio::test]
async fn offline_transport_loss_then_leave_retains_abandoned_connection_leave() {
    let (mut worker, _) = worker();
    let (gateway, mut rx) = async_mpsc::channel(32);
    worker.gateway_commands = Some(gateway);
    worker.voice_target = Some(VoiceTarget {
        guild_id: Some(10.into()),
        channel_id: 100.into(),
        connection_id: Some("old".into()),
    });
    worker.state.voice.join(100.into());
    worker.media_event(MediaEvent::TransportLost).await.unwrap();
    worker.leave_voice(true).await.unwrap();
    while rx.try_recv().is_ok() {}
    worker.gateway(GatewayEvent::Connected).await.unwrap();
    let GatewayCommand::VoiceState(leave) = rx.try_recv().unwrap() else {
        panic!("expected old connection leave");
    };
    assert_eq!(leave.connection_id.as_deref(), Some("old"));
    assert!(leave.channel_id.is_none());
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn account_reset_cancels_pending_voice_job_and_ignores_late_completion() {
    use std::sync::atomic::{AtomicBool, Ordering};
    struct OnDrop(Arc<AtomicBool>);
    impl Drop for OnDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let (mut worker, _) = worker();
    let (gateway, _) = async_mpsc::channel(8);
    worker.gateway_commands = Some(gateway);
    worker.state.current_user = Some(User {
        id: 1.into(),
        ..Default::default()
    });
    worker.voice_target = Some(VoiceTarget {
        guild_id: Some(10.into()),
        channel_id: 100.into(),
        connection_id: Some("a".into()),
    });
    let dropped = Arc::new(AtomicBool::new(false));
    let guard = OnDrop(dropped.clone());
    let task = tokio::spawn(async move {
        let _guard = guard;
        pending::<(MediaEngine, Result<MediaEvents>)>().await
    });
    tokio::task::yield_now().await;
    worker.voice_connect = Some(VoiceConnectJob {
        generation: 1,
        update: voice_update(Some("100"), "a"),
        revoked_share: false,
        task,
    });
    worker.reset().await;
    assert!(dropped.load(Ordering::SeqCst));
    assert!(worker.voice_connect.is_none());
    assert!(worker.voice_target.is_none());
    let failed = tokio::spawn(async { panic!("simulated abandoned completion") })
        .await
        .unwrap_err();
    worker.finish_voice_connect(Err(failed)).await.unwrap();
    assert!(worker.voice_target.is_none());
    assert!(worker.media.is_none());
}

#[tokio::test]
async fn retry_limit_ends_call_and_clears_the_timer() {
    let (mut worker, _) = worker();
    let (gateway, _rx) = async_mpsc::channel(8);
    worker.gateway_commands = Some(gateway);
    worker.voice_target = Some(VoiceTarget {
        guild_id: Some(10.into()),
        channel_id: 100.into(),
        connection_id: None,
    });
    worker.voice_recovery_started = Some(tokio::time::Instant::now());
    worker.voice_recovery_attempts = 5;
    assert!(worker.schedule_voice_retry().await.is_err());
    assert!(worker.voice_recovery_at.is_none());
    assert!(worker.voice_target.is_none());
}

#[tokio::test]
async fn same_channel_rejoin_rejects_retired_state_and_credentials_before_new_state() {
    let (mut worker, _) = worker();
    let (gateway, _rx) = async_mpsc::channel(32);
    worker.gateway_commands = Some(gateway);
    worker.state.current_user = Some(User {
        id: 1.into(),
        ..Default::default()
    });
    worker.state.session_id = Some("session".into());
    worker
        .state
        .apply_gateway(
            "GUILD_CREATE",
            json!({"id":"10","name":"G","channels":[{"id":"100","name":"v","type":2}]}),
        )
        .unwrap();
    worker.voice_target = Some(VoiceTarget {
        guild_id: Some(10.into()),
        channel_id: 100.into(),
        connection_id: Some("old".into()),
    });
    worker.leave_voice(false).await.unwrap();
    worker.voice_target = Some(VoiceTarget {
        guild_id: Some(10.into()),
        channel_id: 100.into(),
        connection_id: None,
    });
    let dispatch = |name: &str, data: Value| GatewayEvent::Dispatch {
        name: name.into(),
        data,
        sequence: None,
    };
    let old_credentials = json!({"token":"stale","endpoint":"wss://example.invalid","connection_id":"old","guild_id":"10","channel_id":"100","ice_servers":[{}]});
    worker
        .gateway(dispatch("VOICE_SERVER_UPDATE", old_credentials.clone()))
        .await
        .unwrap();
    worker.gateway(dispatch("VOICE_STATE_UPDATE", json!({"user_id":"1","guild_id":"10","channel_id":"100","connection_id":"old","session_id":"session"}))).await.unwrap();
    assert!(
        worker
            .voice_target
            .as_ref()
            .unwrap()
            .connection_id
            .is_none()
    );
    assert!(worker.pending_voice_server.is_empty());
    assert!(worker.media.is_none());
    let new_credentials = json!({"token":"new","endpoint":"wss://example.invalid","connection_id":"new","guild_id":"10","ice_servers":[{}]});
    worker
        .gateway(dispatch("VOICE_SERVER_UPDATE", new_credentials))
        .await
        .unwrap();
    worker
        .gateway(dispatch("VOICE_SERVER_UPDATE", old_credentials))
        .await
        .unwrap();
    assert!(worker.pending_voice_server.contains_key("new"));
    assert!(worker.media.is_none());
    // Mesh is deliberately rejected before media initialization. Its error
    // proves that only the fresh matching credentials reached connect_voice.
    let result = worker.gateway(dispatch("VOICE_STATE_UPDATE", json!({"user_id":"1","guild_id":"10","channel_id":"100","connection_id":"new","session_id":"session"}))).await;
    assert!(result.unwrap_err().contains("mesh voice"));
    assert!(worker.media.is_none());
}

#[tokio::test]
async fn expired_status_failure_retries_with_backoff_then_succeeds() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        for status in ["503 Service Unavailable", "200 OK"] {
            let (mut socket, _) = listener.accept().await.unwrap();
            let request = read_request(&mut socket).await;
            assert_eq!(
                serde_json::from_str::<Value>(request.split_once("\r\n\r\n").unwrap().1).unwrap(),
                json!({"status":"online","status_resets_at":null,"status_resets_to":null})
            );
            let body = "{}";
            socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        }
    });
    let (mut worker, _) = worker();
    worker.state = ClientState::demo();
    worker.state.user_settings = json!({"status":"dnd","status_resets_at":"2000-01-01T00:00:00Z","status_resets_to":"online"});
    worker.client = Some(
        Client::new(
            Instance::from_endpoints(&format!("{origin}/api"), "ws://127.0.0.1/gateway").unwrap(),
        )
        .unwrap()
        .with_token(SessionToken::new("fixture-secret").unwrap()),
    );
    let mut attempted = None;
    worker.presence_expiry_tick(&mut attempted).await;
    let event = timeout(Duration::from_secs(5), worker.presence.rx.recv())
        .await
        .unwrap()
        .unwrap();
    worker.presence_event(event).await;
    assert_eq!(worker.state.user_settings["status"], "dnd");
    worker.presence_expiry_tick(&mut attempted).await;
    assert!(
        timeout(Duration::from_millis(100), worker.presence.rx.recv())
            .await
            .is_err(),
        "backoff should suppress an immediate retry"
    );
    tokio::time::sleep(Duration::from_millis(2100)).await;
    worker.presence_expiry_tick(&mut attempted).await;
    let event = timeout(Duration::from_secs(5), worker.presence.rx.recv())
        .await
        .unwrap()
        .unwrap();
    worker.presence_event(event).await;
    assert_eq!(worker.state.user_settings["status"], "online");
    assert!(worker.state.user_settings["status_resets_at"].is_null());
    server.await.unwrap();
}

#[tokio::test]
async fn notification_cleanup_follows_accepted_cross_client_acks_and_deletes_only() {
    let (mut worker, events) = worker();
    worker.state = ClientState::demo();
    worker
        .state
        .read_states
        .get_mut(&Snowflake(11))
        .unwrap()
        .version = Some("10".into());
    for (name, data, clears) in [
        (
            "MESSAGE_ACK",
            json!({"channel_id":"11","message_id":"2000","mention_count":0,"version":"9"}),
            false,
        ),
        (
            "MESSAGE_ACK",
            json!({"channel_id":"11","message_id":"2000","mention_count":0,"version":"11"}),
            true,
        ),
        (
            "MESSAGE_ACK",
            json!({"channel_id":"11","message_id":"1000","mention_count":2,"version":"12","manual":true}),
            true,
        ),
        (
            "MESSAGE_DELETE",
            json!({"channel_id":"11","id":"2001"}),
            true,
        ),
        (
            "MESSAGE_DELETE_BULK",
            json!({"channel_id":"11","ids":["2002","2003"]}),
            true,
        ),
    ] {
        worker
            .gateway(GatewayEvent::Dispatch {
                name: name.into(),
                data,
                sequence: None,
            })
            .await
            .unwrap();
        let cleared = events
            .try_iter()
            .filter_map(|event| match event {
                Event::ClearChannelNotifications {
                    account_id,
                    channel_id,
                } => Some((account_id, channel_id)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            cleared,
            if clears {
                vec![(Snowflake(100), Snowflake(11))]
            } else {
                vec![]
            },
            "{name}"
        );
    }
    let read = &worker.state.read_states[&Snowflake(11)];
    assert_eq!(read.last_message_id, Some(Snowflake(1000)));
    assert!(read.manual_ack);
}

#[tokio::test]
async fn transport_recovery_rejects_retired_server_credentials_and_own_state() {
    let (mut worker, _) = worker();
    let (gateway, _rx) = async_mpsc::channel(32);
    worker.gateway_commands = Some(gateway);
    worker.gateway_ready = true;
    worker.state.current_user = Some(User {
        id: 1.into(),
        ..Default::default()
    });
    worker.state.session_id = Some("session".into());
    worker
        .state
        .apply_gateway(
            "GUILD_CREATE",
            json!({"id":"10","name":"G","channels":[{"id":"100","name":"v","type":2}]}),
        )
        .unwrap();
    worker.voice_target = Some(VoiceTarget {
        guild_id: Some(10.into()),
        channel_id: 100.into(),
        connection_id: Some("old".into()),
    });
    worker.state.voice.join(100.into());
    worker.media_event(MediaEvent::TransportLost).await.unwrap();
    let dispatch = |name: &str, data: Value| GatewayEvent::Dispatch {
        name: name.into(),
        data,
        sequence: None,
    };
    worker.gateway(dispatch("VOICE_SERVER_UPDATE", json!({"token":"stale","endpoint":"wss://example.invalid","connection_id":"old","guild_id":"10","channel_id":"100","ice_servers":[{}]}))).await.unwrap();
    worker.gateway(dispatch("VOICE_STATE_UPDATE", json!({"user_id":"1","guild_id":"10","channel_id":"100","connection_id":"old","session_id":"session"}))).await.unwrap();
    assert!(
        worker
            .voice_target
            .as_ref()
            .unwrap()
            .connection_id
            .is_none()
    );
    assert!(worker.pending_voice_server.is_empty());
    assert!(worker.media.is_none());
    worker.gateway(dispatch("VOICE_SERVER_UPDATE", json!({"token":"fresh","endpoint":"wss://example.invalid","connection_id":"new","guild_id":"10","ice_servers":[{}]}))).await.unwrap();
    let result = worker.gateway(dispatch("VOICE_STATE_UPDATE", json!({"user_id":"1","guild_id":"10","channel_id":"100","connection_id":"new","session_id":"session"}))).await;
    assert!(result.unwrap_err().contains("mesh voice"));
    assert!(worker.media.is_none());
}

#[test]
fn guild_sfu_credentials_are_accepted_before_own_voice_state() {
    // guild_voice_connection_join.erl stores the SFU join pending;
    // guild_voice_connection_confirm.erl broadcasts own state after LiveKit joins.
    let target = VoiceTarget {
        guild_id: Some(10.into()),
        channel_id: 100.into(),
        connection_id: None,
    };
    let update: VoiceServerUpdate = serde_json::from_value(json!({
        "token":"synthetic", "endpoint":"wss://voice.example", "connection_id":"fresh",
        "guild_id":"10", "channel_id":"100"
    }))
    .unwrap();
    assert!(update.ice_servers.is_empty());
    assert!(target.accepts(&update));
}

#[tokio::test]
async fn own_profile_save_projects_returned_values_and_clears_fields() {
    let (mut worker, _) = worker();
    worker.demo = true;
    worker.state = ClientState::demo();
    worker
        .update_own_profile("Ada".into(), "Hello\nworld".into(), "she/her".into())
        .unwrap();
    let user = worker.state.current_user.as_ref().unwrap();
    assert_eq!(user.global_name.as_deref(), Some("Ada"));
    assert_eq!(user.extra.get("bio"), Some(&json!("Hello\nworld")));
    assert_eq!(worker.state.users.get(&user.id), Some(user));
    worker
        .update_own_profile(String::new(), String::new(), String::new())
        .unwrap();
    let user = worker.state.current_user.as_ref().unwrap();
    assert_eq!(user.global_name, None);
    assert_eq!(user.extra.get("bio"), Some(&Value::Null));
    assert_eq!(user.extra.get("pronouns"), Some(&Value::Null));
}

#[tokio::test]
async fn own_profile_wire_patch_and_account_reset_fence() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let (received_tx, received_rx) = tokio::sync::oneshot::channel();
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let request = read_request(&mut socket).await;
        assert!(request.starts_with("PATCH /api/v1/users/@me "));
        let body: Value = serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(
            body,
            json!({"global_name":null,"bio":"New bio","pronouns":null})
        );
        received_tx.send(()).unwrap();
        let _ = reply_rx.await;
        let body = r#"{"id":"1","username":"stale","global_name":"stale"}"#;
        let _ = socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await;
    });
    let (mut worker, _) = worker();
    worker.state = ClientState::demo();
    worker.client = Some(
        Client::new(
            Instance::from_endpoints(&format!("{origin}/api"), "ws://127.0.0.1/gateway").unwrap(),
        )
        .unwrap()
        .with_token(SessionToken::new("fixture-secret").unwrap()),
    );
    worker
        .update_own_profile(String::new(), "New bio".into(), String::new())
        .unwrap();
    timeout(Duration::from_secs(5), received_rx)
        .await
        .unwrap()
        .unwrap();
    assert!(
        worker
            .update_own_profile("second".into(), String::new(), String::new())
            .is_err()
    );
    worker.clear_own_profile();
    let original = worker.state.current_user.clone();
    let _ = reply_tx.send(());
    server.await.unwrap();
    assert_eq!(worker.state.current_user, original);
}

#[tokio::test]
async fn camera_watch_is_explicit_roster_bound_and_independent_of_screen() {
    let (mut worker, events, mut outgoing) = watch_worker();
    let identity = "user_123_live".to_owned();
    gateway_voice(&mut worker, json!({"guild_id":"10","channel_id":"100","user_id":"123","connection_id":"live","self_stream":true,"self_video":true})).await;
    assert!(worker.available_cameras.contains(&identity));
    assert!(worker.watched_cameras.is_empty());
    worker
        .set_watch(identity.clone(), true, true)
        .await
        .unwrap();
    outgoing.try_recv().unwrap();
    worker
        .set_camera_watch(identity.clone(), true, true)
        .unwrap();
    assert!(
        outgoing.try_recv().is_err(),
        "camera watch must not change screen viewer signaling"
    );
    let generation = worker.camera_generations[&identity];
    let mut camera = watch_frame(&identity, generation);
    camera.source = fluxer_media::VideoSource::Camera;
    camera.track_id = "camera-sid".into();
    worker
        .media_event(MediaEvent::Video(camera.clone()))
        .await
        .unwrap();
    worker
        .media_event(MediaEvent::Video(watch_frame(
            &identity,
            worker.video_generations[&identity],
        )))
        .await
        .unwrap();
    assert_eq!(worker.frames.lock().unwrap().len(), 2);
    worker
        .set_camera_watch(identity.clone(), false, false)
        .unwrap();
    assert_eq!(worker.frames.lock().unwrap().len(), 1);
    assert!(worker.watched_streams.contains(&identity));
    worker
        .set_camera_watch(identity.clone(), true, true)
        .unwrap();
    worker.media_event(MediaEvent::Video(camera)).await.unwrap();
    assert_eq!(
        worker.frames.lock().unwrap().len(),
        1,
        "old camera generation rejected after rewatch"
    );
    gateway_voice(
        &mut worker,
        json!({"user_id":"123","connection_id":"live","self_video":false}),
    )
    .await;
    assert!(!worker.watched_cameras.contains(&identity));
    assert!(worker.watched_streams.contains(&identity));
    assert!(events.try_iter().any(|event| matches!(event, Event::CameraAvailable { identity: ref id, available: false } if id == &identity)));
    assert!(outgoing.try_recv().is_err());
}

#[tokio::test]
async fn camera_replacement_reports_missing_watch_and_hide_failure_is_not_start_failure() {
    let (mut worker, events, _) = watch_worker();
    let identity = "user_123_live".to_owned();
    gateway_voice(&mut worker, json!({"guild_id":"10","channel_id":"100","user_id":"123","connection_id":"live","self_video":true})).await;
    worker
        .set_camera_watch(identity.clone(), true, true)
        .unwrap();
    events.try_iter().for_each(drop);
    // Replacement forgets advertised availability while retaining watch intent.
    worker.available_cameras.clear();
    worker
        .state
        .apply_gateway(
            "VOICE_STATE_UPDATE",
            json!({"user_id":"123","connection_id":"live","self_video":false}),
        )
        .unwrap();
    worker.sync_stream_availability().await.unwrap();
    assert!(events.try_iter().any(|event| matches!(event, Event::CameraAvailable { identity: ref id, available: false } if id == &identity)));
    assert!(!worker.watched_cameras.contains(&identity));
    worker
        .command(Command::WatchCamera {
            identity: "invalid".into(),
            watch: false,
        })
        .await
        .unwrap();
    let error = events
        .try_iter()
        .find(|event| matches!(event, Event::Error(_) | Event::CameraWatchFailed { .. }))
        .unwrap();
    assert!(matches!(error, Event::Error(_)));
}

#[tokio::test]
async fn group_add_survives_navigation_and_rejects_duplicate_job() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let request = read_request(&mut socket).await;
        assert!(request.starts_with("PUT /api/v1/channels/50/recipients/42 "));
        accepted_tx.send(()).unwrap();
        release_rx.await.unwrap();
        socket
            .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
    });
    let (mut worker, _) = worker();
    worker.state = ClientState::demo();
    worker
        .state
        .apply_gateway(
            "CHANNEL_CREATE",
            json!({"id":"50", "type":3, "recipients":[]}),
        )
        .unwrap();
    worker.state.selected_channel = Some(50.into());
    worker.state.relationships.insert(
        42.into(),
        fluxer_core::Relationship {
            id: 42.into(),
            kind: 1,
            ..Default::default()
        },
    );
    worker.client = Some(
        Client::new(
            Instance::from_endpoints(&format!("{origin}/api"), "ws://127.0.0.1/gateway").unwrap(),
        )
        .unwrap()
        .with_token(SessionToken::new("fixture-secret").unwrap()),
    );
    worker
        .add_group_recipients(50.into(), vec![42.into()])
        .unwrap();
    timeout(Duration::from_secs(2), accepted_rx)
        .await
        .unwrap()
        .unwrap();
    assert!(
        worker
            .add_group_recipients(50.into(), vec![42.into()])
            .is_err()
    );
    worker.demo = true;
    worker
        .command(Command::SelectChannel {
            channel_id: 12.into(),
        })
        .await
        .unwrap();
    release_tx.send(()).unwrap();
    let event = timeout(Duration::from_secs(2), worker.dm_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(event.result.is_ok());
    worker.finish_open_dm(event).await.unwrap();
    assert_eq!(worker.state.selected_channel, Some(12.into()));
    assert!(worker.group_add_job.is_none());
    server.await.unwrap();
}

#[tokio::test]
async fn group_removal_checks_owner_and_waits_for_gateway() {
    let (mut worker, _) = worker();
    worker.state = ClientState::demo();
    let me = worker.state.current_user.as_ref().unwrap().id;
    worker.state.apply_gateway("CHANNEL_CREATE", json!({"id":"50", "type":3, "owner_id":"999", "recipients":[{"id":"42", "username":"Ada"}]})).unwrap();
    assert!(worker.remove_group_recipient(50.into(), 42.into()).is_err());
    assert_eq!(worker.state.channels[&50.into()].recipients.len(), 1);
    worker.state.channels.get_mut(&50.into()).unwrap().owner_id = Some(me);
    // Without a session no request is sent and the gateway cache is untouched.
    assert!(worker.remove_group_recipient(50.into(), 42.into()).is_err());
    assert_eq!(worker.state.channels[&50.into()].recipients.len(), 1);
}

#[path = "worker_loopback_tests.rs"]
mod loopback;

#[tokio::test]
async fn incoming_call_accept_requires_our_ring_and_sends_private_voice_join() {
    let (mut worker, _) = worker();
    worker.state.apply_gateway("READY", json!({"user":{"id":"1","username":"me"},"private_channels":[{"id":"20","type":1,"recipients":[{"id":"2","username":"caller"}]}]})).unwrap();
    worker
        .state
        .apply_gateway("CALL_CREATE", json!({"channel_id":"20","ringing":["2"]}))
        .unwrap();
    worker.gateway_ready = true;
    let (tx, mut outgoing) = async_mpsc::channel(8);
    worker.gateway_commands = Some(tx);
    assert!(
        worker
            .command(Command::AcceptPrivateCall {
                channel_id: 20.into()
            })
            .await
            .is_err()
    );
    assert!(outgoing.try_recv().is_err());
    worker
        .state
        .apply_gateway("CALL_UPDATE", json!({"channel_id":"20","ringing":["1"]}))
        .unwrap();
    worker
        .command(Command::AcceptPrivateCall {
            channel_id: 20.into(),
        })
        .await
        .unwrap();
    let GatewayCommand::VoiceState(payload) = outgoing.try_recv().unwrap() else {
        panic!("expected voice join")
    };
    assert_eq!(payload.channel_id.as_deref(), Some("20"));
    assert!(payload.guild_id.is_none());
    assert!(
        !worker.state.private_calls[&20.into()]
            .ringing
            .contains(&1.into())
    );
    assert!(worker.media.is_none()); // No media or capture before credentials.
}

#[tokio::test]
async fn camera_capture_requires_frame_and_current_generation() {
    let (mut worker, _events, mut gateway) = watch_worker();
    worker.pending_camera = Some(2);
    worker.camera_event(1, false, false).await.unwrap();
    assert_eq!(worker.pending_camera, Some(2));
    worker.camera_event(1, true, true).await.unwrap();
    worker.camera_event(2, true, false).await.unwrap();
    assert!(!worker.state.voice.camera);
    worker.camera_event(2, true, true).await.unwrap();
    assert!(worker.state.voice.camera);
    worker.camera_event(1, false, false).await.unwrap();
    assert!(worker.state.voice.camera);
    worker.camera_event(2, false, false).await.unwrap();
    assert!(!worker.state.voice.camera);
    let mut states = Vec::new();
    while let Ok(GatewayCommand::VoiceState(state)) = gateway.try_recv() {
        states.push(state.self_video);
    }
    assert_eq!(states, [true, false]);
    worker.camera_event(2, true, true).await.unwrap();
    assert!(!worker.state.voice.camera);
    assert!(worker.media.is_none());
}

#[test]
fn revoke_only_accepts_one_explicit_non_current_session() {
    use serde_json::json;
    assert!(super::is_non_current_session(
        &json!([{"id_hash":"target","current":false}]),
        "target"
    ));
    for input in [
        json!([]),
        json!([{"id_hash":"target","current":true}]),
        json!([{"id_hash":"target"}]),
        json!([{"id_hash":"target","current":false},{"id_hash":"target","current":true}]),
    ] {
        assert!(!super::is_non_current_session(&input, "target"));
    }
}

#[tokio::test]
async fn demo_private_call_decline_is_local_and_never_joins_voice() {
    let (mut worker, _) = worker();
    assert!(worker.command(Command::DemoPrivateCall).await.is_err());
    worker.command(Command::Demo).await.unwrap();
    worker.command(Command::DemoPrivateCall).await.unwrap();
    assert!(
        worker.state.private_calls[&40.into()]
            .ringing
            .contains(&100.into())
    );
    assert!(
        worker
            .command(Command::AcceptPrivateCall {
                channel_id: 40.into()
            })
            .await
            .is_err()
    );
    worker
        .command(Command::DeclinePrivateCall {
            channel_id: 40.into(),
        })
        .await
        .unwrap();
    assert!(
        !worker.state.private_calls[&40.into()]
            .ringing
            .contains(&100.into())
    );
    assert!(worker.voice_target.is_none());
    assert!(worker.media.is_none());
    assert!(worker.client.is_none());
}
