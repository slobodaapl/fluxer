// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::*;
use crate::net::outbound::{HostLookup, LookupFuture, OutboundError};

const HOST: &str = "files.test";

/// Every host name resolves to loopback; `files.test` is registered as an instance origin, so
/// its plain-http, private address is allowed as `get()` allowed registered origins.
struct LoopbackLookup;

impl HostLookup for LoopbackLookup {
    fn lookup(&self, _hostname: String) -> LookupFuture {
        Box::pin(async { Ok(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]) })
    }
}

/// Raw responses by request path; unknown paths get a 404.
type Routes = HashMap<String, Vec<u8>>;

async fn read_request_path(stream: &mut TcpStream) -> String {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte).await.unwrap_or(0) == 0 {
            break;
        }
        head.push(byte[0]);
    }
    String::from_utf8_lossy(&head)
        .split(' ')
        .nth(1)
        .unwrap_or_default()
        .to_owned()
}

async fn serve(routes: Routes) -> u16 {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let routes = Arc::new(routes);
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let routes = Arc::clone(&routes);
            tokio::spawn(async move {
                let path = read_request_path(&mut stream).await;
                let response = routes.get(&path).cloned().unwrap_or_else(|| {
                    b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                        .to_vec()
                });
                let _ = stream.write_all(&response).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    port
}

fn ok(body: &[u8]) -> Vec<u8> {
    let mut response = format!(
        "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    response.extend_from_slice(body);
    response
}

fn status(code: u16) -> Vec<u8> {
    format!("HTTP/1.1 {code} X\r\ncontent-length: 0\r\nconnection: close\r\n\r\n").into_bytes()
}

fn redirect(code: u16, location: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 {code} Moved\r\nlocation: {location}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
    )
    .into_bytes()
}

struct Fixture {
    outbound: OutboundHttp,
    origin: String,
    directory: tempfile::TempDir,
}

impl Fixture {
    async fn new(routes: Routes) -> Self {
        let port = serve(routes).await;
        let tls = crate::net::trust::client_config().unwrap();
        let outbound = OutboundHttp::with_lookup(tls, Arc::new(LoopbackLookup));
        let origin = format!("http://{HOST}:{port}");
        outbound
            .register_anchored_origins(&origin, &[], None)
            .await
            .unwrap();
        Self {
            outbound,
            origin,
            directory: tempfile::tempdir().unwrap(),
        }
    }

    fn destination(&self) -> PathBuf {
        self.directory.path().join("file.bin")
    }

    async fn download(
        &self,
        path: &str,
        sha256: Option<&str>,
        max_bytes: u64,
    ) -> Result<(), DownloadError> {
        let url = format!("{}{path}", self.origin);
        let sha256 = sha256.map(Value::from);
        download(
            &self.outbound,
            &InFlight::new(),
            Some(&url),
            &self.destination(),
            sha256.as_ref(),
            max_bytes,
        )
        .await
    }

    fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

fn sha256_hex(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}

fn chain(hops: usize) -> Routes {
    let mut routes = Routes::new();
    for hop in 0..hops {
        // Relative and absolute Locations both resolve against the hop's URL.
        let location = if hop % 2 == 0 {
            format!("/hop{}", hop + 1)
        } else {
            format!("hop{}", hop + 1)
        };
        routes.insert(
            format!("/hop{hop}"),
            redirect(if hop % 2 == 0 { 301 } else { 307 }, &location),
        );
    }
    routes.insert(format!("/hop{hops}"), ok(b"payload"));
    routes
}

#[tokio::test]
async fn follows_five_redirects_and_verifies_the_checksum() {
    let fixture = Fixture::new(chain(5)).await;
    let expected = format!("  {}\n", sha256_hex(b"payload").to_uppercase());
    fixture
        .download("/hop0", Some(&expected), DOWNLOAD_MAX_BYTES)
        .await
        .unwrap();
    assert_eq!(std::fs::read(fixture.destination()).unwrap(), b"payload");
    assert_eq!(fixture.names(), ["file.bin"]);
}

#[tokio::test]
async fn a_sixth_redirect_fails() {
    let fixture = Fixture::new(chain(6)).await;
    assert_eq!(
        fixture.download("/hop0", None, DOWNLOAD_MAX_BYTES).await,
        Err(failed("Too many download redirects"))
    );
    assert!(fixture.names().is_empty());
}

#[tokio::test]
async fn an_unusable_redirect_target_fails() {
    let routes = Routes::from([("/r".to_owned(), redirect(302, "ftp://files.test/x"))]);
    let fixture = Fixture::new(routes).await;
    assert_eq!(
        fixture.download("/r", None, DOWNLOAD_MAX_BYTES).await,
        Err(failed("HTTP 302 redirect target is unusable"))
    );
}

#[tokio::test]
async fn a_checksum_mismatch_keeps_the_existing_file() {
    let routes = Routes::from([("/f".to_owned(), ok(b"tampered"))]);
    let fixture = Fixture::new(routes).await;
    std::fs::write(fixture.destination(), b"previous").unwrap();
    let expected = sha256_hex(b"original");
    assert_eq!(
        fixture
            .download("/f", Some(&expected), DOWNLOAD_MAX_BYTES)
            .await,
        Err(DownloadError::Checksum(format!(
            "Download checksum mismatch: expected {expected}, downloaded {}",
            sha256_hex(b"tampered")
        )))
    );
    assert_eq!(std::fs::read(fixture.destination()).unwrap(), b"previous");
    assert_eq!(fixture.names(), ["file.bin"]);
}

#[tokio::test]
async fn a_body_over_the_cap_fails_and_leaves_nothing() {
    let routes = Routes::from([
        ("/big".to_owned(), ok(&[7u8; 11])),
        ("/fits".to_owned(), ok(&[7u8; 10])),
    ]);
    let fixture = Fixture::new(routes).await;
    assert_eq!(
        fixture.download("/big", None, 10).await,
        Err(failed("Download exceeds 10 bytes"))
    );
    assert!(fixture.names().is_empty());
    fixture.download("/fits", None, 10).await.unwrap();
    assert_eq!(std::fs::read(fixture.destination()).unwrap(), [7u8; 10]);
}

#[tokio::test]
async fn statuses_other_than_success_fail_and_no_content_writes_an_empty_file() {
    let routes = Routes::from([
        ("/empty".to_owned(), status(204)),
        ("/gone".to_owned(), status(410)),
    ]);
    let fixture = Fixture::new(routes).await;
    assert_eq!(
        fixture.download("/missing", None, DOWNLOAD_MAX_BYTES).await,
        Err(failed("HTTP 404"))
    );
    assert_eq!(
        fixture.download("/gone", None, DOWNLOAD_MAX_BYTES).await,
        Err(failed("HTTP 410"))
    );
    assert!(fixture.names().is_empty());
    fixture
        .download("/empty", Some(&sha256_hex(b"")), DOWNLOAD_MAX_BYTES)
        .await
        .unwrap();
    assert_eq!(std::fs::read(fixture.destination()).unwrap(), b"");
}

#[tokio::test]
async fn targets_and_checksums_are_validated_before_any_request() {
    let fixture = Fixture::new(Routes::new()).await;
    let gets = InFlight::new();
    let attempt = |url: &'static str, sha256: Option<Value>| {
        let fixture = &fixture;
        let gets = &gets;
        async move {
            download(
                &fixture.outbound,
                gets,
                Some(url),
                &fixture.destination(),
                sha256.as_ref(),
                DOWNLOAD_MAX_BYTES,
            )
            .await
        }
    };
    let not_http = Err(failed("Download URL must use http or https"));
    assert_eq!(attempt("ftp://files.test/x", None).await, not_http);
    assert_eq!(
        attempt("https://user:pw@files.test/x", None).await,
        not_http
    );
    let malformed = Err(failed("Download checksum is malformed"));
    assert_eq!(
        attempt("https://files.test/x", Some(Value::from("abc"))).await,
        malformed
    );
    assert_eq!(
        attempt("https://files.test/x", Some(Value::from(5))).await,
        malformed
    );
    // An origin no instance registered must use https and resolve publicly.
    assert_eq!(
        attempt("http://elsewhere.test/x", None).await,
        Err(failed(OutboundError::InsecureTransport.to_string()))
    );
    assert_eq!(
        attempt("https://elsewhere.test/x", None).await,
        Err(failed(OutboundError::PublicAddressRequired.to_string()))
    );
}

#[tokio::test]
async fn the_thirty_third_concurrent_request_is_refused() {
    let routes = Routes::from([("/f".to_owned(), ok(b"x"))]);
    let fixture = Fixture::new(routes).await;
    let gets = InFlight::new();
    let held: Vec<_> = (0..MAX_IN_FLIGHT)
        .map(|_| gets.acquire().unwrap())
        .collect();
    let url = format!("{}/f", fixture.origin);
    let destination = fixture.destination();
    let attempt = || {
        download(
            &fixture.outbound,
            &gets,
            Some(&url),
            &destination,
            None,
            DOWNLOAD_MAX_BYTES,
        )
    };
    assert_eq!(attempt().await, Err(failed(CAPACITY_MESSAGE)));
    drop(held);
    attempt().await.unwrap();
    // The slot was released with the body.
    assert_eq!(gets.0.load(Ordering::Acquire), 0);
}

#[test]
fn suggested_names_keep_only_their_last_component() {
    assert_eq!(
        suggested_file_name("photo.png").as_deref(),
        Some("photo.png")
    );
    assert_eq!(
        suggested_file_name("../../.ssh/authorized_keys").as_deref(),
        Some("authorized_keys")
    );
    assert_eq!(
        suggested_file_name("/etc/passwd").as_deref(),
        Some("passwd")
    );
    assert_eq!(suggested_file_name(".."), None);
    assert_eq!(suggested_file_name(""), None);
}
