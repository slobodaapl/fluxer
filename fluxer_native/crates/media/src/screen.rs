// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::{MediaError, MediaEvent, Result};
use livekit::webrtc::{
    desktop_capturer::{
        CaptureError, DesktopCaptureSourceType, DesktopCapturer, DesktopCapturerOptions,
    },
    native::yuv_helper::argb_to_i420,
    video_frame::{I420Buffer, VideoFrame, VideoRotation},
    video_source::{VideoResolution, native::NativeVideoSource},
};
use serde::{Deserialize, Serialize};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use tokio::sync::mpsc;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ScreenSourceKind {
    Screen,
    Window,
    SystemPicker,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScreenSource {
    pub id: u64,
    pub label: String,
    pub kind: ScreenSourceKind,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScreenShareOptions {
    pub source: ScreenSource,
    pub fps: u32,
    pub max_width: u32,
    pub max_height: u32,
    pub max_bitrate: u64,
}
impl Default for ScreenShareOptions {
    fn default() -> Self {
        Self {
            source: ScreenSource {
                id: 0,
                label: "Choose a display or window".into(),
                kind: ScreenSourceKind::SystemPicker,
            },
            fps: 30,
            max_width: 1920,
            max_height: 1080,
            max_bitrate: 4_000_000,
        }
    }
}
impl ScreenShareOptions {
    /// Match ScreenShareOptions.ts: one full-rate layer, not the SDK's default
    /// screenshare simulcast fallback whose low layer runs at three FPS.
    pub fn publish_options(&self) -> livekit::options::TrackPublishOptions {
        livekit::options::TrackPublishOptions {
            source: livekit::prelude::TrackSource::Screenshare,
            video_encoding: Some(livekit::options::VideoEncoding {
                max_bitrate: self.max_bitrate,
                max_framerate: self.fps as f64,
            }),
            simulcast: false,
            stream: "screen_share".into(),
            degradation_preference: Some(
                livekit::webrtc::rtp_parameters::DegradationPreference::MaintainFramerate,
            ),
            ..Default::default()
        }
    }
    pub fn validate(&self) -> Result<()> {
        if !(1..=60).contains(&self.fps) {
            return Err(MediaError::InvalidSetting("screen frame rate must be 1–60"));
        }
        if !(64..=4096).contains(&self.max_width) || !(64..=2160).contains(&self.max_height) {
            return Err(MediaError::InvalidSetting("screen dimensions out of range"));
        }
        if !(100_000..=20_000_000).contains(&self.max_bitrate) {
            return Err(MediaError::InvalidSetting("screen bitrate out of range"));
        }
        Ok(())
    }
}

fn options(kind: ScreenSourceKind) -> DesktopCapturerOptions {
    let source = match kind {
        ScreenSourceKind::Window => DesktopCaptureSourceType::Window,
        ScreenSourceKind::Screen => DesktopCaptureSourceType::Screen,
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        ScreenSourceKind::SystemPicker => DesktopCaptureSourceType::Generic,
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        ScreenSourceKind::SystemPicker => DesktopCaptureSourceType::Screen,
    };
    let mut options = DesktopCapturerOptions::new(source);
    options.set_include_cursor(true);
    options
}

pub(crate) fn sources() -> Result<Vec<ScreenSource>> {
    let session_type = std::env::var("XDG_SESSION_TYPE").ok();
    let wayland_display = std::env::var("WAYLAND_DISPLAY").ok();
    if system_picker_only(
        std::env::consts::OS,
        session_type.as_deref(),
        wayland_display.as_deref(),
    ) {
        // Constructing/enumerating WebRTC capturers on Wayland may create
        // portal sessions. The descriptor is inert; capture opens the system
        // dialog only after the user explicitly starts this source.
        return Ok(vec![ScreenShareOptions::default().source]);
    }
    let mut result = Vec::new();
    for kind in [ScreenSourceKind::Screen, ScreenSourceKind::Window] {
        if let Some(capturer) = DesktopCapturer::new(options(kind)) {
            result.extend(
                capturer
                    .get_source_list()
                    .into_iter()
                    .map(|s| ScreenSource {
                        id: s.id(),
                        label: s.title(),
                        kind,
                    }),
            );
        }
    }
    if result.is_empty() {
        return Err(MediaError::Screen(
            "No capture sources are available".into(),
        ));
    }
    Ok(result)
}

fn system_picker_only(
    platform: &str,
    session_type: Option<&str>,
    wayland_display: Option<&str>,
) -> bool {
    platform == "macos"
        || (platform == "linux"
            && (session_type.is_some_and(|session| session.eq_ignore_ascii_case("wayland"))
                || wayland_display.is_some_and(|display| !display.is_empty())))
}

pub(crate) fn video_source(options: &ScreenShareOptions) -> NativeVideoSource {
    NativeVideoSource::new(
        VideoResolution {
            width: options.max_width,
            height: options.max_height,
        },
        true,
    )
}

#[derive(Clone)]
pub(crate) struct ScreenStopHandle {
    stop: Arc<AtomicBool>,
    thread: thread::Thread,
}
impl ScreenStopHandle {
    #[cfg(test)]
    pub fn for_test(stop: Arc<AtomicBool>) -> Self {
        Self {
            stop,
            thread: thread::current(),
        }
    }
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        self.thread.unpark();
    }
}

pub(crate) struct ScreenSession {
    stop: Arc<AtomicBool>,
    has_frame: Arc<AtomicBool>,
    thread: thread::Thread,
}
impl ScreenSession {
    pub fn controller(&self) -> ScreenStopHandle {
        ScreenStopHandle {
            stop: self.stop.clone(),
            thread: self.thread.clone(),
        }
    }
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.thread.unpark();
    }
    pub fn has_frame(&self) -> bool {
        self.has_frame.load(Ordering::Acquire) && !self.is_stopped()
    }
    pub fn is_stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }
}
impl Drop for ScreenSession {
    fn drop(&mut self) {
        self.stop();
    }
}

