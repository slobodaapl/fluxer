//! Event-driven recovery when a compositor suspends a surface frame callback.
//!
//! A dirty notification schedules a bounded fallback, not a permanent timer.
//! Normal frame callbacks invalidate that fallback. Recovery gets one replacement
//! callback; a hidden surface cannot keep generating callbacks/buffers on a timer.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct RedrawRequest {
    generation: u64,
    pub immediate: bool,
}

#[derive(Default)]
pub(super) struct RedrawScheduler {
    configured: bool,
    closed: bool,
    generation: u64,
    queued: Option<RedrawRequest>,
    recovered: bool,
}

impl RedrawScheduler {
    pub fn configure(&mut self) {
        if !self.closed {
            self.configured = true;
        }
    }

    /// Cancel in-flight recovery and gate all frame work until a new configure.
    pub fn suspend(&mut self) {
        self.configured = false;
        self.queued = None;
        self.recovered = false;
        self.generation = self.generation.wrapping_add(1);
    }

    pub fn can_draw(&self) -> bool {
        self.configured && !self.closed
    }

    pub fn close(&mut self) {
        self.closed = true;
        self.queued = None;
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    pub fn request(&mut self, immediate: bool) -> Option<RedrawRequest> {
        if self.closed
            || !self.configured
            || (!immediate && (self.recovered || self.queued.is_some()))
            || self.queued.is_some_and(|request| request.immediate)
        {
            return None;
        }
        self.generation = self.generation.wrapping_add(1);
        let request = RedrawRequest {
            generation: self.generation,
            immediate,
        };
        self.queued = Some(request);
        Some(request)
    }

    pub fn frame_arrived(&mut self) {
        self.queued = None;
        self.recovered = false;
    }

    pub fn fire(&mut self, request: RedrawRequest) -> bool {
        if self.queued != Some(request) {
            return false;
        }
        self.queued = None;
        self.recovered = true;
        true
    }
}

/// Select from outputs the surface is on now, retaining the previous choice only
/// as a tie breaker. A removed higher-scale output must never remain selected.
pub(super) fn select_output<K: Eq>(
    outputs: impl IntoIterator<Item = (K, i32)>,
    previous: Option<&K>,
) -> Option<K> {
    outputs
        .into_iter()
        .max_by_key(|(id, scale)| (*scale, Some(id) == previous))
        .map(|(id, _)| id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_presents_before_initial_configure() {
        let mut scheduler = RedrawScheduler::default();
        assert!(scheduler.request(false).is_none());
        assert!(scheduler.request(true).is_none());
        scheduler.configure();
        assert!(scheduler.request(false).is_some());
    }

    #[test]
    fn normal_frames_cancel_fallback_and_do_not_consume_recovery() {
        let mut scheduler = RedrawScheduler::default();
        scheduler.configure();
        for _ in 0..120 {
            let request = scheduler.request(false).unwrap();
            scheduler.frame_arrived();
            assert!(!scheduler.fire(request));
        }
    }

    #[test]
    fn delayed_callback_recovers_once_without_hidden_render_loop() {
        let mut scheduler = RedrawScheduler::default();
        scheduler.configure();
        let request = scheduler.request(false).unwrap();
        for _ in 0..100 {
            assert!(scheduler.request(false).is_none());
        }
        assert!(scheduler.fire(request));
        assert!(!scheduler.fire(request));
        for _ in 0..100 {
            assert!(scheduler.request(false).is_none());
        }
        scheduler.frame_arrived();
        assert!(scheduler.request(false).is_some());
    }

    #[test]
    fn lifecycle_event_promotes_fallback_to_immediate_and_coalesces() {
        let mut scheduler = RedrawScheduler::default();
        scheduler.configure();
        let fallback = scheduler.request(false).unwrap();
        let transition = scheduler.request(true).unwrap();
        assert!(transition.immediate);
        assert!(!scheduler.fire(fallback));
        for _ in 0..100 {
            assert!(scheduler.request(true).is_none());
        }
        assert!(scheduler.fire(transition));
        // Re-entry or a new configure can recover a previously hidden surface.
        let restored = scheduler.request(true).unwrap();
        assert!(scheduler.fire(restored));
    }

    #[test]
    fn old_fallback_cannot_steal_a_new_generation() {
        let mut scheduler = RedrawScheduler::default();
        scheduler.configure();
        let old = scheduler.request(false).unwrap();
        scheduler.frame_arrived();
        let new = scheduler.request(false).unwrap();
        assert!(!scheduler.fire(old));
        assert!(scheduler.fire(new));
    }

    #[test]
    fn leaving_high_scale_output_selects_remaining_output_or_none() {
        assert_eq!(select_output([("a", 2), ("b", 1)], Some(&"a")), Some("a"));
        assert_eq!(select_output([("b", 1)], Some(&"a")), Some("b"));
        assert_eq!(select_output([] as [(&str, i32); 0], Some(&"b")), None);
        assert_eq!(select_output([("b", 1)], None), Some("b"));
    }

    #[test]
    fn equal_scale_retains_current_output_only_while_present() {
        assert_eq!(select_output([("a", 1), ("b", 1)], Some(&"a")), Some("a"));
        assert_eq!(select_output([("a", 1), ("b", 1)], Some(&"b")), Some("b"));
        assert_eq!(select_output([("a", 2), ("b", 1)], Some(&"b")), Some("a"));
    }

    #[test]
    fn suspend_invalidates_old_recovery_until_remap_is_configured() {
        let mut scheduler = RedrawScheduler::default();
        scheduler.configure();
        let old = scheduler.request(true).unwrap();
        scheduler.suspend();
        assert!(!scheduler.can_draw());
        assert!(!scheduler.fire(old));
        assert!(scheduler.request(true).is_none());
        scheduler.configure();
        assert!(scheduler.can_draw());
        assert!(!scheduler.fire(old));
        assert!(scheduler.request(true).is_some());
    }

    #[test]
    fn close_cancels_queued_work_and_cannot_be_rearmed() {
        let mut scheduler = RedrawScheduler::default();
        scheduler.configure();
        let request = scheduler.request(true).unwrap();
        scheduler.close();
        assert!(scheduler.is_closed());
        assert!(!scheduler.fire(request));
        scheduler.frame_arrived();
        scheduler.configure();
        assert!(scheduler.request(false).is_none());
        assert!(scheduler.request(true).is_none());
    }
}
