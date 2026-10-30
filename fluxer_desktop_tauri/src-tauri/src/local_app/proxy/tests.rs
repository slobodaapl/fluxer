// SPDX-License-Identifier: AGPL-3.0-or-later

//! The proxy routes end to end through `LocalApp::handle`, against a raw HTTP/1.1 upstream on
//! loopback registered the way discovery registers an instance.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use flate2::Compression;
use flate2::write::GzEncoder;
use tauri::http::{Method, Request, Response};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::admission::PROTOCOL_AUTHORIZATION_HEADER;
use super::budget::BufferBudget;
use super::upload_progress::test_support::{Recorded, recorder};
use super::upload_progress::{UploadProgress, UploadProgressHub};
use super::{Proxy, route, take_held_buffer};
use crate::config::DesktopConfig;
use crate::documents::DocumentRegistry;
use crate::local_app::LocalApp;
use crate::local_app::url::origin;
use crate::runtime::RuntimeConfig;
use crate::runtime::plans::{LocalAppRuntimePlan, project, trusted_http_origins};
use crate::runtime::test_support::{
    MemoryMarkers, TOKEN_A, TOKEN_B, caller, loaded_documents, loopback_plan, outbound,
};
use crate::window::MAIN_WINDOW_LABEL;

#[derive(Clone, Debug)]
struct Captured {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Captured {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// Records each request (head and de-chunked body) and answers with a canned raw response.
#[derive(Clone)]
struct Upstream {
    port: u16,
    replies: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    captured: Arc<Mutex<Vec<Captured>>>,
}

fn raw_reply(status: u16, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let mut reply = format!("HTTP/1.1 {status} Test\r\nConnection: close\r\n");
    for (name, value) in headers {
        reply.push_str(&format!("{name}: {value}\r\n"));
    }
    if !headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("content-length"))
    {
        reply.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    reply.push_str("\r\n");
    let mut bytes = reply.into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

impl Upstream {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream = Self {
            port: listener.local_addr().unwrap().port(),
            replies: Arc::default(),
            captured: Arc::default(),
        };
        let serving = upstream.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(serving.clone().serve(stream));
            }
        });
        upstream
    }

    fn base(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn reply(&self, target: &str, reply: Vec<u8>) {
        self.replies
            .lock()
            .unwrap()
            .insert(target.to_owned(), reply);
    }

    fn captured(&self) -> Vec<Captured> {
        self.captured.lock().unwrap().clone()
    }

    async fn serve(self, mut stream: TcpStream) {
        let mut buffered = Vec::new();
        let head_end = loop {
            if let Some(end) = find(&buffered, b"\r\n\r\n") {
                break end;
            }
            if !read_more(&mut stream, &mut buffered).await {
                return;
            }
        };
        let head = String::from_utf8_lossy(&buffered[..head_end]).into_owned();
        let mut rest = buffered[head_end + 4..].to_vec();
        let mut lines = head.split("\r\n");
        let mut request_line = lines.next().unwrap_or_default().split(' ');
        let method = request_line.next().unwrap_or_default().to_owned();
        let target = request_line.next().unwrap_or_default().to_owned();
        let headers: Vec<(String, String)> = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.trim().to_lowercase(), value.trim().to_owned()))
            .collect();
        let header = |name: &str| {
            headers
                .iter()
                .find(|(candidate, _)| candidate == name)
                .map(|(_, value)| value.clone())
        };
        let body = if header("transfer-encoding").is_some_and(|value| value == "chunked") {
            let Some(body) = read_chunked(&mut stream, &mut rest).await else {
                return;
            };
            body
        } else {
            let length: usize = header("content-length").map_or(0, |value| value.parse().unwrap());
            while rest.len() < length {
                if !read_more(&mut stream, &mut rest).await {
                    return;
                }
            }
            rest
        };
        self.captured.lock().unwrap().push(Captured {
            method,
            target: target.clone(),
            headers,
            body,
        });
        let reply = self
            .replies
            .lock()
            .unwrap()
            .get(&target)
            .cloned()
            .unwrap_or_else(|| raw_reply(404, &[], b"unrouted"));
        let _ = stream.write_all(&reply).await;
        let _ = stream.shutdown().await;
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

async fn read_more(stream: &mut TcpStream, buffered: &mut Vec<u8>) -> bool {
    let mut chunk = [0u8; 16 * 1024];
    match stream.read(&mut chunk).await {
        Ok(0) | Err(_) => false,
        Ok(read) => {
            buffered.extend_from_slice(&chunk[..read]);
            true
        }
    }
}

