// SPDX-License-Identifier: AGPL-3.0-or-later
//! Exercise the public actor with real loopback HTTP bytes and isolated storage.
//! No account vault, microphone, screen capture or external server is used.
use fluxer_client::{
    AttachmentStatus, Command, Controller, Event, PendingMessage, PendingMessageStatus,
    StagedAttachment, next_pending_message_id,
};
use fluxer_core::{ClientState, Snowflake};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{Mutex, Notify},
    task::JoinHandle,
    time::timeout,
};
use tokio_tungstenite::{accept_async, tungstenite::Message};

const BYTES: &[u8] = b"generated attachment bytes\0\xff\n";
type RecordedRequests = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

#[derive(Default)]
struct Seen {
    state: ClientState,
    attachments: BTreeMap<Snowflake, Vec<StagedAttachment>>,
    outbox: BTreeMap<Snowflake, Vec<PendingMessage>>,
    sent: usize,
    failed: usize,
}
impl Seen {
    fn drain(&mut self, controller: &Controller) {
        for _ in 0..1024 {
            let Some(event) = controller.try_recv_control() else {
                break;
            };
            match event {
                Event::State(state) => self.state = *state,
                Event::Attachments {
                    channel_id,
                    attachments,
                } => {
                    self.attachments.insert(channel_id, attachments);
                }
                Event::AttachmentsCleared => {
                    self.attachments.clear();
                    self.outbox.clear();
                }
                Event::Outbox {
                    channel_id,
                    messages,
                } => {
                    self.outbox.insert(channel_id, messages);
                }
                Event::MessageSent { .. } => self.sent += 1,
                Event::MessageOperationFailed { .. } => self.failed += 1,
                _ => {}
            }
        }
    }
    async fn until(&mut self, controller: &Controller, condition: impl Fn(&Self) -> bool) {
        timeout(Duration::from_secs(8), async {
            loop {
                self.drain(controller);
                if condition(self) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("attachment actor did not reach the expected state");
    }
    fn files(&self, channel: u64) -> &[StagedAttachment] {
        self.attachments
            .get(&Snowflake(channel))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }
    fn pending(&self, channel: u64) -> &[PendingMessage] {
        self.outbox
            .get(&Snowflake(channel))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }
}

struct Fixture {
    origin: String,
    requests: RecordedRequests,
    messages: Arc<Mutex<Vec<Value>>>,
    held: Arc<Notify>,
    release: Arc<Notify>,
    tasks: Vec<JoinHandle<()>>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}
impl Fixture {
    async fn new(presigned: bool) -> Self {
        let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", http.local_addr().unwrap());
        let gateway = format!("ws://{}", ws.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let messages = Arc::new(Mutex::new(Vec::new()));
        let held = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let websocket = tokio::spawn(async move {
            let (socket, _) = ws.accept().await.unwrap();
            let mut socket = accept_async(socket).await.unwrap();
            socket
                .send(Message::text(
                    json!({"op":10,"d":{"heartbeat_interval":60000}}).to_string(),
                ))
                .await
                .unwrap();
            // The gateway negotiates zstd; the existing transport accepts text
            // dispatches as well. No fake local state is injected into the actor.
            socket.next().await.unwrap().unwrap();
            socket.send(Message::text(json!({"op":0,"t":"READY","s":1,"d":{
                "session_id":"attachments-test","user":{"id":"1","username":"fixture"},
                "user_settings":{"synced_preferences":if presigned {"ggUCGAE="}else{"ggUCGAA="}},
                "guilds":[{"id":"10","properties":{"name":"Fixture","owner_id":"1"},"channels":[
                    {"id":"100","name":"one","type":0},{"id":"101","name":"two","type":0}
                ]}]
            }}).to_string())).await.unwrap();
            while let Some(Ok(message)) = socket.next().await {
                if matches!(message, Message::Close(_)) {
                    break;
                }
                if matches!(message, Message::Text(_) | Message::Binary(_))
                    && socket.send(Message::text("{\"op\":11}")).await.is_err()
                {
                    break;
                }
            }
        });
        let http_origin = origin.clone();
        let recorded = requests.clone();
        let posted = messages.clone();
        let held_signal = held.clone();
        let release_signal = release.clone();
        let server = tokio::spawn(async move {
            let mut children = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = http.accept() => {
                        let (mut socket, _) = accepted.unwrap();
                        let origin = http_origin.clone();
                        let gateway = gateway.clone();
                        let recorded = recorded.clone();
                        let posted = posted.clone();
                        let held = held_signal.clone();
                        let release = release_signal.clone();
                        children.spawn(async move {
                            let (headers, bytes) = request(&mut socket).await;
                            let first = headers.lines().next().unwrap().to_owned();
                            recorded.lock().await.push((first.clone(), bytes.clone()));
                            let fields: Vec<_> = first.split_whitespace().collect();
                            let method = fields[0];
                            let path = fields[1];
                            let lower = headers.to_ascii_lowercase();
                            if path.starts_with("/api/") {
                                assert!(lower.contains("authorization: attachment-test-token\r\n"));
                            }
                            let (status, body) = match (method, path) {
                                ("GET", "/.well-known/fluxer") => {
                                    assert!(!lower.contains("authorization:"));
                                    let api = format!("{origin}/api");
                                    (200,json!({"codename":"tungsten","api_code_version":1,
                                        "features":{"presigned_attachment_uploads":presigned},
                                        "endpoints":{"api":api,"api_client":api,"api_public":api,"media":api,
                                            "static_cdn":api,"marketing":api,"admin":api,"invite":api,"gift":api,
                                            "webapp":origin,"gateway":gateway}}))
                                }
                                ("GET", "/api/v1/users/@me") => (200,json!({"id":"1","username":"fixture"})),
                                ("GET", "/api/v1/channels/100/messages?limit=50" | "/api/v1/channels/101/messages?limit=50") => (200,json!([])),
                                ("GET", "/api/v1/channels/100/slowmode" | "/api/v1/channels/101/slowmode") => (200,json!({"rate_limit_per_user":0,"retry_after_ms":0,"can_bypass":false})),
                                ("POST", "/api/v1/channels/100/attachments") => {
                                    assert!(presigned);
                                    let body: Value = serde_json::from_slice(&bytes).unwrap();
                                    (200,json!({"attachments":body["attachments"].as_array().unwrap().iter().map(|file| {
                                        let mut plan = file.clone();
                                        plan["upload_mode"] = "singlepart".into();
                                        plan["upload_filename"] = format!("prepared/{}",file["id"].as_str().unwrap()).into();
                                        plan["upload_url"] = format!("{origin}/storage").into();
                                        plan
                                    }).collect::<Vec<_>>()}))
                                }
                                ("PUT", "/storage") => {
                                    assert_eq!(bytes,BYTES);
                                    assert!(!lower.contains("authorization:"));
                                    assert!(!lower.contains("x-fluxer-"));
                                    assert!(!lower.contains("origin:"));
                                    (200,Value::Null)
                                }
                                ("POST", "/api/v1/channels/100/messages") => {
                                    let body = if presigned {
                                        serde_json::from_slice::<Value>(&bytes).unwrap()
                                    } else {
                                        assert!(lower.contains("multipart/form-data; boundary="));
                                        assert!(bytes.windows(BYTES.len()).any(|chunk|chunk==BYTES));
                                        assert!(bytes.windows(b"name=\"files[0]\"".len()).any(|chunk|chunk==b"name=\"files[0]\""));
                                        let text = String::from_utf8_lossy(&bytes);
                                        let payload = text.split("name=\"payload_json\"").nth(1).unwrap().split_once("\r\n\r\n").unwrap().1.split("\r\n").next().unwrap();
                                        serde_json::from_str::<Value>(payload).unwrap()
                                    };
                                    let attempt = {
                                        let mut messages = posted.lock().await;
                                        messages.push(body.clone());
                                        messages.len()
                                    };
                                    if !presigned && attempt == 1 {
                                        (403,json!({"code":"MISSING_PERMISSIONS"}))
                                    } else {
                                        // Hold the real request while the UI changes channels/drafts.
                                        held.notify_one();
                                        release.notified().await;
                                        (200,json!({"id":format!("{}",900+attempt),"channel_id":"100",
                                            "author":{"id":"1","username":"fixture"},"content":body.get("content").and_then(Value::as_str).unwrap_or(""),
                                            "nonce":body["nonce"],"attachments":[],"message_reference":body.get("message_reference")}))
                                    }
                                }
                                _ => panic!("unexpected attachment request {first}"),
                            };
                            respond(&mut socket,status,body).await;
                        });
                    }
                    finished = children.join_next(), if !children.is_empty() => { finished.unwrap().unwrap(); }
                }
            }
        });
        Self {
            origin,
            requests,
            messages,
            held,
            release,
            tasks: vec![server, websocket],
        }
    }
    async fn controller(&self) -> (Controller, Seen) {
        let controller =
            Controller::spawn_with_store(Arc::new(fluxer_session::MemorySessionStore::default()))
                .unwrap();
        controller
            .send(Command::TokenLogin {
                instance: self.origin.clone(),
                token: "attachment-test-token".into(),
            })
            .unwrap();
        let mut seen = Seen::default();
        seen.until(&controller, |seen| seen.state.channels.len() == 2)
            .await;
        controller
            .send(Command::SelectChannel {
                channel_id: 100.into(),
            })
            .unwrap();
        seen.until(&controller, |seen| {
            seen.state.selected_channel == Some(100.into())
        })
        .await;
        (controller, seen)
    }
}

async fn request(socket: &mut TcpStream) -> (String, Vec<u8>) {
    let mut bytes = Vec::new();
    let mut chunk = [0; 8192];
    loop {
        let size = socket.read(&mut chunk).await.unwrap();
        assert!(size > 0, "request closed early");
        bytes.extend_from_slice(&chunk[..size]);
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            let headers = String::from_utf8(bytes[..end + 2].to_vec()).unwrap();
            let length: usize = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().unwrap())
                })
                .unwrap_or(0);
            if bytes.len() >= end + 4 + length {
                return (headers, bytes[end + 4..end + 4 + length].to_vec());
            }
        }
    }
}
async fn respond(socket: &mut TcpStream, status: u16, body: Value) {
    let body = body.to_string();
    let response = format!(
        "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = socket.write_all(response.as_bytes()).await;
    let _ = socket.shutdown().await;
}

#[tokio::test]
async fn multipart_failure_retains_files_and_actor_handles_channel_changes_during_retry() {
    let fixture = Fixture::new(false).await;
    let (controller, mut seen) = fixture.controller().await;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("generated.bin");
    std::fs::write(&path, BYTES).unwrap();
    controller
        .send(Command::StageFiles {
            channel_id: 100.into(),
            paths: vec![path],
        })
        .unwrap();
    seen.until(&controller, |seen| {
        seen.files(100).len() == 1 && seen.files(100)[0].status == AttachmentStatus::Staged
    })
    .await;
    assert!(
        !fixture
            .requests
            .lock()
            .await
            .iter()
            .any(|(request, _)| request.contains("/attachments"))
    );
    controller
        .send(Command::SetDraft("caption".into()))
        .unwrap();
    let id = next_pending_message_id();
    controller
        .send(Command::SendPendingMessage {
            id,
            channel_id: 100.into(),
            content: "caption".into(),
            reply_to: Some(200.into()),
            mention_author: false,
        })
        .unwrap();
    seen.until(&controller, |seen| seen.failed == 1).await;
    seen.until(&controller, |seen| {
        seen.pending(100).iter().any(|message| {
            message.id == id && matches!(message.status, PendingMessageStatus::Failed(_))
        })
    })
    .await;
    assert!(seen.files(100).is_empty());
    assert_eq!(seen.pending(100)[0].content, "caption");
    assert_eq!(seen.pending(100)[0].attachments.len(), 1);
    controller
        .send(Command::RetryPendingMessage {
            channel_id: 100.into(),
            id,
        })
        .unwrap();
    timeout(Duration::from_secs(8), fixture.held.notified())
        .await
        .unwrap();
    controller
        .send(Command::SelectChannel {
            channel_id: 101.into(),
        })
        .unwrap();
    controller
        .send(Command::SetDraft("other channel draft".into()))
        .unwrap();
    controller
        .send(Command::SelectGuild {
            guild_id: 10.into(),
        })
        .unwrap();
    seen.until(&controller, |seen| {
        seen.state.drafts.get(&101.into()).map(String::as_str) == Some("other channel draft")
    })
    .await;
    assert_eq!(
        seen.pending(100)[0].attachments.len(),
        1,
        "switching channels retains sending files"
    );
    fixture.release.notify_one();
    seen.until(&controller, |seen| seen.sent == 1).await;
    seen.until(&controller, |seen| seen.pending(100).is_empty())
        .await;
    assert_eq!(seen.state.selected_channel, Some(101.into()));
    assert_eq!(
        seen.state.drafts.get(&101.into()).map(String::as_str),
        Some("other channel draft")
    );
    let posted = fixture.messages.lock().await;
    assert_eq!(posted.len(), 2);
    assert_eq!(
        posted[0]["nonce"], posted[1]["nonce"],
        "an unchanged retry must reuse its nonce"
    );
    assert_eq!(posted[1]["message_reference"]["message_id"], "200");
    assert_eq!(posted[1]["allowed_mentions"]["replied_user"], false);
}

#[tokio::test]
async fn preuploaded_attachment_only_send_keeps_new_draft_and_newly_staged_file() {
    let fixture = Fixture::new(true).await;
    let (controller, mut seen) = fixture.controller().await;
    controller
        .send(Command::StageClipboardImage {
            channel_id: 100.into(),
            filename: "clipboard.bin".into(),
            mime_type: "application/octet-stream".into(),
            bytes: Arc::new(BYTES.to_vec()),
        })
        .unwrap();
    seen.until(&controller, |seen| {
        seen.files(100).len() == 1 && seen.files(100)[0].status == AttachmentStatus::Ready
    })
    .await;
    let id = seen.files(100)[0].id;
    controller
        .send(Command::SetAttachmentSpoiler {
            channel_id: 100.into(),
            id,
            spoiler: true,
        })
        .unwrap();
    controller
        .send(Command::SendMessage {
            content: String::new(),
            reply_to: None,
            mention_author: false,
        })
        .unwrap();
    timeout(Duration::from_secs(8), fixture.held.notified())
        .await
        .unwrap();
    controller
        .send(Command::SetDraft("typed after Send".into()))
        .unwrap();
    controller
        .send(Command::StageClipboardImage {
            channel_id: 100.into(),
            filename: "next.bin".into(),
            mime_type: "application/octet-stream".into(),
            bytes: Arc::new(BYTES.to_vec()),
        })
        .unwrap();
    seen.until(&controller, |seen| {
        seen.files(100)
            .iter()
            .any(|file| file.id != id && file.status == AttachmentStatus::Ready)
    })
    .await;
    fixture.release.notify_one();
    seen.until(&controller, |seen| seen.sent == 1).await;
    assert_eq!(
        seen.state.drafts.get(&100.into()).map(String::as_str),
        Some("typed after Send")
    );
    assert_eq!(seen.files(100).len(), 1);
    assert_ne!(seen.files(100)[0].id, id);
    let posted = fixture.messages.lock().await;
    assert_eq!(posted.len(), 1);
    assert!(
        posted[0].get("content").is_none(),
        "attachment-only sends omit content"
    );
    assert_eq!(posted[0]["attachments"][0]["flags"], 8);
    assert!(
        posted[0]["attachments"][0]["upload_filename"]
            .as_str()
            .unwrap()
            .starts_with("prepared/")
    );
}

#[tokio::test]
async fn changed_file_is_rejected_before_upload_and_preserved_in_outbox() {
    let fixture = Fixture::new(false).await;
    let (controller, mut seen) = fixture.controller().await;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("changed.bin");
    std::fs::write(&path, BYTES).unwrap();
    controller
        .send(Command::StageFiles {
            channel_id: 100.into(),
            paths: vec![path.clone()],
        })
        .unwrap();
    seen.until(&controller, |seen| {
        seen.files(100).len() == 1 && seen.files(100)[0].status == AttachmentStatus::Staged
    })
    .await;
    std::fs::write(&path, [BYTES, BYTES].concat()).unwrap();
    controller
        .send(Command::SetDraft("keep this text".into()))
        .unwrap();
    controller
        .send(Command::SendMessage {
            content: "keep this text".into(),
            reply_to: None,
            mention_author: false,
        })
        .unwrap();
    seen.until(&controller, |seen| seen.failed == 1).await;
    seen.until(&controller, |seen| {
        seen.pending(100).iter().any(|message| {
            message.content == "keep this text"
                && matches!(message.status, PendingMessageStatus::Failed(_))
        })
    })
    .await;
    assert!(
        fixture.messages.lock().await.is_empty(),
        "changed files must not reach message creation"
    );
    assert!(seen.files(100).is_empty());
    assert_eq!(seen.pending(100)[0].attachments.len(), 1);
}
