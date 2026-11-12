use fluxer_protocol::{
    Client, Error, Gateway, GatewayCommand, GatewayEvent, Instance, LoginIdentifier, LoginOutcome,
    SessionToken, Url, VoiceStateUpdate,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::mpsc,
    time::timeout,
};
use tokio_tungstenite::{accept_async, tungstenite::Message};

async fn http_server(responses: Vec<String>) -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (requests_tx, requests) = mpsc::channel(16);
    tokio::spawn(async move {
        for response in responses {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 4096];
            loop {
                let size = socket.read(&mut buffer).await.unwrap();
                if size == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..size]);
                if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
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
            let _ = requests_tx.send(String::from_utf8(request).unwrap()).await;
            socket.write_all(response.as_bytes()).await.unwrap();
            socket.shutdown().await.unwrap();
        }
    });
    (format!("http://{address}"), requests)
}

fn response(status: &str, body: Value) -> String {
    let body = body.to_string();
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[tokio::test]
async fn login_then_message_preserves_api_prefix_and_raw_session_auth() {
    let (origin, mut requests) = http_server(vec![
        response("200 OK", json!({"token":"account-secret","user_id":"123"})),
        response("200 OK", json!({"id":"456","content":"hello"})),
    ])
    .await;
    let instance =
        Instance::from_endpoints(&format!("{origin}/custom/api"), "ws://127.0.0.1/gateway")
            .unwrap();
    let client = Client::new(instance).unwrap();
    let session = client
        .login(
            LoginIdentifier::Email("hello@example.com".into()),
            "password",
        )
        .await
        .unwrap();
    let LoginOutcome::Authenticated { token, .. } = session else {
        panic!("expected session");
    };
    let login = requests.recv().await.unwrap();
    assert!(login.starts_with("POST /custom/api/v1/auth/login HTTP/1.1"));
    assert!(!login.to_lowercase().contains("authorization:"));
    let client = client.with_token(token);
    assert_eq!(
        client
            .send_message("1", "hello", Some("nonce-1"))
            .await
            .unwrap()["id"],
        "456"
    );
    let send = requests.recv().await.unwrap();
    assert!(send.starts_with("POST /custom/api/v1/channels/1/messages HTTP/1.1"));
    assert!(
        send.to_lowercase()
            .contains("authorization: account-secret\r\n")
    );
    let body: Value = serde_json::from_str(send.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(body, json!({"content":"hello","nonce":"nonce-1"}));
}

#[tokio::test]
async fn rate_limits_are_typed_without_replaying_mutations() {
    let (origin, mut requests) = http_server(vec![response(
        "429 Too Many Requests",
        json!({"retry_after":0.25,"global":true}),
    )])
    .await;
    let client = Client::new(
        Instance::from_endpoints(&format!("{origin}/api"), "ws://127.0.0.1/gateway").unwrap(),
    )
    .unwrap();
    assert!(
        matches!(client.send_message("1", "hello", None).await, Err(Error::RateLimited { retry_after, global:true }) if retry_after == Duration::from_millis(250))
    );
    assert!(requests.recv().await.is_some());
    assert!(requests.recv().await.is_none());
}

#[tokio::test]
async fn redirect_response_is_not_followed_with_account_credentials() {
    let redirect = "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/steal\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned();
    let (origin, _) = http_server(vec![redirect]).await;
    let client = Client::new(
        Instance::from_endpoints(&format!("{origin}/api"), "ws://127.0.0.1/gateway").unwrap(),
    )
    .unwrap()
    .with_token(SessionToken::new("secret").unwrap());
    assert!(matches!(
        client.current_user().await,
        Err(Error::Api { status: 302, .. })
    ));
}

async fn next_json(
    socket: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
) -> Value {
    loop {
        match timeout(Duration::from_secs(5), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
        {
            Message::Text(text) => return serde_json::from_str(&text).unwrap(),
            Message::Ping(bytes) => socket.send(Message::Pong(bytes)).await.unwrap(),
            other => panic!("unexpected websocket message: {other:?}"),
        }
    }
}

async fn gateway_send(
    socket: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    value: Value,
) {
    socket
        .send(Message::Text(value.to_string().into()))
        .await
        .unwrap();
}

#[tokio::test]
async fn websocket_identifies_heartbeats_transmits_voice_and_resumes() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = accept_async(stream).await.unwrap();
        gateway_send(&mut socket, json!({"op":10,"d":{"heartbeat_interval":100}})).await;
        let identify = next_json(&mut socket).await;
        assert_eq!(identify["op"], 2);
        assert_eq!(identify["d"]["token"], "secret");
        assert_eq!(identify["d"]["properties"]["e2ee_capable"], true);
        gateway_send(
            &mut socket,
            json!({"op":0,"t":"READY","s":1,"d":{"session_id":"session"}}),
        )
        .await;
        let mut got_voice = false;
        let mut got_heartbeat = false;
        while !(got_voice && got_heartbeat) {
            let message = next_json(&mut socket).await;
            match message["op"].as_u64().unwrap() {
                1 => {
                    assert_eq!(message["d"], 1);
                    got_heartbeat = true;
                    gateway_send(&mut socket, json!({"op":11})).await;
                }
                4 => {
                    assert_eq!(message["d"]["channel_id"], "voice-channel");
                    got_voice = true;
                }
                other => panic!("unexpected opcode {other}"),
            }
        }
        gateway_send(&mut socket, json!({"op":0,"t":"VOICE_SERVER_UPDATE","s":2,"d":{"token":"voice-secret","endpoint":"wss://voice.example","connection_id":"conn"}})).await;
        gateway_send(&mut socket, json!({"op":7})).await;
        drop(socket);
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = accept_async(stream).await.unwrap();
        gateway_send(
            &mut socket,
            json!({"op":10,"d":{"heartbeat_interval":1000}}),
        )
        .await;
        let resume = next_json(&mut socket).await;
        assert_eq!(resume["op"], 6);
        assert_eq!(resume["d"]["seq"], 2);
        assert_eq!(resume["d"]["session_id"], "session");
        gateway_send(&mut socket, json!({"op":0,"t":"VOICE_SERVER_UPDATE","s":2,"d":{"token":"voice-secret","endpoint":"wss://voice.example","connection_id":"duplicate"}})).await;
        gateway_send(&mut socket, json!({"op":0,"t":"RESUMED","s":3,"d":{}})).await;
        // Keep the server side alive until the client explicitly closes.
        while let Some(Ok(message)) = socket.next().await {
            if matches!(message, Message::Close(_)) {
                break;
            }
        }
    });
    let (events_tx, mut events) = mpsc::channel(32);
    let (commands, commands_rx) = mpsc::channel(16);
    let gateway = Gateway::new(
        Url::parse(&format!("ws://{address}/gateway")).unwrap(),
        SessionToken::new("secret").unwrap(),
    )
    .with_e2ee_capable(true);
    let client = tokio::spawn(gateway.run(events_tx, commands_rx));
    timeout(Duration::from_secs(8), async {
        let mut connections = 0;
        let mut voice_updates = 0;
        while let Some(event) = events.recv().await {
            match event {
                GatewayEvent::Connected => {
                    connections += 1;
                    if connections == 1 {
                        commands
                            .send(GatewayCommand::VoiceState(VoiceStateUpdate::join(
                                Some("guild".into()),
                                "voice-channel".into(),
                            )))
                            .await
                            .unwrap();
                    } else {
                        assert_eq!(
                            voice_updates, 1,
                            "resumed dispatch must not repeat a voice transition"
                        );
                        commands.send(GatewayCommand::Close).await.unwrap();
                        break;
                    }
                }
                GatewayEvent::Dispatch { name, data, .. } if name == "VOICE_SERVER_UPDATE" => {
                    assert_eq!(data["connection_id"], "conn");
                    voice_updates += 1;
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    timeout(Duration::from_secs(5), client)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    server.await.unwrap();
}