async fn read_chunked(stream: &mut TcpStream, buffered: &mut Vec<u8>) -> Option<Vec<u8>> {
    let mut body = Vec::new();
    loop {
        let line_end = loop {
            if let Some(end) = find(buffered, b"\r\n") {
                break end;
            }
            if !read_more(stream, buffered).await {
                return None;
            }
        };
        let size_line = String::from_utf8_lossy(&buffered[..line_end]).into_owned();
        let size = usize::from_str_radix(size_line.split(';').next()?.trim(), 16).ok()?;
        while buffered.len() < line_end + 2 + size + 2 {
            if !read_more(stream, buffered).await {
                return None;
            }
        }
        body.extend_from_slice(&buffered[line_end + 2..line_end + 2 + size]);
        buffered.drain(..line_end + 2 + size + 2);
        if size == 0 {
            return Some(body);
        }
    }
}

struct Harness {
    app: LocalApp,
    runtime: Arc<RuntimeConfig>,
    documents: Arc<DocumentRegistry>,
    events: Recorded,
    _config_dir: tempfile::TempDir,
}

impl Harness {
    fn new() -> Self {
        let documents = loaded_documents();
        let runtime = Arc::new(RuntimeConfig::new(
            outbound(),
            Arc::new(MemoryMarkers::default()).markers(),
            documents.clone(),
        ));
        let config_dir = tempfile::tempdir().unwrap();
        let (emit, events) = recorder();
        let app = LocalApp::new(
            PathBuf::from("/nonexistent-renderer"),
            Arc::new(DesktopConfig::load(config_dir.path(), &[])),
            runtime.clone(),
            emit,
        );
        Self {
            app,
            runtime,
            documents,
            events,
            _config_dir: config_dir,
        }
    }

    /// Registers the plan's origins (IP literals) and caches it, as discovery would.
    async fn cache(&self, plan: LocalAppRuntimePlan) -> Arc<LocalAppRuntimePlan> {
        let plan = Arc::new(plan);
        self.runtime
            .outbound()
            .register_anchored_origins("http://127.0.0.1:1", &trusted_http_origins(&plan), None)
            .await
            .unwrap();
        self.runtime.plans().cache(plan.clone()).unwrap();
        plan
    }

    fn subscribe(&self) {
        self.app
            .upload_progress()
            .subscribe(&self.documents, &caller(TOKEN_A), MAIN_WINDOW_LABEL);
    }

    fn events(&self) -> Vec<UploadProgress> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .map(|(label, progress)| {
                assert_eq!(label, MAIN_WINDOW_LABEL);
                progress.clone()
            })
            .collect()
    }

    async fn send(&self, request: Request<Vec<u8>>) -> Response<Vec<u8>> {
        self.app.handle(MAIN_WINDOW_LABEL, request).await
    }

    /// A proxy over the harness runtime with its own response buffer budget.
    fn budgeted_proxy(&self, budget: &BufferBudget) -> Arc<Proxy> {
        let (emit, _) = recorder();
        Arc::new(Proxy::with_budget(
            self.runtime.clone(),
            Arc::new(UploadProgressHub::new(emit)),
            budget.clone(),
        ))
    }
}

async fn send_to(proxy: &Proxy, request: Request<Vec<u8>>) -> Response<Vec<u8>> {
    let route = route(&request).expect("a proxy route");
    proxy.handle(MAIN_WINDOW_LABEL, request, route).await
}

/// `{origin}/api/<encoded key>` as the renderer is handed it.
fn api_base(plan: &LocalAppRuntimePlan) -> String {
    project(plan).api_endpoint
}

fn proxy_url(plan: &LocalAppRuntimePlan, target: &str) -> String {
    let encoded_key = api_base(plan).rsplit('/').next().unwrap().to_owned();
    let encoded_target: String = url::form_urlencoded::byte_serialize(target.as_bytes()).collect();
    format!("{}/proxy/{encoded_key}?url={encoded_target}", origin())
}

/// A request from the current main-window document, carrying its token as the bridge does.
fn request(method: Method, url: &str, headers: &[(&str, &str)], body: &[u8]) -> Request<Vec<u8>> {
    let mut request = unauthorized_request(method, url, headers, body);
    request.headers_mut().insert(
        PROTOCOL_AUTHORIZATION_HEADER,
        tauri::http::HeaderValue::from_static(TOKEN_A),
    );
    request
}

fn unauthorized_request(
    method: Method,
    url: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Request<Vec<u8>> {
    let mut builder = Request::builder().method(method).uri(url);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    builder.body(body.to_vec()).unwrap()
}

fn same_origin() -> Vec<(&'static str, String)> {
    vec![
        ("sec-fetch-site", "same-origin".to_owned()),
        ("sec-fetch-mode", "cors".to_owned()),
        ("sec-fetch-dest", "empty".to_owned()),
        ("referer", format!("{}/channels/@me", origin())),
    ]
}

fn with_same_origin<'a>(
    extra: &[(&'a str, &'a str)],
    owned: &'a [(&'static str, String)],
) -> Vec<(&'a str, &'a str)> {
    owned
        .iter()
        .map(|(name, value)| (*name, value.as_str()))
        .chain(extra.iter().copied())
        .collect()
}

fn text(response: &Response<Vec<u8>>) -> &str {
    std::str::from_utf8(response.body()).unwrap()
}

