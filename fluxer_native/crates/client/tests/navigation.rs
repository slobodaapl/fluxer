// SPDX-License-Identifier: AGPL-3.0-or-later
//! Public-controller acceptance against isolated HTTP/WebSocket peers. Every
//! unexpected request fails; these fixtures never access a saved real account.
use fluxer_client::{
    Command, Controller, Event, HistorySnapshot, PinsSnapshot, SearchScope, SearchSnapshot,
    SearchSort, SearchStatus,
};
use fluxer_core::{ClientState, Snowflake};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{Mutex, Notify, mpsc},
    task::JoinHandle,
    time::timeout,
};
use tokio_tungstenite::{accept_async, tungstenite::Message};

#[derive(Clone)]
struct Request {
    method: String,
    path: String,
    body: Value,
}
type Requests = Arc<Mutex<Vec<Request>>>;
struct Fixture {
    origin: String,
    requests: Requests,
    tasks: Vec<JoinHandle<()>>,
    dispatch: mpsc::UnboundedSender<Value>,
    held: Arc<Notify>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}
#[derive(Default)]
struct Seen {
    state: ClientState,
    search: Option<SearchSnapshot>,
    pins: Option<PinsSnapshot>,
    history: BTreeMap<Snowflake, HistorySnapshot>,
    pin_done: Vec<(Snowflake, bool, Option<String>)>,
    attachments: BTreeMap<Snowflake, usize>,
    errors: Vec<String>,
}
impl Seen {
    fn drain(&mut self, controller: &Controller) {
        for _ in 0..1024 {
            let Some(event) = controller.try_recv_control() else {
                break;
            };
            match event {
                Event::State(state) => self.state = *state,
                Event::Search(search) => self.search = Some(search),
                Event::Pins(pins) => self.pins = Some(pins),
                Event::History(history) => {
                    self.history.insert(history.channel_id, history);
                }
                Event::PinMutation {
                    message_id,
                    pinned,
                    pending: false,
                    error,
                    ..
                } => self.pin_done.push((message_id, pinned, error)),
                Event::Attachments {
                    channel_id,
                    attachments,
                } => {
                    self.attachments.insert(channel_id, attachments.len());
                }
                Event::Error(error) => self.errors.push(error),
                _ => {}
            }
        }
    }
    async fn until(&mut self, controller: &Controller, condition: impl Fn(&Self) -> bool) {
        let result = timeout(Duration::from_secs(8), async {
            loop {
                self.drain(controller);
                if condition(self) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        assert!(
            result.is_ok(),
            "navigation condition timed out; errors={:?}, search={:?}, history={:?}",
            self.errors,
            self.search.as_ref().map(|s| (&s.query, s.status, &s.error)),
            self.history
        );
    }
    fn ready_history(&self, channel: u64) -> bool {
        self.history
            .get(&channel.into())
            .is_some_and(|h| !h.loading)
    }
}
fn message(id: u64, channel: u64) -> Value {
    json!({"id":id.to_string(),"channel_id":channel.to_string(),"author":{"id":"1"},"content":format!("archive release {id}")})
}
fn channel() -> Value {
    json!({"id":"103","guild_id":"10","name":"archived-results","type":0,"last_message_id":"1200"})
}
impl Fixture {
    async fn new() -> Self {
        let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", http.local_addr().unwrap());
        let gateway = format!("ws://{}", ws.local_addr().unwrap());
        let (dispatch, mut incoming) = mpsc::unbounded_channel::<Value>();
        let socket = tokio::spawn(async move {
            let (socket, _) = ws.accept().await.unwrap();
            let mut socket = accept_async(socket).await.unwrap();
            socket
                .send(Message::text(
                    json!({"op":10,"d":{"heartbeat_interval":60000}}).to_string(),
                ))
                .await
                .unwrap();
            socket.next().await.unwrap().unwrap();
            socket.send(Message::text(json!({"op":0,"t":"READY","s":1,"d":{
                "session_id":"navigation-fixture","user":{"id":"1","username":"fixture"},
                "user_settings":{"synced_preferences":"ggUCGAA="},
                "read_states":[{"id":"100","last_message_id":"50","mention_count":2}],
                "guilds":[{"id":"10","properties":{"name":"Fixture","owner_id":"1"},"channels":[
                    {"id":"100","name":"general","type":0,"last_message_id":"1000"},
                    {"id":"101","name":"other","type":0,"last_message_id":"1100"}
                ]}]
            }}).to_string())).await.unwrap();
            loop {
                tokio::select! {
                    event=incoming.recv()=>match event{Some(event)=>{if socket.send(Message::text(event.to_string())).await.is_err(){break;}},None=>break},
                    event=socket.next()=>match event{
                        Some(Ok(Message::Text(_)|Message::Binary(_)))=>{if socket.send(Message::text("{\"op\":11}")).await.is_err(){break;}},
                        Some(Ok(Message::Close(_)))|None|Some(Err(_))=>break,_=>{}
                    }
                }
            }
        });
        let requests: Requests = Arc::default();
        let recorded = requests.clone();
        let http_origin = origin.clone();
        let held = Arc::new(Notify::new());
        let waiting = held.clone();
        let server = tokio::spawn(async move {
            let mut workers = tokio::task::JoinSet::new();
            let search_attempts = Arc::new(Mutex::new(BTreeMap::<String, u32>::new()));
            loop {
                tokio::select! {
                    accepted=http.accept()=>{
                        let(mut socket,_)=accepted.unwrap();let origin=http_origin.clone();let gateway=gateway.clone();let recorded=recorded.clone();let attempts=search_attempts.clone();let held=waiting.clone();
                        workers.spawn(async move {
                            let (headers,bytes)=read_request(&mut socket).await;let first=headers.lines().next().unwrap();let fields=first.split_whitespace().collect::<Vec<_>>();
                            let(method,path)=(fields[0],fields[1]);let body=if bytes.is_empty(){Value::Null}else{serde_json::from_slice(&bytes).unwrap()};
                            if path.starts_with("/api/"){assert!(headers.to_ascii_lowercase().contains("authorization: navigation-test-token\r\n"));}
                            recorded.lock().await.push(Request{method:method.into(),path:path.into(),body:body.clone()});
                            let(status,response)=match(method,path){
                                ("GET","/.well-known/fluxer")=>{
                                    assert!(!headers.to_ascii_lowercase().contains("authorization:"));let api=format!("{origin}/api");
                                    (200,json!({"codename":"tungsten","api_code_version":1,"features":{},"endpoints":{"api":api,"api_client":api,"api_public":api,"media":api,"static_cdn":api,"marketing":api,"admin":api,"invite":api,"gift":api,"webapp":origin,"gateway":gateway}}))
                                },
                                ("GET","/api/v1/users/@me")=>(200,json!({"id":"1","username":"fixture"})),
                                ("GET","/api/v1/channels/100/messages?limit=50")=>(200,json!([message(1000,100)])),
                                ("GET","/api/v1/channels/101/messages?limit=50")=>(200,json!([message(1100,101)])),
                                ("GET","/api/v1/channels/103/messages?limit=50")=>(200,json!([message(1200,103)])),
                                ("GET","/api/v1/channels/103/messages?limit=50&around=200")=>(200,json!((176..226).rev().map(|id|message(id,103)).collect::<Vec<_>>())),
                                ("GET","/api/v1/channels/103/messages?limit=50&after=225")=>(200,json!((226..276).map(|id|message(id,103)).collect::<Vec<_>>())),
                                ("POST","/api/v1/search/messages")=>{
                                    let query=body.get("content").and_then(Value::as_str).unwrap_or("").to_owned();
                                    let attempt={let mut counts=attempts.lock().await;let count=counts.entry(query.clone()).or_default();*count+=1;*count};
                                    if query=="index"&&attempt==1 {(202,json!({"indexing":true}))}
                                    else if query=="stalled" {held.notify_one();let mut probe=[0];let _=socket.read(&mut probe).await;return;}
                                    else{(200,json!({"messages":[message(200,103)],"channels":[channel()],"total":26,"page":body["page"],"hits_per_page":25}))}
                                },
                                ("GET","/api/v1/channels/100/messages/pins?limit=25")=>(200,json!({"items":[{"message":message(300,100),"pinned_at":"2026-11-12T12:00:00.000Z"}],"has_more":true})),
                                ("GET","/api/v1/channels/100/messages/pins?limit=25&before=2026%2D11%2D12T12%3A00%3A00%2E000Z")=>(200,json!({"items":[{"message":message(301,100),"pinned_at":"2026-11-11T12:00:00.000Z"}],"has_more":false})),
                                ("DELETE","/api/v1/channels/100/pins/300")=>{assert!(bytes.is_empty());(403,json!({"code":"MISSING_PERMISSIONS"}))},
                                ("PUT","/api/v1/channels/100/pins/1000")=>{assert!(bytes.is_empty());(200,Value::Null)},
                                ("POST","/api/v1/read-states/ack")=>{
                                    if body["read_states"][0]["channel_id"] == "100" { (200,json!({"read_states":[{"id":"100","last_message_id":body["read_states"][0]["message_id"],"mention_count":4,"version":"10"}]})) }
                                    else {(200,json!({"read_states":[{"id":"103","last_message_id":"1200","mention_count":7,"version":"9"}]}))}
                                },
                                ("PATCH","/api/v1/users/@me/guilds/10/settings")=>(200,json!({})),
                                _=>panic!("unexpected navigation request {first}"),
                            };respond(&mut socket,status,response).await;
                        });
                    },
                    finished=workers.join_next(),if !workers.is_empty()=>{finished.unwrap().unwrap();}
                }
            }
        });
        Self {
            origin,
            requests,
            tasks: vec![socket, server],
            dispatch,
            held,
        }
    }
    async fn controller(&self) -> (Controller, Seen) {
        let controller =
            Controller::spawn_with_store(Arc::new(fluxer_session::MemorySessionStore::default()))
                .unwrap();
        controller
            .send(Command::TokenLogin {
                instance: self.origin.clone(),
                token: "navigation-test-token".into(),
            })
            .unwrap();
        let mut seen = Seen::default();
        seen.until(&controller, |s| s.state.channels.len() == 2)
            .await;
        controller
            .send(Command::SelectChannel {
                channel_id: 100.into(),
            })
            .unwrap();
        seen.until(&controller, |s| s.ready_history(100)).await;
        (controller, seen)
    }
    async fn search(&self, controller: &Controller, query: &str, page: u32) {
        controller
            .send(Command::SearchMessages {
                channel_id: 100.into(),
                query: query.into(),
                scope: SearchScope::Current,
                scope_override: None,
                sort: SearchSort::Newest,
                page,
            })
            .unwrap();
    }
}
async fn read_request(socket: &mut TcpStream) -> (String, Vec<u8>) {
    let mut bytes = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        let count = socket.read(&mut buffer).await.unwrap();
        assert!(count > 0);
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            let headers = String::from_utf8(bytes[..end + 2].to_vec()).unwrap();
            let length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            if bytes.len() >= end + 4 + length {
                return (headers, bytes[end + 4..end + 4 + length].to_vec());
            }
        }
    }
}
async fn respond(socket: &mut TcpStream, status: u16, body: Value) {
    let body = if body.is_null() {
        String::new()
    } else {
        body.to_string()
    };
    let response = format!(
        "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = socket.write_all(response.as_bytes()).await;
    let _ = socket.shutdown().await;
}

#[tokio::test]
async fn real_search_jump_and_return_keep_history_contiguous_and_preserve_unread_drafts_files() {
    let fixture = Fixture::new().await;
    let (controller, mut seen) = fixture.controller().await;
    assert_eq!(
        seen.state.read_states[&100.into()].last_message_id,
        Some(50.into())
    );
    controller
        .send(Command::SetDraft("draft before jump".into()))
        .unwrap();
    controller
        .send(Command::StageClipboardImage {
            channel_id: 100.into(),
            filename: "kept.bin".into(),
            mime_type: "application/octet-stream".into(),
            bytes: Arc::new(b"generated bytes".to_vec()),
        })
        .unwrap();
    seen.until(&controller, |s| s.attachments.get(&100.into()) == Some(&1))
        .await;
    fixture
        .search(&controller, "archive from:@me has:file scope:all_guilds", 1)
        .await;
    seen.until(&controller, |s| {
        s.search
            .as_ref()
            .is_some_and(|s| s.status == SearchStatus::Ready)
    })
    .await;
    assert_eq!(
        seen.search.as_ref().unwrap().messages[0].author.username,
        "fixture",
        "compact author references must hydrate"
    );
    assert!(
        seen.state.channel_messages(103.into()).next().is_none(),
        "detached search hits must not enter the timeline"
    );
    let search = fixture
        .requests
        .lock()
        .await
        .iter()
        .find(|request| request.method == "POST" && request.path == "/api/v1/search/messages")
        .unwrap()
        .body
        .clone();
    assert_eq!(
        search,
        json!({"content":"archive","author_id":["1"],"has":["file"],"scope":"all_guilds","context_channel_id":"100","context_guild_id":"10","page":1,"hits_per_page":25,"sort_by":"timestamp","sort_order":"desc"})
    );
    controller
        .send(Command::JumpToMessage {
            channel_id: 103.into(),
            message_id: 200.into(),
        })
        .unwrap();
    seen.until(&controller, |s| {
        s.ready_history(103) && s.history[&103.into()].target == Some(200.into())
    })
    .await;
    let history = &seen.history[&103.into()];
    assert!(history.has_older && history.has_newer && !history.latest);
    assert_eq!(history.message_ids.len(), 50);
    assert_eq!(
        seen.state.drafts.get(&100.into()).map(String::as_str),
        Some("draft before jump")
    );
    assert_eq!(seen.attachments.get(&100.into()), Some(&1));
    assert!(
        seen.state
            .read_states
            .get(&103.into())
            .and_then(|read| read.last_message_id)
            .is_none()
    );
    controller
        .send(Command::LoadNewerMessages {
            channel_id: 103.into(),
        })
        .unwrap();
    seen.until(&controller, |s| {
        s.ready_history(103) && s.history[&103.into()].message_ids.last() == Some(&275.into())
    })
    .await;
    assert_eq!(seen.history[&103.into()].message_ids.len(), 100);
    fixture
        .search(&controller, "archive from:@me has:file scope:all_guilds", 2)
        .await;
    seen.until(&controller, |s| {
        s.search.as_ref().is_some_and(|result| {
            result.channel_id == Snowflake(100)
                && result.page == 2
                && result.status == SearchStatus::Ready
        })
    })
    .await;
    assert_eq!(
        seen.state.selected_channel,
        Some(103.into()),
        "paging the retained search must not navigate away from the result"
    );
    assert_eq!(seen.history[&103.into()].message_ids.len(), 100);
    controller
        .send(Command::ReturnToLatest {
            channel_id: 103.into(),
        })
        .unwrap();
    seen.until(&controller, |s| {
        s.ready_history(103)
            && s.history[&103.into()].target.is_none()
            && s.history[&103.into()].latest
    })
    .await;
    assert_eq!(
        seen.history[&103.into()].message_ids,
        vec![1200.into()],
        "latest must replace the visible window, not stitch across an unfetched gap"
    );
    assert!(
        !fixture
            .requests
            .lock()
            .await
            .iter()
            .any(|request| request.path.contains("read-states/ack")),
        "search/jump/history fetches must not acknowledge unseen messages"
    );
    controller
        .send(Command::AcknowledgeMessage {
            channel_id: 103.into(),
            message_id: 1200.into(),
        })
        .unwrap();
    seen.until(&controller, |s| {
        s.state
            .read_states
            .get(&103.into())
            .and_then(|read| read.last_message_id)
            == Some(1200.into())
    })
    .await;
    assert_eq!(
        seen.state.read_states[&103.into()].mention_count,
        7,
        "apply the server's acknowledgement response rather than synthesizing cleared mentions"
    );
    let requests = fixture.requests.lock().await;
    let acks = requests
        .iter()
        .filter(|r| r.path.contains("read-states/ack"))
        .collect::<Vec<_>>();
    assert_eq!(acks.len(), 1);
    assert_eq!(
        acks[0].body,
        json!({"read_states":[{"channel_id":"103","message_id":"1200"}]})
    );
}

#[tokio::test]
async fn indexing_and_stalled_search_do_not_block_actor_and_replaced_results_cannot_win() {
    let fixture = Fixture::new().await;
    let (controller, mut seen) = fixture.controller().await;
    fixture.search(&controller, "index", 2).await;
    seen.until(&controller, |s| {
        s.search
            .as_ref()
            .is_some_and(|s| s.status == SearchStatus::Indexing)
    })
    .await;
    assert_eq!(seen.search.as_ref().unwrap().http_status, Some(202));
    controller
        .send(Command::SetDraft("while indexing".into()))
        .unwrap();
    controller
        .send(Command::SelectGuild {
            guild_id: 10.into(),
        })
        .unwrap();
    seen.until(&controller, |s| {
        s.state.drafts.get(&100.into()).map(String::as_str) == Some("while indexing")
    })
    .await;
    seen.until(&controller, |s| {
        s.search
            .as_ref()
            .is_some_and(|s| s.query == "index" && s.status == SearchStatus::Ready)
    })
    .await;
    let pages = fixture
        .requests
        .lock()
        .await
        .iter()
        .filter(|r| r.path.ends_with("/search/messages") && r.body["content"] == "index")
        .map(|r| r.body["page"].clone())
        .collect::<Vec<_>>();
    assert_eq!(
        pages,
        vec![json!(2), json!(1)],
        "index polling restarts at page one as in the existing search state machine"
    );
    fixture.search(&controller, "stalled", 1).await;
    timeout(Duration::from_secs(5), fixture.held.notified())
        .await
        .unwrap();
    fixture.search(&controller, "replacement", 1).await;
    seen.until(&controller, |s| {
        s.search
            .as_ref()
            .is_some_and(|s| s.query == "replacement" && s.status == SearchStatus::Ready)
    })
    .await;
    controller
        .send(Command::CloseSearch {
            channel_id: 100.into(),
        })
        .unwrap();
    fixture
        .dispatch
        .send(json!({"op":0,"t":"MESSAGE_CREATE","s":2,"d":message(1001,100)}))
        .unwrap();
    seen.until(&controller, |s| {
        s.state
            .messages
            .get(&100.into())
            .is_some_and(|messages| messages.contains_key(&1001.into()))
    })
    .await;
    assert_eq!(seen.search.as_ref().unwrap().query, "replacement");
    assert_eq!(
        seen.state.read_states[&100.into()].last_message_id,
        Some(50.into())
    );
}

#[tokio::test]
async fn pins_page_by_timestamp_and_failed_mutation_preserves_real_message_state() {
    let fixture = Fixture::new().await;
    let (controller, mut seen) = fixture.controller().await;
    controller
        .send(Command::LoadPins {
            channel_id: 100.into(),
            more: false,
        })
        .unwrap();
    seen.until(&controller, |s| {
        s.pins.as_ref().is_some_and(|p| p.fetched && !p.loading)
    })
    .await;
    assert_eq!(
        seen.pins.as_ref().unwrap().items[0].message.author.username,
        "fixture"
    );
    assert!(
        !seen.state.messages[&100.into()].contains_key(&300.into()),
        "pin previews are detached from the visible timeline"
    );
    controller
        .send(Command::LoadPins {
            channel_id: 100.into(),
            more: true,
        })
        .unwrap();
    seen.until(&controller, |s| {
        s.pins
            .as_ref()
            .is_some_and(|p| !p.loading && p.items.len() == 2)
    })
    .await;
    assert!(!seen.pins.as_ref().unwrap().has_more);
    controller
        .send(Command::SetMessagePinned {
            channel_id: 100.into(),
            message_id: 300.into(),
            pinned: false,
        })
        .unwrap();
    seen.until(&controller, |s| {
        s.pin_done
            .iter()
            .any(|(id, _, error)| *id == 300.into() && error.is_some())
    })
    .await;
    assert_eq!(
        seen.pins.as_ref().unwrap().items.len(),
        2,
        "a rejected unpin must leave the item visible"
    );
    controller
        .send(Command::SetMessagePinned {
            channel_id: 100.into(),
            message_id: 1000.into(),
            pinned: true,
        })
        .unwrap();
    seen.until(&controller, |s| {
        s.pin_done
            .iter()
            .any(|(id, pinned, error)| *id == 1000.into() && *pinned && error.is_none())
    })
    .await;
    fixture.dispatch.send(json!({"op":0,"t":"MESSAGE_UPDATE","s":2,"d":{"id":"1000","channel_id":"100","pinned":true}})).unwrap();
    seen.until(&controller, |s| {
        s.state.messages[&100.into()][&1000.into()].pinned
    })
    .await;
    assert_eq!(
        seen.state.read_states[&100.into()].last_message_id,
        Some(50.into())
    );
    assert!(
        !fixture
            .requests
            .lock()
            .await
            .iter()
            .any(|r| r.path.contains("read-states/ack"))
    );
}

#[tokio::test]
async fn explicit_read_unread_and_notifications_use_wire_contract() {
    let fixture = Fixture::new().await;
    let (controller, mut seen) = fixture.controller().await;
    fixture
        .dispatch
        .send(json!({"op":0,"t":"MESSAGE_CREATE","s":2,"d":message(999,100)}))
        .unwrap();
    seen.until(&controller, |s| {
        s.state
            .messages
            .get(&100.into())
            .is_some_and(|m| m.contains_key(&999.into()))
    })
    .await;
    controller
        .send(Command::MarkMessageUnread {
            channel_id: 100.into(),
            message_id: 1000.into(),
        })
        .unwrap();
    seen.until(&controller, |s| {
        s.state
            .read_states
            .get(&100.into())
            .is_some_and(|r| r.manual_ack && r.version.as_deref() == Some("10"))
    })
    .await;
    assert_eq!(
        seen.state.read_states[&100.into()].last_message_id,
        Some(999.into())
    );
    assert_eq!(seen.state.read_states[&100.into()].mention_count, 4);
    controller
        .send(Command::MarkChannelRead {
            channel_id: 100.into(),
        })
        .unwrap();
    seen.until(&controller, |s| {
        s.state
            .read_states
            .get(&100.into())
            .is_some_and(|r| !r.manual_ack && r.last_message_id == Some(1000.into()))
    })
    .await;
    controller
        .send(Command::SetChannelNotificationSettings {
            guild_id: Some(10.into()),
            channel_id: 100.into(),
            muted: Some(true),
            mute_seconds: None,
            message_notifications: Some(2),
        })
        .unwrap();
    seen.until(&controller, |s| {
        s.state
            .guild_notification_settings(Some(10.into()))
            .channel_overrides
            .get(&100.into())
            .is_some_and(|row| row.muted)
    })
    .await;
    let requests = fixture.requests.lock().await;
    let acks: Vec<_> = requests
        .iter()
        .filter(|r| r.path == "/api/v1/read-states/ack")
        .collect();
    assert_eq!(acks.len(), 2);
    assert_eq!(
        acks[0].body,
        json!({"read_states":[{"channel_id":"100","message_id":"999","manual":true,"mention_count":0}]})
    );
    assert_eq!(
        acks[1].body,
        json!({"read_states":[{"channel_id":"100","message_id":"1000"}]})
    );
    let settings = requests
        .iter()
        .find(|r| r.path == "/api/v1/users/@me/guilds/10/settings")
        .unwrap();
    assert_eq!(settings.method, "PATCH");
    assert_eq!(
        settings.body,
        json!({"channel_overrides":{"100":{"collapsed":false,"message_notifications":2,"muted":true,"mute_config":null}}})
    );
}
