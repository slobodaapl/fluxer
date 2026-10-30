// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::HashMap;
use std::net::Ipv4Addr;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Notify;

use super::*;

fn ip(value: &str) -> IpAddr {
    value.parse().unwrap()
}

#[derive(Clone)]
enum Answer {
    Addresses(Vec<IpAddr>),
    Fail,
    Hang,
    /// Answers once the gate is notified.
    Gated(Arc<Notify>, Vec<IpAddr>),
}

#[derive(Default)]
struct FakeLookup {
    answers: Mutex<HashMap<String, Answer>>,
    calls: Mutex<Vec<String>>,
}

impl FakeLookup {
    fn answer(&self, hostname: &str, answer: Answer) {
        self.answers
            .lock()
            .unwrap()
            .insert(hostname.to_owned(), answer);
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl HostLookup for FakeLookup {
    fn lookup(&self, hostname: String) -> LookupFuture {
        self.calls.lock().unwrap().push(hostname.clone());
        let answer = self.answers.lock().unwrap().get(&hostname).cloned();
        Box::pin(async move {
            match answer {
                Some(Answer::Addresses(addresses)) => Ok(addresses),
                Some(Answer::Gated(gate, addresses)) => {
                    gate.notified().await;
                    Ok(addresses)
                }
                Some(Answer::Hang) => std::future::pending().await,
                Some(Answer::Fail) | None => Err(io::Error::other("no such host")),
            }
        })
    }
}

fn outbound() -> (OutboundHttp, Arc<FakeLookup>) {
    let lookup = Arc::new(FakeLookup::default());
    let tls = crate::net::trust::client_config().unwrap();
    (OutboundHttp::with_lookup(tls, lookup.clone()), lookup)
}

fn request(origin: &str, url: &str) -> OriginRequest {
    OriginRequest {
        expected_origin: origin.to_owned(),
        url: url.to_owned(),
        method: Method::GET,
        headers: HeaderMap::new(),
        body: RequestBody::Empty,
        body_limit: DEFAULT_REQUEST_BODY_LIMIT,
        timeout: Duration::from_secs(10),
        origin_trust: OriginTrust::Registered,
        cancel: CancellationToken::new(),
    }
}

async fn read_body(body: &mut ResponseBody) -> Result<Vec<u8>, OutboundError> {
    let mut collected = Vec::new();
    while let Some(chunk) = body.next().await {
        collected.extend_from_slice(&chunk?);
    }
    Ok(collected)
}

async fn read_head(stream: &mut TcpStream) -> String {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte).await.unwrap_or(0) == 0 {
            break;
        }
        head.push(byte[0]);
    }
    String::from_utf8_lossy(&head).into_owned()
}

/// Minimal HTTP/1.1 server on 127.0.0.1 running `handler` per connection.
async fn serve<F, Fut>(handler: F) -> u16
where
    F: Fn(TcpStream) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let handler = Arc::new(handler);
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(handler(stream));
        }
    });
    port
}

/// Replies with the request head it received.
async fn echo_head(mut stream: TcpStream) {
    let head = read_head(&mut stream).await;
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nx-test: yes\r\n\r\n{head}",
        head.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
}

async fn register_local(
    outbound: &OutboundHttp,
    lookup: &FakeLookup,
    host: &str,
    port: u16,
) -> String {
    lookup.answer(
        &format!("{host}."),
        Answer::Addresses(vec![ip("127.0.0.1")]),
    );
    let origin = format!("http://{host}:{port}");
    let requirement = outbound
        .register_anchored_origins(&origin, &[], None)
        .await
        .unwrap();
    assert_eq!(requirement, AddressRequirement::Any);
    origin
}

#[test]
fn outbound_http_is_send_sync_and_body_is_send() {
    fn send_sync<T: Send + Sync + Clone>() {}
    fn send<T: Send>() {}
    send_sync::<OutboundHttp>();
    send::<ResponseBody>();
    send::<OriginRequest>();
}

