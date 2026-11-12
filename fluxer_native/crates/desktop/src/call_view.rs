// SPDX-License-Identifier: AGPL-3.0-or-later
//! Local viewing intent is independent of delayed transport acknowledgements.
use std::collections::{BTreeMap, BTreeSet};

pub struct StreamFrame<T> {
    pub track_id: String,
    pub watch_generation: u64,
    pub width: u32,
    pub height: u32,
    pub image: T,
}

pub struct Viewer<T> {
    pub desired: BTreeSet<String>,
    pub focused: Option<String>,
    pub frames: BTreeMap<String, StreamFrame<T>>,
    pub ended: BTreeSet<String>,
    pub errors: BTreeMap<String, String>,
    stopped_generations: BTreeMap<String, u64>,
    latest_generations: BTreeMap<String, u64>,
    pub chat_open: bool,
    pub expanded: bool,
}
impl<T> Default for Viewer<T> {
    fn default() -> Self {
        Self {
            desired: BTreeSet::new(),
            focused: None,
            frames: BTreeMap::new(),
            ended: BTreeSet::new(),
            errors: BTreeMap::new(),
            stopped_generations: BTreeMap::new(),
            latest_generations: BTreeMap::new(),
            chat_open: false,
            expanded: false,
        }
    }
}
impl<T> Viewer<T> {
    pub fn watch(&mut self, identity: String) {
        self.desired.insert(identity.clone());
        self.ended.remove(&identity);
        self.errors.remove(&identity);
        self.focused = Some(identity);
    }
    pub fn stop(&mut self, identity: &str) {
        if let Some(generation) = self.latest_generations.get(identity) {
            self.stopped_generations
                .insert(identity.into(), *generation);
        }
        self.desired.remove(identity);
        self.frames.remove(identity);
        self.ended.remove(identity);
        self.errors.remove(identity);
        if self.focused.as_deref() == Some(identity) {
            self.focused = self.desired.iter().next().cloned();
        }
    }
    pub fn accepts(&self, identity: &str, generation: u64) -> bool {
        self.desired.contains(identity)
            && !self
                .stopped_generations
                .get(identity)
                .is_some_and(|stopped| generation <= *stopped)
            && !self
                .latest_generations
                .get(identity)
                .is_some_and(|latest| generation < *latest)
    }
    pub fn receive(&mut self, identity: String, frame: StreamFrame<T>) -> bool {
        if !self.accepts(&identity, frame.watch_generation) {
            return false;
        }
        self.ended.remove(&identity);
        self.errors.remove(&identity);
        self.latest_generations
            .insert(identity.clone(), frame.watch_generation);
        self.frames.insert(identity, frame);
        true
    }
    pub fn end_track(&mut self, track: &str, generation: u64) {
        let identity = self
            .frames
            .iter()
            .find(|(_, frame)| frame.track_id == track && frame.watch_generation == generation)
            .map(|(identity, _)| identity.clone());
        if let Some(identity) = identity {
            self.frames.remove(&identity);
            self.ended.insert(identity);
        }
    }
    pub fn unavailable(&mut self, identity: &str) {
        if let Some(generation) = self.latest_generations.get(identity) {
            self.stopped_generations
                .insert(identity.into(), *generation);
        }
        self.desired.remove(identity);
        self.frames.remove(identity);
        self.ended.insert(identity.into());
        self.errors.remove(identity);
    }
    /// Only media failures while awaiting this focused stream belong on its stage.
    pub fn fail_pending(&mut self, message: String) -> bool {
        if let Some(identity) = &self.focused
            && self.desired.contains(identity)
            && !self.frames.contains_key(identity)
        {
            self.errors.insert(identity.clone(), message);
            true
        } else {
            false
        }
    }
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn transport_reset(&mut self) {
        self.frames.clear();
        self.errors.clear();
        self.ended.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transport_replacement_keeps_focus_and_intent_but_releases_old_pixels() {
        let mut viewer = Viewer::default();
        viewer.watch("alice".into());
        viewer.receive("alice".into(), frame("old-room", 1));
        viewer.chat_open = true;
        viewer.transport_reset();
        assert_eq!(viewer.focused.as_deref(), Some("alice"));
        assert!(viewer.desired.contains("alice"));
        assert!(viewer.frames.is_empty());
        assert!(viewer.chat_open);
        assert!(viewer.receive("alice".into(), frame("new-room", 2)));
        viewer.stop("alice");
        viewer.transport_reset();
        assert!(!viewer.receive("alice".into(), frame("old-room", 3)));
    }
    fn frame(track: &str, image: u8) -> StreamFrame<u8> {
        StreamFrame {
            track_id: track.into(),
            watch_generation: 1,
            width: 1600,
            height: 900,
            image,
        }
    }
    #[test]
    fn stop_drops_only_its_frame_and_rejects_queued_frames() {
        let mut viewer = Viewer::default();
        viewer.watch("alice".into());
        viewer.receive("alice".into(), frame("a", 1));
        viewer.watch("bob".into());
        viewer.receive("bob".into(), frame("b", 2));
        viewer.stop("bob");
        assert_eq!(viewer.focused.as_deref(), Some("alice"));
        assert_eq!(viewer.frames["alice"].image, 1);
        assert!(!viewer.frames.contains_key("bob"));
        assert!(!viewer.receive("bob".into(), frame("b", 3)));
        viewer.stop("alice");
        assert!(viewer.focused.is_none());
        assert!(viewer.frames.is_empty());
    }
    #[test]
    fn switching_focus_keeps_latest_frame_per_identity_and_actual_dimensions() {
        let mut viewer = Viewer::default();
        viewer.watch("alice".into());
        viewer.receive("alice".into(), frame("old", 1));
        viewer.receive("alice".into(), frame("new", 2));
        viewer.watch("bob".into());
        viewer.watch("alice".into());
        assert_eq!(viewer.focused.as_deref(), Some("alice"));
        assert_eq!(viewer.desired.len(), 2);
        assert_eq!(viewer.frames.len(), 1);
        assert_eq!(viewer.frames["alice"].image, 2);
        viewer.end_track("old", 1);
        assert_eq!(viewer.frames["alice"].image, 2);
        assert_eq!(
            (viewer.frames["alice"].width, viewer.frames["alice"].height),
            (1600, 900)
        );
    }
    #[test]
    fn ended_track_keeps_stage_and_unavailable_stream_rejects_frames() {
        let mut viewer = Viewer::default();
        viewer.watch("alice".into());
        viewer.receive("alice".into(), frame("a", 1));
        viewer.end_track("a", 1);
        assert!(viewer.ended.contains("alice"));
        assert_eq!(viewer.focused.as_deref(), Some("alice"));
        viewer.unavailable("alice");
        assert!(!viewer.receive("alice".into(), frame("a", 2)));
        viewer.clear();
        assert!(viewer.focused.is_none());
    }

    #[test]
    fn media_failure_belongs_only_to_pending_stage_and_frame_clears_it() {
        let mut viewer = Viewer::default();
        assert!(!viewer.fail_pending("not viewing".into()));
        viewer.watch("alice".into());
        assert!(viewer.fail_pending("decoder failed".into()));
        assert_eq!(viewer.errors["alice"], "decoder failed");
        viewer.receive("alice".into(), frame("a", 1));
        assert!(!viewer.errors.contains_key("alice"));
        assert!(!viewer.fail_pending("unrelated to healthy stream".into()));
        viewer.stop("alice");
        assert!(viewer.errors.is_empty());
    }

    #[test]
    fn rapid_rewatch_rejects_previous_generation_frames_and_same_sid_end_events() {
        let mut viewer = Viewer::default();
        viewer.watch("alice".into());
        viewer.receive("alice".into(), frame("same-sid", 1));
        viewer.stop("alice");
        viewer.watch("alice".into());
        assert!(!viewer.receive("alice".into(), frame("same-sid", 2)));
        let mut fresh = frame("same-sid", 3);
        fresh.watch_generation = 2;
        assert!(viewer.receive("alice".into(), fresh));
        viewer.end_track("same-sid", 1);
        assert_eq!(viewer.frames["alice"].image, 3);
    }
}
