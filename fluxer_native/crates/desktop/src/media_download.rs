// SPDX-License-Identifier: AGPL-3.0-or-later
//! Credential-free HTTP media spooling. Decode only a bounded local file, never
//! a remote playlist or URL with application authentication attached.
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::Duration,
};

const MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const WORKERS: usize = 4;

pub(super) struct Download {
    receiver: mpsc::Receiver<Result<Arc<tempfile::NamedTempFile>, String>>,
    cancelled: Arc<AtomicBool>,
    bytes: Arc<AtomicU64>,
    total: Arc<AtomicU64>,
}
impl Download {
    pub fn start(url: String, size: Option<u64>) -> Result<Self, String> {
        if crate::image_gallery::safe_image_url(&url).is_none() {
            return Err("Invalid media URL".into());
        }
        if size.is_some_and(|size| size > MAX_BYTES) {
            return Err("This media file is too large for native playback".into());
        }
        let (tx, receiver) = mpsc::sync_channel(1);
        let cancelled = Arc::new(AtomicBool::new(false));
        let bytes = Arc::new(AtomicU64::new(0));
        let total = Arc::new(AtomicU64::new(size.unwrap_or(0)));
        let job = Job {
            url,
            cancelled: cancelled.clone(),
            bytes: bytes.clone(),
            total: total.clone(),
            result: tx,
        };
        service()
            .try_send(job)
            .map_err(|_| "Media loading is busy; try again".to_string())?;
        Ok(Self {
            receiver,
            cancelled,
            bytes,
            total,
        })
    }
    pub fn progress(&self) -> Option<f32> {
        let total = self.total.load(Ordering::Relaxed);
        (total > 0).then(|| {
            (self.bytes.load(Ordering::Relaxed) as f64 / total as f64).clamp(0., 1.) as f32
        })
    }
    pub fn try_finish(&self) -> Option<Result<Arc<tempfile::NamedTempFile>, String>> {
        match self.receiver.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(Err("Media download stopped".into())),
        }
    }
}
impl Drop for Download {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

struct Job {
    url: String,
    cancelled: Arc<AtomicBool>,
    bytes: Arc<AtomicU64>,
    total: Arc<AtomicU64>,
    result: mpsc::SyncSender<Result<Arc<tempfile::NamedTempFile>, String>>,
}
fn service() -> &'static mpsc::SyncSender<Job> {
    static SERVICE: OnceLock<mpsc::SyncSender<Job>> = OnceLock::new();
    SERVICE.get_or_init(|| {
        let (tx, rx) = mpsc::sync_channel::<Job>(32);
        let rx = Arc::new(Mutex::new(rx));
        for index in 0..WORKERS {
            let rx = rx.clone();
            std::thread::Builder::new()
                .name(format!("media-download-{index}"))
                .spawn(move || {
                    let client = client_builder(Duration::from_secs(20)).build();
                    loop {
                        let job = match rx.lock() {
                            Ok(rx) => rx.recv(),
                            Err(_) => return,
                        };
                        let Ok(job) = job else { return };
                        let result = match &client {
                            Ok(client) => fetch(client, &job),
                            Err(_) => Err("Could not start the media downloader".into()),
                        };
                        let _ = job.result.send(result.map(Arc::new));
                    }
                })
                .expect("spawn media download worker");
        }
        tx
    })
}

// Blocking Response::read applies this timeout to each read operation. The
// underlying async client's timeout separately bounds the whole transfer.
// Do not use async read_timeout here: reqwest 0.12.24 constructs its read timer
// outside the blocking client's Tokio reactor when the response body is read.
fn client_builder(read_timeout: Duration) -> reqwest::blocking::ClientBuilder {
    reqwest::blocking::ClientBuilder::from(
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(600))
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= 5 {
                    attempt.error("Too many media redirects")
                } else if crate::image_gallery::safe_image_url(attempt.url().as_str()).is_none() {
                    attempt.error("Invalid media redirect")
                } else {
                    attempt.follow()
                }
            })),
    )
    .timeout(read_timeout)
}

