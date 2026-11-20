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
    http_server_with_origin_policy(responses, false, None).await
}

async fn http_server_with_origin_policy(
    responses: Vec<String>,
    require_origin: bool,
    web_app_origin: Option<&str>,
) -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let origin = format!("http://{address}");
    let expected_origin = web_app_origin.unwrap_or(&origin).to_owned();
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
            let request = String::from_utf8(request).unwrap();
            let method = request.split_whitespace().next().unwrap();
            let request_origin = request.lines().find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("origin").then(|| value.trim())
            });
            // Mirror the production web-host gate in MiddlewarePipeline.ts.
            let response = if require_origin
                && method != "GET"
                && request_origin != Some(expected_origin.as_str())
            {
                self::response("403 Forbidden", json!({"code":"INVALID_API_ORIGIN"}))
            } else {
                response
            };
            let _ = requests_tx.send(request).await;
            socket.write_all(response.as_bytes()).await.unwrap();
            socket.shutdown().await.unwrap();
        }
    });
    (origin, requests)
}

fn response(status: &str, body: Value) -> String {
    let body = body.to_string();
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[tokio::test]
async fn login_and_bodyless_mutations_satisfy_the_production_origin_gate() {
    let (origin, mut requests) = http_server_with_origin_policy(
        vec![
            response("200 OK", json!({"token":"account-secret","user_id":"123"})),
            response("200 OK", json!({"id":"123"})),
            response("200 OK", json!({"content":"edited"})),
            response("200 OK", Value::Null),
            response("200 OK", Value::Null),
            response("200 OK", Value::Null),
        ],
        true,
        None,
    )
    .await;
    let client = Client::new(
        Instance::from_endpoints(&format!("{origin}/custom/api"), "ws://127.0.0.1/gateway")
            .unwrap(),
    )
    .unwrap();
    let LoginOutcome::Authenticated { token, .. } = client
        .login(
            LoginIdentifier::Email("test@example.com".into()),
            "password",
        )
        .await
        .expect("login must pass the production Origin gate")
    else {
        panic!("expected authenticated login");
    };
    let client = client.with_token(token);
    client.current_user().await.unwrap();
    client.edit_message("1", "2", "edited").await.unwrap();
    client.add_reaction("1", "2", "👍").await.unwrap();
    client.delete_message("1", "2").await.unwrap();
    client.typing("1").await.unwrap();
    for method in ["POST", "GET", "PATCH", "PUT", "DELETE", "POST"] {
        let request = requests.recv().await.unwrap();
        assert!(request.starts_with(method));
        let header = request.to_ascii_lowercase();
        if method == "GET" {
            assert!(!header.contains("\r\norigin:"));
        } else {
            assert!(header.contains(&format!("\r\norigin: {origin}\r\n")));
        }
    }
}

#[tokio::test]
async fn desktop_origin_comes_from_discovered_webapp_and_excludes_get_and_head() {
    // Reference: LocalAppAPIProxyRequestHandler.ts forward() and
    // LocalAppProxyHTTPPolicy.ts buildAPIRequestHeaders()/requestHasBody().
    // Different API/webapp authorities catch an accidental API-origin fallback.
    let web_app = "https://client.example.test:8443";
    let web_app_origin = "https://client.example.test:8443";
    let (origin, mut requests) = http_server_with_origin_policy(
        vec![response("200 OK", json!({})); 5],
        false,
        Some(web_app_origin),
    )
    .await;
    let api = format!("{origin}/api");
    let instance = Instance::from_document(
        &Url::parse(&format!("{origin}/.well-known/fluxer")).unwrap(),
        json!({"api_code_version":1,"endpoints":{
            "api":api,"api_client":api,"api_public":api,"webapp":web_app,
            "gateway":"ws://127.0.0.1/gateway", "media":format!("{origin}/media"),
            "static_cdn":format!("{origin}/static"), "marketing":origin,
            "admin":format!("{origin}/admin"), "invite":format!("{origin}/invite"),
            "gift":format!("{origin}/gift")
        }}),
    )
    .unwrap();
    let client = Client::new(instance).unwrap();
    for method in [
        fluxer_protocol::Method::GET,
        fluxer_protocol::Method::HEAD,
        fluxer_protocol::Method::POST,
        fluxer_protocol::Method::PUT,
        fluxer_protocol::Method::DELETE,
    ] {
        client
            .request(method.clone(), "/contract", None)
            .await
            .unwrap();
        let request = requests.recv().await.unwrap().to_ascii_lowercase();
        if method == fluxer_protocol::Method::GET || method == fluxer_protocol::Method::HEAD {
            assert!(!request.contains("\r\norigin:"));
        } else {
            assert!(request.contains(&format!("\r\norigin: {web_app_origin}\r\n")));
            assert!(!request.contains(&format!("\r\norigin: {origin}\r\n")));
        }
    }
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
    .with_e2ee_capable(true)
    // Lifecycle regression uses the existing codec-fallback mode; default
    // bidirectional zstd wire tests live alongside Gateway's source fixtures.
    .with_compression_enabled(false);
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

#[tokio::test]
async fn auth_headers_and_ip_authorization_follow_existing_desktop_contract() {
    let (origin, mut requests) = http_server(vec![
        response(
            "403 Forbidden",
            json!({"code":"IP_AUTHORIZATION_REQUIRED", "details":{"context":{
                "ticket":"test +/ticket", "email":"masked@example.test", "resend_available_in":0
            }}}),
        ),
        response("200 OK", json!({"completed":false})),
        response("200 OK", Value::Null),
        response(
            "200 OK",
            json!({"completed":true,"token":"authorized-secret","user_id":"123"}),
        ),
    ])
    .await;
    let client = Client::new(
        Instance::from_endpoints(&format!("{origin}/api"), "ws://127.0.0.1/gateway").unwrap(),
    )
    .unwrap()
    .with_auth_locale("de")
    .unwrap();
    assert!(matches!(
        client
            .login(
                LoginIdentifier::Email("test@example.test".into()),
                "password"
            )
            .await
            .unwrap(),
        LoginOutcome::IpAuthorization {
            resend_available_in: 0,
            ..
        }
    ));
    assert!(
        client
            .poll_ip_authorization("test +/ticket")
            .await
            .unwrap()
            .is_none()
    );
    client
        .resend_ip_authorization("test +/ticket")
        .await
        .unwrap();
    assert_eq!(
        client
            .poll_ip_authorization("test +/ticket")
            .await
            .unwrap()
            .unwrap()
            .user_id,
        "123"
    );
    let mut installation = None;
    for (index, route) in [
        "POST /api/v1/auth/login",
        "GET /api/v1/auth/ip-authorization/poll?ticket=test+%2B%2Fticket",
        "POST /api/v1/auth/ip-authorization/resend",
        "GET /api/v1/auth/ip-authorization/poll?ticket=test+%2B%2Fticket",
    ]
    .into_iter()
    .enumerate()
    {
        let request = requests.recv().await.unwrap();
        assert!(request.starts_with(route), "{request}");
        let headers = request
            .split_once("\r\n\r\n")
            .unwrap()
            .0
            .to_ascii_lowercase();
        for expected in [
            "accept-language: de",
            "accept-encoding: gzip, br",
            "x-fluxer-features: view_channel_members_permission,channel_threads",
            "x-fluxer-capabilities: guild_visits",
        ] {
            assert!(headers.contains(expected));
        }
        assert!(!headers.contains("authorization:"));
        let id = headers
            .lines()
            .find_map(|line| line.strip_prefix("x-fluxer-client-installation-id: "))
            .unwrap()
            .to_owned();
        assert_eq!(id.len(), 32);
        assert!(
            id.bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        );
        if let Some(previous) = &installation {
            assert_eq!(previous, &id);
        }
        installation = Some(id);
        if index == 2 {
            let body: Value =
                serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
            assert_eq!(body, json!({"ticket":"test +/ticket"}));
        }
    }
}

#[tokio::test]
async fn read_acks_use_authoritative_endpoint_and_only_missing_route_fallback() {
    // ReadStates.postAckEntries/postLegacyAckEntries + ReadStateAckQueue entries.
    for (status, expected_calls) in [
        ("200 OK", 1),
        ("404 Not Found", 2),
        ("405 Method Not Allowed", 2),
        ("403 Forbidden", 1),
    ] {
        let mut replies = vec![response(status, json!({"read_states":[]}))];
        if expected_calls == 2 {
            replies.push(response("200 OK", Value::Null));
        }
        let (origin, mut requests) = http_server(replies).await;
        let client = Client::new(
            Instance::from_endpoints(&format!("{origin}/api"), "ws://127.0.0.1/gateway").unwrap(),
        )
        .unwrap();
        let result = client.acknowledge("1", "2").await;
        if status.starts_with("403") {
            assert!(matches!(result, Err(Error::Api { status: 403, .. })));
        } else {
            result.unwrap();
        }
        for index in 0..expected_calls {
            let request = requests.recv().await.unwrap();
            let route = if index == 0 {
                "/api/v1/read-states/ack"
            } else {
                "/api/v1/read-states/ack-bulk"
            };
            assert!(request.starts_with(&format!("POST {route} HTTP/1.1")));
            let body: Value =
                serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
            assert_eq!(
                body,
                json!({"read_states":[{"channel_id":"1","message_id":"2"}]})
            );
        }
    }
}

#[tokio::test]
async fn one_to_one_dm_uses_recipient_id_request_shape() {
    let (origin, mut requests) = http_server(vec![response(
        "200 OK",
        json!({"id":"90","type":1,"recipients":[{"id":"42","username":"Ada"}]}),
    )])
    .await;
    let client = Client::new(
        Instance::from_endpoints(&format!("{origin}/api"), "ws://127.0.0.1/gateway").unwrap(),
    )
    .unwrap()
    .with_token(SessionToken::new("secret").unwrap());
    let result = client.create_dm("42").await.unwrap();
    assert_eq!(result["id"], "90");
    let request = requests.recv().await.unwrap();
    assert!(
        request.starts_with("POST /api/v1/users/@me/channels "),
        "{request}"
    );
    let body: Value = serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body, json!({"recipient_id":"42"}));
}

#[tokio::test]
async fn group_dm_uses_recipients_request_shape() {
    let (origin, mut requests) = http_server(vec![response(
        "200 OK",
        json!({"id":"91","type":3,"recipients":[]}),
    )])
    .await;
    let client = Client::new(
        Instance::from_endpoints(&format!("{origin}/api"), "ws://127.0.0.1/gateway").unwrap(),
    )
    .unwrap()
    .with_token(SessionToken::new("secret").unwrap());
    client
        .create_group_dm(&["42".into(), "43".into()])
        .await
        .unwrap();
    let request = requests.recv().await.unwrap();
    assert!(request.starts_with("POST /api/v1/users/@me/channels "));
    let body: Value = serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body, json!({"recipients":["42","43"]}));
}

