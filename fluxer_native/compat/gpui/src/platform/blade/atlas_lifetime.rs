//! Atlas eviction removes a CPU key, not the GPU's ownership of its texture.
//! Kept independent of the GPU backend so submission/scene races are testable.

pub(super) struct AtlasLifetime<S> {
    keys: u32,
    pending_upload: bool,
    last_submission: Option<S>,
}

impl<S: Clone> AtlasLifetime<S> {
    pub fn new() -> Self {
        Self {
            keys: 0,
            pending_upload: true, // Newly created textures also need initialization.
            last_submission: None,
        }
    }

    pub fn add_key(&mut self) {
        self.keys += 1;
        self.pending_upload = true;
    }

    pub fn remove_key(&mut self) {
        assert!(self.keys > 0, "atlas key removed twice");
        self.keys -= 1;
    }

    pub fn has_keys(&self) -> bool {
        self.keys != 0
    }

    pub fn submitted(&mut self, sync: &S) {
        self.pending_upload = false;
        self.last_submission = Some(sync.clone());
    }

    pub fn can_destroy(&self, in_scene: bool, completed: impl FnOnce(&S) -> bool) -> bool {
        !self.has_keys()
            && !self.pending_upload
            && !in_scene
            && self.last_submission.as_ref().is_some_and(completed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evicted_animation_frame_waits_for_its_own_submission() {
        let mut frame = AtlasLifetime::new();
        frame.add_key();
        frame.submitted(&10);
        frame.remove_key();
        // The previous frame completed; the latest frame is still using it.
        assert!(!frame.can_destroy(false, |sync| *sync <= 9));
        assert!(frame.can_destroy(false, |sync| *sync <= 10));
    }

    #[test]
    fn retained_scene_can_resubmit_an_evicted_texture() {
        let mut frame = AtlasLifetime::new();
        frame.add_key();
        frame.submitted(&1);
        frame.remove_key();
        assert!(!frame.can_destroy(true, |_| true));
        frame.submitted(&2); // Redraw recovery presents the retained scene.
        assert!(!frame.can_destroy(false, |sync| *sync <= 1));
        assert!(frame.can_destroy(false, |sync| *sync <= 2));
    }

    #[test]
    fn eviction_before_first_draw_keeps_pending_initialization_and_upload() {
        let mut frame = AtlasLifetime::new();
        frame.add_key();
        frame.remove_key();
        assert!(!frame.can_destroy(false, |_| true));
        frame.submitted(&3);
        assert!(!frame.can_destroy(false, |_| false));
        assert!(frame.can_destroy(false, |_| true));
    }

    #[test]
    fn shared_texture_waits_for_all_keys_and_new_uploads() {
        let mut frame = AtlasLifetime::new();
        frame.add_key();
        frame.submitted(&1);
        frame.add_key();
        frame.remove_key();
        assert!(!frame.can_destroy(false, |_| true));
        frame.remove_key();
        assert!(!frame.can_destroy(false, |_| true));
        frame.submitted(&2);
        assert!(frame.can_destroy(false, |_| true));
    }
}
