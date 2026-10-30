// SPDX-License-Identifier: AGPL-3.0-or-later

//! When the hidden main window first shows: `MainWindowRevealGate` of
//! fluxer_desktop/src/main/MainWindowReveal.ts and its wiring in Window.ts `createWindow`. The
//! window reveals as soon as the renderer reports painted content, 6 s after its document
//! loaded, or 10 s after creation, whichever comes first. A start-minimized launch settles the
//! gate without showing the window.
//!
//! Electron armed the grace period on `ready-to-show` (first paint); the closest signal here is
//! the main document's load finishing.

use std::sync::Arc;
use std::time::Duration;

use tauri::WebviewWindow;

use super::session::MainWindowSession;
use super::show;

const CONTENT_GRACE: Duration = Duration::from_millis(6000);
const HARD_FALLBACK: Duration = Duration::from_millis(10000);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RevealReason {
    ContentPainted,
    ContentGraceExpired,
    HardFallback,
}

impl RevealReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::ContentPainted => "content-painted",
            Self::ContentGraceExpired => "content-grace-expired",
            Self::HardFallback => "hard-fallback",
        }
    }
}

#[derive(Clone, Copy)]
enum RevealTimer {
    Grace,
    Hard,
}

/// `MainWindowRevealGate` without its timers: the arming methods say whether to start one, and
/// a timer that fires reports through [`RevealGate::expire`]. A reveal happens at most once;
/// timers that fire after it are ignored, as cancelled ones were.
#[derive(Default)]
pub struct RevealGate {
    grace_armed: bool,
    hard_armed: bool,
    settled: bool,
}

impl RevealGate {
    pub fn is_settled(&self) -> bool {
        self.settled
    }

    /// `start`: true when the hard fallback timer is to be started.
    fn start(&mut self) -> bool {
        if self.settled || self.hard_armed {
            return false;
        }
        self.hard_armed = true;
        true
    }

    /// `markReadyToShow`: true when the content grace timer is to be started.
    fn mark_ready_to_show(&mut self) -> bool {
        if self.settled || self.grace_armed {
            return false;
        }
        self.grace_armed = true;
        true
    }

    fn mark_content_painted(&mut self) -> Option<RevealReason> {
        self.reveal(RevealReason::ContentPainted)
    }

    fn expire(&mut self, timer: RevealTimer) -> Option<RevealReason> {
        self.reveal(match timer {
            RevealTimer::Grace => RevealReason::ContentGraceExpired,
            RevealTimer::Hard => RevealReason::HardFallback,
        })
    }

    /// `dispose`: settles without revealing.
    pub fn dispose(&mut self) {
        self.settled = true;
    }

    fn reveal(&mut self, reason: RevealReason) -> Option<RevealReason> {
        if self.settled {
            return None;
        }
        self.settled = true;
        Some(reason)
    }
}

/// `revealGate.start()`, right after the window is built.
pub fn start(window: &WebviewWindow, session: &Arc<MainWindowSession>) {
    if session.reveal_gate().start() {
        schedule(window, session, RevealTimer::Hard, HARD_FALLBACK);
    }
}

/// `ready-to-show`.
pub fn ready_to_show(window: &WebviewWindow, session: &Arc<MainWindowSession>) {
    if session.reveal_gate().mark_ready_to_show() {
        schedule(window, session, RevealTimer::Grace, CONTENT_GRACE);
    }
}

/// `desktop-window:first-content-painted` from the main document.
pub fn content_painted(window: &WebviewWindow, session: &Arc<MainWindowSession>) {
    let reason = session.reveal_gate().mark_content_painted();
    if let Some(reason) = reason {
        reveal(window, session, reason);
    }
}

fn schedule(
    window: &WebviewWindow,
    session: &Arc<MainWindowSession>,
    timer: RevealTimer,
    delay: Duration,
) {
    let window = window.clone();
    let session = session.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(delay).await;
        let reason = session.reveal_gate().expire(timer);
        if let Some(reason) = reason {
            reveal(&window, &session, reason);
        }
    });
}

/// `onReveal`. Electron then signalled the boot splash (`signalMainWindowReady`); this shell has
/// no splash yet.
fn reveal(window: &WebviewWindow, session: &MainWindowSession, reason: RevealReason) {
    let shows_window = session.reveal_shows_window();
    tracing::info!(
        reason = reason.as_str(),
        reveal_shows_window = shows_window,
        elapsed_ms = session.elapsed().as_millis() as u64,
        "Main window revealed"
    );
    if reason != RevealReason::ContentPainted {
        tracing::warn!(
            reason = reason.as_str(),
            "The renderer never reported painted content, revealing the main window anyway"
        );
    }
    if !shows_window || window.is_visible().unwrap_or(true) {
        return;
    }
    session.apply_pending_maximize(window);
    show::show_and_focus(window, session);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn painted_content_reveals_once_and_silences_the_timers() {
        let mut gate = RevealGate::default();
        assert!(gate.start());
        assert!(gate.mark_ready_to_show());
        assert_eq!(
            gate.mark_content_painted(),
            Some(RevealReason::ContentPainted)
        );
        assert!(gate.is_settled());
        assert_eq!(gate.mark_content_painted(), None);
        assert_eq!(gate.expire(RevealTimer::Grace), None);
        assert_eq!(gate.expire(RevealTimer::Hard), None);
        assert!(!gate.mark_ready_to_show());
    }

    #[test]
    fn each_timer_is_armed_once() {
        let mut gate = RevealGate::default();
        assert!(gate.start());
        assert!(!gate.start());
        assert!(gate.mark_ready_to_show());
        assert!(!gate.mark_ready_to_show());
    }

    #[test]
    fn expiry_reports_the_timer_that_fired_first() {
        let mut gate = RevealGate::default();
        gate.start();
        gate.mark_ready_to_show();
        assert_eq!(
            gate.expire(RevealTimer::Grace),
            Some(RevealReason::ContentGraceExpired)
        );
        assert_eq!(gate.expire(RevealTimer::Hard), None);

        let mut gate = RevealGate::default();
        gate.start();
        assert_eq!(
            gate.expire(RevealTimer::Hard),
            Some(RevealReason::HardFallback)
        );
    }

    #[test]
    fn a_disposed_gate_never_reveals() {
        let mut gate = RevealGate::default();
        gate.start();
        gate.dispose();
        assert_eq!(gate.mark_content_painted(), None);
        assert_eq!(gate.expire(RevealTimer::Hard), None);
        assert!(!gate.start());
    }
}