fn header<'a>(response: &'a Response<Vec<u8>>, name: &str) -> Option<&'a str> {
    response
        .headers()
        .get(name)
        .map(|value| value.to_str().unwrap())
}

async fn unused_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    listener.local_addr().unwrap().port()
}

#[tokio::test]
async fn api_get_passes_json_through_under_the_header_policy() {
    let harness = Harness::new();
    let upstream = Upstream::start().await;
    let plan = harness.cache(loopback_plan(upstream.port)).await;
    upstream.reply(
        "/api/v1/users/@me?with=1",
        raw_reply(
            200,
            &[
                ("Content-Type", "application/json"),
                ("Set-Cookie", "session=1"),
                ("Access-Control-Allow-Origin", "*"),
                ("Content-Security-Policy", "default-src *"),
                ("X-RateLimit-Remaining", "4"),
            ],
            br#"{"id":"1"}"#,
        ),
    );
    let owned = same_origin();
    let headers = with_same_origin(
        &[
            ("authorization", "token"),
            ("cookie", "a=b"),
            ("x-custom", "1"),
            ("accept-encoding", "zstd"),
        ],
        &owned,
    );
    let response = harness
        .send(request(
            Method::GET,
            &format!("{}/v1/users/@me?with=1", api_base(&plan)),
            &headers,
            b"",
        ))
        .await;

    assert_eq!(response.status(), 200);
    assert_eq!(response.body(), br#"{"id":"1"}"#);
    assert_eq!(header(&response, "content-type"), Some("application/json"));
    assert_eq!(header(&response, "x-ratelimit-remaining"), Some("4"));
    assert_eq!(header(&response, "cache-control"), Some("no-store"));
    assert_eq!(
        header(&response, "content-security-policy"),
        Some("sandbox; default-src 'none'; frame-ancestors 'none'")
    );
    assert_eq!(header(&response, "x-content-type-options"), Some("nosniff"));
    for dropped in [
        "set-cookie",
        "access-control-allow-origin",
        "content-length",
        "connection",
    ] {
        assert_eq!(header(&response, dropped), None, "{dropped}");
    }

    let [captured] = upstream.captured().try_into().unwrap();
    assert_eq!(captured.method, "GET");
    assert_eq!(captured.target, "/api/v1/users/@me?with=1");
    assert_eq!(captured.header("authorization"), Some("token"));
    assert_eq!(captured.header("x-custom"), Some("1"));
    assert_eq!(captured.header("accept-encoding"), Some("gzip, br"));
    for dropped in ["cookie", "referer", "origin"] {
        assert_eq!(captured.header(dropped), None, "{dropped}");
    }
    // Other fetch metadata is forwarded, as the TS blocklist left it.
    assert_eq!(captured.header("sec-fetch-site"), Some("same-origin"));
}

#[tokio::test]
async fn api_post_forwards_the_body_with_the_web_app_origin() {
    let harness = Harness::new();
    let upstream = Upstream::start().await;
    let plan = harness.cache(loopback_plan(upstream.port)).await;
    upstream.reply(
        "/api/v1/channels/1/messages",
        raw_reply(201, &[("Content-Type", "application/json")], b"{}"),
    );
    let owned = same_origin();
    let headers = with_same_origin(
        &[("origin", origin()), ("content-type", "application/json")],
        &owned,
    );
    let response = harness
        .send(request(
            Method::POST,
            &format!("{}/v1/channels/1/messages", api_base(&plan)),
            &headers,
            br#"{"content":"hi"}"#,
        ))
        .await;

    assert_eq!(response.status(), 201);
    let [captured] = upstream.captured().try_into().unwrap();
    assert_eq!(captured.method, "POST");
    assert_eq!(captured.body, br#"{"content":"hi"}"#);
    assert_eq!(captured.header("origin"), Some(upstream.base().as_str()));
    assert_eq!(captured.header("content-type"), Some("application/json"));
}

#[tokio::test]
async fn api_gzip_bodies_are_decoded() {
    let harness = Harness::new();
    let upstream = Upstream::start().await;
    let plan = harness.cache(loopback_plan(upstream.port)).await;
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(b"[1,2,3]").unwrap();
    upstream.reply(
        "/api/v1/x",
        raw_reply(
            200,
            &[
                ("Content-Encoding", "gzip"),
                ("Content-Type", "application/json"),
            ],
            &encoder.finish().unwrap(),
        ),
    );
    let response = harness
        .send(request(
            Method::GET,
            &format!("{}/v1/x", api_base(&plan)),
            &[],
            b"",
        ))
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.body(), b"[1,2,3]");
    assert_eq!(header(&response, "content-encoding"), None);
}

#[tokio::test]
async fn api_unsupported_encodings_fail_with_502() {
    let harness = Harness::new();
    let upstream = Upstream::start().await;
    let plan = harness.cache(loopback_plan(upstream.port)).await;
    upstream.reply(
        "/api/v1/x",
        raw_reply(200, &[("Content-Encoding", "zstd")], b"xx"),
    );
    let response = harness
        .send(request(
            Method::GET,
            &format!("{}/v1/x", api_base(&plan)),
            &[],
            b"",
        ))
        .await;
    assert_eq!(response.status(), 502);
    assert_eq!(text(&response), "Network error during API request");
}

#[tokio::test]
async fn api_upstream_errors_pass_through() {
    let harness = Harness::new();
    let upstream = Upstream::start().await;
    let plan = harness.cache(loopback_plan(upstream.port)).await;
    upstream.reply(
        "/api/v1/x",
        raw_reply(500, &[("Content-Type", "text/plain")], b"boom"),
    );
    let response = harness
        .send(request(
            Method::GET,
            &format!("{}/v1/x", api_base(&plan)),
            &[],
            b"",
        ))
        .await;
    assert_eq!(response.status(), 500);
    assert_eq!(text(&response), "boom");
    assert_eq!(header(&response, "cache-control"), Some("no-store"));
}

#[tokio::test]
async fn api_head_has_no_body() {
    let harness = Harness::new();
    let upstream = Upstream::start().await;
    let plan = harness.cache(loopback_plan(upstream.port)).await;
    upstream.reply("/api/v1/x", raw_reply(200, &[("Content-Length", "4")], b""));
    let response = harness
        .send(request(
            Method::HEAD,
            &format!("{}/v1/x", api_base(&plan)),
            &[],
            b"",
        ))
        .await;
    assert_eq!(response.status(), 200);
    assert!(response.body().is_empty());
    assert_eq!(header(&response, "content-length"), None);
}

#[tokio::test]
async fn unknown_runtimes_are_not_found() {
    let harness = Harness::new();
    let response = harness
        .send(request(
            Method::GET,
            &format!("{}/api/nope/v1/x", origin()),
            &[],
            b"",
        ))
        .await;
    assert_eq!(response.status(), 404);
    assert_eq!(text(&response), "Unknown local app API runtime");
    assert_eq!(
        header(&response, "content-type"),
        Some("text/plain; charset=utf-8")
    );

    let response = harness
        .send(request(
            Method::GET,
            &format!("{}/proxy/nope?url=http%3A%2F%2F127.0.0.1%2F", origin()),
            &[],
            b"",
        ))
        .await;
    assert_eq!(response.status(), 404);
    assert_eq!(text(&response), "Unknown local app resource runtime");
}

#[tokio::test]
async fn non_http_api_endpoints_are_unavailable() {
    let harness = Harness::new();
    let mut plan = loopback_plan(5500);
    plan.instance_key = "ftp-instance".to_owned();
    plan.endpoints.api = "ftp://127.0.0.1:5500/api".to_owned();
    harness.cache(plan).await;
    let response = harness
        .send(request(
            Method::GET,
            &format!("{}/api/ftp-instance/v1/x", origin()),
            &[],
            b"",
        ))
        .await;
    assert_eq!(response.status(), 503);
    assert_eq!(text(&response), "No server is selected for this request.");
    assert_eq!(header(&response, "cache-control"), Some("no-store"));
}

#[tokio::test]
async fn unreachable_upstreams_fail_with_502_and_a_failed_upload() {
    let harness = Harness::new();
    harness.subscribe();
    let plan = harness.cache(loopback_plan(unused_port().await)).await;
    let response = harness
        .send(request(
            Method::POST,
            &format!("{}/v1/x", api_base(&plan)),
            &[("x-fluxer-local-upload-id", "up-1")],
            b"payload",
        ))
        .await;
    assert_eq!(response.status(), 502);
    assert_eq!(text(&response), "Network error during API request");
    assert_eq!(header(&response, "cache-control"), Some("no-store"));
    assert_eq!(
        harness.events().last(),
        Some(&UploadProgress {
            upload_id: "up-1".to_owned(),
            loaded: 0,
            total: None,
            done: true,
            failed: true,
        })
    );
}

#[tokio::test]
async fn uploads_stream_chunked_with_progress_events() {
    let harness = Harness::new();
    harness.subscribe();
    let upstream = Upstream::start().await;
    let plan = harness.cache(loopback_plan(upstream.port)).await;
    upstream.reply(
        "/api/v1/attachments",
        raw_reply(200, &[("Content-Type", "application/json")], b"{}"),
    );
    let body = vec![9u8; 128 * 1024];
    let response = harness
        .send(request(
            Method::POST,
            &format!("{}/v1/attachments", api_base(&plan)),
            &[
                ("x-fluxer-local-upload-id", "up-1"),
                ("content-length", "131072"),
            ],
            &body,
        ))
        .await;
    assert_eq!(response.status(), 200);

    let [captured] = upstream.captured().try_into().unwrap();
    assert_eq!(captured.body, body);
    assert_eq!(captured.header("transfer-encoding"), Some("chunked"));
    assert_eq!(captured.header("content-length"), None);
    assert_eq!(captured.header("x-fluxer-local-upload-id"), None);
    let progress = |loaded, done| UploadProgress {
        upload_id: "up-1".to_owned(),
        loaded,
        total: Some(131_072),
        done,
        failed: false,
    };
    assert_eq!(
        harness.events(),
        vec![
            progress(65_536, false),
            progress(131_072, false),
            progress(131_072, true)
        ]
    );
}

fn relay_plan(upstream: &Upstream) -> LocalAppRuntimePlan {
    let mut plan = loopback_plan(upstream.port);
    plan.endpoints.upload_relay = Some(format!("{}/upload", upstream.base()));
    plan
}

#[tokio::test]
async fn relay_puts_reach_the_upload_relay_without_credentials() {
    let harness = Harness::new();
    let upstream = Upstream::start().await;
    let plan = harness.cache(relay_plan(&upstream)).await;
    upstream.reply(
        "/upload/v1/relay/key?t=tok&partNumber=1",
        raw_reply(200, &[("ETag", "\"e\"")], b"ok"),
    );
    let target = format!("{}/upload/v1/relay/key?t=tok&partNumber=1", upstream.base());
    let owned = same_origin();
    let headers = with_same_origin(
        &[("authorization", "token"), ("x-fluxer-sudo-mode-jwt", "j")],
        &owned,
    );
    let response = harness
        .send(request(
            Method::PUT,
            &proxy_url(&plan, &target),
            &headers,
            b"part",
        ))
        .await;

    assert_eq!(response.status(), 200);
    assert_eq!(text(&response), "ok");
    assert_eq!(header(&response, "etag"), Some("\"e\""));
    assert_eq!(header(&response, "content-length"), Some("2"));
    assert_eq!(header(&response, "cache-control"), None);
    let [captured] = upstream.captured().try_into().unwrap();
    assert_eq!(captured.method, "PUT");
    assert_eq!(captured.body, b"part");
    assert_eq!(captured.header("authorization"), None);
    assert_eq!(captured.header("x-fluxer-sudo-mode-jwt"), None);
    assert_eq!(captured.header("origin"), None);
    assert_eq!(captured.header("accept-encoding"), None);
}

#[tokio::test]
async fn relay_puts_outside_the_relay_rules_are_forbidden() {
    let harness = Harness::new();
    let upstream = Upstream::start().await;
    let plan = harness.cache(relay_plan(&upstream)).await;
    for target in [
        format!("{}/upload/v1/relay/key?t=tok&x=1", upstream.base()),
        format!("{}/upload/v1/relay/key", upstream.base()),
        format!("{}/api/v1/x?t=tok", upstream.base()),
    ] {
        let response = harness
            .send(request(
                Method::PUT,
                &proxy_url(&plan, &target),
                &[],
                b"part",
            ))
            .await;
        assert_eq!(response.status(), 403, "{target}");
        assert_eq!(
            text(&response),
            "Remote resource is not allowed for the requested instance"
        );
    }
    assert!(upstream.captured().is_empty());
}

#[tokio::test]
async fn remote_reads_follow_the_plan_origins() {
    let harness = Harness::new();
    let upstream = Upstream::start().await;
    let plan = harness.cache(loopback_plan(upstream.port)).await;
    upstream.reply(
        "/media/a.png",
        raw_reply(
            200,
            &[
                ("Content-Type", "image/png"),
                ("Cache-Control", "max-age=60"),
            ],
            b"png",
        ),
    );
    let target = format!("{}/media/a.png", upstream.base());
    let response = harness
        .send(request(Method::GET, &proxy_url(&plan, &target), &[], b""))
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.body(), b"png");
    assert_eq!(header(&response, "cache-control"), Some("max-age=60"));
    assert_eq!(header(&response, "content-length"), Some("3"));

    let foreign = proxy_url(&plan, "http://127.0.0.2/a.png");
    let response = harness.send(request(Method::GET, &foreign, &[], b"")).await;
    assert_eq!(response.status(), 403);

    let response = harness
        .send(request(Method::POST, &proxy_url(&plan, &target), &[], b""))
        .await;
    assert_eq!(response.status(), 405);
    assert_eq!(header(&response, "allow"), Some("GET, HEAD, PUT"));
    assert_eq!(text(&response), "Method not allowed");

    let missing = format!(
        "{}/proxy/{}",
        origin(),
        api_base(&plan).rsplit('/').next().unwrap()
    );
    let response = harness.send(request(Method::GET, &missing, &[], b"")).await;
    assert_eq!(response.status(), 400);
    assert_eq!(text(&response), "Missing remote resource URL");
    assert_eq!(upstream.captured().len(), 1);
}

