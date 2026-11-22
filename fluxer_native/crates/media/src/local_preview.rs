// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::{VideoSource, screen::fit_dimensions};
use futures_util::StreamExt;
use livekit::webrtc::{
    video_frame::{VideoBuffer, VideoFormatType},
    video_stream::native::NativeVideoStream,
    video_track::RtcVideoTrack,
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::task::JoinHandle;

const PREVIEW_SIZE: (u32, u32) = (384, 216);
const PREVIEW_INTERVAL: Duration = Duration::from_millis(83);

pub struct PreviewFrame {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

struct Slot {
    visible: bool,
    revision: u64,
    capture: u64,
    latest: Option<PreviewFrame>,
}

impl Default for Slot {
    fn default() -> Self {
        Self {
            visible: true,
            revision: 0,
            capture: 0,
            latest: None,
        }
    }
}

/// Local playback state shared between the media engine and its UI.
#[derive(Clone, Default)]
pub struct LocalVideoPreviews {
    slots: Arc<Mutex<BTreeMap<VideoSource, Slot>>>,
}

impl LocalVideoPreviews {
    pub fn visible(&self, source: VideoSource) -> bool {
        self.slots
            .lock()
            .is_ok_and(|slots| slots.get(&source).is_none_or(|slot| slot.visible))
    }

    pub fn set_visible(&self, source: VideoSource, visible: bool) {
        if let Ok(mut slots) = self.slots.lock() {
            let slot = slots.entry(source).or_default();
            if slot.visible != visible {
                slot.visible = visible;
                slot.revision = slot.revision.wrapping_add(1);
                slot.latest = None;
            }
        }
    }

    pub fn take_frame(&self, source: VideoSource) -> Option<PreviewFrame> {
        self.slots.lock().ok()?.get_mut(&source)?.latest.take()
    }

    pub(crate) fn attach(&self, source: VideoSource, track: RtcVideoTrack) -> Option<Renderer> {
        let capture = {
            let mut slots = self.slots.lock().ok()?;
            let slot = slots.entry(source).or_default();
            slot.latest = None;
            slot.capture = slot.capture.wrapping_add(1);
            slot.capture
        };
        let previews = self.clone();
        // The local track sink has a one-frame queue and runs independently of the SFU.
        let mut stream = NativeVideoStream::new(track);
        let task = tokio::spawn(async move {
            let mut next = Instant::now();
            while let Some(frame) = stream.next().await {
                if Instant::now() < next {
                    continue;
                }
                let Some(revision) = previews.preview_revision(source, capture) else {
                    continue;
                };
                let (width, height) = (frame.buffer.width(), frame.buffer.height());
                if width < 2 || height < 2 || width > 16384 || height > 16384 {
                    continue;
                }
                let (w, h) = fit_dimensions(width, height, PREVIEW_SIZE.0, PREVIEW_SIZE.1);
                let mut buffer = frame.buffer.to_i420();
                if (width, height) != (w, h) {
                    buffer = buffer.scale(w as i32, h as i32);
                }
                let mut bgra = vec![0; w as usize * h as usize * 4];
                (&buffer as &dyn VideoBuffer).to_argb(
                    VideoFormatType::ARGB,
                    &mut bgra,
                    w * 4,
                    w as i32,
                    h as i32,
                );
                previews.publish(
                    source,
                    capture,
                    revision,
                    PreviewFrame {
                        width: w,
                        height: h,
                        bgra,
                    },
                );
                next = Instant::now() + PREVIEW_INTERVAL;
            }
        });
        Some(Renderer {
            previews: self.clone(),
            source,
            capture,
            task,
        })
    }

    fn preview_revision(&self, source: VideoSource, capture: u64) -> Option<u64> {
        let slots = self.slots.lock().ok()?;
        let slot = slots.get(&source)?;
        (slot.visible && slot.capture == capture).then_some(slot.revision)
    }

    fn publish(&self, source: VideoSource, capture: u64, revision: u64, frame: PreviewFrame) {
        // A conversion already in progress when a preview was hidden or
        // restarted cannot repopulate the slot.
        if let Ok(mut slots) = self.slots.lock()
            && let Some(slot) = slots.get_mut(&source)
            && slot.visible
            && slot.capture == capture
            && slot.revision == revision
        {
            slot.latest = Some(frame);
        }
    }
}

pub(crate) struct Renderer {
    previews: LocalVideoPreviews,
    source: VideoSource,
    capture: u64,
    task: JoinHandle<()>,
}

impl Drop for Renderer {
    fn drop(&mut self) {
        if let Ok(mut slots) = self.previews.slots.lock()
            && let Some(slot) = slots.get_mut(&self.source)
            && slot.capture == self.capture
        {
            slot.capture = slot.capture.wrapping_add(1);
            slot.latest = None;
        }
        self.task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use livekit::{
        prelude::LocalVideoTrack,
        webrtc::{
            video_frame::{I420Buffer, VideoFrame, VideoRotation},
            video_source::{RtcVideoSource, VideoResolution, native::NativeVideoSource},
        },
    };

    fn attach_test_source(
        previews: &LocalVideoPreviews,
        kind: VideoSource,
        width: u32,
        height: u32,
    ) -> (NativeVideoSource, Renderer) {
        let source = NativeVideoSource::new(VideoResolution { width, height }, false);
        let track = LocalVideoTrack::create_video_track(
            "Local preview test",
            RtcVideoSource::Native(source.clone()),
        );
        let renderer = previews.attach(kind, track.rtc_track()).unwrap();
        (source, renderer)
    }

    async fn preview_from(
        previews: &LocalVideoPreviews,
        kind: VideoSource,
        source: &NativeVideoSource,
        width: u32,
        height: u32,
    ) -> PreviewFrame {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                send_frame(source, width, height);
                tokio::time::sleep(Duration::from_millis(30)).await;
                if let Some(frame) = previews.take_frame(kind) {
                    return frame;
                }
            }
        })
        .await
        .expect("frames from the local track should reach the preview without a LiveKit room")
    }

    fn send_frame(source: &NativeVideoSource, width: u32, height: u32) {
        let mut buffer = I420Buffer::new(width, height);
        let (y, u, v) = buffer.data_mut();
        y.fill(120);
        u.fill(128);
        v.fill(128);
        source.capture_frame(&VideoFrame::new(VideoRotation::VideoRotation0, buffer));
    }

    #[tokio::test]
    async fn local_tracks_preview_without_a_room_and_toggle_independently() {
        let previews = LocalVideoPreviews::default();
        let (camera, camera_preview) = attach_test_source(&previews, VideoSource::Camera, 640, 480);
        let (screen, _screen_preview) =
            attach_test_source(&previews, VideoSource::ScreenShare, 1280, 720);
        let frame = preview_from(&previews, VideoSource::Camera, &camera, 640, 480).await;
        assert_eq!((frame.width, frame.height), (288, 216));
        assert_eq!(frame.bgra.len(), 288 * 216 * 4);
        let old_revision = previews
            .preview_revision(VideoSource::Camera, camera_preview.capture)
            .unwrap();
        previews.set_visible(VideoSource::Camera, false);
        send_frame(&camera, 640, 480);
        let frame = preview_from(&previews, VideoSource::ScreenShare, &screen, 1280, 720).await;
        assert_eq!((frame.width, frame.height), (384, 216));
        assert!(previews.take_frame(VideoSource::Camera).is_none());

        previews.set_visible(VideoSource::Camera, true);
        previews.publish(
            VideoSource::Camera,
            camera_preview.capture,
            old_revision,
            frame,
        );
        assert!(previews.take_frame(VideoSource::Camera).is_none());
        preview_from(&previews, VideoSource::Camera, &camera, 640, 480).await;
        let capture = camera_preview.capture;
        drop(camera_preview);
        assert!(
            previews
                .preview_revision(VideoSource::Camera, capture)
                .is_none()
        );
        assert!(previews.take_frame(VideoSource::Camera).is_none());
    }
}