#[test]
fn selects_non_public_addresses_when_any_resolve() {
    assert_eq!(
        select_pinned_addresses([
            ip("8.8.8.8"),
            ip("10.0.0.1"),
            ip("8.8.8.8"),
            ip("10.0.0.1"),
            ip("fd00::1"),
        ]),
        vec![ip("10.0.0.1"), ip("fd00::1")]
    );
    assert_eq!(
        select_pinned_addresses([ip("2606:4700::1"), ip("8.8.8.8"), ip("2606:4700::1")]),
        vec![ip("2606:4700::1"), ip("8.8.8.8")]
    );
    assert!(select_pinned_addresses([]).is_empty());
}

#[test]
fn lookup_hostnames_are_absolute_and_compared_normalized() {
    assert_eq!(absolute_lookup_hostname("example.com"), "example.com.");
    assert_eq!(absolute_lookup_hostname("example.com."), "example.com.");
    assert_eq!(absolute_lookup_hostname("localhost"), "localhost");
    assert_eq!(normalize_lookup_hostname(" Example.COM. "), "example.com");
    assert_eq!(normalize_lookup_hostname("[::1]"), "::1");
    assert_eq!(normalize_lookup_hostname("."), ".");
}

#[tokio::test]
async fn public_anchor_registers_public_and_tightens_only() {
    let (outbound, lookup) = outbound();
    lookup.answer("local.test.", Answer::Addresses(vec![ip("10.0.0.2")]));
    lookup.answer("anchor.test.", Answer::Addresses(vec![ip("8.8.8.8")]));
    let any = outbound
        .register_anchored_origins("http://local.test", &["http://cdn.local.test".into()], None)
        .await
        .unwrap();
    assert_eq!(any, AddressRequirement::Any);
    assert_eq!(
        outbound.registered_cleartext_origins(),
        vec!["http://cdn.local.test", "http://local.test"]
    );
    let public = outbound
        .register_anchored_origins("anchor.test", &["http://cdn.local.test/".into()], None)
        .await
        .unwrap();
    assert_eq!(public, AddressRequirement::Public);
    // cdn.local.test tightened to Public; local.test stays Any.
    assert_eq!(
        outbound.registered_cleartext_origins(),
        vec!["http://local.test"]
    );
    // Re-registering under Any never loosens.
    outbound
        .register_anchored_origins("http://local.test", &["http://cdn.local.test".into()], None)
        .await
        .unwrap();
    assert_eq!(
        outbound.registered_cleartext_origins(),
        vec!["http://local.test"]
    );
    outbound
        .require_registered_origins(&["https://anchor.test".into(), "local.test:80".into()])
        .unwrap_err();
    outbound
        .require_registered_origins(&[
            "https://anchor.test:443".into(),
            "http://local.test:80".into(),
        ])
        .unwrap();
    assert!(matches!(
        outbound.require_registered_origins(&["https://other.test".into()]),
        Err(OutboundError::OriginNotRegistered(origin)) if origin == "https://other.test"
    ));
}

#[tokio::test]
async fn registration_is_all_or_nothing() {
    let (outbound, lookup) = outbound();
    lookup.answer("private.test.", Answer::Addresses(vec![ip("192.168.1.5")]));
    lookup.answer("anchor.test.", Answer::Addresses(vec![ip("8.8.8.8")]));
    // Bind private.test without registering it.
    outbound
        .require_cleartext_transport_address("http://private.test")
        .await
        .unwrap();
    let error = outbound
        .register_anchored_origins(
            "https://anchor.test",
            &["https://fresh.test".into(), "http://private.test".into()],
            None,
        )
        .await
        .unwrap_err();
    assert!(matches!(error, OutboundError::PublicAddressRequired));
    assert!(matches!(
        outbound.require_registered_origins(&["https://anchor.test".into()]),
        Err(OutboundError::OriginNotRegistered(_))
    ));
    assert!(matches!(
        outbound.require_registered_origins(&["https://fresh.test".into()]),
        Err(OutboundError::OriginNotRegistered(_))
    ));
    // An invalid origin fails before the anchor is resolved.
    let calls = lookup.calls().len();
    let error = outbound
        .register_anchored_origins("https://other.test", &["https://x.test/path".into()], None)
        .await
        .unwrap_err();
    assert!(matches!(error, OutboundError::InvalidOrigin));
    assert_eq!(lookup.calls().len(), calls);
}