#[tokio::test]
async fn private_call_ring_and_stop_use_explicit_recipients() {
    let (origin, mut requests) = http_server(vec![
        response("200 OK", Value::Null),
        response("200 OK", Value::Null),
    ])
    .await;
    let client = Client::new(
        Instance::from_endpoints(&format!("{origin}/api"), "ws://127.0.0.1/gateway").unwrap(),
    )
    .unwrap();
    let recipients = vec!["12".to_owned(), "34".to_owned()];
    client.call_ringing("56", &recipients, false).await.unwrap();
    client.call_ringing("56", &recipients, true).await.unwrap();
    for action in ["ring", "stop-ringing"] {
        let request = requests.recv().await.unwrap();
        assert!(request.starts_with(&format!("POST /api/v1/channels/56/call/{action} ")));
        let body = request.split_once("\r\n\r\n").unwrap().1;
        assert_eq!(
            serde_json::from_str::<Value>(body).unwrap(),
            json!({"recipients": ["12", "34"]})
        );
    }
}

#[tokio::test]
async fn channel_topic_update_preserves_omission_and_empty_clear() {
    let (origin, mut requests) = http_server(vec![response("200 OK", json!({})); 3]).await;
    let client = Client::new(
        Instance::from_endpoints(&format!("{origin}/api"), "ws://127.0.0.1/gateway").unwrap(),
    )
    .unwrap();
    for topic in [None, Some("hello"), Some("")] {
        client
            .update_basic_channel("123", "general", topic)
            .await
            .unwrap();
        let request = requests.recv().await.unwrap();
        assert!(request.starts_with("PATCH /api/v1/channels/123 "));
        let body: Value = serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(body.get("name"), Some(&json!("general")));
        assert_eq!(body.get("topic"), topic.map(|t| json!(t)).as_ref());
    }
}

