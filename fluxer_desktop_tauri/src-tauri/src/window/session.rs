// SPDX-License-Identifier: AGPL-3.0-or-later

//! The state Window.ts kept per main window in `createWindow` closures: the reveal gate and
//! whether it shows the window, the maximize deferred from saved state, the saved-bounds
//! tracker, and the last maximized/minimized state seen (Tauri has no events for those, so the
//! transitions are read off `Resized`). A recreated main window gets a new session.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use tauri::WebviewWindow;

use super::reveal::RevealGate;
use super::state;

pub struct MainWindowSession {
    started_at: Instant,
    gate: Mutex<RevealGate>,
    /// `revealShowsWindow`: false for a start-minimized launch until something asks to show.
    reveal_shows_window: AtomicBool,
    pending_maximize: AtomicBool,
    maximized: AtomicBool,
    minimized: AtomicBool,
    pub state: Arc<state::Tracker>,
    #[cfg(target_os = "linux")]
    pub renderer: super::renderer::Tracker,
    #[cfg(windows)]
    pub live_resize: Arc<super::live_resize::LiveResize>,
    /// `pendingHides` of MacFullScreenHide.ts.
    #[cfg(target_os = "macos")]
    pub fullscreen_hide_pending: AtomicBool,
}

impl MainWindowSession {
    pub fn new(
        started_at: Instant,
        start_hidden: bool,
        normal_bounds: state::Bounds,
        saved_maximized: bool,
    ) -> Self {
        Self {
            started_at,
            gate: Mutex::new(RevealGate::default()),
            reveal_shows_window: AtomicBool::new(!start_hidden),
            pending_maximize: AtomicBool::new(saved_maximized),
            maximized: AtomicBool::new(false),
            minimized: AtomicBool::new(false),
            state: Arc::new(state::Tracker::new(normal_bounds, saved_maximized)),
            #[cfg(target_os = "linux")]
            renderer: super::renderer::Tracker::default(),
            #[cfg(windows)]
            live_resize: Arc::default(),
            #[cfg(target_os = "macos")]
            fullscreen_hide_pending: AtomicBool::new(false),
        }
    }

    pub fn reveal_gate(&self) -> MutexGuard<'_, RevealGate> {
        self.gate.lock().expect("reveal gate lock")
    }

    pub fn elapsed(&self) -> Duration {
        self.started_at.elapsed()
    }

    pub fn reveal_shows_window(&self) -> bool {
        self.reveal_shows_window.load(Ordering::SeqCst)
    }

    /// `pendingMainWindowReveal.requestShow()`.
    pub fn request_show(&self) {
        self.reveal_shows_window.store(true, Ordering::SeqCst);
    }

    /// `applyPendingMaximize`.
    pub fn apply_pending_maximize(&self, window: &WebviewWindow) {
        if !self.pending_maximize.swap(false, Ordering::SeqCst) {
            return;
        }
        if let Err(error) = window.maximize() {
            tracing::warn!(%error, "Failed to maximize the main window");
        }
    }

    /// The main window's `show` listeners: the pending maximize, then the gate is disposed.
    pub fn shown(&self, window: &WebviewWindow) {
        self.apply_pending_maximize(window);
        self.reveal_gate().dispose();
    }

    /// Records the maximized state; `Some(state)` when it changed.
    pub fn maximized_changed(&self, maximized: bool) -> Option<bool> {
        (self.maximized.swap(maximized, Ordering::SeqCst) != maximized).then_some(maximized)
    }

    /// Records the minimized state; `Some(state)` when it changed.
    pub fn minimized_changed(&self, minimized: bool) -> Option<bool> {
        (self.minimized.swap(minimized, Ordering::SeqCst) != minimized).then_some(minimized)
    }
}

/// The current main window's session, for callers outside its event handlers.
#[derive(Default)]
pub struct MainWindowSlot(Mutex<Option<Arc<MainWindowSession>>>);

impl MainWindowSlot {
    fn lock(&self) -> MutexGuard<'_, Option<Arc<MainWindowSession>>> {
        self.0.lock().expect("main window slot lock")
    }

    pub fn current(&self) -> Option<Arc<MainWindowSession>> {
        self.lock().clone()
    }

    pub fn set(&self, session: Arc<MainWindowSession>) {
        *self.lock() = Some(session);
    }

    /// Empties the slot if it still holds `session`.
    pub fn clear(&self, session: &Arc<MainWindowSession>) {
        let mut current = self.lock();
        if current
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, session))
        {
            *current = None;
        }
    }
}