fn cache_directory() -> Result<PathBuf, String> {
    // /tmp is RAM-backed on the development desktop. Large files belong in the
    // application's disk cache, not a shared temporary-memory filesystem.
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .ok_or("No media cache directory is available")?;
    let directory = base.join("fluxer-native").join("media");
    fs::create_dir_all(&directory).map_err(|_| "Could not create the media cache")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
            .map_err(|_| "Could not secure the media cache")?;
    }
    Ok(directory)
}

fn fetch(client: &reqwest::blocking::Client, job: &Job) -> Result<tempfile::NamedTempFile, String> {
    if job.cancelled.load(Ordering::Relaxed) {
        return Err("Media download cancelled".into());
    }
    fetch_to(client, job, &cache_directory()?, MAX_BYTES)
}

fn fetch_to(
    client: &reqwest::blocking::Client,
    job: &Job,
    directory: &Path,
    limit: u64,
) -> Result<tempfile::NamedTempFile, String> {
    if job.cancelled.load(Ordering::Relaxed) {
        return Err("Media download cancelled".into());
    }
    // No Fluxer authorization, cookies, Origin overrides or credentials are
    // copied into this client. Signed proxy query fields are kept verbatim.
    let response = client
        .get(&job.url)
        .send()
        .map_err(|_| "Could not download media")?;
    if !response.status().is_success() {
        return Err(format!(
            "Could not load media (HTTP {})",
            response.status().as_u16()
        ));
    }
    if let Some(length) = response.content_length() {
        if length > limit {
            return Err("This media file is too large for native playback".into());
        }
        job.total.store(length, Ordering::Relaxed);
    }
    let mut file = tempfile::Builder::new()
        .prefix("playback-")
        .tempfile_in(directory)
        .map_err(|_| "Could not create the media cache file")?;
    copy_bounded(
        response,
        file.as_file_mut(),
        limit,
        &job.cancelled,
        &job.bytes,
    )?;
    file.as_file_mut()
        .flush()
        .map_err(|_| "Could not finish caching media")?;
    Ok(file)
}

