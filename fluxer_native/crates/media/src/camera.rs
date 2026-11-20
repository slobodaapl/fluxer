// SPDX-License-Identifier: AGPL-3.0-or-later
//! Linux V4L2 capture. Device discovery reads sysfs only; opening a device is
//! deferred until an explicit start request. ffmpeg is an optional runtime tool.
use crate::{MediaError, MediaEvent, Result};
use livekit::webrtc::{
    video_frame::{I420Buffer, VideoFrame, VideoRotation},
    video_source::{VideoResolution, native::NativeVideoSource},
};
use serde::{Deserialize, Serialize};
use std::{
    io::Read,
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Instant,
};
use tokio::sync::mpsc;

const WIDTH: u32 = 640;
const HEIGHT: u32 = 480;
const FRAME_BYTES: usize = WIDTH as usize * HEIGHT as usize * 3 / 2;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CameraDevice {
    pub id: String,
    pub label: String,
}

fn video_name(name: &str) -> bool {
    name.strip_prefix("video")
        .is_some_and(|suffix| !suffix.is_empty() && suffix.bytes().all(|c| c.is_ascii_digit()))
}

pub fn devices() -> Result<Vec<CameraDevice>> {
    #[cfg(target_os = "linux")]
    {
        let entries = match std::fs::read_dir("/sys/class/video4linux") {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(MediaError::Camera(e.to_string())),
        };
        let mut result = Vec::new();
        for entry in entries.take(256).flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !video_name(&name) {
                continue;
            }
            let label = std::fs::read_to_string(entry.path().join("name"))
                .unwrap_or_else(|_| name.clone())
                .trim()
                .chars()
                .take(128)
                .collect();
            result.push(CameraDevice {
                id: format!("/dev/{name}"),
                label,
            });
        }
        result.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(result)
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(Vec::new())
    }
}

pub(crate) fn source() -> NativeVideoSource {
    NativeVideoSource::new(
        VideoResolution {
            width: WIDTH,
            height: HEIGHT,
        },
        false,
    )
}

#[derive(Clone)]
pub(crate) struct StopHandle {
    stopped: Arc<AtomicBool>,
    child: Arc<Mutex<Child>>,
}
impl StopHandle {
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
        }
    }
}
pub(crate) struct Session {
    handle: StopHandle,
    frame: Arc<AtomicBool>,
}
impl Session {
    pub fn controller(&self) -> StopHandle {
        self.handle.clone()
    }
    pub fn stop(&mut self) {
        self.handle.stop();
    }
    pub fn has_frame(&self) -> bool {
        self.frame.load(Ordering::Acquire) && !self.handle.stopped.load(Ordering::Acquire)
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
    }
}

pub(crate) fn start(
    id: &str,
    source: NativeVideoSource,
    tx: mpsc::Sender<MediaEvent>,
    call_stopped: Arc<AtomicBool>,
    generation: u64,
) -> Result<Session> {
    if !devices()?.iter().any(|d| d.id == id) {
        return Err(MediaError::UnknownDevice("camera"));
    }
    let mut command = Command::new("ffmpeg");
    command.args([
        "-nostdin",
        "-loglevel",
        "error",
        "-f",
        "v4l2",
        "-i",
        id,
        "-vf",
        "scale=640:480",
        "-r",
        "30",
        "-pix_fmt",
        "yuv420p",
        "-f",
        "rawvideo",
        "pipe:1",
    ]);
    start_process(command, source, tx, call_stopped, generation)
}

