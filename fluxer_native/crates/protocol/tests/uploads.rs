// SPDX-License-Identifier: AGPL-3.0-or-later
//! Wire fixtures derived from AttachmentUploadPlan, ChunkedAttachmentUploader,
//! MessageQueue and MessageAttachmentUtils; all files/servers are synthetic.
use fluxer_protocol::{
    Client, Error, Instance, SessionToken, UploadAttachment, UploadCancellation, UploadPhase,
    UploadProgress, UploadProgressCallback,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::mpsc,
    time::timeout,
};

#[derive(Clone, Debug)]
struct Request {
    path: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
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
    async fn new(handler: impl Fn(&Request) -> (u16, Value) + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let (tx, requests) = mpsc::unbounded_channel();
        let handler = Arc::new(handler);
        let task = tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let tx = tx.clone();
                let handler = handler.clone();
                tokio::spawn(async move {
                    let mut raw = Vec::new();
                    let mut buf = [0; 8192];
                    let (end, length) = loop {
                        let n = socket.read(&mut buf).await.unwrap();
                        if n == 0 {
                            return;
                        }
                        raw.extend_from_slice(&buf[..n]);
                        if let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                            let header = String::from_utf8_lossy(&raw[..end]);
                            let length = header
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
                        let n = socket.read(&mut buf).await.unwrap();
                        if n == 0 {
                            return;
                        }
                        raw.extend_from_slice(&buf[..n]);
                    }
                    let header = String::from_utf8_lossy(&raw[..end]);
                    let mut lines = header.lines();
                    let path = lines.next().unwrap().to_owned();
                    let headers = lines
                        .map(|l| {
                            let (k, v) = l.split_once(':').unwrap();
                            (k.to_ascii_lowercase(), v.trim().to_owned())
                        })
                        .collect();
                    let request = Request {
                        path,
                        headers,
                        body: raw[end + 4..end + 4 + length].to_vec(),
                    };
                    let (status, body) = handler(&request);
                    tx.send(request).unwrap();
                    let body = body.to_string();
                    let response = format!(
                        "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                });
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
        assert!(
            timeout(Duration::from_millis(80), self.requests.recv())
                .await
                .is_err()
        );
    }
}
struct FixtureFile(PathBuf);
impl FixtureFile {
    fn new(bytes: &[u8]) -> Self {
        let path = std::env::temp_dir().join(format!(
            "fluxer-synthetic-upload-{}-{}",
            std::process::id(),
            fastrand::u64(..)
        ));
        std::fs::write(&path, bytes).unwrap();
        Self(path)
    }
    fn attachment(&self, id: &str) -> UploadAttachment {
        UploadAttachment::new(id, &self.0, "fixture.bin", "application/test-fixture")
    }
}
impl Drop for FixtureFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
fn client(origin: &str, presigned: bool) -> Client {
    let mut instance =
        Instance::from_endpoints(&format!("{origin}/api"), "ws://127.0.0.1/unused").unwrap();
    instance.web_app = "https://desktop.example.test".parse().unwrap();
    instance.features = json!({"presigned_attachment_uploads":presigned});
    Client::new(instance)
        .unwrap()
        .with_token(SessionToken::new("fixture-account-secret").unwrap())
}
fn progress() -> (UploadProgressCallback, Arc<Mutex<Vec<UploadProgress>>>) {
    let values = Arc::new(Mutex::new(Vec::new()));
    let copy = values.clone();
    (Arc::new(move |p| copy.lock().unwrap().push(p)), values)
}
fn assert_storage(request: &Request, bytes: &[u8]) {
    assert!(request.path.starts_with("PUT "));
    assert_eq!(request.body, bytes);
    assert_eq!(request.headers["content-type"], "application/planned");
    assert_eq!(request.headers["content-length"], bytes.len().to_string());
    assert!(
        !request
            .headers
            .keys()
            .any(|name| name.starts_with("x-fluxer-")),
        "storage leaked first-party Fluxer metadata"
    );
    for forbidden in [
        "authorization",
        "origin",
        "cookie",
        "user-agent",
        "accept-encoding",
        "x-super-properties",
        "x-fluxer-platform",
        "x-fluxer-installation-id",
        "x-fluxer-client-features",
        "x-fluxer-client-capabilities",
    ] {
        assert!(
            !request.headers.contains_key(forbidden),
            "storage leaked {forbidden}"
        );
    }
}
fn single_plan(storage: &str, id: &str, size: usize) -> Value {
    json!({"attachments":[{"id":id,"upload_mode":"singlepart","filename":"fixture.bin","upload_filename":"uploaded/object","upload_url":format!("{storage}/signed?signature=storage-secret"),"file_size":size,"content_type":"application/planned"}]})
}

#[tokio::test]
async fn singlepart_streams_exact_bytes_without_credentials_then_sends_exact_json() {
    let bytes = b"synthetic\0file\xffbytes";
    let file = FixtureFile::new(bytes);
    let mut storage = Server::new(|_| (200, json!({}))).await;
    let plan = single_plan(&storage.origin, "0", bytes.len());
    let mut api = Server::new(move |r| {
        if r.path.contains("/attachments ") {
            (200, plan.clone())
        } else {
            (200, json!({"id":"sent"}))
        }
    })
    .await;
    let mut attachment = file.attachment("0");
    attachment.description = Some("caption".into());
    attachment.flags = Some(8);
    attachment.duration = Some(2);
    attachment.waveform = Some("wave".into());
    let (callback, events) = progress();
    let message = json!({"content":"hello","nonce":"123","message_reference":{"message_id":"456"},"allowed_mentions":{"replied_user":false}});
    let result = client(&api.origin, true)
        .send_message_with_attachments(
            "1",
            message.clone(),
            &[attachment],
            &UploadCancellation::new(),
            callback,
        )
        .await
        .unwrap();
    assert_eq!(result["id"], "sent");
    let plan_request = api.next().await;
    assert_eq!(
        serde_json::from_slice::<Value>(&plan_request.body).unwrap(),
        json!({"attachments":[{"id":"0","filename":"fixture.bin","file_size":bytes.len(),"content_type":"application/test-fixture"}]})
    );
    assert_eq!(
        plan_request.headers["authorization"],
        "fixture-account-secret"
    );
    assert_eq!(
        plan_request.headers["origin"],
        "https://desktop.example.test"
    );
    for required in [
        "x-fluxer-features",
        "x-fluxer-capabilities",
        "x-fluxer-client-installation-id",
    ] {
        assert!(
            plan_request.headers.contains_key(required),
            "API request omitted {required}"
        );
    }
    assert_storage(&storage.next().await, bytes);
    let final_request = api.next().await;
    assert_eq!(final_request.headers["content-type"], "application/json");
    let sent: Value = serde_json::from_slice(&final_request.body).unwrap();
    let mut expected = message;
    expected["attachments"] = json!([{"id":"0","filename":"fixture.bin","title":"fixture.bin","description":"caption","flags":8,"duration":2,"waveform":"wave","upload_filename":"uploaded/object","file_size":bytes.len(),"content_type":"application/planned"}]);
    assert_eq!(sent, expected);
    {
        let events = events.lock().unwrap();
        assert!(
            events
                .windows(2)
                .all(|w| w[1].uploaded_bytes >= w[0].uploaded_bytes)
        );
        assert_eq!(events.last().unwrap().phase, UploadPhase::Complete);
        assert_eq!(events.last().unwrap().completed_files, 1);
    }
    api.none().await;
    storage.none().await;
}

#[tokio::test]
async fn multipart_retries_transient_parts_uses_array_offsets_and_completes_before_message() {
    let bytes = b"ABCDEFGHIJKL";
    let file = FixtureFile::new(bytes);
    let attempts = Arc::new(AtomicUsize::new(0));
    let count = attempts.clone();
    let mut storage = Server::new(move |r| {
        if r.path.contains("/part0 ") && count.fetch_add(1, Ordering::SeqCst) == 0 {
            (503, json!({"retry_after":0.001}))
        } else {
            (200, json!({}))
        }
    })
    .await;
    let parts = (0..4)
        .map(|n| json!({"part_number":10-n,"upload_url":format!("{}/part{n}",storage.origin)}))
        .collect::<Vec<_>>();
    let plan = json!({"attachments":[{"id":0,"upload_mode":"multipart","filename":"fixture.bin","upload_filename":"multipart/object","upload_id":"upload-handle","part_size":3,"parts":parts,"file_size":bytes.len(),"content_type":"application/planned"}]});
    let mut api = Server::new(move |r| {
        if r.path.contains("/attachments ") {
            (200, plan.clone())
        } else {
            (200, json!({"id":"sent"}))
        }
    })
    .await;
    let (callback, events) = progress();
    client(&api.origin, true)
        .send_message_with_attachments(
            "1",
            json!({"content":"multipart"}),
            &[file.attachment("0")],
            &UploadCancellation::new(),
            callback,
        )
        .await
        .unwrap();
    for _ in 0..5 {
        let request = storage.next().await;
        let n = request
            .path
            .split("/part")
            .nth(1)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .parse::<usize>()
            .unwrap();
        assert_storage(&request, &bytes[n * 3..n * 3 + 3]);
    }
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert!(api.next().await.path.contains("/attachments "));
    let complete = api.next().await;
    assert!(complete.path.contains("/attachments/complete "));
    assert_eq!(
        serde_json::from_slice::<Value>(&complete.body).unwrap(),
        json!({"uploads":[{"upload_filename":"multipart/object","upload_id":"upload-handle"}]})
    );
    assert!(api.next().await.path.contains("/messages "));
    let events = events.lock().unwrap();
    assert!(
        events
            .windows(2)
            .all(|w| w[1].uploaded_bytes >= w[0].uploaded_bytes)
    );
    assert!(events.iter().any(|e| e.phase == UploadPhase::Completing));
}

#[tokio::test]
async fn feature_disabled_sends_streamed_multipart_with_raw_metadata() {
    let bytes = b"fallback-data-unique";
    let file = FixtureFile::new(bytes);
    let mut api = Server::new(|_| (200, json!({"id":"sent"}))).await;
    client(&api.origin, false)
        .send_message_with_attachments(
            "1",
            json!({"content":"fallback","nonce":"42"}),
            &[file.attachment("0")],
            &UploadCancellation::new(),
            progress().0,
        )
        .await
        .unwrap();
    let request = api.next().await;
    assert!(request.path.contains("/messages "));
    assert!(request.headers["content-type"].starts_with("multipart/form-data; boundary="));
    let body = String::from_utf8(request.body).unwrap();
    assert!(body.contains("name=\"payload_json\""));
    assert!(body.contains("name=\"files[0]\"; filename=\"fixture.bin\""));
    assert!(body.contains("Content-Type: application/test-fixture"));
    assert!(body.contains("fallback-data-unique"));
    assert!(body.contains(
        "\"attachments\":[{\"filename\":\"fixture.bin\",\"id\":\"0\",\"title\":\"fixture.bin\"}]"
    ));
    assert!(!body.contains("upload_filename"));
    api.none().await;
}

#[tokio::test]
async fn preupload_returns_reusable_metadata_and_does_not_post_message() {
    let file = FixtureFile::new(b"data");
    let mut storage = Server::new(|_| (200, json!({}))).await;
    let plan = single_plan(&storage.origin, "78", 4);
    let mut api = Server::new(move |_| (200, plan.clone())).await;
    let client = client(&api.origin, true);
    let uploaded = client
        .preupload_attachments(
            "1",
            &[file.attachment("78")],
            &UploadCancellation::new(),
            progress().0,
        )
        .await
        .unwrap();
    assert_eq!(uploaded[0]["id"], "78");
    assert_eq!(uploaded[0]["upload_filename"], "uploaded/object");
    api.next().await;
    assert_storage(&storage.next().await, b"data");
    api.none().await;
}

#[tokio::test]
async fn put_failure_falls_back_but_missing_plan_and_complete_failure_do_not() {
    let file = FixtureFile::new(b"data");
    let mut storage = Server::new(|_| (403, json!({}))).await;
    let plan = single_plan(&storage.origin, "0", 4);
    let mut api = Server::new(move |r| {
        if r.path.contains("/attachments ") {
            (200, plan.clone())
        } else {
            (200, json!({"id":"fallback"}))
        }
    })
    .await;
    client(&api.origin, true)
        .send_message_with_attachments(
            "1",
            json!({}),
            &[file.attachment("0")],
            &UploadCancellation::new(),
            progress().0,
        )
        .await
        .unwrap();
    api.next().await;
    assert!(api.next().await.headers["content-type"].starts_with("multipart/form-data"));
    storage.next().await;
    storage.none().await;
    let mut api = Server::new(|_| (200, json!({"attachments":[]}))).await;
    assert!(matches!(
        client(&api.origin, true)
            .send_message_with_attachments(
                "1",
                json!({}),
                &[file.attachment("0")],
                &UploadCancellation::new(),
                progress().0
            )
            .await,
        Err(Error::AttachmentPlanMissing)
    ));
    api.next().await;
    api.none().await;
    let mut storage = Server::new(|_| (200, json!({}))).await;
    let plan = json!({"attachments":[{"id":"0","upload_mode":"multipart","filename":"fixture.bin","upload_filename":"key","upload_id":"multipart","part_size":4,"parts":[{"part_number":1,"upload_url":format!("{}/part",storage.origin)}],"file_size":4,"content_type":"application/planned"}]});
    let mut api = Server::new(move |r| {
        if r.path.contains("/attachments ") {
            (200, plan.clone())
        } else {
            (500, json!({"code":"FAILED"}))
        }
    })
    .await;
    assert!(matches!(
        client(&api.origin, true)
            .send_message_with_attachments(
                "1",
                json!({}),
                &[file.attachment("0")],
                &UploadCancellation::new(),
                progress().0
            )
            .await,
        Err(Error::AttachmentCompletion(_))
    ));
    api.next().await;
    api.next().await;
    storage.next().await;
    api.none().await;
}

#[tokio::test]
async fn cancel_after_storage_headers_interrupts_stalled_error_body_without_fallback_or_abort() {
    let file = FixtureFile::new(b"data");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = tokio::sync::oneshot::channel();
    let stalled = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0; 8192];
        let _ = socket.read(&mut buf).await.unwrap();
        socket
            .write_all(
                b"HTTP/1.1 503 Fixture\r\nContent-Length: 10000\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        tx.send(()).unwrap();
        std::future::pending::<()>().await;
    });
    let plan = single_plan(&origin, "0", 4);
    let mut api = Server::new(move |_| (200, plan.clone())).await;
    let client = client(&api.origin, true);
    let cancel = UploadCancellation::new();
    let signal = cancel.clone();
    let attachment = file.attachment("0");
    let upload = tokio::spawn(async move {
        client
            .send_message_with_attachments("1", json!({}), &[attachment], &cancel, progress().0)
            .await
    });
    rx.await.unwrap();
    signal.cancel();
    assert!(matches!(
        timeout(Duration::from_secs(1), upload)
            .await
            .unwrap()
            .unwrap(),
        Err(Error::UploadCancelled)
    ));
    api.next().await;
    api.none().await;
    stalled.abort();
}

#[tokio::test]
async fn background_retry_exhaustion_has_four_attempts_and_never_posts_fallback() {
    let file = FixtureFile::new(b"data");
    let mut storage = Server::new(|_| (503, json!({}))).await;
    let plan = json!({"attachments":[{"id":"1","upload_mode":"multipart","filename":"fixture.bin","upload_filename":"key","upload_id":"multipart","part_size":4,"parts":[{"part_number":1,"upload_url":format!("{}/part",storage.origin)}],"file_size":4,"content_type":"application/planned"}]});
    let mut api = Server::new(move |_| (200, plan.clone())).await;
    assert!(matches!(
        client(&api.origin, true)
            .preupload_attachments(
                "1",
                &[file.attachment("1")],
                &UploadCancellation::new(),
                progress().0
            )
            .await,
        Err(Error::StorageUpload { status: 503 })
    ));
    for _ in 0..4 {
        assert_storage(&storage.next().await, b"data");
    }
    storage.none().await;
    api.next().await;
    api.none().await;
}

#[tokio::test]
async fn large_plan_failure_fallback_depends_on_endpoint_reachability() {
    let file = FixtureFile::new(&[]);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&file.0)
        .unwrap()
        .set_len(25 * 1024 * 1024 + 1)
        .unwrap();
    let mut api = Server::new(|_| (500, json!({}))).await;
    assert!(matches!(
        client(&api.origin, true)
            .send_message_with_attachments(
                "1",
                json!({}),
                &[file.attachment("0")],
                &UploadCancellation::new(),
                progress().0
            )
            .await,
        Err(Error::UploadFallbackUnavailable)
    ));
    api.next().await;
    api.none().await;
    let mut api = Server::new(|r| {
        if r.path.contains("/attachments ") {
            (400, json!({"code":"PLAN_NOT_SUPPORTED"}))
        } else {
            (200, json!({"id":"fallback"}))
        }
    })
    .await;
    client(&api.origin, true)
        .send_message_with_attachments(
            "1",
            json!({}),
            &[file.attachment("0")],
            &UploadCancellation::new(),
            progress().0,
        )
        .await
        .unwrap();
    api.next().await;
    let request = api.next().await;
    assert!(request.headers["content-type"].starts_with("multipart/form-data"));
    assert!(request.body.len() > 25 * 1024 * 1024);
    api.none().await;
}