#[tokio::test]
async fn registration_caps_registered_origins() {
    let (outbound, _lookup) = outbound();
    let origins: Vec<String> = (1..MAX_ORIGINS)
        .map(|index| format!("http://10.0.{}.{}", index / 256, index % 256))
        .collect();
    outbound
        .register_anchored_origins("http://10.9.9.9", &origins, None)
        .await
        .unwrap();
    // Already-registered origins do not count against the cap.
    outbound
        .register_anchored_origins("http://10.9.9.9", &origins[..5], None)
        .await
        .unwrap();
    let error = outbound
        .register_anchored_origins("http://10.9.9.9", &["http://10.8.8.8".into()], None)
        .await
        .unwrap_err();
    assert!(matches!(error, OutboundError::Capacity));
    assert!(matches!(
        outbound.require_registered_origins(&["http://10.8.8.8".into()]),
        Err(OutboundError::OriginNotRegistered(_))
    ));
}

#[tokio::test]
async fn unresolvable_anchor_uses_remembered_requirement() {
    let (outbound, lookup) = outbound();
    lookup.answer("gone.test.", Answer::Fail);
    let error = outbound
        .register_anchored_origins("https://gone.test", &[], None)
        .await
        .unwrap_err();
    assert!(matches!(error, OutboundError::Resolution(_)));
    let requirement = outbound
        .register_anchored_origins(
            "http://gone.test",
            &["http://cdn.gone.test".into()],
            Some(AddressRequirement::Public),
        )
        .await
        .unwrap();
    assert_eq!(requirement, AddressRequirement::Public);
    assert!(outbound.registered_cleartext_origins().is_empty());
    outbound
        .require_registered_origins(&["http://cdn.gone.test".into()])
        .unwrap();
    // Empty answers are not resolution failures.
    lookup.answer("empty.test.", Answer::Addresses(Vec::new()));
    let error = outbound
        .register_anchored_origins("https://empty.test", &[], Some(AddressRequirement::Any))
        .await
        .unwrap_err();
    assert!(matches!(error, OutboundError::EmptyResolution));
}

/// Runs in real time (tokio `test-util` is not enabled): both resolutions
/// hang concurrently, so the test takes one resolution timeout.
#[tokio::test]
async fn resolution_times_out_after_ten_seconds() {
    let (outbound, lookup) = outbound();
    lookup.answer("slow.test.", Answer::Hang);
    let started = tokio::time::Instant::now();
    let (address, registration) = tokio::join!(
        outbound.require_cleartext_transport_address("http://slow.test"),
        outbound.register_anchored_origins("https://slow.test", &[], Some(AddressRequirement::Any)),
    );
    assert!(matches!(address, Err(OutboundError::ResolutionTimeout)));
    assert_eq!(registration.unwrap(), AddressRequirement::Any);
    assert!(started.elapsed() >= RESOLUTION_TIMEOUT);
    assert!(started.elapsed() < RESOLUTION_TIMEOUT * 2);
}

#[tokio::test]
async fn concurrent_callers_share_one_resolution() {
    let (outbound, lookup) = outbound();
    let gate = Arc::new(Notify::new());
    lookup.answer(
        "shared.test.",
        Answer::Gated(gate.clone(), vec![ip("fd00::5"), ip("10.0.0.5")]),
    );
    let first = tokio::spawn({
        let outbound = outbound.clone();
        async move {
            outbound
                .require_cleartext_transport_address("http://shared.test")
                .await
        }
    });
    let second = tokio::spawn({
        let outbound = outbound.clone();
        async move {
            outbound
                .require_cleartext_transport_address("HTTP://Shared.test:80/")
                .await
        }
    });
    while lookup.calls().is_empty() {
        tokio::task::yield_now().await;
    }
    tokio::task::yield_now().await;
    gate.notify_one();
    assert_eq!(first.await.unwrap().unwrap(), ip("10.0.0.5"));
    assert_eq!(second.await.unwrap().unwrap(), ip("10.0.0.5"));
    outbound
        .require_cleartext_transport_address("http://shared.test")
        .await
        .unwrap();
    assert_eq!(lookup.calls(), vec!["shared.test."]);
}