#[tokio::test]
async fn thread_membership_uses_empty_join_and_leave_requests() {
    let no_content =
        "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned();
    let (origin, mut requests) = http_server(vec![no_content.clone(), no_content]).await;
    let client = Client::new(
        Instance::from_endpoints(&format!("{origin}/api"), "ws://127.0.0.1/gateway").unwrap(),
    )
    .unwrap()
    .with_token(SessionToken::new("secret").unwrap());
    client.set_thread_membership("42", true).await.unwrap();
    client.set_thread_membership("42", false).await.unwrap();
    let join = requests.recv().await.unwrap();
    let leave = requests.recv().await.unwrap();
    assert!(join.starts_with("PUT /api/v1/channels/42/thread-members/@me HTTP/1.1"));
    assert!(leave.starts_with("DELETE /api/v1/channels/42/thread-members/@me HTTP/1.1"));
    assert!(join.ends_with("\r\n\r\n"));
    assert!(leave.ends_with("\r\n\r\n"));
}

#[tokio::test]
async fn group_recipient_removal_uses_delete_route() {
    let (origin, mut requests) = http_server(vec![response("204 No Content", Value::Null)]).await;
    let client = Client::new(
        Instance::from_endpoints(&format!("{origin}/api"), "ws://127.0.0.1/gateway").unwrap(),
    )
    .unwrap()
    .with_token(SessionToken::new("secret").unwrap());
    client.remove_group_recipient("50", "42").await.unwrap();
    let request = requests.recv().await.unwrap();
    assert!(
        request.starts_with("DELETE /api/v1/channels/50/recipients/42 "),
        "{request}"
    );
}