fn copy_bounded(
    mut source: impl Read,
    mut target: impl Write,
    limit: u64,
    cancelled: &AtomicBool,
    progress: &AtomicU64,
) -> Result<(), String> {
    let mut buffer = [0_u8; 64 * 1024];
    let mut written = 0_u64;
    loop {
        if cancelled.load(Ordering::Relaxed) {
            return Err("Media download cancelled".into());
        }
        let count = source
            .read(&mut buffer)
            .map_err(|_| "Media download was interrupted")?;
        // Cancellation may arrive while read blocks; do not persist that chunk
        // or publish a successful file after its owner has gone away.
        if cancelled.load(Ordering::Relaxed) {
            return Err("Media download cancelled".into());
        }
        if count == 0 {
            return Ok(());
        }
        written = written
            .checked_add(count as u64)
            .filter(|written| *written <= limit)
            .ok_or("This media file is too large for native playback")?;
        target
            .write_all(&buffer[..count])
            .map_err(|_| "Could not cache media on disk")?;
        progress.store(written, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_limit_and_cancellation_do_not_write_an_oversized_chunk() {
        let mut output = vec![];
        let bytes = AtomicU64::new(0);
        let cancel = AtomicBool::new(false);
        assert!(copy_bounded(&b"abcdef"[..], &mut output, 5, &cancel, &bytes).is_err());
        assert!(output.is_empty());
        cancel.store(true, Ordering::Relaxed);
        assert!(copy_bounded(&b"abc"[..], &mut output, 5, &cancel, &bytes).is_err());
        assert!(output.is_empty());
    }

    #[test]
    fn media_spooling_preserves_exact_bytes_with_bounded_buffering() {
        let data: Vec<_> = (0..200_000).map(|index| (index % 251) as u8).collect();
        let mut output = vec![];
        let bytes = AtomicU64::new(0);
        copy_bounded(
            data.as_slice(),
            &mut output,
            data.len() as u64,
            &AtomicBool::new(false),
            &bytes,
        )
        .unwrap();
        assert_eq!(output, data);
        assert_eq!(bytes.load(Ordering::Relaxed), data.len() as u64);
    }
}

#[cfg(test)]
mod http_contract_tests {
    use super::*;
    use std::{
        io::Cursor,
        net::{TcpListener, TcpStream},
        thread,
    };

    fn request_headers(socket: &mut TcpStream) -> String {
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut request = Vec::new();
        let mut byte = [0; 1];
        while !request.ends_with(b"\r\n\r\n") {
            assert!(request.len() < 16 * 1024, "unbounded request headers");
            assert_eq!(socket.read(&mut byte).unwrap(), 1);
            request.push(byte[0]);
        }
        String::from_utf8(request).unwrap()
    }

    fn client() -> reqwest::blocking::Client {
        client_builder(Duration::from_secs(3))
            .no_proxy()
            .build()
            .unwrap()
    }

    fn job(url: String) -> Job {
        let (result, _) = mpsc::sync_channel(1);
        Job {
            url,
            cancelled: Arc::new(AtomicBool::new(false)),
            bytes: Arc::new(AtomicU64::new(0)),
            total: Arc::new(AtomicU64::new(0)),
            result,
        }
    }

    fn server(response: &[u8]) -> (String, thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/clip", listener.local_addr().unwrap());
        let response = response.to_vec();
        let handle = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let request = request_headers(&mut socket);
            socket.write_all(&response).unwrap();
            request
        });
        (url, handle)
    }

    fn cache_entries(directory: &Path) -> usize {
        fs::read_dir(directory).unwrap().count()
    }

    #[test]
    fn actual_http_get_preserves_signed_query_omits_account_credentials_and_unlinks_cache() {
        let (url, server) =
            server(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nmedia");
        let job = job(format!("{url}?signature=a%2Fb%20c&ex=123&hm=456"));
        let cache = tempfile::tempdir().unwrap();
        let file = fetch_to(&client(), &job, cache.path(), 1024).unwrap();
        let request = server.join().unwrap();
        assert!(
            request.starts_with("GET /clip?signature=a%2Fb%20c&ex=123&hm=456 HTTP/1.1\r\n"),
            "{request}"
        );
        let headers = request.to_ascii_lowercase();
        for forbidden in [
            "\r\nauthorization:",
            "\r\ncookie:",
            "\r\nproxy-authorization:",
            "\r\norigin:",
        ] {
            assert!(!headers.contains(forbidden), "{headers}");
        }
        assert_eq!(fs::read(file.path()).unwrap(), b"media");
        assert_eq!(job.bytes.load(Ordering::Relaxed), 5);
        assert_eq!(job.total.load(Ordering::Relaxed), 5);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                file.as_file().metadata().unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let path = file.path().to_owned();
        assert_eq!(cache_entries(cache.path()), 1);
        drop(file);
        assert!(!path.exists());
        assert_eq!(cache_entries(cache.path()), 0);
    }

    #[test]
    fn http_status_truncation_and_both_size_limits_leave_no_partial_cache_files() {
        for (response, limit, error_fragment) in [
            (&b"HTTP/1.1 404 Not Found\r\nContent-Length: 4\r\nConnection: close\r\n\r\noops"[..], 20, "HTTP 404"),
            (&b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\nshort"[..], 200, "interrupted"),
            (&b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\n123456"[..], 5, "too large"),
            (&b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n6\r\n123456\r\n0\r\n\r\n"[..], 5, "too large"),
        ] {
            let (url, server) = server(response);
            let cache = tempfile::tempdir().unwrap();
            let error = fetch_to(&client(), &job(url), cache.path(), limit).unwrap_err();
            assert!(error.contains(error_fragment), "expected {error_fragment:?}: {error}");
            server.join().unwrap();
            assert_eq!(cache_entries(cache.path()), 0, "{error}");
        }
    }

    #[test]
    fn cancellation_before_request_does_not_connect_or_create_a_cache_file() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let job = job(format!("http://{}/clip", listener.local_addr().unwrap()));
        job.cancelled.store(true, Ordering::Relaxed);
        let cache = tempfile::tempdir().unwrap();
        assert!(
            fetch_to(&client(), &job, cache.path(), 20)
                .unwrap_err()
                .contains("cancelled")
        );
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert_eq!(cache_entries(cache.path()), 0);
    }

    #[test]
    fn cancellation_while_waiting_for_http_body_discards_the_partial_file() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/clip", listener.local_addr().unwrap());
        let (headers_tx, headers_rx) = mpsc::sync_channel(1);
        let (body_tx, body_rx) = mpsc::sync_channel(1);
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            request_headers(&mut socket);
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\n")
                .unwrap();
            headers_tx.send(()).unwrap();
            body_rx.recv_timeout(Duration::from_secs(3)).unwrap();
            // A cancelled client may already have closed the socket.
            let _ = socket.write_all(b"media");
        });
        let job = job(url);
        let cancelled = job.cancelled.clone();
        let progress = job.bytes.clone();
        let cache = tempfile::tempdir().unwrap();
        let path = cache.path().to_owned();
        let fetch = thread::spawn(move || fetch_to(&client(), &job, &path, 20));
        headers_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        cancelled.store(true, Ordering::Relaxed);
        body_tx.send(()).unwrap();
        assert!(fetch.join().unwrap().unwrap_err().contains("cancelled"));
        server.join().unwrap();
        assert_eq!(progress.load(Ordering::Relaxed), 0);
        assert_eq!(cache_entries(cache.path()), 0);
    }

    #[test]
    fn a_cancellation_delivered_during_read_never_writes_its_returned_chunk() {
        struct CancelOnRead<'a> {
            cancelled: &'a AtomicBool,
            bytes: Cursor<&'static [u8]>,
        }
        impl Read for CancelOnRead<'_> {
            fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
                let count = self.bytes.read(output)?;
                self.cancelled.store(true, Ordering::Relaxed);
                Ok(count)
            }
        }
        let cancelled = AtomicBool::new(false);
        let progress = AtomicU64::new(0);
        let mut output = Vec::new();
        let source = CancelOnRead {
            cancelled: &cancelled,
            bytes: Cursor::new(&b"media"[..]),
        };
        assert!(
            copy_bounded(source, &mut output, 20, &cancelled, &progress)
                .unwrap_err()
                .contains("cancelled")
        );
        assert!(output.is_empty());
        assert_eq!(progress.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn redirects_reject_credentials_and_non_http_schemes_before_contacting_the_target() {
        let target = TcpListener::bind("127.0.0.1:0").unwrap();
        target.set_nonblocking(true).unwrap();
        for location in [
            format!(
                "http://user:secret@{}/private",
                target.local_addr().unwrap()
            ),
            "file:///tmp/not-media".into(),
            "ftp://example.invalid/clip".into(),
        ] {
            let response = format!(
                "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            let (url, server) = server(response.as_bytes());
            let cache = tempfile::tempdir().unwrap();
            let error = fetch_to(&client(), &job(url), cache.path(), 20).unwrap_err();
            assert!(
                error == "Could not download media" || error == "Could not load media (HTTP 302)",
                "{error}"
            );
            // Neither URL credentials nor the target URL enter diagnostics.
            assert!(!error.contains("secret"));
            server.join().unwrap();
            assert_eq!(cache_entries(cache.path()), 0);
        }
        assert_eq!(
            target.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn a_stalled_response_body_hits_the_per_read_timeout_and_removes_its_file() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/clip", listener.local_addr().unwrap());
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            request_headers(&mut socket);
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\n")
                .unwrap();
            release_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        });
        let client = client_builder(Duration::from_millis(100))
            .no_proxy()
            .build()
            .unwrap();
        let cache = tempfile::tempdir().unwrap();
        let error = fetch_to(&client, &job(url), cache.path(), 20).unwrap_err();
        release_tx.send(()).unwrap();
        server.join().unwrap();
        assert_eq!(error, "Media download was interrupted");
        assert_eq!(cache_entries(cache.path()), 0);
    }
}