#[tokio::test]
async fn registration_tightens_a_pending_resolution() {
    let (outbound, lookup) = outbound();
    let gate = Arc::new(Notify::new());
    lookup.answer(
        "late.test.",
        Answer::Gated(gate.clone(), vec![ip("127.0.0.1")]),
    );
    lookup.answer("anchor.test.", Answer::Addresses(vec![ip("8.8.8.8")]));
    let waiter = tokio::spawn({
        let outbound = outbound.clone();
        async move {
            outbound
                .require_cleartext_transport_address("http://late.test")
                .await
        }
    });
    while lookup.calls().is_empty() {
        tokio::task::yield_now().await;
    }
    outbound
        .register_anchored_origins("https://anchor.test", &["http://late.test".into()], None)
        .await
        .unwrap();
    gate.notify_one();
    assert!(matches!(
        waiter.await.unwrap(),
        Err(OutboundError::PublicAddressRequired)
    ));
    // The private answer was never stored: the next caller resolves again.
    lookup.answer("late.test.", Answer::Addresses(vec![ip("127.0.0.1")]));
    assert!(matches!(
        outbound
            .require_cleartext_transport_address("http://late.test")
            .await,
        Err(OutboundError::PublicAddressRequired)
    ));
    assert_eq!(
        lookup
            .calls()
            .iter()
            .filter(|name| *name == "late.test.")
            .count(),
        2
    );
}

#[tokio::test]
async fn cleartext_transport_address_refuses_public_bindings() {
    let (outbound, lookup) = outbound();
    lookup.answer("public.test.", Answer::Addresses(vec![ip("8.8.8.8")]));
    assert!(matches!(
        outbound
            .require_cleartext_transport_address("http://public.test")
            .await,
        Err(OutboundError::InsecureTransport)
    ));
    assert_eq!(
        outbound
            .require_cleartext_transport_address("http://[fd00::9]:8080")
            .await
            .unwrap(),
        ip("fd00::9")
    );
}

#[tokio::test]
async fn request_succeeds_against_registered_any_origin() {
    let (outbound, lookup) = outbound();
    let port = serve(echo_head).await;
    let origin = register_local(&outbound, &lookup, "local.test", port).await;
    let mut req = request(&origin, &format!("{origin}/api/v1/ping?x=1"));
    req.headers
        .insert(header::HOST, "evil.test".parse().unwrap());
    req.headers.insert("x-custom", "kept".parse().unwrap());
    let mut response = outbound.request(req).await.unwrap();
    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.headers["x-test"], "yes");
    assert_eq!(response.url, format!("{origin}/api/v1/ping?x=1"));
    let head = String::from_utf8(read_body(&mut response.body).await.unwrap()).unwrap();
    let head = head.to_lowercase();
    assert!(
        head.starts_with("get /api/v1/ping?x=1 http/1.1\r\n"),
        "{head}"
    );
    assert!(
        head.contains(&format!("\r\nhost: local.test:{port}\r\n")),
        "{head}"
    );
    assert!(head.contains("\r\nx-custom: kept\r\n"), "{head}");
    assert!(!head.contains("evil.test"), "{head}");
    assert_eq!(lookup.calls(), vec!["local.test."]);
}

#[tokio::test]
async fn bound_trust_allows_unregistered_origins() {
    let (outbound, lookup) = outbound();
    let port = serve(echo_head).await;
    lookup.answer("bound.test.", Answer::Addresses(vec![ip("127.0.0.1")]));
    let origin = format!("http://bound.test:{port}");
    let mut req = request(&origin, &format!("{origin}/"));
    req.origin_trust = OriginTrust::Bound;
    let response = outbound.request(req).await.unwrap();
    assert_eq!(response.status, StatusCode::OK);
}

#[tokio::test]
async fn request_rejects_unregistered_origin() {
    let (outbound, lookup) = outbound();
    let error = outbound
        .request(request("https://nope.test", "https://nope.test/x"))
        .await
        .err()
        .unwrap();
    assert!(
        matches!(error, OutboundError::OriginNotRegistered(origin) if origin == "https://nope.test")
    );
    assert!(lookup.calls().is_empty());
}

