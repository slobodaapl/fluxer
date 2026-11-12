use super::*;
use futures_util::{SinkExt, StreamExt};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    time::timeout,
};
use tokio_tungstenite::{accept_async, tungstenite::Message as WireMessage};

fn worker() -> (Worker, mpsc::Receiver<Event>) {
    let (tx, rx) = mpsc::channel();
    (Worker::new(tx, Arc::new(Mutex::new(BTreeMap::new()))), rx)
}

fn voice_update(channel: Option<&str>, connection: &str) -> VoiceServerUpdate {
    serde_json::from_value(json!({"token":"voice-secret","endpoint":"wss://voice.example","connection_id":connection,"guild_id":"10","channel_id":channel})).unwrap()
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
        let mut socket = accept_async(socket).await.unwrap();
        socket
            .send(WireMessage::Text(
                json!({"op":10,"d":{"heartbeat_interval":1000}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        let identify = socket.next().await.unwrap().unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(identify.to_text().unwrap()).unwrap()["op"],
            2
        );
        socket.send(WireMessage::Text(json!({"op":0,"t":"READY","s":1,"d":{"session_id":"session","user":ws_user,"guilds":[ws_guild]}}).to_string().into())).await.unwrap();
        loop {
            tokio::select! {
                value=dispatch_rx.recv()=>match value{Some(value)=>{if socket.send(WireMessage::Text(value.to_string().into())).await.is_err(){break;}},None=>break},
                value=socket.next()=>match value{
                    Some(Ok(WireMessage::Text(text)))=>{let value:Value=serde_json::from_str(&text).unwrap();if value["op"]==1 && socket.send(WireMessage::Text(json!({"op":11}).to_string().into())).await.is_err(){break;}},
                    Some(Ok(WireMessage::Close(_)))|None|Some(Err(_))=>break,
                    _=>{},
                }
            }
        }
    });
    let advertised_api = format!("{origin}/api");
    let ws_endpoint = gateway.clone();
    let rest_sent = sent.clone();
    let http_server = tokio::spawn(async move {
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
                    json!({"codename":"tungsten","api_code_version":1,"endpoints":{"api":advertised_api,"gateway":ws_endpoint}}),
                ),
                ("GET", "/api/v1/users/@me") => (200, user.clone()),
                ("GET", "/api/v1/users/@me/guilds") => (200, json!([guild.clone()])),
                ("GET", "/api/v1/users/@me/channels") => (200, json!([])),
                ("GET", "/api/v1/guilds/10/channels") => (200, json!([channel.clone()])),
                ("GET", "/api/v1/channels/100/messages?limit=50") => {
                    (200, json!([initial.clone()]))
                }
                ("POST", "/api/v1/channels/100/messages/200/ack") => (200, json!({})),
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
    let (mut worker, events) = worker();
    worker
        .command(Command::TokenLogin {
            instance: origin,
            token: "secret".into(),
        })
        .await
        .unwrap();
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
    worker.reset().await;
    http_server.abort();
    websocket.abort();
}