#[tokio::test]
async fn unadmitted_requests_never_reach_upstream() {
    let harness = Harness::new();
    let upstream = Upstream::start().await;
    let plan = harness.cache(loopback_plan(upstream.port)).await;
    let url = format!("{}/v1/x", api_base(&plan));
    let rejected = [
        harness
            .app
            .handle("popout", request(Method::GET, &url, &[], b""))
            .await,
        harness
            .send(request(Method::GET, &url, &[("origin", "null")], b""))
            .await,
        harness
            .send(request(
                Method::GET,
                &url,
                &[("sec-fetch-site", "cross-site")],
                b"",
            ))
            .await,
        harness
            .send(request(
                Method::GET,
                &url,
                &[("referer", "https://evil.test/")],
                b"",
            ))
            .await,
        harness
            .send(request(
                Method::GET,
                &url,
                &[("sec-fetch-dest", "iframe")],
                b"",
            ))
            .await,
        harness
            .send(request(
                Method::GET,
                &url,
                &[("upgrade-insecure-requests", "1")],
                b"",
            ))
            .await,
        harness
            .send(request(
                Method::GET,
                &url,
                &[("sec-fetch-dest", "script")],
                b"",
            ))
            .await,
        harness
            .send(request(
                Method::GET,
                &proxy_url(&plan, &format!("{}/media/a.js", upstream.base())),
                &[("sec-fetch-dest", "style")],
                b"",
            ))
            .await,
        harness
            .send(unauthorized_request(Method::GET, &url, &[], b""))
            .await,
        harness
            .send(unauthorized_request(
                Method::POST,
                &url,
                &[(PROTOCOL_AUTHORIZATION_HEADER, TOKEN_B)],
                b"{}",
            ))
            .await,
        harness
            .send(unauthorized_request(
                Method::PUT,
                &proxy_url(&plan, &format!("{}/v1/relay/x?t=1", upstream.base())),
                &[],
                b"part",
            ))
            .await,
    ];
    for response in &rejected {
        assert_eq!(response.status(), 404);
        assert_eq!(text(response), "Not found");
        assert_eq!(
            header(response, "content-type"),
            Some("text/plain; charset=utf-8")
        );
    }
    assert!(upstream.captured().is_empty());
}

