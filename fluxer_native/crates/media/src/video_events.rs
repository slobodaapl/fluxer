// SPDX-License-Identifier: AGPL-3.0-or-later
//! Lifecycle messages have their own queue; decoded pixels are latest-only per
//! track. A slow consumer never turns 32 old full-resolution frames into latency.
use crate::{MediaEvent, VideoFrame, subscriptions::Registry};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tokio::sync::{Notify, mpsc};

pub(crate) struct VideoMailbox {
    frames: Mutex<BTreeMap<String, VideoFrame>>,
    ready: Notify,
    subscriptions: Arc<Mutex<Registry>>,
}
impl VideoMailbox {
    pub fn new(subscriptions: Arc<Mutex<Registry>>) -> Self {
        Self {
            frames: Mutex::new(BTreeMap::new()),
            ready: Notify::new(),
            subscriptions,
        }
    }
    pub fn publish(&self, frame: VideoFrame) {
        // Hold policy through insertion: local unwatch cannot clear the slot
        // and then have an old renderer repopulate it after cancellation.
        let Ok(registry) = self.subscriptions.lock() else {
            return;
        };
        if !registry.desired_track_at(&frame.track_id, frame.watch_generation) {
            return;
        }
        self.retain_latest(frame);
    }
    fn retain_latest(&self, frame: VideoFrame) {
        if let Ok(mut frames) = self.frames.lock() {
            frames.insert(frame.track_id.clone(), frame);
            self.ready.notify_one();
        }
    }
    pub fn remove(&self, sid: &str) {
        if let Ok(mut frames) = self.frames.lock() {
            frames.remove(sid);
        }
    }
    pub fn clear(&self) {
        if let Ok(mut frames) = self.frames.lock() {
            frames.clear();
        }
    }
    fn pop(&self) -> Option<VideoFrame> {
        loop {
            let frame = self.frames.lock().ok()?.pop_first()?.1;
            if self.subscriptions.lock().is_ok_and(|registry| {
                registry.desired_track_at(&frame.track_id, frame.watch_generation)
            }) {
                return Some(frame);
            }
        }
    }
}
/// Control messages are received before video. Pixel storage remains bounded
/// to the newest frame per subscribed track, independent of consumer cadence.
pub struct MediaEvents {
    control: mpsc::Receiver<MediaEvent>,
    pub(crate) video: Arc<VideoMailbox>,
}
impl MediaEvents {
    pub(crate) fn new(control: mpsc::Receiver<MediaEvent>, video: Arc<VideoMailbox>) -> Self {
        Self { control, video }
    }
    pub async fn recv(&mut self) -> Option<MediaEvent> {
        loop {
            match self.control.try_recv() {
                Ok(event) => return Some(event),
                Err(mpsc::error::TryRecvError::Disconnected) => return None,
                Err(mpsc::error::TryRecvError::Empty) => (),
            }
            if let Some(frame) = self.video.pop() {
                return Some(MediaEvent::Video(frame));
            }
            tokio::select! {biased;
                event=self.control.recv()=>return event,
                _=self.video.ready.notified()=>(),
            }
        }
    }
    pub fn try_recv(&mut self) -> Result<MediaEvent, mpsc::error::TryRecvError> {
        match self.control.try_recv() {
            Ok(event) => Ok(event),
            Err(mpsc::error::TryRecvError::Disconnected) => {
                Err(mpsc::error::TryRecvError::Disconnected)
            }
            Err(mpsc::error::TryRecvError::Empty) => self
                .video
                .pop()
                .map(MediaEvent::Video)
                .ok_or(mpsc::error::TryRecvError::Empty),
        }
    }
    pub fn pending_video_frames(&self) -> usize {
        self.video
            .frames
            .lock()
            .map(|frames| frames.len())
            .unwrap_or(0)
    }
}
impl From<mpsc::Receiver<MediaEvent>> for MediaEvents {
    fn from(control: mpsc::Receiver<MediaEvent>) -> Self {
        Self::new(
            control,
            Arc::new(VideoMailbox::new(Arc::new(Mutex::new(Registry::default())))),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frame(sequence: u8, generation: u64) -> VideoFrame {
        VideoFrame {
            source: crate::VideoSource::ScreenShare,
            track_id: "screen".into(),
            participant: "alice".into(),
            watch_generation: generation,
            width: 1,
            height: 1,
            bgra: Arc::new(vec![sequence, 0, 0, 255]),
        }
    }
    #[tokio::test]
    async fn newest_only_storage_cannot_put_video_ahead_of_stop_control() {
        let registry = Arc::new(Mutex::new(Registry::default()));
        let video = Arc::new(VideoMailbox::new(registry));
        for sequence in 0..200 {
            video.retain_latest(frame(sequence, 1));
        }
        assert_eq!(video.frames.lock().unwrap().len(), 1);
        assert_eq!(video.frames.lock().unwrap()["screen"].bgra[0], 199);
        let (sender, receiver) = mpsc::channel(4);
        let mut events = MediaEvents::new(receiver, video.clone());
        sender
            .send(MediaEvent::VideoEnded {
                source: crate::VideoSource::ScreenShare,
                track_id: "screen".into(),
                participant: "alice".into(),
                watch_generation: 1,
            })
            .await
            .unwrap();
        assert!(
            matches!(events.recv().await, Some(MediaEvent::VideoEnded { .. })),
            "local lifecycle event precedes all pending pixels"
        );
        assert_eq!(
            events.pending_video_frames(),
            1,
            "control was delivered without consuming queued pixels"
        );
        video.remove("screen");
        assert_eq!(events.pending_video_frames(), 0);
    }
    #[test]
    fn stopped_or_old_epoch_renderer_cannot_repopulate_latest_slot() {
        let registry = Arc::new(Mutex::new(Registry::default()));
        registry
            .lock()
            .unwrap()
            .replace_watched(vec!["alice".into()]);
        let old = registry.lock().unwrap().watch_generation("alice");
        let video = VideoMailbox::new(registry.clone());
        video.retain_latest(frame(1, old));
        registry.lock().unwrap().replace_watched(vec![]);
        video.remove("screen");
        video.publish(frame(2, old));
        assert!(video.frames.lock().unwrap().is_empty());
        registry
            .lock()
            .unwrap()
            .replace_watched(vec!["alice".into()]);
        assert!(!registry.lock().unwrap().watch_is_current("alice", old));
        video.publish(frame(3, old));
        assert!(video.frames.lock().unwrap().is_empty());
    }
}
