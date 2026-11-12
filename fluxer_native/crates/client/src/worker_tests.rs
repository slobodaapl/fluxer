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

fn voice_update(channel: Option<&str>, connection: &str) -> VoiceServerUpdate {
    serde_json::from_value(json!({"token":"voice-secret","endpoint":"wss://voice.example","connection_id":connection,"guild_id":"10","channel_id":channel})).unwrap()
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
        .media_event(MediaEvent::ScreenStreamAvailable {
            identity: identity.clone(),
            available: true,
        })
        .await
        .unwrap();
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
    let GatewayCommand::VoiceState(payload) = outgoing.try_recv().unwrap() else {
        panic!("expected voice state");
    };
    assert!(payload.viewer_stream_keys.is_empty());
    assert!(worker.watched_streams.is_empty());
    assert!(events.try_iter().any(|event|matches!(event,Event::WatchedStreams(ref streams) if streams==&vec![identity.clone()])));
    worker
        .media_event(MediaEvent::ScreenStreamAvailable {
            identity: identity.clone(),
            available: true,
        })
        .await
        .unwrap();
    worker
        .set_watch(identity.clone(), true, true)
        .await
        .unwrap();
    worker
        .media_event(MediaEvent::ParticipantLeft { identity })
        .await
        .unwrap();
    assert!(worker.watched_streams.is_empty());
    worker.leave_voice(true).await.unwrap();
    assert!(worker.available_streams.is_empty());
    assert!(worker.media.is_none());
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
    worker.gateway(GatewayEvent::Dispatch{name:"READY".into(),data:json!({"session_id":"first","user":{"id":"1","username":"alice"},"guilds":[{"id":"10","name":"A"},{"id":"20","name":"B"}]}),sequence:Some(1)}).await.unwrap();
    worker.gateway(GatewayEvent::Connected).await.unwrap();
    let GatewayCommand::Send { op, data } = outgoing.try_recv().unwrap() else {
        panic!("expected op14");
    };
    assert_eq!(op, 14);
    assert_eq!(
        data,
        json!({"subscriptions":{"10":{"active":true,"sync":true}}})
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
    assert!(worker.pending_voice_server.is_some());
    assert!(worker.media.is_none());
    worker.leave_voice(false).await.unwrap();
    assert!(worker.pending_voice_server.is_none());
    assert!(worker.voice_target.is_none());
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
    worker.screen_event(false, false).await.unwrap();
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
async fn actual_http_and_gateway_send_merge_preserve_failed_drafts_without_capture() {
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
    worker
        .command(Command::SendMessage {
            content: "hello".into(),
        })
        .await
        .unwrap();
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
    assert!(
        worker
            .command(Command::SendMessage {
                content: "fail".into()
            })
            .await
            .is_err()
    );
    assert_eq!(
        worker.state.drafts.get(&100.into()).map(String::as_str),
        Some("fail")
    );
    assert_eq!(
        events
            .try_iter()
            .filter(|event| matches!(event, Event::MessageSent { .. }))
            .count(),
        1
    );
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
