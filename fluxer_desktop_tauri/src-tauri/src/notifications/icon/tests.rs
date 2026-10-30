// SPDX-License-Identifier: AGPL-3.0-or-later

use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::*;
use crate::local_app::url::origin;
use crate::net::outbound::{HostLookup, LookupFuture};

const JPEG_2X3: &[u8] = include_bytes!("../testdata/red-2x3.jpg");

/// Every host is 127.0.0.1; the names asked for are recorded.
#[derive(Default)]
struct LoopbackLookup {
    calls: Mutex<Vec<String>>,
}

impl HostLookup for LoopbackLookup {
    fn lookup(&self, hostname: String) -> LookupFuture {
        self.calls.lock().unwrap().push(hostname);
        Box::pin(async { Ok(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]) })
    }
}

struct Fixture {
    outbound: OutboundHttp,
    lookup: Arc<LoopbackLookup>,
    files: LocalAppFiles,
    _root: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("index.html"), "<!doctype html>").unwrap();
        std::fs::create_dir(root.path().join("assets")).unwrap();
        std::fs::write(root.path().join("assets/icon.png"), png(1, 1)).unwrap();
        std::fs::create_dir(root.path().join("api")).unwrap();
        std::fs::write(root.path().join("api/icon.png"), png(1, 1)).unwrap();
        let lookup = Arc::new(LoopbackLookup::default());
        let tls = crate::net::trust::client_config().unwrap();
        Self {
            outbound: OutboundHttp::with_lookup(tls, lookup.clone()),
            lookup,
            files: LocalAppFiles::new(root.path().to_owned()),
            _root: root,
        }
    }

    async fn resolve(&self, source: &str) -> Result<RgbaImage, IconError> {
        resolve(
            source,
            IconSources {
                outbound: &self.outbound,
                files: &self.files,
            },
        )
        .await
    }

    /// An origin on a local server, registered the way a loopback instance is (any address).
    async fn register(&self, port: u16) -> String {
        let origin = format!("http://icons.test:{port}");
        self.outbound
            .register_anchored_origins(&origin, &[], None)
            .await
            .unwrap();
        origin
    }
}

fn png(width: u32, height: u32) -> Vec<u8> {
    png_image::encode(&RgbaImage {
        width,
        height,
        pixels: vec![0x80; (width * height * 4) as usize],
    })
}

fn base64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

async fn read_path(stream: &mut TcpStream) -> String {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte).await.unwrap_or(0) == 0 {
            break;
        }
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head).into_owned();
    head.split(' ').nth(1).unwrap_or_default().to_owned()
}

/// `/r/<n>` redirects to `/r/<n - 1>`, `/r/0` is a PNG, `/big` declares 3 MiB + 1 bytes,
/// `/stream` sends 3 MiB + 1 bytes without a length, anything else is a 404.
async fn serve() -> u16 {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let path = read_path(&mut stream).await;
                let response = match path.strip_prefix("/r/").map(str::parse::<u32>) {
                    Some(Ok(0)) => {
                        let body = png(2, 2);
                        let mut response =
                            format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n", body.len())
                                .into_bytes();
                        response.extend_from_slice(&body);
                        response
                    }
                    Some(Ok(hops)) => format!(
                        "HTTP/1.1 302 Found\r\nlocation: /r/{}\r\ncontent-length: 0\r\n\r\n",
                        hops - 1
                    )
                    .into_bytes(),
                    _ if path == "/big" => format!(
                        "HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n",
                        MAX_BYTES + 1
                    )
                    .into_bytes(),
                    _ if path == "/stream" => {
                        let _ = stream
                            .write_all(b"HTTP/1.1 200 OK\r\nconnection: close\r\n\r\n")
                            .await;
                        let chunk = vec![0u8; 64 * 1024];
                        for _ in 0..(MAX_BYTES / chunk.len() as u64 + 1) {
                            if stream.write_all(&chunk).await.is_err() {
                                return;
                            }
                        }
                        return;
                    }
                    _ => b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\n\r\n".to_vec(),
                };
                let _ = stream.write_all(&response).await;
            });
        }
    });
    port
}

