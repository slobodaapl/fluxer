use super::*;

const DEADLINE: Duration = Duration::from_secs(5);

fn dispatch(sequence: u64, name: &str, data: Value) -> Value {
    json!({"op":0,"t":name,"s":sequence,"d":data})
}

async fn through(worker: &mut Worker, sequence: u64) {
    timeout(DEADLINE, async {
        while !worker.gateway_ready || worker.last_sequence != Some(sequence) {
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
    .expect("gateway dispatch was not delivered");
}

async fn loopback_gateway(
    socket: tokio::net::TcpStream,
    user: Value,
    token: &'static str,
    mut dispatches: async_mpsc::UnboundedReceiver<Value>,
    commands: async_mpsc::UnboundedSender<Value>,
) {
    let mut socket = tokio_tungstenite::accept_async(socket).await.unwrap();
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
    assert_eq!(identify["d"]["token"], token);
    assert_eq!(identify["d"]["flags"], 14);
    let permissions =
        Permissions::VIEW_CHANNEL | Permissions::SEND_MESSAGES | Permissions::READ_MESSAGE_HISTORY;
    socket
        .send(compressed(
            dispatch(
                1,
                "READY",
                json!({
                    "session_id":user["id"],"user":user,"guilds":[{
                        "id":"10","name":"Loopback","owner_id":"1",
                        "roles":[{"id":"10","name":"@everyone","permissions":permissions}],
                        "members":[{"user":user}],"channels":[
                            {"id":"100","guild_id":"10","name":"shared","type":0},
                            {"id":"101","guild_id":"10","name":"call","type":2}
                        ]
                    }]
                }),
            ),
            &mut encoder,
        ))
        .await
        .unwrap();
    loop {
        tokio::select! {
            frame = socket.next() => match frame {
                Some(Ok(message @ (WireMessage::Text(_) | WireMessage::Binary(_)))) => {
                    let packet = uncompressed(message, &mut decoder);
                    match packet["op"].as_u64() {
                        Some(1) => socket.send(compressed(json!({"op":11}), &mut encoder)).await.unwrap(),
                        Some(4 | 14) => commands.send(packet).unwrap(),
                        _ => panic!("unexpected outbound gateway packet"),
                    }
                }
                Some(Ok(WireMessage::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            },
            data = dispatches.recv() => {
                let Some(data) = data else { break };
                socket.send(compressed(data, &mut encoder)).await.unwrap();
            }
        }
    }
}

async fn expect_packet(commands: &mut async_mpsc::UnboundedReceiver<Value>, packet: Value) {
    assert_eq!(
        timeout(DEADLINE, commands.recv()).await.unwrap(),
        Some(packet)
    );
}

#[tokio::test]
async fn two_loopback_workers_deliver_update_navigate_and_signal_calls() {
    timeout(Duration::from_secs(25), two_worker_workflow())
        .await
        .unwrap();
}

async fn two_worker_workflow() {
    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let websocket = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", http.local_addr().unwrap());
    let gateway = format!("ws://{}", websocket.local_addr().unwrap());
    let alice = json!({"id":"1","username":"alice"});
    let bob = json!({"id":"2","username":"bob"});
    let (alice_dispatch, alice_recv) = async_mpsc::unbounded_channel();
    let (bob_dispatch, bob_recv) = async_mpsc::unbounded_channel();
    let (alice_commands, mut alice_wire) = async_mpsc::unbounded_channel();
    let (bob_commands, mut bob_wire) = async_mpsc::unbounded_channel();
    let alice_user = alice.clone();
    let bob_user = bob.clone();
    let gateway_server = tokio::spawn(async move {
        // The test finishes Alice's handshake before starting Bob's login.
        let (alice_socket, _) = websocket.accept().await.unwrap();
        tokio::join!(
            loopback_gateway(
                alice_socket,
                alice_user,
                "alice-loopback",
                alice_recv,
                alice_commands
            ),
            async {
                let (bob_socket, _) = websocket.accept().await.unwrap();
                loopback_gateway(bob_socket, bob_user, "bob-loopback", bob_recv, bob_commands)
                    .await;
            },
        );
    });
    let api = format!("{origin}/api");
    let webapp = origin.clone();
    let sent = json!({"id":"200","channel_id":"100","author":alice,"content":"sent by Alice"});
    let sent_http = sent.clone();
    let alice_delivery = alice_dispatch.clone();
    let bob_delivery = bob_dispatch.clone();
    let http_server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = http.accept().await.unwrap();
            let request = read_request(&mut socket).await;
            let (headers, body) = request.split_once("\r\n\r\n").unwrap();
            let authorization = headers.lines().find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("authorization")
                    .then(|| value.trim())
            });
            let mut route = headers.lines().next().unwrap().split_whitespace();
            let (status, response) = match (
                route.next().unwrap(),
                route.next().unwrap(),
                authorization,
            ) {
                ("GET", "/.well-known/fluxer", None) => (
                    200,
                    json!({
                        "codename":"tungsten","api_code_version":1,"endpoints":{
                            "api":api,"api_client":api,"api_public":api,"media":api,"static_cdn":api,
                            "marketing":api,"admin":api,"invite":api,"gift":api,"webapp":webapp,"gateway":gateway
                        }
                    }),
                ),
                ("GET", "/api/v1/users/@me", Some("alice-loopback")) => (200, alice.clone()),
                ("GET", "/api/v1/users/@me", Some("bob-loopback")) => (200, bob.clone()),
                (
                    "GET",
                    "/api/v1/channels/100/messages?limit=50",
                    Some("alice-loopback" | "bob-loopback"),
                ) => (200, json!([])),
                (
                    "GET",
                    "/api/v1/channels/100/slowmode",
                    Some("alice-loopback" | "bob-loopback"),
                ) => (
                    200,
                    json!({"rate_limit_per_user":0,"retry_after_ms":0,"can_bypass":false}),
                ),
                (
                    "POST",
                    "/api/v1/channels/100/messages",
                    Some("alice-loopback" | "bob-loopback"),
                ) => {
                    let body: Value = serde_json::from_str(body).unwrap();
                    let nonce = body["nonce"]
                        .as_str()
                        .filter(|nonce| !nonce.is_empty())
                        .unwrap();
                    if authorization == Some("alice-loopback") {
                        assert_eq!(body, json!({"content":"sent by Alice","nonce":nonce}));
                        let packet = dispatch(2, "MESSAGE_CREATE", sent_http.clone());
                        alice_delivery.send(packet.clone()).unwrap();
                        bob_delivery.send(packet).unwrap();
                        (200, sent_http.clone())
                    } else {
                        assert_eq!(body, json!({"content":"denied for Bob","nonce":nonce}));
                        (403, json!({"code":"MISSING_PERMISSIONS"}))
                    }
                }
                _ => panic!(
                    "unexpected fixture request {}",
                    headers.lines().next().unwrap()
                ),
            };
            let response = response.to_string();
            socket.write_all(format!(
                "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                response.len()
            ).as_bytes()).await.unwrap();
        }
    });

    let (mut alice_worker, alice_events) = worker();
    let (mut bob_worker, bob_events) = worker();
    for (worker, token, id, wire) in [
        (&mut alice_worker, "alice-loopback", 1, &mut alice_wire),
        (&mut bob_worker, "bob-loopback", 2, &mut bob_wire),
    ] {
        worker
            .command(Command::TokenLogin {
                instance: origin.clone(),
                token: token.into(),
            })
            .await
            .unwrap();
        through(worker, 1).await;
        assert_eq!(
            worker.state.current_user.as_ref().unwrap().id,
            Snowflake(id)
        );
        worker
            .command(Command::SelectChannel {
                channel_id: 100.into(),
            })
            .await
            .unwrap();
        finish_history(worker, 100.into()).await;
        assert!(
            worker
                .command(Command::SelectChannel {
                    channel_id: 999.into()
                })
                .await
                .is_err()
        );
        assert_eq!(worker.state.selected_channel, Some(100.into()));
        expect_packet(
            wire,
            json!({"op":14,"d":{"subscriptions":{"10":{"active":true,"sync":true}}}}),
        )
        .await;
    }

    alice_worker
        .command(Command::SendMessage {
            content: "sent by Alice".into(),
            reply_to: None,
            mention_author: false,
        })
        .await
        .unwrap();
    finish_attachment_send(&mut alice_worker, 100.into()).await;
    for worker in [&mut alice_worker, &mut bob_worker] {
        through(worker, 2).await;
        let messages = worker
            .state
            .channel_messages(100.into())
            .collect::<Vec<_>>();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "sent by Alice");
        assert_eq!(messages[0].author.id, Snowflake(1));
    }
    assert!(bob_events.try_iter().any(|event| matches!(event, Event::LiveMessageCreated { account_id: Snowflake(2), message } if message.id == Snowflake(200))));

    bob_worker
        .command(Command::SendMessage {
            content: "denied for Bob".into(),
            reply_to: None,
            mention_author: false,
        })
        .await
        .unwrap();
    finish_attachment_send(&mut bob_worker, 100.into()).await;
    let failure_events = bob_events.try_iter().collect::<Vec<_>>();
    assert!(
        failure_events
            .iter()
            .any(|event| matches!(event, Event::MessageOperationFailed {
        channel_id: Snowflake(100), content, error
    } if content == "denied for Bob" && error.contains("403")))
    );
    assert!(
        failure_events
            .iter()
            .any(|event| matches!(event, Event::Outbox {
                channel_id: Snowflake(100), messages
            } if messages.iter().any(|message| {
                message.content == "denied for Bob"
                    && matches!(message.status, crate::PendingMessageStatus::Failed(_))
            })))
    );

    for tx in [&alice_dispatch, &bob_dispatch] {
        // The unknown target must stay absent, and an update cannot change the author's identity.
        tx.send(dispatch(
            3,
            "MESSAGE_UPDATE",
            json!({"id":"999","channel_id":"100","content":"missing"}),
        ))
        .unwrap();
        tx.send(dispatch(
            4,
            "MESSAGE_UPDATE",
            json!({"id":"200","channel_id":"100","content":"edited by Alice"}),
        ))
        .unwrap();
    }
    for worker in [&mut alice_worker, &mut bob_worker] {
        through(worker, 4).await;
        let messages = worker
            .state
            .channel_messages(100.into())
            .collect::<Vec<_>>();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "edited by Alice");
        assert_eq!(messages[0].author.id, Snowflake(1));
    }
    assert!(
        !bob_events
            .try_iter()
            .any(|event| matches!(event, Event::LiveMessageCreated { .. }))
    );
    assert_eq!(
        alice_events
            .try_iter()
            .filter(|event| matches!(event, Event::MessageSent { .. }))
            .count(),
        1
    );

    let voice_packet = |channel: Value| {
        json!({"op":4,"d":{
            "guild_id":"10","channel_id":channel,"self_mute":false,"self_deaf":false,
            "self_video":false,"self_stream":false,"connection_id":null,"viewer_stream_keys":[],
            "p2p":false,"is_mobile":false
        }})
    };
    for (worker, wire) in [
        (&mut alice_worker, &mut alice_wire),
        (&mut bob_worker, &mut bob_wire),
    ] {
        assert!(
            worker
                .command(Command::JoinVoice {
                    guild_id: Some(10.into()),
                    channel_id: 100.into()
                })
                .await
                .is_err()
        );
        worker
            .command(Command::JoinVoice {
                guild_id: Some(10.into()),
                channel_id: 101.into(),
            })
            .await
            .unwrap();
        expect_packet(wire, voice_packet(json!("101"))).await;
        assert!(worker.media.is_none());
        worker.command(Command::LeaveVoice).await.unwrap();
        expect_packet(wire, voice_packet(Value::Null)).await;
        assert!(worker.voice_target.is_none());
        assert!(worker.media.is_none());
        worker.reset().await;
    }

    gateway_server.abort();
    http_server.abort();
    for server in [gateway_server, http_server] {
        if let Err(error) = server.await {
            assert!(error.is_cancelled(), "fixture server failed: {error}");
        }
    }
}
