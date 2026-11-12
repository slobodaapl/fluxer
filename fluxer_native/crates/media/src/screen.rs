// SPDX-License-Identifier: AGPL-3.0-or-later
use std::{sync::{Arc, atomic::{AtomicBool, Ordering}}, thread, time::{Duration, Instant}};
use livekit::webrtc::{
    desktop_capturer::{DesktopCapturer, DesktopCapturerOptions, DesktopCaptureSourceType, CaptureError},
    video_source::{native::NativeVideoSource, VideoResolution},
    video_frame::{I420Buffer, VideoFrame, VideoRotation},
    native::yuv_helper::argb_to_i420,
};
use serde::{Serialize, Deserialize};
use tokio::sync::mpsc;
use crate::{MediaError, MediaEvent, Result};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ScreenSourceKind { Screen, Window, SystemPicker }
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScreenSource { pub id: u64, pub label: String, pub kind: ScreenSourceKind }
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
        Self { source: ScreenSource { id: 0, label: "Choose a display or window".into(), kind: ScreenSourceKind::SystemPicker }, fps: 30, max_width: 1920, max_height: 1080, max_bitrate: 4_000_000 }
    }
}
impl ScreenShareOptions {
    pub fn validate(&self) -> Result<()> {
        if !(1..=60).contains(&self.fps) { return Err(MediaError::InvalidSetting("screen frame rate must be 1–60")); }
        if !(64..=4096).contains(&self.max_width) || !(64..=2160).contains(&self.max_height) {
            return Err(MediaError::InvalidSetting("screen dimensions out of range"));
        }
        if !(100_000..=20_000_000).contains(&self.max_bitrate) { return Err(MediaError::InvalidSetting("screen bitrate out of range")); }
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
    let mut result = Vec::new();
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    result.push(ScreenShareOptions::default().source);
    for kind in [ScreenSourceKind::Screen, ScreenSourceKind::Window] {
        if let Some(capturer) = DesktopCapturer::new(options(kind)) {
            result.extend(capturer.get_source_list().into_iter().map(|s| ScreenSource { id: s.id(), label: s.title(), kind }));
        }
    }
    if result.is_empty() { return Err(MediaError::Screen("No capture sources are available".into())); }
    Ok(result)
}

pub(crate) fn video_source(options: &ScreenShareOptions) -> NativeVideoSource {
    NativeVideoSource::new(VideoResolution { width: options.max_width, height: options.max_height }, true)
}

pub(crate) struct ScreenSession { stop: Arc<AtomicBool> }
impl ScreenSession { pub fn stop(&mut self) { self.stop.store(true, Ordering::Release); } }
impl Drop for ScreenSession { fn drop(&mut self) { self.stop(); } }

pub(crate) fn start(config: ScreenShareOptions, source: NativeVideoSource, tx: mpsc::Sender<MediaEvent>) -> Result<ScreenSession> {
    config.validate()?;
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    // DesktopCapturer must be created, driven, and dropped on its capture thread.
    thread::Builder::new().name("fluxer-screen-capture".into()).spawn(move || {
        let Some(mut capturer) = DesktopCapturer::new(options(config.source.kind)) else {
            let _ = tx.blocking_send(MediaEvent::Error("Screen capture is unavailable; check OS permissions".into()));
            let _ = tx.blocking_send(MediaEvent::ScreenShareStopped);
            return;
        };
        let selected = if config.source.kind == ScreenSourceKind::SystemPicker {
            None
        } else {
            match capturer.get_source_list().into_iter().find(|s| s.id() == config.source.id) {
                Some(s) => Some(s),
                None => {
                    let _ = tx.blocking_send(MediaEvent::Error("The selected capture source disappeared".into()));
                    let _ = tx.blocking_send(MediaEvent::ScreenShareStopped);
                    return;
                }
            }
        };
        let start = Instant::now();
        let stopped_callback = stop_thread.clone();
        let tx_callback = tx.clone();
        let mut first_frame = true;
        capturer.start_capture(selected, move |result| {
            if stopped_callback.load(Ordering::Acquire) { return; }
            match result {
                Ok(frame) => {
                    let w = frame.width(); let h = frame.height();
                    if w <= 0 || h <= 0 || w > 16384 || h > 16384 { return; }
                    let mut buffer = I420Buffer::new(w as u32, h as u32);
                    let (sy, su, sv) = buffer.strides();
                    let (y, u, v) = buffer.data_mut();
                    // libwebrtc desktop frames contain BGRA bytes (libyuv ARGB).
                    argb_to_i420(frame.data(), frame.stride(), y, sy, u, su, v, sv, w, h);
                    let (out_w, out_h) = fit_dimensions(w as u32, h as u32, config.max_width, config.max_height);
                    if out_w != w as u32 || out_h != h as u32 { buffer = buffer.scale(out_w as i32, out_h as i32); }
                    source.capture_frame(&VideoFrame { rotation: VideoRotation::VideoRotation0, timestamp_us: start.elapsed().as_micros() as i64, frame_metadata: None, buffer });
                    if first_frame { let _ = tx_callback.try_send(MediaEvent::ScreenShareStarted); first_frame = false; }
                }
                Err(CaptureError::Permanent) => {
                    stopped_callback.store(true, Ordering::Release);
                    let _ = tx_callback.try_send(MediaEvent::Error("Screen capture ended or permission was denied".into()));
                }
                Err(CaptureError::Temporary) => {}
            }
        });
        let interval = Duration::from_secs_f64(1.0 / config.fps as f64);
        while !stop_thread.load(Ordering::Acquire) && !tx.is_closed() {
            let tick = Instant::now();
            capturer.capture_frame();
            thread::sleep(interval.saturating_sub(tick.elapsed()));
        }
        drop(capturer);
        let _ = tx.try_send(MediaEvent::ScreenShareStopped);
    }).map_err(|e| MediaError::Screen(e.to_string()))?;
    Ok(ScreenSession { stop })
}

fn fit_dimensions(w: u32, h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    let scale = (max_w as f64 / w as f64).min(max_h as f64 / h as f64).min(1.0);
    // YUV420 encoders need even dimensions. Never upscale or exceed limits.
    (((w as f64 * scale) as u32 / 2 * 2).max(2), ((h as f64 * scale) as u32 / 2 * 2).max(2))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn screen_sizing_preserves_aspect_and_bounds() {
        assert_eq!(fit_dimensions(3840, 2160, 1920, 1080), (1920, 1080));
        assert_eq!(fit_dimensions(1080, 1920, 1920, 1080), (606, 1080));
        assert_eq!(fit_dimensions(1280, 720, 1920, 1080), (1280, 720));
    }
    #[test]
    fn invalid_capture_parameters_are_rejected_before_capture() {
        let mut config = ScreenShareOptions::default();
        config.fps = 0;
        assert!(config.validate().is_err());
        config.fps = 30;
        config.max_width = u32::MAX;
        assert!(config.validate().is_err());
    }
}