#[tokio::test]
async fn remote_reads_need_no_document_token() {
    let harness = Harness::new();
    let upstream = Upstream::start().await;
    let plan = harness.cache(loopback_plan(upstream.port)).await;
    upstream.reply(
        "/media/a.png",
        raw_reply(200, &[("content-type", "image/png")], b"png"),
    );
    let url = proxy_url(&plan, &format!("{}/media/a.png", upstream.base()));
    let response = harness
        .send(unauthorized_request(Method::GET, &url, &[], b""))
        .await;
    assert_eq!(response.status(), 200);
    let captured = upstream.captured();
    assert_eq!(captured.len(), 1);
    assert!(
        !captured[0]
            .headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case(PROTOCOL_AUTHORIZATION_HEADER))
    );
}

#[tokio::test]
async fn the_document_token_never_reaches_the_instance() {
    let harness = Harness::new();
    let upstream = Upstream::start().await;
    let plan = harness.cache(loopback_plan(upstream.port)).await;
    upstream.reply(
        "/api/v1/x",
        raw_reply(200, &[("content-type", "application/json")], b"{}"),
    );
    let response = harness
        .send(request(
            Method::GET,
            &format!("{}/v1/x", api_base(&plan)),
            &[],
            b"",
        ))
        .await;
    assert_eq!(response.status(), 200);
    assert!(
        !upstream.captured()[0]
            .headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case(PROTOCOL_AUTHORIZATION_HEADER))
    );
}