#[tokio::test]
async fn session_revocation_challenge_and_totp_use_the_same_target() {
    use fluxer_protocol::{SessionRevocationOutcome, SessionRevocationProof};
    let challenge = json!({"code":"SUDO_MODE_REQUIRED", "has_mfa":true,
        "methods":{"totp":true,"webauthn":false,"backup_codes":true,
        "email_code":false,"password":false,"sso_reauth":false}});
    let (origin, mut requests) = http_server(vec![
        response("403 Forbidden", challenge),
        "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
    ])
    .await;
    let client = Client::new(
        Instance::from_endpoints(&format!("{origin}/api"), "ws://127.0.0.1/gateway").unwrap(),
    )
    .unwrap()
    .with_token(SessionToken::new("synthetic-token").unwrap());
    assert!(matches!(
        client
            .revoke_auth_session("synthetic_hash", None)
            .await
            .unwrap(),
        SessionRevocationOutcome::Challenge(_)
    ));
    assert_eq!(
        client
            .revoke_auth_session(
                "synthetic_hash",
                Some(SessionRevocationProof::Totp("123456"))
            )
            .await
            .unwrap(),
        SessionRevocationOutcome::Revoked
    );
    for (index, request) in [
        requests.recv().await.unwrap(),
        requests.recv().await.unwrap(),
    ]
    .iter()
    .enumerate()
    {
        assert!(request.starts_with("POST /api/v1/auth/sessions/logout "));
        let body: Value = serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(body["session_id_hashes"], json!(["synthetic_hash"]));
        if index == 0 {
            assert!(body.get("mfa_code").is_none());
        } else {
            assert_eq!(body["mfa_method"], "totp");
            assert_eq!(body["mfa_code"], "123456");
        }
    }
}

