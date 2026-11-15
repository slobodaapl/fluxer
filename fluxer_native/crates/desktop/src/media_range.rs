// SPDX-License-Identifier: AGPL-3.0-or-later
//! Credential-free seekable HTTP source. FFmpeg receives bytes, never a URL.
use crate::media_download::{self, MAX_BYTES};
use reqwest::{blocking::Client, header};
use std::{
    io::{self, Read, Seek, SeekFrom, Write},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::Duration,
};
#[path = "media_resource_cache.rs"]
mod resource_cache;

fn http_client() -> Result<Client, String> {
    static CLIENT: OnceLock<Result<Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            media_download::client_builder(Duration::from_secs(2))
                .connect_timeout(Duration::from_secs(2))
                .build()
                .map_err(|_| "Could not start the media downloader".to_owned())
        })
        .clone()
}
const CHUNK: u64 = 256 * 1024;
fn bad(message: &'static str) -> io::Error {
    io::Error::other(message)
}

#[derive(Default)]
pub(super) struct RangeProgress {
    active: AtomicBool,
    bytes: AtomicU64,
    total: AtomicU64,
    waiting: Arc<AtomicBool>,
}
impl RangeProgress {
    pub fn wait_handle(&self) -> Arc<AtomicBool> {
        self.waiting.clone()
    }
    pub fn progress(&self) -> Option<f32> {
        let total = self.total.load(Ordering::Relaxed);
        (self.active.load(Ordering::Relaxed) && total > 0).then(|| {
            (self.bytes.load(Ordering::Relaxed) as f64 / total as f64).clamp(0.0, 1.0) as f32
        })
    }
}
struct WaitingGuard(Arc<AtomicBool>);
impl WaitingGuard {
    fn new(waiting: Arc<AtomicBool>) -> Self {
        waiting.store(true, Ordering::Release);
        Self(waiting)
    }
}
impl Drop for WaitingGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
pub(super) struct RangeReader {
    progress: Arc<RangeProgress>,
    url: String,
    client: Client,
    cancel: Arc<AtomicBool>,
    interrupt: Arc<AtomicU64>,
    size: Option<u64>,
    pos: u64,
    chunk_at: u64,
    chunk: Vec<u8>,
    validator: Option<(header::HeaderName, header::HeaderValue)>,
    fallback: Option<resource_cache::Body>,
    prefetch: Option<(u64, mpsc::Receiver<io::Result<Vec<u8>>>)>,
    allow_prefetch: bool,
}
impl RangeReader {
    pub fn invalidate_cache(url: &str) {
        resource_cache::invalidate(url);
    }
    pub fn new(
        url: String,
        expected_size: Option<u64>,
        cancel: Arc<AtomicBool>,
        interrupt: Arc<AtomicU64>,
    ) -> Result<Self, String> {
        if crate::image_gallery::safe_image_url(&url).is_none() {
            return Err("Invalid media URL".into());
        }
        if expected_size.is_some_and(|size| size > MAX_BYTES) {
            return Err("This media file is too large for native playback".into());
        }
        let client = http_client()?;
        Ok(Self {
            url,
            progress: Arc::new(RangeProgress::default()),
            client,
            cancel,
            interrupt,
            size: None,
            pos: 0,
            chunk_at: 0,
            chunk: Vec::new(),
            validator: None,
            fallback: None,
            prefetch: None,
            allow_prefetch: true,
        })
    }
    pub fn progress_handle(&self) -> Arc<RangeProgress> {
        self.progress.clone()
    }
    fn cancelled(&self) -> io::Result<()> {
        if self.cancel.load(Ordering::Acquire) {
            Err(bad("Media loading cancelled"))
        } else {
            Ok(())
        }
    }
    fn request(&mut self, start: u64) -> io::Result<()> {
        self.cancelled()?;
        // Align seeks so a previously fetched range also serves interior reads.
        let start = start / CHUNK * CHUNK;
        if let Some(range) = resource_cache::get_range(&self.url, start)
            && self.size.is_none_or(|size| size == range.total)
            && self
                .validator
                .as_ref()
                .is_none_or(|validator| validator == &range.validator)
        {
            self.size = Some(range.total);
            self.validator = Some(range.validator.clone());
            self.chunk_at = start;
            self.chunk = range.bytes.to_vec();
            self.schedule_prefetch();
            return Ok(());
        }
        let _waiting = self
            .allow_prefetch
            .then(|| WaitingGuard::new(self.progress.waiting.clone()));
        self.cancelled()?;
        let generation = self.interrupt.load(Ordering::Acquire);
        if start >= MAX_BYTES {
            return Err(bad("Media exceeds size limit"));
        }
        let end = start.saturating_add(CHUNK - 1).min(MAX_BYTES - 1);
        let mut request = self
            .client
            .get(&self.url)
            .header(header::ACCEPT_ENCODING, "identity")
            .header(header::RANGE, format!("bytes={start}-{end}"));
        if let Some((_, value)) = &self.validator {
            request = request.header(header::IF_RANGE, value.clone());
        }
        let mut response = request.send().map_err(|_| bad("Media request failed"))?;
        self.cancelled()?;
        if self.interrupt.load(Ordering::Acquire) != generation {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "Media seek"));
        }
        if response
            .headers()
            .get(header::CONTENT_ENCODING)
            .is_some_and(|v| v.as_bytes() != b"identity")
        {
            return Err(bad("Encoded media response"));
        }
        if response.status() == reqwest::StatusCode::OK && self.size.is_none() && start == 0 {
            return self.full_download(response);
        }
        if response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
            return Err(bad("Media range request rejected"));
        }
        let range = response
            .headers()
            .get(header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| bad("Missing content range"))?;
        let (actual_start, actual_end, total) =
            parse_range(range).ok_or_else(|| bad("Invalid content range"))?;
        if total == 0
            || total > MAX_BYTES
            || actual_start != start
            || actual_end != end.min(total - 1)
            || self.size.is_some_and(|size| size != total)
        {
            return Err(bad("Inconsistent content range"));
        }
        let len = actual_end - actual_start + 1;
        if response.content_length() != Some(len) {
            return Err(bad("Invalid media range length"));
        }
        let expires = resource_cache::expiration(response.headers());
        if self.size.is_none() {
            self.validator = response
                .headers()
                .get(header::ETAG)
                .filter(|v| !v.as_bytes().starts_with(b"W/"))
                .cloned()
                .map(|v| (header::ETAG, v))
                .or_else(|| {
                    response
                        .headers()
                        .get(header::LAST_MODIFIED)
                        .cloned()
                        .map(|v| (header::LAST_MODIFIED, v))
                });
            if self.validator.is_none() {
                drop(response);
                self.cancelled()?;
                let response = self
                    .client
                    .get(&self.url)
                    .header(header::ACCEPT_ENCODING, "identity")
                    .send()
                    .map_err(|_| bad("Media request failed"))?;
                if response.status() != reqwest::StatusCode::OK {
                    return Err(bad("Media download rejected"));
                }
                return self.full_download(response);
            }
            self.size = Some(total);
        } else if let Some((name, value)) = &self.validator
            && response.headers().get(name) != Some(value)
        {
            return Err(bad("Media changed during playback"));
        }
        let mut chunk = Vec::with_capacity(len as usize);
        let mut buf = [0u8; 32768];
        loop {
            self.cancelled()?;
            let n = response.read(&mut buf)?;
            self.cancelled()?;
            if self.interrupt.load(Ordering::Acquire) != generation {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "Media seek"));
            }
            if n == 0 {
                break;
            }
            if chunk.len() + n > len as usize {
                return Err(bad("Invalid media range length"));
            }
            chunk.extend_from_slice(&buf[..n]);
        }
        if chunk.len() != len as usize {
            return Err(bad("Incomplete media range"));
        }
        self.chunk_at = start;
        self.chunk = chunk;
        if start == 0 && self.chunk.len() as u64 == total && expires.is_some() {
            resource_cache::insert(
                &self.url,
                resource_cache::Body::Memory(self.chunk.clone().into()),
                expires,
            );
        } else if let Some(validator) = &self.validator
            && expires.is_some()
        {
            resource_cache::insert_range(
                &self.url,
                start,
                resource_cache::CachedRange {
                    bytes: self.chunk.clone().into(),
                    total,
                    validator: validator.clone(),
                },
                expires,
            );
        }
        self.schedule_prefetch();
        Ok(())
    }
    fn schedule_prefetch(&mut self) {
        if !self.allow_prefetch || self.fallback.is_some() {
            return;
        }
        if let Some((_, rx)) = &self.prefetch {
            match rx.try_recv() {
                Err(mpsc::TryRecvError::Empty) => return,
                _ => self.prefetch = None,
            }
        }
        let Some(size) = self.size else {
            return;
        };
        let next = self.chunk_at.saturating_add(self.chunk.len() as u64);
        if next >= size {
            return;
        }
        let mut source = Self {
            progress: self.progress.clone(),
            url: self.url.clone(),
            client: self.client.clone(),
            cancel: self.cancel.clone(),
            interrupt: self.interrupt.clone(),
            size: Some(size),
            pos: next,
            chunk_at: 0,
            chunk: Vec::new(),
            validator: self.validator.clone(),
            fallback: None,
            prefetch: None,
            allow_prefetch: false,
        };
        let (tx, rx) = mpsc::sync_channel(1);
        if std::thread::Builder::new()
            .name("media-range-prefetch".into())
            .spawn(move || {
                let result = source.request(next).map(|()| source.chunk);
                let _ = tx.send(result);
            })
            .is_ok()
        {
            self.prefetch = Some((next, rx));
        }
    }
    fn full_download(&mut self, mut response: reqwest::blocking::Response) -> io::Result<()> {
        let expires = resource_cache::expiration(response.headers());
        if response
            .headers()
            .get(header::CONTENT_ENCODING)
            .is_some_and(|v| v.as_bytes() != b"identity")
        {
            return Err(bad("Encoded media response"));
        }
        let expected = response.content_length();
        if expected.is_some_and(|length| length > MAX_BYTES) {
            return Err(bad("Media exceeds size limit"));
        }
        let mut file = tempfile::tempfile_in(
            media_download::cache_directory().map_err(|_| bad("Media cache unavailable"))?,
        )
        .map_err(|_| bad("Media cache unavailable"))?;
        self.progress
            .total
            .store(expected.unwrap_or(0), Ordering::Relaxed);
        self.progress.active.store(true, Ordering::Relaxed);
        media_download::copy_bounded(
            &mut response,
            &mut file,
            MAX_BYTES,
            &self.cancel,
            &self.progress.bytes,
        )
        .map_err(|_| bad("Media download failed"))?;
        let length = self.progress.bytes.load(Ordering::Relaxed);
        if expected.is_some_and(|expected| expected != length) {
            return Err(bad("Incomplete media download"));
        }
        file.flush()?;
        self.size = Some(length);
        let body = resource_cache::Body::Disk(Arc::new(Mutex::new(file)), length);
        resource_cache::insert(&self.url, body.clone(), expires);
        self.fallback = Some(body);
        self.progress.active.store(false, Ordering::Relaxed);
        Ok(())
    }
    fn ensure(&mut self) -> io::Result<()> {
        if self.size.is_none() {
            if let Some(body) = resource_cache::get(&self.url) {
                self.size = Some(body.len());
                self.fallback = Some(body);
            } else {
                self.request(0)?;
            }
        }
        Ok(())
    }
}
fn parse_range(value: &str) -> Option<(u64, u64, u64)> {
    let (range, total) = value.strip_prefix("bytes ")?.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let (start, end, total) = (start.parse().ok()?, end.parse().ok()?, total.parse().ok()?);
    if start > end || end >= total {
        return None;
    }
    Some((start, end, total))
}
impl Read for RangeReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        self.ensure()?;
        self.cancelled()?;
        if self.pos >= self.size.unwrap_or(0) {
            return Ok(0);
        }
        if let Some(body) = &self.fallback {
            let n = body.read_at(self.pos, out)?;
            self.pos += n as u64;
            return Ok(n);
        }
        if self.pos < self.chunk_at || self.pos >= self.chunk_at + self.chunk.len() as u64 {
            let prefetched = self.prefetch.as_ref().is_some_and(|(start, _)| {
                self.pos >= *start && self.pos < start.saturating_add(CHUNK)
            });
            if prefetched {
                let (start, rx) = self.prefetch.take().unwrap();
                let _waiting = WaitingGuard::new(self.progress.waiting.clone());
                match rx.recv_timeout(Duration::from_secs(3)) {
                    Ok(Ok(chunk)) if self.pos < start + chunk.len() as u64 => {
                        self.chunk_at = start;
                        self.chunk = chunk;
                        self.schedule_prefetch();
                    }
                    _ => self.request(self.pos)?,
                }
            } else {
                self.request(self.pos)?;
            }
        }
        let offset = (self.pos - self.chunk_at) as usize;
        let n = out.len().min(self.chunk.len() - offset);
        out[..n].copy_from_slice(&self.chunk[offset..offset + n]);
        self.pos += n as u64;
        Ok(n)
    }
}
impl Seek for RangeReader {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        self.ensure()?;
        self.cancelled()?;
        let base = match from {
            SeekFrom::Start(n) => i128::from(n),
            SeekFrom::Current(n) => i128::from(self.pos) + i128::from(n),
            SeekFrom::End(n) => i128::from(self.size.unwrap_or(0)) + i128::from(n),
        };
        if base < 0 || base > i128::from(self.size.unwrap_or(0)) {
            return Err(bad("Invalid media seek"));
        }
        self.pos = base as u64;
        Ok(self.pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{net::TcpListener, sync::Mutex, thread};

    fn server(
        data: Vec<u8>,
        mode: &'static str,
    ) -> (String, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let observed = requests.clone();
        let task = thread::spawn(move || {
            for (index, stream) in listener
                .incoming()
                .take(if mode == "cachemultirange" {
                    3
                } else if mode == "changed" || mode == "novalidator" {
                    2
                } else {
                    1
                })
                .enumerate()
            {
                let mut stream = stream.unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut request = Vec::new();
                let mut one = [0u8; 1];
                while !request.ends_with(b"\r\n\r\n") {
                    if stream.read(&mut one).unwrap_or(0) == 0 {
                        break;
                    }
                    request.push(one[0]);
                }
                let request = String::from_utf8(request).unwrap();
                observed.lock().unwrap().push(request.clone());
                let cache_header = match mode {
                    "cachefull" | "cacherange" | "cachemultirange" => {
                        "Cache-Control: public, max-age=3600\r\n"
                    }
                    "nostorefull" => "Cache-Control: no-store, max-age=3600\r\n",
                    _ => "",
                };
                if mode == "ignored"
                    || mode.ends_with("full")
                    || (mode == "novalidator" && index == 1)
                {
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n{cache_header}Connection: close\r\n\r\n",
                        data.len()
                    )
                    .unwrap();
                    stream.write_all(&data).unwrap();
                    continue;
                }
                let range = request
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("range: bytes=")
                            .map(str::to_owned)
                    })
                    .unwrap();
                let (start, end) = range.split_once('-').unwrap();
                let start: usize = start.parse().unwrap();
                let end: usize = end.parse::<usize>().unwrap().min(data.len() - 1);
                let etag = if mode == "changed" && index == 1 {
                    "\"v2\""
                } else {
                    "\"v1\""
                };
                let reported_start = if mode == "malformed" {
                    start + 1
                } else {
                    start
                };
                let total = if mode == "oversize" {
                    MAX_BYTES + 1
                } else {
                    data.len() as u64
                };
                let length = if mode == "badlength" {
                    end - start + 2
                } else {
                    end - start + 1
                };
                let etag_header = if mode == "novalidator" {
                    String::new()
                } else {
                    format!("ETag: {etag}\r\n")
                };
                let encoding = if mode == "encoding" {
                    "Content-Encoding: gzip\r\n"
                } else {
                    ""
                };
                write!(stream, "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {}-{}/{}\r\nContent-Length: {}\r\n{}{}{cache_header}Connection: close\r\n\r\n", reported_start, end, total, length, etag_header, encoding).unwrap();
                let _ = stream.write_all(&data[start..=end]);
            }
        });
        (format!("http://{addr}/signed?token=a%2Fb"), requests, task)
    }
    #[test]
    fn completed_media_reopens_after_decoder_retirement_without_a_server() {
        for mode in ["cachefull", "cacherange", "cachemultirange"] {
            let length = if mode == "cachemultirange" {
                CHUNK as usize * 2 + 17
            } else {
                65536
            };
            let data: Vec<u8> = (0..length).map(|i| (i % 251) as u8).collect();
            let (url, requests, task) = server(data.clone(), mode);
            let cancel = Arc::new(AtomicBool::new(false));
            let mut reader = RangeReader::new(
                url.clone(),
                None,
                cancel.clone(),
                Arc::new(AtomicU64::new(0)),
            )
            .unwrap();
            let mut first = Vec::new();
            reader.read_to_end(&mut first).unwrap();
            assert_eq!(first, data);
            cancel.store(true, Ordering::Release);
            drop(reader);
            task.join().unwrap(); // Network is now unavailable, not merely fast.
            for _ in 0..3 {
                let mut reader = RangeReader::new(
                    url.clone(),
                    None,
                    Arc::new(AtomicBool::new(false)),
                    Arc::new(AtomicU64::new(0)),
                )
                .unwrap();
                reader.seek(SeekFrom::End(-16)).unwrap();
                let mut tail = Vec::new();
                reader.read_to_end(&mut tail).unwrap();
                assert_eq!(tail, data[data.len() - 16..]);
                reader.seek(SeekFrom::Start(0)).unwrap();
                let mut all = Vec::new();
                reader.read_to_end(&mut all).unwrap();
                assert_eq!(all, data);
            }
            assert_eq!(
                requests.lock().unwrap().len(),
                if mode == "cachemultirange" { 3 } else { 1 },
                "{mode}"
            );
        }
    }

    #[test]
    fn no_store_media_is_not_retained_after_reader_drop() {
        let (url, _, task) = server(vec![5; 64], "nostorefull");
        let mut reader = RangeReader::new(
            url.clone(),
            None,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU64::new(0)),
        )
        .unwrap();
        reader.read_to_end(&mut Vec::new()).unwrap();
        drop(reader);
        task.join().unwrap();
        let mut again = RangeReader::new(
            url,
            None,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU64::new(0)),
        )
        .unwrap();
        assert!(again.read(&mut [0; 1]).is_err());
    }

    #[test]
    fn range_seek_sends_validator_and_rejects_changed_object() {
        let data = vec![42u8; CHUNK as usize + 100];
        let (url, requests, task) = server(data, "changed");
        let cancel = Arc::new(AtomicBool::new(false));
        let mut reader = RangeReader::new(url, None, cancel, Arc::new(AtomicU64::new(0))).unwrap();
        let mut out = [0u8; 4];
        reader.read_exact(&mut out).unwrap();
        assert_eq!(out, [42; 4]);
        reader.seek(SeekFrom::Start(CHUNK)).unwrap();
        assert!(reader.read(&mut out).is_err());
        task.join().unwrap();
        let requests = requests.lock().unwrap();
        assert!(requests[0].starts_with("GET /signed?token=a%2Fb "));
        assert!(
            requests[1]
                .to_ascii_lowercase()
                .contains("if-range: \"v1\"")
        );
        assert!(!requests[0].to_ascii_lowercase().contains("authorization:"));
    }
    #[test]
    fn rejects_malformed_range_and_falls_back_when_range_ignored() {
        let (url, _, task) = server(vec![7u8; 1024], "malformed");
        let mut reader = RangeReader::new(
            url,
            None,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU64::new(0)),
        )
        .unwrap();
        assert!(reader.read(&mut [0u8; 2]).is_err());
        task.join().unwrap();
        let (url, _, task) = server(vec![9u8; 1024], "ignored");
        let mut reader = RangeReader::new(
            url,
            None,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU64::new(0)),
        )
        .unwrap();
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, vec![9; 1024]);
        reader.seek(SeekFrom::End(-2)).unwrap();
        let mut tail = [0; 2];
        reader.read_exact(&mut tail).unwrap();
        assert_eq!(tail, [9; 2]);
        task.join().unwrap();
    }
    #[test]
    fn server_without_validator_uses_a_single_complete_representation() {
        let (url, requests, task) = server(vec![11u8; 1024], "novalidator");
        let mut reader = RangeReader::new(
            url,
            None,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU64::new(0)),
        )
        .unwrap();
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, vec![11; 1024]);
        task.join().unwrap();
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(!requests[1].to_ascii_lowercase().contains("range:"));
    }
    #[test]
    fn rejects_wrong_lengths_encodings_and_oversize() {
        for mode in ["badlength", "encoding", "oversize"] {
            let (url, _, task) = server(vec![3u8; 1024], mode);
            let mut reader = RangeReader::new(
                url,
                None,
                Arc::new(AtomicBool::new(false)),
                Arc::new(AtomicU64::new(0)),
            )
            .unwrap();
            assert!(reader.read(&mut [0u8; 8]).is_err(), "{mode}");
            task.join().unwrap();
        }
        assert!(parse_range("bytes 5-4/6").is_none());
        assert!(parse_range("bytes 0-6/6").is_none());
    }
    #[test]
    fn generated_mp4_starts_and_seeks_without_fetching_full_file() {
        use fluxer_media::{FilePlaybackOptions, FilePlaybackStatus, FilePlayer};
        use std::sync::atomic::AtomicU64;
        for faststart in [true, false] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("sample.mp4");
            let mut command = std::process::Command::new("ffmpeg");
            command.args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=320x240:rate=30:duration=10",
                "-c:v",
                "mpeg4",
                "-q:v",
                "2",
            ]);
            if faststart {
                command.args(["-movflags", "+faststart"]);
            }
            assert!(command.arg(&path).status().unwrap().success());
            let data = Arc::new(std::fs::read(&path).unwrap());
            assert!(
                data.len() > CHUNK as usize * 3,
                "fixture must span several ranges"
            );
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let addr = listener.local_addr().unwrap();
            let stopped = Arc::new(AtomicBool::new(false));
            let server_stop = stopped.clone();
            let stall_next = Arc::new(AtomicBool::new(false));
            let server_stall = stall_next.clone();
            let stall_entered = Arc::new(AtomicBool::new(false));
            let server_entered = stall_entered.clone();
            let stall_release = Arc::new(AtomicBool::new(false));
            let server_release = stall_release.clone();
            let sent = Arc::new(AtomicU64::new(0));
            let server_sent = sent.clone();
            let requests = Arc::new(AtomicU64::new(0));
            let server_requests = requests.clone();
            let server_data = data.clone();
            let server = thread::spawn(move || {
                while !server_stop.load(Ordering::Acquire) {
                    let (mut stream, _) = match listener.accept() {
                        Ok(value) => value,
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(5));
                            continue;
                        }
                        Err(_) => break,
                    };
                    let mut request = Vec::new();
                    let mut byte = [0u8; 1];
                    while !request.ends_with(b"\r\n\r\n") {
                        if stream.read(&mut byte).unwrap_or(0) == 0 {
                            break;
                        }
                        request.push(byte[0]);
                    }
                    let request = String::from_utf8_lossy(&request);
                    let Some(range) = request.lines().find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("range: bytes=")
                            .map(str::to_owned)
                    }) else {
                        break;
                    };
                    server_requests.fetch_add(1, Ordering::Release);
                    if server_stall.swap(false, Ordering::AcqRel) {
                        server_entered.store(true, Ordering::Release);
                        while !server_release.load(Ordering::Acquire)
                            && !server_stop.load(Ordering::Acquire)
                        {
                            thread::sleep(Duration::from_millis(5));
                        }
                    }
                    let (start, end) = range.split_once('-').unwrap();
                    let start: usize = start.parse().unwrap();
                    let end = end.parse::<usize>().unwrap().min(server_data.len() - 1);
                    if write!(stream, "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{end}/{}\r\nContent-Length: {}\r\nETag: \"fixture\"\r\nConnection: close\r\n\r\n", server_data.len(), end-start+1).is_err() { continue; }
                    for part in server_data[start..=end].chunks(16 * 1024) {
                        if stream.write_all(part).is_err() {
                            break;
                        }
                        server_sent.fetch_add(part.len() as u64, Ordering::Relaxed);
                        thread::sleep(Duration::from_millis(8));
                    }
                }
            });
            let cancel = Arc::new(AtomicBool::new(false));
            let interrupt = Arc::new(AtomicU64::new(0));
            let source = RangeReader::new(
                format!("http://{addr}/video.mp4?sig=abc"),
                None,
                cancel.clone(),
                interrupt.clone(),
            )
            .unwrap();
            let waiting = source.progress_handle().wait_handle();
            let player = FilePlayer::open_reader(
                Box::new(source),
                cancel,
                interrupt,
                waiting,
                FilePlaybackOptions::default(),
            )
            .unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(15);
            loop {
                let snapshot = player.snapshot();
                assert_ne!(
                    snapshot.status,
                    FilePlaybackStatus::Failed,
                    "{:?}",
                    snapshot.error
                );
                if player.take_frame().is_some() {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "first frame timed out: {snapshot:?}"
                );
                thread::sleep(Duration::from_millis(10));
            }
            assert!(
                sent.load(Ordering::Relaxed) < data.len() as u64,
                "first frame required full transfer"
            );
            let before = requests.load(Ordering::Acquire);
            player.play();
            let cadence_deadline = std::time::Instant::now() + Duration::from_secs(3);
            let mut frames = 0;
            while std::time::Instant::now() < cadence_deadline {
                if player.take_frame().is_some() {
                    frames += 1;
                }
                assert_ne!(
                    player.snapshot().status,
                    FilePlaybackStatus::Failed,
                    "{:?}",
                    player.snapshot().error
                );
                thread::sleep(Duration::from_millis(10));
            }
            assert!(
                frames >= 30,
                "insufficient frame cadence: {frames} frames in 3 seconds"
            );
            assert!(
                player.snapshot().position >= 2.0,
                "playback clock stalled: {:?}",
                player.snapshot()
            );
            let request_deadline = std::time::Instant::now() + Duration::from_secs(8);
            while requests.load(Ordering::Acquire) == before
                && std::time::Instant::now() < request_deadline
            {
                thread::sleep(Duration::from_millis(5));
            }
            assert!(
                requests.load(Ordering::Acquire) > before,
                "playback did not issue another range"
            );
            stall_next.store(true, Ordering::Release);
            let stall_deadline = std::time::Instant::now() + Duration::from_secs(8);
            while (!stall_entered.load(Ordering::Acquire)
                || player.snapshot().status != FilePlaybackStatus::Loading)
                && std::time::Instant::now() < stall_deadline
            {
                assert_ne!(
                    player.snapshot().status,
                    FilePlaybackStatus::Failed,
                    "{:?}",
                    player.snapshot().error
                );
                thread::sleep(Duration::from_millis(10));
            }
            assert!(
                stall_entered.load(Ordering::Acquire),
                "slow range did not start"
            );
            assert_eq!(
                player.snapshot().status,
                FilePlaybackStatus::Loading,
                "underflow must show buffering"
            );
            let frozen = player.snapshot().position;
            thread::sleep(Duration::from_millis(300));
            assert!(
                (player.snapshot().position - frozen).abs() < 0.05,
                "clock advanced during underflow"
            );
            stall_release.store(true, Ordering::Release);
            let resume_deadline = std::time::Instant::now() + Duration::from_secs(3);
            while player.snapshot().status == FilePlaybackStatus::Loading
                && std::time::Instant::now() < resume_deadline
            {
                thread::sleep(Duration::from_millis(10));
            }
            assert_ne!(
                player.snapshot().status,
                FilePlaybackStatus::Failed,
                "{:?}",
                player.snapshot().error
            );
            assert!(
                player.snapshot().position - frozen < 0.5,
                "clock skipped buffered content after stall"
            );
            player.pause();
            player.seek(3.0);
            player.seek(8.0);
            let reference = FilePlayer::open(path.clone(), FilePlaybackOptions::default()).unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            loop {
                if reference.take_frame().is_some() {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "reference open timed out"
                );
                thread::sleep(Duration::from_millis(10));
            }
            reference.seek(8.0);
            let expected = loop {
                if let Some(frame) = reference.take_frame()
                    && reference.snapshot().position >= 7.9
                {
                    break frame;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "reference seek timed out"
                );
                thread::sleep(Duration::from_millis(10));
            };
            let actual = loop {
                if let Some(frame) = player.take_frame()
                    && player.snapshot().position >= 7.9
                {
                    break frame;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "seek timed out: {:?}",
                    player.snapshot()
                );
                thread::sleep(Duration::from_millis(10));
            };
            assert_eq!(
                actual.bgra.as_ref(),
                expected.bgra.as_ref(),
                "network seek must show the same frame as local decoding"
            );
            drop(reference);
            drop(player);
            stopped.store(true, Ordering::Release);
            server.join().unwrap();
        }
    }
    #[test]
    fn cancelling_a_stalled_open_unblocks_the_source() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (accepted, ready) = std::sync::mpsc::channel();
        let server = thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            accepted.send(()).unwrap();
            thread::sleep(Duration::from_secs(4));
        });
        let cancel = Arc::new(AtomicBool::new(false));
        let source_cancel = cancel.clone();
        let worker = thread::spawn(move || {
            let mut source = RangeReader::new(
                format!("http://{addr}/slow"),
                None,
                source_cancel,
                Arc::new(AtomicU64::new(0)),
            )
            .unwrap();
            source.read(&mut [0u8; 8])
        });
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        let start = std::time::Instant::now();
        cancel.store(true, Ordering::Release);
        assert!(worker.join().unwrap().is_err());
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "cancellation did not bound stalled request"
        );
        server.join().unwrap();
    }
}