#[tokio::test]
async fn remote_active_content_is_served_inert() {
    let harness = Harness::new();
    let upstream = Upstream::start().await;
    let plan = harness.cache(loopback_plan(upstream.port)).await;
    upstream.reply(
        "/media/a.html",
        raw_reply(
            200,
            &[("Content-Type", "text/html; charset=utf-8")],
            b"<script>1</script>",
        ),
    );
    let target = format!("{}/media/a.html", upstream.base());
    let response = harness
        .send(request(Method::GET, &proxy_url(&plan, &target), &[], b""))
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.body(), b"<script>1</script>");
    assert_eq!(
        header(&response, "content-type"),
        Some("application/octet-stream")
    );
    assert_eq!(header(&response, "x-content-type-options"), Some("nosniff"));
}

const MEDIA_BYTES: usize = 40 * 1024;

fn media_reply(upstream: &Upstream, path: &str) -> String {
    upstream.reply(
        path,
        raw_reply(
            200,
            &[("Content-Type", "image/png")],
            &vec![7u8; MEDIA_BYTES],
        ),
    );
    format!("{}{path}", upstream.base())
}

#[tokio::test]
async fn proxied_bodies_hold_budget_until_handed_over() {
    let harness = Harness::new();
    let upstream = Upstream::start().await;
    let plan = harness.cache(loopback_plan(upstream.port)).await;
    let budget = BufferBudget::new(64 * 1024);
    let proxy = harness.budgeted_proxy(&budget);
    let target = media_reply(&upstream, "/media/a.png");

    let mut response = send_to(
        &proxy,
        request(Method::GET, &proxy_url(&plan, &target), &[], b""),
    )
    .await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.body().len(), MEDIA_BYTES);
    assert_eq!(budget.available(), budget.total() - MEDIA_BYTES);
    let held = take_held_buffer(&mut response).expect("proxied responses carry their budget");
    drop(response);
    assert_eq!(budget.available(), budget.total() - MEDIA_BYTES);
    drop(held);
    assert_eq!(budget.available(), budget.total());

    // A response dropped without being handed over returns its budget too.
    let response = send_to(
        &proxy,
        request(Method::GET, &proxy_url(&plan, &target), &[], b""),
    )
    .await;
    assert_eq!(response.status(), 200);
    drop(response);
    assert_eq!(budget.available(), budget.total());
}