#[tokio::test]
async fn request_requires_public_address_for_public_registration() {
    let (outbound, lookup) = outbound();
    let port = serve(echo_head).await;
    lookup.answer("anchor.test.", Answer::Addresses(vec![ip("8.8.8.8")]));
    lookup.answer("local.test.", Answer::Addresses(vec![ip("127.0.0.1")]));
    let origin = format!("http://local.test:{port}");
    outbound
        .register_anchored_origins("https://anchor.test", std::slice::from_ref(&origin), None)
        .await
        .unwrap();
    let error = outbound
        .request(request(&origin, &format!("{origin}/")))
        .await
        .err()
        .unwrap();
    assert!(matches!(error, OutboundError::PublicAddressRequired));
}

#[tokio::test]
async fn request_refuses_cleartext_to_public_binding() {
    let (outbound, lookup) = outbound();
    lookup.answer("public.test.", Answer::Addresses(vec![ip("8.8.8.8")]));
    outbound
        .register_anchored_origins("http://public.test", &[], None)
        .await
        .unwrap();
    let error = outbound
        .request(request("http://public.test", "http://public.test/x"))
        .await
        .err()
        .unwrap();
    assert!(matches!(error, OutboundError::InsecureTransport));
}

#[tokio::test]
async fn request_rejects_targets_outside_the_origin() {
    let (outbound, lookup) = outbound();
    for (origin, url) in [
        ("http://a.test", "http://b.test/x"),
        ("http://a.test", "https://a.test/x"),
        ("http://a.test", "http://a.test:8080/x"),
        ("http://a.test", "http://user@a.test/x"),
        ("http://a.test", "ftp://a.test/x"),
        ("http://a.test", "not a url"),
    ] {
        let error = outbound.request(request(origin, url)).await.err().unwrap();
        assert!(matches!(error, OutboundError::InvalidTarget), "{url}");
    }
    let long = format!("http://a.test/{}", "a".repeat(MAX_TARGET_URL_BYTES));
    let error = outbound
        .request(request("http://a.test", &long))
        .await
        .err()
        .unwrap();
    assert!(matches!(error, OutboundError::InvalidTarget));
    let error = outbound
        .request(request("http://a.test/path", "http://a.test/x"))
        .await
        .err()
        .unwrap();
    assert!(matches!(error, OutboundError::InvalidOrigin));
    assert!(lookup.calls().is_empty());
}

#[tokio::test]
async fn bytes_body_over_limit_fails_before_network() {
    let (outbound, lookup) = outbound();
    let mut req = request("http://a.test", "http://a.test/x");
    req.body = RequestBody::Bytes(Bytes::from_static(b"12345"));
    req.body_limit = 4;
    let error = outbound.request(req).await.err().unwrap();
    assert!(matches!(error, OutboundError::BodyLimit));
    assert!(lookup.calls().is_empty());
}

#[tokio::test]
async fn bytes_body_at_limit_is_sent() {
    let (outbound, lookup) = outbound();
    let port = serve(|mut stream: TcpStream| async move {
        let head = read_head(&mut stream).await;
        let mut body = [0u8; 4];
        stream.read_exact(&mut body).await.unwrap();
        let reply = format!(
            "{}|{}",
            head.to_lowercase().contains("content-length: 4"),
            String::from_utf8_lossy(&body)
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n{reply}",
            reply.len()
        );
        let _ = stream.write_all(response.as_bytes()).await;
    })
    .await;
    let origin = register_local(&outbound, &lookup, "local.test", port).await;
    let mut req = request(&origin, &format!("{origin}/upload"));
    req.method = Method::POST;
    req.body = RequestBody::Bytes(Bytes::from_static(b"abcd"));
    req.body_limit = 4;
    let mut response = outbound.request(req).await.unwrap();
    assert_eq!(read_body(&mut response.body).await.unwrap(), b"true|abcd");
}

