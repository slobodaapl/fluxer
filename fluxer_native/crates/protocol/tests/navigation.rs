// SPDX-License-Identifier: AGPL-3.0-or-later
//! Synthetic loopback fixtures for existing search/pin/history requests.
use fluxer_protocol::{Client, Error, Instance, SessionToken};
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::mpsc,
    time::timeout,
};
#[derive(Debug)]
struct Request {
    line: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}
struct Reply {
    status: u16,
    body: Option<Value>,
    headers: Vec<(&'static str, &'static str)>,
}
impl Reply {
    fn json(status: u16, body: Value) -> Self {
        Self {
            status,
            body: Some(body),
            headers: vec![],
        }
    }
    fn empty() -> Self {
        Self {
            status: 204,
            body: None,
            headers: vec![],
        }
    }
}
struct Server {
    origin: String,
    requests: mpsc::UnboundedReceiver<Request>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Server {
    async fn new(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let (tx, requests) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            for reply in replies {
                let (mut socket, _) = listener.accept().await.unwrap();
                let request = read_request(&mut socket).await;
                tx.send(request).unwrap();
                let body = reply.body.map(|b| b.to_string()).unwrap_or_default();
                let headers = reply
                    .headers
                    .iter()
                    .map(|(k, v)| format!("{k}: {v}\r\n"))
                    .collect::<String>();
                let response = format!(
                    "HTTP/1.1 {} Fixture\r\nContent-Length: {}\r\nContent-Type: application/json\r\n{headers}Connection: close\r\n\r\n{body}",
                    reply.status,
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        Self {
            origin,
            requests,
            task,
        }
    }
    async fn next(&mut self) -> Request {
        timeout(Duration::from_secs(5), self.requests.recv())
            .await
            .unwrap()
            .unwrap()
    }
    async fn none(&mut self) {
        assert!(matches!(
            timeout(Duration::from_millis(80), self.requests.recv()).await,
            Err(_) | Ok(None)
        ));
    }
}
async fn read_request(socket: &mut tokio::net::TcpStream) -> Request {
    let mut raw = Vec::new();
    let mut buffer = [0; 4096];
    let (end, length) = loop {
        let n = socket.read(&mut buffer).await.unwrap();
        assert!(n > 0);
        raw.extend_from_slice(&buffer[..n]);
        if let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
            let text = String::from_utf8_lossy(&raw[..end]);
            let length = text
                .lines()
                .find_map(|line| {
                    let (k, v) = line.split_once(':')?;
                    k.eq_ignore_ascii_case("content-length")
                        .then(|| v.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            break (end, length);
        }
    };
    while raw.len() < end + 4 + length {
        let n = socket.read(&mut buffer).await.unwrap();
        assert!(n > 0);
        raw.extend_from_slice(&buffer[..n]);
    }
    let head = String::from_utf8_lossy(&raw[..end]);
    let mut lines = head.lines();
    let line = lines.next().unwrap().into();
    let headers = lines
        .map(|line| {
            let (k, v) = line.split_once(':').unwrap();
            (k.to_ascii_lowercase(), v.trim().into())
        })
        .collect();
    Request {
        line,
        headers,
        body: raw[end + 4..end + 4 + length].to_vec(),
    }
}
fn client(origin: &str) -> Client {
    let mut instance =
        Instance::from_endpoints(&format!("{origin}/api"), "ws://127.0.0.1/unused").unwrap();
    instance.web_app = "https://webapp.example.test".parse().unwrap();
    Client::new(instance)
        .unwrap()
        .with_token(SessionToken::new("synthetic-session").unwrap())
}
fn assert_api(request: &Request, method: &str) {
    assert!(request.line.starts_with(method));
    assert_eq!(request.headers["authorization"], "synthetic-session");
    for key in [
        "x-fluxer-features",
        "x-fluxer-capabilities",
        "x-fluxer-client-installation-id",
    ] {
        assert!(request.headers.contains_key(key));
    }
    if method == "GET" {
        assert!(!request.headers.contains_key("origin"));
    } else {
        assert_eq!(request.headers["origin"], "https://webapp.example.test");
    }
}
fn query(request: &Request) -> BTreeMap<String, String> {
    let path = request.line.split_whitespace().nth(1).unwrap();
    let url = fluxer_protocol::Url::parse(&format!("http://fixture.test{path}")).unwrap();
    url.query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

#[tokio::test]
async fn search_preserves_api_filters_indexing_status_and_complete_metadata_without_polling() {
    let complete = json!({"messages":[{"id":"7","channel_id":"1"}],"channels":[{"id":"1"}],"threads":[{"id":"9"}],"members":[{"user_id":"2"}],"total":1,"hits_per_page":25,"page":2});
    let mut indexing = Reply::json(202, json!({"indexing":true,"retry_after":99}));
    indexing.headers.push(("Retry-After", "4"));
    let mut server = Server::new(vec![
        indexing,
        Reply::json(200, json!({"indexing":true})),
        Reply::json(200, complete.clone()),
    ])
    .await;
    let client = client(&server.origin);
    // Source toApiParams retains false and empty arrays; context and channel_ids
    // coexist with the legacy channel_id filter field.
    let params = json!({"context_channel_id":"1","context_guild_id":"3","channel_id":["1","4"],"channel_ids":["1","4"],"exclude_author_id":["5"],"exact_phrases":["hello world"],"mention_everyone":false,"has":[],"include_nsfw":false,"scope":"current","sort_by":"timestamp","sort_order":"desc","hits_per_page":25,"page":2});
    let first = client.search_messages(&params).await.unwrap();
    assert_eq!(first.status, 202);
    assert!(first.is_indexing());
    assert_eq!(first.retry_after, Some(Duration::from_secs(4)));
    assert_eq!(first.body["retry_after"], 99);
    let request = server.next().await;
    assert_api(&request, "POST");
    assert_eq!(request.line, "POST /api/v1/search/messages HTTP/1.1");
    assert_eq!(
        serde_json::from_slice::<Value>(&request.body).unwrap(),
        params
    );
    server.none().await;
    let second = client.search_messages(&params).await.unwrap();
    assert_eq!(second.status, 200);
    assert!(second.is_indexing());
    server.next().await;
    let result = client.search_messages(&params).await.unwrap();
    assert_eq!(result.body, complete);
    assert!(!result.is_indexing());
    server.next().await;
    server.none().await;
}

#[tokio::test]
async fn pins_use_timestamp_pages_exact_mutation_routes_and_explicit_bodyless_ack() {
    let first = json!({"items":[{"message":{"id":"2"},"pinned_at":"2026-11-12T01:02:03.456Z"}],"has_more":true});
    let second = json!({"items":[],"has_more":false});
    let mut server = Server::new(vec![
        Reply::json(200, first.clone()),
        Reply::json(200, second.clone()),
        Reply::empty(),
        Reply::empty(),
        Reply::empty(),
    ])
    .await;
    let client = client(&server.origin);
    assert_eq!(client.pins("1").await.unwrap(), first);
    let request = server.next().await;
    assert_api(&request, "GET");
    assert_eq!(
        query(&request),
        BTreeMap::from([("limit".into(), "25".into())])
    );
    assert_eq!(
        client
            .pins_page("1", Some("2026-11-12T01:02:03.456Z"))
            .await
            .unwrap(),
        second
    );
    let request = server.next().await;
    assert_eq!(
        query(&request),
        BTreeMap::from([
            ("limit".into(), "25".into()),
            ("before".into(), "2026-11-12T01:02:03.456Z".into())
        ])
    );
    // No acknowledgement was synthesized by fetching either page.
    server.none().await;
    client.pin_message("1", "2").await.unwrap();
    client.unpin_message("1", "2").await.unwrap();
    client.acknowledge_pins("1").await.unwrap();
    for (method, path) in [
        ("PUT", "/api/v1/channels/1/pins/2"),
        ("DELETE", "/api/v1/channels/1/pins/2"),
        ("POST", "/api/v1/channels/1/pins/ack"),
    ] {
        let request = server.next().await;
        assert_api(&request, method);
        assert_eq!(request.line, format!("{method} {path} HTTP/1.1"));
        assert!(request.body.is_empty());
        assert!(!request.headers.contains_key("content-type"));
    }
    server.none().await;
}

#[tokio::test]
async fn history_navigation_sends_before_after_around_and_uses_two_source_retries() {
    let mut server = Server::new(vec![
        Reply::json(502, json!({})),
        Reply::json(504, json!({})),
        Reply::json(200, json!([{"id":"4"}])),
        Reply::empty(),
    ])
    .await;
    let client = client(&server.origin);
    assert_eq!(
        client
            .messages_page("1", Some("2"), Some("3"), Some("4"), 50)
            .await
            .unwrap(),
        json!([{"id":"4"}])
    );
    for _ in 0..3 {
        let request = server.next().await;
        assert_api(&request, "GET");
        assert_eq!(
            query(&request),
            BTreeMap::from([
                ("limit".into(), "50".into()),
                ("before".into(), "2".into()),
                ("after".into(), "3".into()),
                ("around".into(), "4".into())
            ])
        );
        assert!(request.body.is_empty());
    }
    assert_eq!(
        client
            .messages_page("1", None, None, None, 100)
            .await
            .unwrap(),
        json!([])
    );
    let request = server.next().await;
    assert_eq!(
        query(&request),
        BTreeMap::from([("limit".into(), "100".into())])
    );
    server.none().await;
}

#[tokio::test]
async fn strict_errors_and_retry_exhaustion_do_not_invent_search_pin_or_history_fallbacks() {
    let mut limited = Reply::json(429, json!({"retry_after":99,"global":true}));
    limited.headers.push(("X-RateLimit-Reset-After", "2500"));
    let mut server = Server::new(vec![
        Reply::json(403, json!({"code":"SEARCH_NOT_PERMITTED"})),
        limited,
        Reply::json(503, json!({"code":"TEMPORARY"})),
        Reply::json(502, json!({})),
        Reply::json(502, json!({})),
        Reply::json(502, json!({})),
        Reply::empty(),
    ])
    .await;
    let client = client(&server.origin);
    assert!(matches!(
        client.search_messages(&json!({"page":1})).await,
        Err(Error::Api { status: 403, .. })
    ));
    server.next().await;
    server.none().await;
    assert!(
        matches!(client.pins_page("1",None).await,Err(Error::RateLimited{retry_after,global:true})if retry_after==Duration::from_millis(2500))
    );
    server.next().await;
    server.none().await;
    assert!(matches!(
        client.messages("1", None, 50).await,
        Err(Error::Api { status: 503, .. })
    ));
    server.next().await;
    server.none().await;
    assert!(matches!(
        client.messages("1", None, 50).await,
        Err(Error::Api { status: 502, .. })
    ));
    for _ in 0..3 {
        server.next().await;
    }
    server.none().await;
    assert!(matches!(
        client.search_messages(&json!({})).await,
        Err(Error::InvalidSearchResponse)
    ));
    server.next().await;
    server.none().await;
}

#[tokio::test]
async fn dropping_background_search_cancels_a_stalled_response_body() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let (tx, ready) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let request = read_request(&mut socket).await;
        assert_api(&request, "POST");
        socket
            .write_all(
                b"HTTP/1.1 200 Fixture\r\nContent-Length: 10000\r\nConnection: close\r\n\r\n{",
            )
            .await
            .unwrap();
        tx.send(()).unwrap();
        let mut one = [0];
        assert_eq!(
            timeout(Duration::from_secs(2), socket.read(&mut one))
                .await
                .unwrap()
                .unwrap(),
            0
        );
    });
    let client = client(&origin);
    let task = tokio::spawn(async move { client.search_messages(&json!({"page":1})).await });
    ready.await.unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    server.await.unwrap();
}

#[tokio::test]
async fn slowmode_snapshot_uses_exact_authenticated_source_get() {
    let body = json!({"rate_limit_per_user":15,"retry_after_ms":2500,"can_bypass":false});
    let mut server = Server::new(vec![Reply::json(200, body.clone())]).await;
    let client = client(&server.origin);
    assert_eq!(client.channel_slowmode("11").await.unwrap(), body);
    let request = server.next().await;
    assert_eq!(request.line, "GET /api/v1/channels/11/slowmode HTTP/1.1");
    assert!(request.body.is_empty());
    assert!(request.headers.contains_key("authorization"));
}

#[tokio::test]
async fn mentions_use_source_filters_limit_and_before_cursor_without_mutating_reads() {
    let mut server = Server::new(vec![
        Reply::json(200, json!([])),
        Reply::json(200, json!([])),
    ])
    .await;
    let client = client(&server.origin);
    client
        .recent_mentions(None, true, true, true)
        .await
        .unwrap();
    client
        .recent_mentions(Some("123"), false, true, false)
        .await
        .unwrap();
    let first = server.next().await;
    assert_eq!(
        first.line,
        "GET /api/v1/users/@me/mentions?everyone=true&roles=true&guilds=true&limit=50 HTTP/1.1"
    );
    assert!(first.body.is_empty());
    assert_eq!(first.headers["authorization"], "synthetic-session");
    let older = server.next().await;
    assert_eq!(
        older.line,
        "GET /api/v1/users/@me/mentions?everyone=false&roles=true&guilds=false&limit=50&before=123 HTTP/1.1"
    );
    assert!(older.body.is_empty());
}

#[tokio::test]
async fn standalone_threads_use_text_and_announcement_types_with_session_capability() {
    let mut server = Server::new(vec![
        Reply::json(201, json!({"id":"51","type":11,"parent_id":"11"})),
        Reply::json(201, json!({"id":"52","type":10,"parent_id":"12"})),
    ])
    .await;
    let client = client(&server.origin);
    for (parent, id, announcement, kind) in [("11", "51", false, 11), ("12", "52", true, 10)] {
        let thread = client
            .create_public_thread(parent, "Planning", announcement)
            .await
            .unwrap();
        assert_eq!(thread["id"], id);
        let request = server.next().await;
        assert_api(&request, "POST");
        assert_eq!(
            request.line,
            format!("POST /api/v1/channels/{parent}/threads HTTP/1.1")
        );
        assert!(
            request.headers["x-fluxer-features"]
                .split(',')
                .any(|f| f == "channel_threads")
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&request.body).unwrap(),
            json!({"name":"Planning","type":kind})
        );
    }
}

#[tokio::test]
async fn private_and_message_threads_use_classic_routes_and_payloads() {
    let mut server = Server::new(vec![
        Reply::json(201, json!({})),
        Reply::json(201, json!({})),
    ])
    .await;
    let client = client(&server.origin);
    client
        .create_thread("11", "Private", 12, None, Some(1440))
        .await
        .unwrap();
    client
        .create_thread("11", "From message", 11, Some("42"), None)
        .await
        .unwrap();
    let private = server.next().await;
    assert_api(&private, "POST");
    assert_eq!(private.line, "POST /api/v1/channels/11/threads HTTP/1.1");
    assert_eq!(
        serde_json::from_slice::<Value>(&private.body).unwrap(),
        json!({"name":"Private","type":12,"auto_archive_duration":1440})
    );
    let message = server.next().await;
    assert_api(&message, "POST");
    assert_eq!(
        message.line,
        "POST /api/v1/channels/11/messages/42/threads HTTP/1.1"
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&message.body).unwrap(),
        json!({"name":"From message"})
    );
}

#[tokio::test]
async fn account_sessions_list_uses_authenticated_source_endpoint() {
    let sessions = json!([
        {"id_hash":"current", "current":true, "masked_ip":"192.0.2.xxx", "approx_last_used_at":"2026-11-19T00:00:00Z", "client_info":{"os":"Linux", "device":"desktop"}},
        {"id_hash":"other", "current":false, "masked_ip":null, "client_info":null}
    ]);
    let mut server = Server::new(vec![Reply::json(200, sessions.clone())]).await;
    assert_eq!(
        client(&server.origin).auth_sessions().await.unwrap(),
        sessions
    );
    let request = server.next().await;
    assert_eq!(request.line, "GET /api/v1/auth/sessions HTTP/1.1");
    assert_eq!(
        request.headers.get("authorization").unwrap(),
        "synthetic-session"
    );
    assert!(request.body.is_empty());
    server.none().await;
}

#[tokio::test]
async fn role_overwrite_uses_decimal_masks_and_single_target_route() {
    let mut server = Server::new(vec![Reply::empty()]).await;
    let client = client(&server.origin);
    let overwrite = fluxer_core::PermissionOverwrite {
        id: 42.into(),
        kind: 0,
        allow: fluxer_core::Permissions::VIEW_CHANNEL,
        deny: fluxer_core::Permissions::SEND_MESSAGES,
    };
    client
        .set_channel_overwrite("12", &overwrite)
        .await
        .unwrap();
    let request = server.next().await;
    assert_eq!(
        request.line,
        "PUT /api/v1/channels/12/permissions/42 HTTP/1.1"
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&request.body).unwrap(),
        json!({"type":0,"allow":"1024","deny":"2048"})
    );
}

#[tokio::test]
async fn connections_list_and_visibility_use_authenticated_endpoints() {
    let connections = json!([{ "id": "example.org", "type": "domain", "name": "example.org", "verified": true, "visibility_flags": 3, "sort_order": 0 }]);
    let mut server = Server::new(vec![Reply::json(200, connections.clone()), Reply::empty()]).await;
    let client = client(&server.origin);
    assert_eq!(client.connections().await.unwrap(), connections);
    client
        .update_connection_visibility("domain", "example.org", 5)
        .await
        .unwrap();
    let list = server.next().await;
    assert_eq!(list.line, "GET /api/v1/users/@me/connections HTTP/1.1");
    assert_eq!(
        list.headers.get("authorization").unwrap(),
        "synthetic-session"
    );
    let update = server.next().await;
    assert_eq!(
        update.line,
        "PATCH /api/v1/users/@me/connections/domain/example%2Eorg HTTP/1.1"
    );
    assert_eq!(
        update.headers.get("authorization").unwrap(),
        "synthetic-session"
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&update.body).unwrap(),
        json!({"visibility_flags": 5})
    );
}