#[tokio::test]
async fn failed_reads_return_their_budget() {
    let harness = Harness::new();
    let upstream = Upstream::start().await;
    let plan = harness.cache(loopback_plan(upstream.port)).await;
    let budget = BufferBudget::new(64 * 1024);
    let proxy = harness.budgeted_proxy(&budget);
    upstream.reply(
        "/media/cut.png",
        raw_reply(200, &[("Content-Length", "100")], b"truncated"),
    );
    let target = format!("{}/media/cut.png", upstream.base());
    let response = send_to(
        &proxy,
        request(Method::GET, &proxy_url(&plan, &target), &[], b""),
    )
    .await;
    assert_eq!(response.status(), 502);
    assert_eq!(text(&response), "Network error during resource request");
    assert_eq!(budget.available(), budget.total());
}

#[tokio::test]
async fn bodies_larger_than_the_whole_budget_fail_with_502() {
    let harness = Harness::new();
    let upstream = Upstream::start().await;
    let plan = harness.cache(loopback_plan(upstream.port)).await;
    let budget = BufferBudget::new(MEDIA_BYTES - 1);
    let proxy = harness.budgeted_proxy(&budget);
    let target = media_reply(&upstream, "/media/big.png");
    let response = send_to(
        &proxy,
        request(Method::GET, &proxy_url(&plan, &target), &[], b""),
    )
    .await;
    assert_eq!(response.status(), 502);
    assert_eq!(text(&response), "Network error during resource request");
    assert_eq!(header(&response, "cache-control"), Some("no-store"));

    upstream.reply(
        "/api/v1/big",
        raw_reply(
            200,
            &[("Content-Type", "application/json")],
            &vec![b'1'; MEDIA_BYTES],
        ),
    );
    let response = send_to(
        &proxy,
        request(
            Method::GET,
            &format!("{}/v1/big", api_base(&plan)),
            &[],
            b"",
        ),
    )
    .await;
    assert_eq!(response.status(), 502);
    assert_eq!(text(&response), "Network error during API request");
    assert_eq!(budget.available(), budget.total());
}

#[tokio::test]
async fn concurrent_bodies_over_the_budget_wait_then_succeed() {
    let harness = Harness::new();
    let upstream = Upstream::start().await;
    let plan = harness.cache(loopback_plan(upstream.port)).await;
    let budget = BufferBudget::new(64 * 1024);
    let proxy = harness.budgeted_proxy(&budget);
    let first_target = media_reply(&upstream, "/media/first.png");
    let second_target = media_reply(&upstream, "/media/second.png");

    let first = send_to(
        &proxy,
        request(Method::GET, &proxy_url(&plan, &first_target), &[], b""),
    )
    .await;
    assert_eq!(first.status(), 200);
    let second = {
        let proxy = proxy.clone();
        let request = request(Method::GET, &proxy_url(&plan, &second_target), &[], b"");
        tokio::spawn(async move { send_to(&proxy, request).await })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!second.is_finished(), "the second body waits for budget");
    assert_eq!(upstream.captured().len(), 2);

    drop(first);
    let second = second.await.unwrap();
    assert_eq!(second.status(), 200);
    assert_eq!(second.body(), &vec![7u8; MEDIA_BYTES]);
    drop(second);
    assert_eq!(budget.available(), budget.total());
}

#[tokio::test]
async fn shutdown_ends_a_wait_for_budget_and_returns_its_bytes() {
    let harness = Harness::new();
    let upstream = Upstream::start().await;
    let plan = harness.cache(loopback_plan(upstream.port)).await;
    let budget = BufferBudget::new(64 * 1024);
    let proxy = harness.budgeted_proxy(&budget);
    let first_target = media_reply(&upstream, "/media/first.png");
    let second_target = media_reply(&upstream, "/media/second.png");

    let first = send_to(
        &proxy,
        request(Method::GET, &proxy_url(&plan, &first_target), &[], b""),
    )
    .await;
    let second = {
        let proxy = proxy.clone();
        let request = request(Method::GET, &proxy_url(&plan, &second_target), &[], b"");
        tokio::spawn(async move { send_to(&proxy, request).await })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!second.is_finished());

    harness.runtime.plans().shutdown();
    let second = second.await.unwrap();
    assert_eq!(second.status(), 502);
    assert_eq!(budget.available(), budget.total() - MEDIA_BYTES);
    drop(first);
    assert_eq!(budget.available(), budget.total());
}