#[tokio::test]
async fn stream_body_is_chunked_and_limited() {
    let (outbound, lookup) = outbound();
    let port = serve(|mut stream: TcpStream| async move {
        let head = read_head(&mut stream).await.to_lowercase();
        if !head.contains("transfer-encoding: chunked") {
            return;
        }
        // Drain until the client gives up, then answer if it finished.
        let mut sink = Vec::new();
        loop {
            let mut buffer = [0u8; 1024];
            match stream.read(&mut buffer).await {
                Ok(0) | Err(_) => return,
                Ok(read) => sink.extend_from_slice(&buffer[..read]),
            }
            if sink.ends_with(b"0\r\n\r\n") {
                let _ = stream
                    .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok")
                    .await;
                return;
            }
        }
    })
    .await;
    let origin = register_local(&outbound, &lookup, "local.test", port).await;
    let chunks = || {
        futures_util::stream::iter(
            [b"aaaa", b"bbbb", b"cccc"].map(|chunk| Ok(Bytes::from_static(chunk))),
        )
    };

    let mut req = request(&origin, &format!("{origin}/upload"));
    req.method = Method::POST;
    req.body = RequestBody::Stream(Box::pin(chunks()));
    req.body_limit = 12;
    let mut response = outbound.request(req).await.unwrap();
    assert_eq!(read_body(&mut response.body).await.unwrap(), b"ok");

    let mut req = request(&origin, &format!("{origin}/upload"));
    req.method = Method::POST;
    req.body = RequestBody::Stream(Box::pin(chunks()));
    req.body_limit = 10;
    let error = outbound.request(req).await.err().unwrap();
    assert!(matches!(error, OutboundError::BodyLimit), "{error:?}");
}

#[tokio::test]
async fn timeout_covers_the_response_body() {
    let (outbound, lookup) = outbound();
    let port = serve(|mut stream: TcpStream| async move {
        read_head(&mut stream).await;
        let _ = stream
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 20\r\n\r\nfirst")
            .await;
        tokio::time::sleep(Duration::from_secs(30)).await;
    })
    .await;
    let origin = register_local(&outbound, &lookup, "local.test", port).await;
    let mut req = request(&origin, &format!("{origin}/slow"));
    req.timeout = Duration::from_millis(400);
    let started = tokio::time::Instant::now();
    let mut response = outbound.request(req).await.unwrap();
    assert_eq!(response.body.next().await.unwrap().unwrap(), "first");
    assert!(matches!(
        response.body.next().await,
        Some(Err(OutboundError::Timeout))
    ));
    assert!(started.elapsed() >= Duration::from_millis(400));
    assert!(response.body.next().await.is_none());
}

#[tokio::test]
async fn timeout_before_response_headers() {
    let (outbound, lookup) = outbound();
    let port = serve(|mut stream: TcpStream| async move {
        read_head(&mut stream).await;
        tokio::time::sleep(Duration::from_secs(30)).await;
    })
    .await;
    let origin = register_local(&outbound, &lookup, "local.test", port).await;
    let mut req = request(&origin, &format!("{origin}/"));
    req.timeout = Duration::from_millis(200);
    let error = outbound.request(req).await.err().unwrap();
    assert!(matches!(error, OutboundError::Timeout));
    // The connection was established, so the binding is kept.
    outbound
        .require_cleartext_transport_address(&origin)
        .await
        .unwrap();
    assert_eq!(lookup.calls().len(), 1);
}

#[tokio::test]
async fn cancel_aborts_the_exchange() {
    let (outbound, lookup) = outbound();
    let port = serve(|mut stream: TcpStream| async move {
        read_head(&mut stream).await;
        tokio::time::sleep(Duration::from_secs(30)).await;
    })
    .await;
    let origin = register_local(&outbound, &lookup, "local.test", port).await;
    let req = request(&origin, &format!("{origin}/hang"));
    let cancel = req.cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel.cancel();
    });
    let error = outbound.request(req).await.err().unwrap();
    assert!(matches!(error, OutboundError::Aborted));

    let req = request(&origin, &format!("{origin}/hang"));
    req.cancel.cancel();
    let error = outbound.request(req).await.err().unwrap();
    assert!(matches!(error, OutboundError::Aborted));
}