#[tokio::test]
async fn reject_incoming_call_sends_classic_empty_object() {
    let (origin, mut requests) = http_server(vec![response("200 OK", Value::Null)]).await;
    let client = Client::new(
        Instance::from_endpoints(&format!("{origin}/api"), "ws://127.0.0.1/gateway").unwrap(),
    )
    .unwrap();
    client.reject_call("56").await.unwrap();
    let request = requests.recv().await.unwrap();
    assert!(request.starts_with("POST /api/v1/channels/56/call/stop-ringing "));
    assert_eq!(
        serde_json::from_str::<Value>(request.split_once("\r\n\r\n").unwrap().1).unwrap(),
        json!({})
    );
}

#[tokio::test]
async fn session_revocation_password_is_sent_only_in_confirmed_request() {
    use fluxer_protocol::{SessionRevocationOutcome, SessionRevocationProof};
    let (origin, mut requests) = http_server(vec![
        "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
    ])
    .await;
    let client = Client::new(
        Instance::from_endpoints(&format!("{origin}/api"), "ws://127.0.0.1/gateway").unwrap(),
    )
    .unwrap()
    .with_token(SessionToken::new("synthetic-token").unwrap());
    assert_eq!(
        client
            .revoke_auth_session(
                "synthetic_hash",
                Some(SessionRevocationProof::Password("synthetic-password"))
            )
            .await
            .unwrap(),
        SessionRevocationOutcome::Revoked
    );
    let request = requests.recv().await.unwrap();
    let body: Value = serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body["session_id_hashes"], json!(["synthetic_hash"]));
    assert_eq!(body["password"], "synthetic-password");
    assert!(body.get("mfa_method").is_none());
}

#[tokio::test]
async fn archived_public_threads_use_timestamp_cursor() {
    let (origin, mut requests) = http_server(vec![
        response(
            "200 OK",
            json!({"threads":[],"members":[],"has_more":false}),
        ),
        response(
            "200 OK",
            json!({"threads":[],"members":[],"has_more":false}),
        ),
    ])
    .await;
    let client = Client::new(
        Instance::from_endpoints(&format!("{origin}/api"), "ws://127.0.0.1/gateway").unwrap(),
    )
    .unwrap();
    client.archived_public_threads("42", None).await.unwrap();
    client
        .archived_public_threads("42", Some("2026-01-02T03:04:05+00:00"))
        .await
        .unwrap();
    assert!(
        requests
            .recv()
            .await
            .unwrap()
            .starts_with("GET /api/v1/channels/42/threads/archived/public HTTP/1.1")
    );
    assert!(requests.recv().await.unwrap().starts_with("GET /api/v1/channels/42/threads/archived/public?before=2026-01-02T03%3A04%3A05%2B00%3A00 HTTP/1.1"));
}

#[tokio::test]
async fn forgot_password_uses_unauthenticated_instance_transport() {
    let (origin, mut requests) =
        http_server_with_origin_policy(vec![response("204 No Content", Value::Null)], true, None)
            .await;
    let client = Client::new(
        Instance::from_endpoints(&format!("{origin}/custom/api"), "ws://127.0.0.1/gateway")
            .unwrap(),
    )
    .unwrap()
    .with_auth_locale("fr")
    .unwrap();
    client.forgot_password("user@example.test").await.unwrap();
    let request = requests.recv().await.unwrap();
    assert!(request.starts_with("POST /custom/api/v1/auth/forgot HTTP/1.1"));
    assert!(!request.to_lowercase().contains("authorization:"));
    assert!(request.to_lowercase().contains("accept-language: fr"));
    let body: Value = serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body, json!({"email":"user@example.test"}));
}