/// A reply of unknown length: `MEDIA_BYTES` sent as 16 KiB chunks.
fn chunked_media_reply(upstream: &Upstream, path: &str) -> String {
    let mut reply = b"HTTP/1.1 200 Test\r\nConnection: close\r\nContent-Type: image/png\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
    for chunk in vec![7u8; MEDIA_BYTES].chunks(16 * 1024) {
        reply.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
        reply.extend_from_slice(chunk);
        reply.extend_from_slice(b"\r\n");
    }
    reply.extend_from_slice(b"0\r\n\r\n");
    upstream.reply(path, reply);
    format!("{}{path}", upstream.base())
}

#[tokio::test]
async fn unknown_length_bodies_over_the_budget_fail_fast_instead_of_stalling() {
    let harness = Harness::new();
    let upstream = Upstream::start().await;
    let plan = harness.cache(loopback_plan(upstream.port)).await;
    let budget = BufferBudget::new(64 * 1024);
    let proxy = harness.budgeted_proxy(&budget);
    let first_target = chunked_media_reply(&upstream, "/media/first.png");
    let second_target = chunked_media_reply(&upstream, "/media/second.png");

    let first = send_to(
        &proxy,
        request(Method::GET, &proxy_url(&plan, &first_target), &[], b""),
    )
    .await;
    assert_eq!(first.status(), 200);
    assert_eq!(first.body(), &vec![7u8; MEDIA_BYTES]);

    let second = tokio::time::timeout(
        Duration::from_secs(5),
        send_to(
            &proxy,
            request(Method::GET, &proxy_url(&plan, &second_target), &[], b""),
        ),
    )
    .await
    .expect("a body that runs out of budget fails at once");
    assert_eq!(second.status(), 502);
    assert_eq!(text(&second), "Network error during resource request");
    assert_eq!(budget.available(), budget.total() - MEDIA_BYTES);
    drop(first);
    assert_eq!(budget.available(), budget.total());
}

#[tokio::test]
async fn decoded_growth_over_the_budget_fails_with_502() {
    let harness = Harness::new();
    let upstream = Upstream::start().await;
    let plan = harness.cache(loopback_plan(upstream.port)).await;
    let budget = BufferBudget::new(64 * 1024);
    let proxy = harness.budgeted_proxy(&budget);
    let media_target = media_reply(&upstream, "/media/first.png");
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&vec![b'1'; MEDIA_BYTES]).unwrap();
    upstream.reply(
        "/api/v1/x",
        raw_reply(
            200,
            &[
                ("Content-Encoding", "gzip"),
                ("Content-Type", "application/json"),
            ],
            &encoder.finish().unwrap(),
        ),
    );

    let media = send_to(
        &proxy,
        request(Method::GET, &proxy_url(&plan, &media_target), &[], b""),
    )
    .await;
    assert_eq!(media.status(), 200);
    let api = send_to(
        &proxy,
        request(Method::GET, &format!("{}/v1/x", api_base(&plan)), &[], b""),
    )
    .await;
    assert_eq!(api.status(), 502);
    assert_eq!(text(&api), "Network error during API request");
    assert_eq!(budget.available(), budget.total() - MEDIA_BYTES);

    drop(media);
    let api = send_to(
        &proxy,
        request(Method::GET, &format!("{}/v1/x", api_base(&plan)), &[], b""),
    )
    .await;
    assert_eq!(api.status(), 200);
    assert_eq!(api.body().len(), MEDIA_BYTES);
}

#[tokio::test]
async fn svg_keeps_its_type_for_image_loads_only() {
    let harness = Harness::new();
    let upstream = Upstream::start().await;
    let plan = harness.cache(loopback_plan(upstream.port)).await;
    upstream.reply(
        "/media/a.svg",
        raw_reply(200, &[("Content-Type", "image/svg+xml")], b"<svg/>"),
    );
    let url = proxy_url(&plan, &format!("{}/media/a.svg", upstream.base()));
    let image = harness
        .send(request(
            Method::GET,
            &url,
            &[("sec-fetch-dest", "image")],
            b"",
        ))
        .await;
    assert_eq!(image.status(), 200);
    assert_eq!(header(&image, "content-type"), Some("image/svg+xml"));
    assert_eq!(header(&image, "x-content-type-options"), Some("nosniff"));
    for headers in [&[][..], &[("sec-fetch-dest", "empty")]] {
        let fetched = harness.send(request(Method::GET, &url, headers, b"")).await;
        assert_eq!(fetched.status(), 200);
        assert_eq!(
            header(&fetched, "content-type"),
            Some("application/octet-stream")
        );
    }
}