#[tokio::test]
async fn cancel_aborts_a_streaming_response_body() {
    let (outbound, lookup) = outbound();
    let port = serve(|mut stream: TcpStream| async move {
        read_head(&mut stream).await;
        let _ = stream
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 20\r\n\r\nfirst")
            .await;
        tokio::time::sleep(Duration::from_secs(30)).await;
    })
    .await;
    let origin = register_local(&outbound, &lookup, "local.test", port).await;
    let req = request(&origin, &format!("{origin}/slow"));
    let cancel = req.cancel.clone();
    let mut response = outbound.request(req).await.unwrap();
    assert_eq!(response.body.next().await.unwrap().unwrap(), "first");
    cancel.cancel();
    assert!(matches!(
        response.body.next().await,
        Some(Err(OutboundError::Aborted))
    ));
}

#[tokio::test]
async fn refused_connection_marks_binding_for_re_resolution() {
    let (outbound, lookup) = outbound();
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let origin = register_local(&outbound, &lookup, "gone.test", port).await;
    let error = outbound
        .request(request(&origin, &format!("{origin}/")))
        .await
        .err()
        .unwrap();
    assert!(matches!(error, OutboundError::Connect(_)), "{error:?}");
    assert!(error.is_unreachable());
    // The remembered binding was resolved at registration; the failed
    // connect forces a fresh resolution on the next request.
    let _ = outbound
        .request(request(&origin, &format!("{origin}/")))
        .await;
    assert_eq!(lookup.calls(), vec!["gone.test.", "gone.test."]);
}

#[tokio::test]
async fn cleanup_closes_the_instance() {
    let (outbound, lookup) = outbound();
    let port = serve(echo_head).await;
    let origin = register_local(&outbound, &lookup, "local.test", port).await;
    outbound.cleanup();
    outbound.cleanup();
    assert!(outbound.registered_cleartext_origins().is_empty());
    assert!(matches!(
        outbound
            .request(request(&origin, &format!("{origin}/")))
            .await,
        Err(OutboundError::Closed)
    ));
    assert!(matches!(
        outbound.register_anchored_origins(&origin, &[], None).await,
        Err(OutboundError::Closed)
    ));
}

#[tokio::test]
async fn public_trust_refuses_cleartext_before_resolving() {
    let (outbound, lookup) = outbound();
    let mut req = request("http://icons.test", "http://icons.test/a.png");
    req.origin_trust = OriginTrust::Public;
    let error = outbound.request(req).await.err().unwrap();
    assert!(matches!(error, OutboundError::InsecureTransport));
    assert!(lookup.calls().is_empty());
}

#[tokio::test]
async fn public_trust_refuses_private_addresses() {
    let (outbound, lookup) = outbound();
    lookup.answer("icons.test.", Answer::Addresses(vec![ip("127.0.0.1")]));
    let mut req = request("https://icons.test", "https://icons.test/a.png");
    req.origin_trust = OriginTrust::Public;
    let error = outbound.request(req).await.err().unwrap();
    assert!(matches!(error, OutboundError::PublicAddressRequired));
}

#[tokio::test]
async fn public_trust_keeps_a_registered_origins_requirement() {
    let (outbound, lookup) = outbound();
    let port = serve(echo_head).await;
    let origin = register_local(&outbound, &lookup, "local.test", port).await;
    let mut req = request(&origin, &format!("{origin}/"));
    req.origin_trust = OriginTrust::Public;
    let response = outbound.request(req).await.unwrap();
    assert_eq!(response.status, StatusCode::OK);
}

#[test]
fn redirect_targets_resolve_against_the_response_url() {
    let base = Url::parse("https://cdn.test/a/b.png").unwrap();
    let target = |location: &[u8]| {
        parse_redirect_target(&base, Some(&header::HeaderValue::from_bytes(location).unwrap()))
            .map(String::from)
    };
    assert_eq!(target(b"../c.png").as_deref(), Some("https://cdn.test/c.png"));
    assert_eq!(target(b"http://other.test/x").as_deref(), Some("http://other.test/x"));
    assert_eq!(target(b"https://cdn.test/\xe9").as_deref(), Some("https://cdn.test/%C3%A9"));
    assert_eq!(target(b"ftp://cdn.test/x"), None);
    assert_eq!(target(b"https://user@cdn.test/x"), None);
    assert_eq!(target(&[b'a'; 4097]), None);
    assert_eq!(target(&[b'a'; 4096]).as_deref().map(str::len), Some("https://cdn.test/a/".len() + 4096));
    assert_eq!(parse_redirect_target(&base, None), None);
}