#[tokio::test]
async fn background_storage_redirect_is_not_followed_and_capability_url_is_not_in_error() {
    let file = FixtureFile::new(b"data");
    let mut storage = Server::new(|_| (302, json!({}))).await;
    let plan = single_plan(&storage.origin, "0", 4);
    let mut api = Server::new(move |_| (200, plan.clone())).await;
    let error = client(&api.origin, true)
        .preupload_attachments(
            "1",
            &[file.attachment("0")],
            &UploadCancellation::new(),
            progress().0,
        )
        .await
        .unwrap_err();
    assert!(matches!(error, Error::StorageUpload { status: 302 }));
    assert!(!format!("{error:?}").contains("storage-secret"));
    storage.next().await;
    storage.none().await;
    api.next().await;
    api.none().await;
}

#[tokio::test]
async fn multipart_opens_exactly_four_concurrent_storage_requests() {
    let file = FixtureFile::new(b"ABCDEFGHIJKLMNO");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let gates = gate.clone();
    let (started, mut requests) = mpsc::unbounded_channel();
    let server = tokio::spawn(async move {
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..5 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let gate = gates.clone();
            let started = started.clone();
            tasks.spawn(async move {
                let mut raw = Vec::new();
                let mut buf = [0; 4096];
                loop {
                    let n = socket.read(&mut buf).await.unwrap();
                    assert!(n > 0);
                    raw.extend_from_slice(&buf[..n]);
                    if let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n")
                        && raw.len() >= end + 4 + 3
                    {
                        break;
                    }
                }
                started.send(()).unwrap();
                gate.acquire().await.unwrap().forget();
                socket
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                    )
                    .await
                    .unwrap();
            });
        }
        while let Some(result) = tasks.join_next().await {
            result.unwrap();
        }
    });
    let plan = json!({"attachments":[{"id":"0","upload_mode":"multipart","filename":"fixture.bin","upload_filename":"key","upload_id":"handle","part_size":3,"parts":(0..5).map(|n|json!({"part_number":n+1,"upload_url":format!("{origin}/part{n}")})).collect::<Vec<_>>(),"file_size":15,"content_type":"application/planned"}]});
    let mut api = Server::new(move |r| {
        if r.path.contains("/attachments ") {
            (200, plan.clone())
        } else {
            (200, json!({}))
        }
    })
    .await;
    let client = client(&api.origin, true);
    let attachment = file.attachment("0");
    let upload = tokio::spawn(async move {
        client
            .preupload_attachments("1", &[attachment], &UploadCancellation::new(), progress().0)
            .await
    });
    for _ in 0..4 {
        timeout(Duration::from_secs(3), requests.recv())
            .await
            .unwrap()
            .unwrap();
    }
    assert!(
        timeout(Duration::from_millis(100), requests.recv())
            .await
            .is_err(),
        "fifth PUT started before a concurrency slot became available"
    );
    gate.add_permits(5);
    timeout(Duration::from_secs(3), requests.recv())
        .await
        .unwrap()
        .unwrap();
    timeout(Duration::from_secs(3), upload)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    server.await.unwrap();
    api.next().await;
    api.next().await;
    api.none().await;
}