pub(crate) fn start(
    config: ScreenShareOptions,
    source: NativeVideoSource,
    tx: mpsc::Sender<MediaEvent>,
    call_stopped: Arc<AtomicBool>,
) -> Result<ScreenSession> {
    config.validate()?;
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let has_frame = Arc::new(AtomicBool::new(false));
    let first_frame_ready = has_frame.clone();
    // DesktopCapturer must be created, driven, and dropped on its capture thread.
    let capture_thread = thread::Builder::new()
        .name("fluxer-screen-capture".into())
        .spawn(move || {
            let Some(mut capturer) = DesktopCapturer::new(options(config.source.kind)) else {
                stop_thread.store(true, Ordering::Release);
                let _ = tx.try_send(MediaEvent::Error(
                    "Screen capture is unavailable; check OS permissions".into(),
                ));
                let _ = tx.try_send(MediaEvent::ScreenShareStopped);
                return;
            };
            let selected = if config.source.kind == ScreenSourceKind::SystemPicker {
                None
            } else {
                match capturer
                    .get_source_list()
                    .into_iter()
                    .find(|s| s.id() == config.source.id)
                {
                    Some(s) => Some(s),
                    None => {
                        stop_thread.store(true, Ordering::Release);
                        let _ = tx.try_send(MediaEvent::Error(
                            "The selected capture source disappeared".into(),
                        ));
                        let _ = tx.try_send(MediaEvent::ScreenShareStopped);
                        return;
                    }
                }
            };
            let start = Instant::now();
            let stopped_callback = stop_thread.clone();
            let call_stopped_callback = call_stopped.clone();
            let tx_callback = tx.clone();
            let mut first_frame = true;
            capturer.start_capture(selected, move |result| {
                if stopped_callback.load(Ordering::Acquire)
                    || call_stopped_callback.load(Ordering::Acquire)
                {
                    return;
                }
                match result {
                    Ok(frame) => {
                        let w = frame.width();
                        let h = frame.height();
                        if w <= 0 || h <= 0 || w > 16384 || h > 16384 {
                            return;
                        }
                        let mut buffer = I420Buffer::new(w as u32, h as u32);
                        let (sy, su, sv) = buffer.strides();
                        let (y, u, v) = buffer.data_mut();
                        // libwebrtc desktop frames contain BGRA bytes (libyuv ARGB).
                        argb_to_i420(frame.data(), frame.stride(), y, sy, u, su, v, sv, w, h);
                        let (out_w, out_h) =
                            fit_dimensions(w as u32, h as u32, config.max_width, config.max_height);
                        if out_w != w as u32 || out_h != h as u32 {
                            buffer = buffer.scale(out_w as i32, out_h as i32);
                        }
                        source.capture_frame(&VideoFrame {
                            rotation: VideoRotation::VideoRotation0,
                            timestamp_us: start.elapsed().as_micros() as i64,
                            frame_metadata: None,
                            buffer,
                        });
                        if first_frame {
                            first_frame_ready.store(true, Ordering::Release);
                            let _ = tx_callback.try_send(MediaEvent::ScreenShareStarted);
                            first_frame = false;
                        }
                    }
                    Err(CaptureError::Permanent) => {
                        stopped_callback.store(true, Ordering::Release);
                        let _ = tx_callback.try_send(MediaEvent::Error(
                            "Screen capture ended or permission was denied".into(),
                        ));
                    }
                    Err(CaptureError::Temporary) => {}
                }
            });
            let interval = Duration::from_secs_f64(1.0 / config.fps as f64);
            while !stop_thread.load(Ordering::Acquire)
                && !call_stopped.load(Ordering::Acquire)
                && !tx.is_closed()
            {
                let tick = Instant::now();
                capturer.capture_frame();
                thread::park_timeout(interval.saturating_sub(tick.elapsed()));
            }
            drop(capturer);
            stop_thread.store(true, Ordering::Release);
            let _ = tx.try_send(MediaEvent::ScreenShareStopped);
        })
        .map_err(|e| MediaError::Screen(e.to_string()))?;
    Ok(ScreenSession {
        stop,
        has_frame,
        thread: capture_thread.thread().clone(),
    })
}