fn start_process(
    mut command: Command,
    source: NativeVideoSource,
    tx: mpsc::Sender<MediaEvent>,
    call_stopped: Arc<AtomicBool>,
    generation: u64,
) -> Result<Session> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| MediaError::Camera(format!("could not start ffmpeg: {e}")))?;
    let mut output = child
        .stdout
        .take()
        .ok_or_else(|| MediaError::Camera("capture output unavailable".into()))?;
    let handle = StopHandle {
        stopped: Arc::new(AtomicBool::new(false)),
        child: Arc::new(Mutex::new(child)),
    };
    let capture = handle.clone();
    let frame = Arc::new(AtomicBool::new(false));
    let received = frame.clone();
    thread::Builder::new()
        .name("fluxer-camera-capture".into())
        .spawn(move || {
            let start = Instant::now();
            let mut bytes = vec![0; FRAME_BYTES];
            while !capture.stopped.load(Ordering::Acquire)
                && !call_stopped.load(Ordering::Acquire)
                && !tx.is_closed()
            {
                if output.read_exact(&mut bytes).is_err() {
                    break;
                }
                if capture.stopped.load(Ordering::Acquire) || call_stopped.load(Ordering::Acquire) {
                    break;
                }
                let mut buffer = I420Buffer::new(WIDTH, HEIGHT);
                copy_i420(&bytes, &mut buffer);
                source.capture_frame(&VideoFrame {
                    rotation: VideoRotation::VideoRotation0,
                    timestamp_us: start.elapsed().as_micros() as i64,
                    frame_metadata: None,
                    buffer,
                });
                if !received.swap(true, Ordering::AcqRel) {
                    let _ = tx.try_send(MediaEvent::CameraStarted(generation));
                }
            }
            let was_stopped = capture.stopped.swap(true, Ordering::AcqRel);
            if let Ok(mut child) = capture.child.lock() {
                let _ = child.kill();
                let _ = child.wait();
            }
            if !was_stopped && !call_stopped.load(Ordering::Acquire) {
                let _ = tx.try_send(MediaEvent::Error(
                    "Camera capture ended or permission was denied".into(),
                ));
                let _ = tx.try_send(MediaEvent::CameraStopped(generation));
            }
        })
        .map_err(|e| {
            handle.stop();
            MediaError::Camera(e.to_string())
        })?;
    Ok(Session { handle, frame })
}

fn copy_i420(bytes: &[u8], buffer: &mut I420Buffer) {
    let (sy, su, sv) = buffer.strides();
    let (y, u, v) = buffer.data_mut();
    let luma = WIDTH as usize * HEIGHT as usize;
    let chroma = luma / 4;
    for (dst, src) in y
        .chunks_mut(sy as usize)
        .zip(bytes[..luma].chunks_exact(WIDTH as usize))
    {
        dst[..src.len()].copy_from_slice(src);
    }
    for (dst, src) in u
        .chunks_mut(su as usize)
        .zip(bytes[luma..luma + chroma].chunks_exact(WIDTH as usize / 2))
    {
        dst[..src.len()].copy_from_slice(src);
    }
    for (dst, src) in v
        .chunks_mut(sv as usize)
        .zip(bytes[luma + chroma..].chunks_exact(WIDTH as usize / 2))
    {
        dst[..src.len()].copy_from_slice(src);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn synthetic_process_announces_frame_and_can_be_stopped() {
        let mut command = Command::new("python3");
        command.args(["-c", &format!("import sys,time; sys.stdout.buffer.write(bytes({FRAME_BYTES})); sys.stdout.buffer.flush(); time.sleep(30)")]);
        let (tx, mut rx) = mpsc::channel(4);
        let mut session =
            start_process(command, source(), tx, Arc::new(AtomicBool::new(false)), 42).unwrap();
        let event = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .unwrap();
        assert!(matches!(event, Some(MediaEvent::CameraStarted(42))));
        assert!(session.has_frame());
        session.stop();
        assert!(!session.has_frame());
    }
    #[test]
    fn names_are_bounded_device_nodes() {
        assert!(video_name("video0"));
        assert!(!video_name("video"));
        assert!(!video_name("video0/../../x"));
    }
    #[test]
    fn synthetic_frame_copies_all_planes() {
        let mut data = vec![17; FRAME_BYTES];
        let y = WIDTH as usize * HEIGHT as usize;
        data[y..y + y / 4].fill(80);
        data[y + y / 4..].fill(140);
        let mut buffer = I420Buffer::new(WIDTH, HEIGHT);
        copy_i420(&data, &mut buffer);
        let (l, u, v) = buffer.data_mut();
        assert!(l.iter().all(|b| *b == 17));
        assert!(u.iter().all(|b| *b == 80));
        assert!(v.iter().all(|b| *b == 140));
    }
}