#[tokio::test]
async fn data_urls_decode_by_their_media_type() {
    let fixture = Fixture::new();
    let image = fixture
        .resolve(&format!("data:image/png;base64,{}", base64(&png(3, 2))))
        .await
        .unwrap();
    assert_eq!((image.width, image.height, image.pixels.len()), (3, 2, 24));

    let jpeg = fixture
        .resolve(&format!("data:IMAGE/JPEG;base64,{}", base64(JPEG_2X3)))
        .await
        .unwrap();
    assert_eq!((jpeg.width, jpeg.height, jpeg.pixels.len()), (2, 3, 24));
    // Red-ish after lossy compression, opaque.
    assert!(jpeg.pixels[0] > 150 && jpeg.pixels[1] < 80 && jpeg.pixels[3] == 255);

    // Unpadded, line-wrapped base64 still decodes.
    let wrapped: String = base64(&png(1, 1))
        .trim_end_matches('=')
        .chars()
        .enumerate()
        .flat_map(|(index, c)| (index % 16 == 15).then_some('\n').into_iter().chain([c]))
        .collect();
    assert!(
        fixture
            .resolve(&format!("data:image/png;base64,{wrapped}"))
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn data_urls_with_another_media_type_or_bad_bytes_are_refused() {
    let fixture = Fixture::new();
    for source in [
        format!("data:text/plain;base64,{}", base64(&png(1, 1))),
        format!("data:image/gif;base64,{}", base64(&png(1, 1))),
        format!("data:image/png;base64,{}", base64(JPEG_2X3)),
        "data:image/png;base64,!!!".to_owned(),
        "data:image/png".to_owned(),
    ] {
        assert!(
            matches!(fixture.resolve(&source).await, Err(IconError::Undecodable)),
            "{source}"
        );
    }
}

#[tokio::test]
async fn other_schemes_are_refused_without_network() {
    let fixture = Fixture::new();
    for source in [
        "file:///etc/passwd",
        "ftp://icons.test/a.png",
        "/assets/icon.png",
        "javascript:alert(1)",
        "HTTPS://icons.test/a.png",
    ] {
        assert!(
            matches!(
                fixture.resolve(source).await,
                Err(IconError::UnsupportedSource)
            ),
            "{source}"
        );
    }
    assert!(fixture.lookup.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn local_app_urls_are_read_from_the_renderer_files() {
    let fixture = Fixture::new();
    let origin = origin();
    let image = fixture
        .resolve(&format!("{origin}/assets/icon.png?v=1"))
        .await
        .unwrap();
    assert_eq!((image.width, image.height), (1, 1));
    assert!(matches!(
        fixture
            .resolve(&format!("{origin}/assets/missing.png"))
            .await,
        Err(IconError::LocalFile(_))
    ));
    // An extensionless path falls back to index.html, which is no icon.
    assert!(matches!(
        fixture.resolve(&format!("{origin}/channels/@me")).await,
        Err(IconError::LocalFile(_))
    ));
    assert!(fixture.lookup.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn local_app_api_and_proxy_routes_are_refused() {
    let fixture = Fixture::new();
    let origin = origin();
    for path in [
        "/api/icon.png",
        "/api",
        "/proxy?url=https%3A%2F%2Ficons.test%2Fa.png",
        "/proxy/a.png",
    ] {
        assert!(
            matches!(
                fixture.resolve(&format!("{origin}{path}")).await,
                Err(IconError::ReservedLocalPath)
            ),
            "{path}"
        );
    }
    assert!(fixture.lookup.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn unregistered_origins_need_https_and_a_public_address() {
    let fixture = Fixture::new();
    let cleartext = fixture.resolve("http://icons.test/a.png").await;
    assert!(
        matches!(cleartext, Err(IconError::Download(_))),
        "{cleartext:?}"
    );
    assert!(fixture.lookup.calls.lock().unwrap().is_empty());
    // icons.test resolves to loopback here.
    let private = fixture.resolve("https://icons.test/a.png").await;
    let Err(IconError::Download(message)) = private else {
        panic!("{private:?}");
    };
    assert!(message.contains("privately resolving"), "{message}");
}

#[tokio::test]
async fn up_to_five_redirects_are_followed() {
    let fixture = Fixture::new();
    let origin = fixture.register(serve().await).await;
    let image = fixture.resolve(&format!("{origin}/r/5")).await.unwrap();
    assert_eq!((image.width, image.height), (2, 2));
    let Err(IconError::Download(message)) = fixture.resolve(&format!("{origin}/r/6")).await else {
        panic!("six redirects were followed");
    };
    assert_eq!(
        message,
        "Notification icon download exceeded redirect limit"
    );
}

#[tokio::test]
async fn bodies_over_three_mebibytes_and_errors_fail() {
    let fixture = Fixture::new();
    let origin = fixture.register(serve().await).await;
    let download_error = |result: Result<RgbaImage, IconError>| match result {
        Err(IconError::Download(message)) => message,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        download_error(fixture.resolve(&format!("{origin}/big")).await),
        "Notification icon download exceeds 3145728 bytes"
    );
    assert_eq!(
        download_error(fixture.resolve(&format!("{origin}/stream")).await),
        "Notification icon download exceeds 3145728 bytes"
    );
    assert_eq!(
        download_error(fixture.resolve(&format!("{origin}/missing")).await),
        "Notification icon download failed with HTTP 404"
    );
}

#[test]
fn described_sources_drop_queries_and_data() {
    assert_eq!(describe("data:image/png;base64,AAAA"), "data-url");
    assert_eq!(
        describe("https://cdn.test/avatars/1.png?token=secret#x"),
        "https://cdn.test/avatars/1.png"
    );
    assert_eq!(
        describe("http://cdn.test:8080/a.png"),
        "http://cdn.test:8080/a.png"
    );
    assert_eq!(describe("assets/icon.png"), "icon.png");
}
