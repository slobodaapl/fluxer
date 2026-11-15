// SPDX-License-Identifier: AGPL-3.0-or-later
//! Credential-free seekable HTTP source. FFmpeg receives bytes, never a URL.
use crate::media_download::{self, MAX_BYTES};
use reqwest::{blocking::Client, header};
use std::{
    io::{self, Read, Seek, SeekFrom, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
const CHUNK: u64 = 256 * 1024;
fn bad(message: &'static str) -> io::Error {
    io::Error::other(message)
}

pub(super) struct RangeReader {
    url: String,
    client: Client,
    cancel: Arc<AtomicBool>,
    interrupt: Arc<AtomicU64>,
    size: Option<u64>,
    pos: u64,
    chunk_at: u64,
    chunk: Vec<u8>,
    validator: Option<(header::HeaderName, header::HeaderValue)>,
    fallback: Option<tempfile::NamedTempFile>,
}
impl RangeReader {
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
        let client = media_download::client_builder(Duration::from_secs(2))
            .build()
            .map_err(|_| "Could not start the media downloader")?;
        Ok(Self {
            url,
            client,
            cancel,
            interrupt,
            size: None,
            pos: 0,
            chunk_at: 0,
            chunk: Vec::new(),
            validator: None,
            fallback: None,
        })
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
        Ok(())
    }
    fn full_download(&mut self, mut response: reqwest::blocking::Response) -> io::Result<()> {
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
        let mut file = tempfile::Builder::new()
            .prefix("playback-")
            .tempfile_in(
                media_download::cache_directory().map_err(|_| bad("Media cache unavailable"))?,
            )
            .map_err(|_| bad("Media cache unavailable"))?;
        let progress = AtomicU64::new(0);
        media_download::copy_bounded(
            &mut response,
            file.as_file_mut(),
            MAX_BYTES,
            &self.cancel,
            &progress,
        )
        .map_err(|_| bad("Media download failed"))?;
        let length = progress.load(Ordering::Relaxed);
        if expected.is_some_and(|expected| expected != length) {
            return Err(bad("Incomplete media download"));
        }
        file.as_file_mut().flush()?;
        self.size = Some(length);
        self.fallback = Some(file);
        Ok(())
    }
    fn ensure(&mut self) -> io::Result<()> {
        if self.size.is_none() {
            self.request(0)?;
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
        if let Some(file) = &mut self.fallback {
            file.as_file_mut().seek(SeekFrom::Start(self.pos))?;
            let n = file.as_file_mut().read(out)?;
            self.pos += n as u64;
            return Ok(n);
        }
        if self.pos < self.chunk_at || self.pos >= self.chunk_at + self.chunk.len() as u64 {
            self.request(self.pos)?;
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
                .take(if mode == "changed" || mode == "novalidator" {
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
                if mode == "ignored" || (mode == "novalidator" && index == 1) {
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
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
                write!(stream, "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {}-{}/{}\r\nContent-Length: {}\r\n{}{}Connection: close\r\n\r\n", reported_start, end, total, length, etag_header, encoding).unwrap();
                let _ = stream.write_all(&data[start..=end]);
            }
        });
        (format!("http://{addr}/signed?token=a%2Fb"), requests, task)
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
            let player = FilePlayer::open_reader(
                Box::new(source),
                cancel,
                interrupt,
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
            player.seek(3.0);
            player.seek(8.0);
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            loop {
                if player.take_frame().is_some() && player.snapshot().position >= 7.9 {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "seek timed out: {:?}",
                    player.snapshot()
                );
                thread::sleep(Duration::from_millis(10));
            }
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