#[tokio::test]
async fn webhook_management_requests_and_secrets_are_scoped() {
    let hook = json!({"id":"42", "guild_id":"1", "channel_id":"2", "name":"Example", "avatar":null, "type":1, "token":"synthetic-secret"});
    let follower = json!({"id":"43", "guild_id":"1", "channel_id":"2", "name":"Follower", "avatar":null, "type":2});
    let (origin, mut requests) = http_server(vec![
        response("200 OK", json!([hook, follower])),
        response("200 OK", hook.clone()),
        response("200 OK", hook.clone()),
        response("200 OK", Value::Null),
        response(
            "403 Forbidden",
            json!({"code":"MISSING_PERMISSIONS", "token":"synthetic-secret"}),
        ),
    ])
    .await;
    let client = Client::new(
        Instance::from_endpoints(&format!("{origin}/api"), "ws://127.0.0.1/gateway").unwrap(),
    )
    .unwrap();
    let rows = client.channel_webhooks("2").await.unwrap();
    assert!(!format!("{rows:?}").contains("synthetic-secret"));
    assert_eq!(
        rows[0].copy_url("https://example.test/api/v1").unwrap(),
        "https://example.test/api/v1/webhooks/42/synthetic%2Dsecret"
    );
    assert!(rows[1].copy_url("https://example.test/api/v1").is_none());
    client.create_webhook("2", "Example").await.unwrap();
    client
        .update_webhook("42", "Renamed", Some("avatar-hash"))
        .await
        .unwrap();
    client.delete_webhook("42").await.unwrap();
    let err = client.channel_webhooks("2").await.unwrap_err();
    assert!(err.to_string().contains("403"));
    assert!(!format!("{err:?}").contains("synthetic-secret"));
    for (method, path, body) in [
        ("GET", "/api/v1/channels/2/webhooks", None),
        (
            "POST",
            "/api/v1/channels/2/webhooks",
            Some(json!({"name":"Example","avatar":null})),
        ),
        (
            "PATCH",
            "/api/v1/webhooks/42",
            Some(json!({"name":"Renamed","avatar":"avatar-hash"})),
        ),
        ("DELETE", "/api/v1/webhooks/42", None),
        ("GET", "/api/v1/channels/2/webhooks", None),
    ] {
        let request = requests.recv().await.unwrap();
        assert!(request.starts_with(&format!("{method} {path} ")));
        if let Some(expected) = body {
            let actual: Value =
                serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
            assert_eq!(actual, expected);
        }
    }
}

#[tokio::test]
async fn discovery_join_uses_authenticated_bodyless_post_and_preserves_refusal() {
    let (origin, mut requests) = http_server_with_origin_policy(
        vec![
            "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
            response("403 Forbidden", json!({"code":"INVITES_DISABLED"})),
        ],
        true,
        None,
    )
    .await;
    let client = Client::new(
        Instance::from_endpoints(&format!("{origin}/api"), "ws://127.0.0.1/gateway").unwrap(),
    )
    .unwrap()
    .with_token(SessionToken::new("synthetic-token").unwrap());
    client
        .discovery_join(fluxer_core::Snowflake(123))
        .await
        .unwrap();
    assert!(
        matches!(client.discovery_join(fluxer_core::Snowflake(456)).await, Err(Error::Api { status: 403, ref code, .. }) if code.as_deref() == Some("INVITES_DISABLED"))
    );
    for id in [123, 456] {
        let request = requests.recv().await.unwrap();
        assert!(request.starts_with(&format!(
            "POST /api/v1/discovery/guilds/{id}/join HTTP/1.1\r\n"
        )));
        let header = request.to_ascii_lowercase();
        assert!(header.contains("\r\nauthorization: synthetic-token\r\n"));
        assert!(header.contains(&format!("\r\norigin: {origin}\r\n")));
        assert!(request.ends_with("\r\n\r\n"));
    }
}