pub(crate) fn fit_dimensions(w: u32, h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    let scale = (max_w as f64 / w as f64)
        .min(max_h as f64 / h as f64)
        .min(1.0);
    // YUV420 encoders need even dimensions. Never upscale or exceed limits.
    (
        ((w as f64 * scale) as u32 / 2 * 2).max(2),
        ((h as f64 * scale) as u32 / 2 * 2).max(2),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn picker_readiness_requires_current_first_frame_and_ends_on_stop() {
        let ready = Arc::new(AtomicBool::new(false));
        let mut session = ScreenSession {
            stop: Arc::new(AtomicBool::new(false)),
            has_frame: ready.clone(),
            thread: thread::current(),
        };
        assert!(
            !session.has_frame(),
            "pending picker is not capture consent"
        );
        ready.store(true, Ordering::Release);
        assert!(session.has_frame());
        session.stop();
        assert!(
            !session.has_frame(),
            "stale first-frame event cannot reopen stopped capture"
        );
    }
    #[test]
    fn production_screen_policy_has_one_selected_rate_layer_at_real_display_sizes() {
        let settings = ScreenShareOptions {
            fps: 30,
            max_width: 1920,
            max_height: 1080,
            ..Default::default()
        };
        let options = settings.publish_options();
        assert!(!options.simulcast);
        assert_eq!(
            options.degradation_preference,
            Some(livekit::webrtc::rtp_parameters::DegradationPreference::MaintainFramerate)
        );
        for (width, height) in [(1280, 720), (1920, 1080)] {
            let encodings = livekit::options::compute_video_encodings(width, height, &options);
            assert_eq!(
                encodings.len(),
                1,
                "never negotiate the SDK's3fps low screenshare layer"
            );
            assert_eq!(encodings[0].max_framerate, Some(30.0));
        }
    }
    #[test]
    fn source_policy_uses_inert_system_picker_on_wayland_and_macos_only() {
        assert!(system_picker_only("linux", Some("wayland"), None));
        assert!(system_picker_only("linux", None, Some("wayland-1")));
        assert!(system_picker_only("macos", None, None));
        assert!(!system_picker_only("linux", Some("x11"), None));
        assert!(!system_picker_only("linux", None, Some("")));
        assert!(!system_picker_only(
            "windows",
            Some("wayland"),
            Some("wayland-1")
        ));
    }
    #[test]
    fn screen_sizing_preserves_aspect_and_bounds() {
        assert_eq!(fit_dimensions(3840, 2160, 1920, 1080), (1920, 1080));
        assert_eq!(fit_dimensions(1080, 1920, 1920, 1080), (606, 1080));
        assert_eq!(fit_dimensions(1280, 720, 1920, 1080), (1280, 720));
    }
    #[test]
    fn invalid_capture_parameters_are_rejected_before_capture() {
        let mut config = ScreenShareOptions {
            fps: 0,
            ..ScreenShareOptions::default()
        };
        assert!(config.validate().is_err());
        config.fps = 30;
        config.max_width = u32::MAX;
        assert!(config.validate().is_err());
    }

    #[test]
    fn stopping_screen_session_wakes_capture_timer_and_marks_stopped() {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            while !worker_stop.load(Ordering::Acquire) {
                thread::park_timeout(Duration::from_secs(10));
            }
            tx.send(()).unwrap();
        });
        let mut session = ScreenSession {
            stop,
            has_frame: Arc::new(AtomicBool::new(false)),
            thread: worker.thread().clone(),
        };
        assert!(!session.is_stopped());
        session.stop();
        assert!(session.is_stopped());
        rx.recv_timeout(Duration::from_secs(1))
            .expect("stop wakes timer promptly");
        worker.join().unwrap();
    }
}
