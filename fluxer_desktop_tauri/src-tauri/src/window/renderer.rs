// SPDX-License-Identifier: AGPL-3.0-or-later

//! Recovery when the main window's web process dies: `render-process-gone` in Window.ts
//! `createWindow` and `getMainWindowRendererGoneAction` of WindowRendererLifecycle.ts. Only
//! WebKitGTK reports it (`web-process-terminated`); wry exposes no such hook for WebView2 or
//! WKWebView, so on Windows and macOS a dead renderer is left as it is.
//!
//! WebKit's reasons map onto Electron's: a crash is `crashed`, the memory limit `oom`, a
//! termination through the API `killed`, and a reason newer than these `abnormal-exit`. WebKit
//! reports no clean exit, so Electron's `clean-exit` rows never apply.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tauri::{Manager, WebviewWindow};
use webkit2gtk::{WebProcessTerminationReason, WebViewExt};

use super::policy::MainWindowPolicy;
use super::session::MainWindowSession;
use crate::config::DesktopConfig;

/// `RENDERER_GONE_REPEAT_WINDOW_MS`.
const REPEAT_WINDOW: Duration = Duration::from_millis(30000);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GoneReason {
    Crashed,
    Oom,
    Killed,
    AbnormalExit,
}

impl GoneReason {
    fn from_webkit(reason: WebProcessTerminationReason) -> Self {
        match reason {
            WebProcessTerminationReason::Crashed => Self::Crashed,
            WebProcessTerminationReason::ExceededMemoryLimit => Self::Oom,
            WebProcessTerminationReason::TerminatedByApi => Self::Killed,
            _ => Self::AbnormalExit,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Crashed => "crashed",
            Self::Oom => "oom",
            Self::Killed => "killed",
            Self::AbnormalExit => "abnormal-exit",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GoneAction {
    Reload,
    DeferReload,
    Ignore,
    Quit,
}

/// `MainWindowRendererGoneContext`.
struct GoneContext {
    darwin: bool,
    quitting: bool,
    hidden: bool,
    close_to_tray: bool,
    reloaded_recently: bool,
}

/// `getMainWindowRendererGoneAction`.
fn gone_action(reason: GoneReason, context: &GoneContext) -> GoneAction {
    if context.quitting {
        return GoneAction::Ignore;
    }
    if context.darwin && context.hidden && context.close_to_tray {
        if context.reloaded_recently {
            return GoneAction::DeferReload;
        }
        if reason == GoneReason::Killed {
            return GoneAction::Reload;
        }
    }
    if reason == GoneReason::Killed || context.reloaded_recently {
        return GoneAction::Quit;
    }
    GoneAction::Reload
}

#[derive(Default)]
struct TrackerState {
    /// `mainWindowRendererGone`.
    gone: bool,
    /// `rendererGoneReloaded`.
    reloaded: bool,
    /// `lastRendererGoneAt`.
    last_gone_at: Option<Instant>,
}

/// The renderer-gone flags of one main window.
#[derive(Default)]
pub struct Tracker(Mutex<TrackerState>);

impl Tracker {
    fn lock(&self) -> std::sync::MutexGuard<'_, TrackerState> {
        self.0.lock().expect("renderer tracker lock")
    }

    /// `did-finish-load`.
    pub fn page_loaded(&self) {
        let mut state = self.lock();
        state.gone = false;
        state.reloaded = false;
    }

    /// Records a termination at `now`; true when it repeats one that was reloaded less than
    /// [`REPEAT_WINDOW`] ago.
    fn record_gone(&self, now: Instant) -> bool {
        let mut state = self.lock();
        let repeated = state
            .last_gone_at
            .is_some_and(|last| now.duration_since(last) < REPEAT_WINDOW);
        state.last_gone_at = Some(now);
        state.gone = true;
        state.reloaded && repeated
    }
}

/// Watches the main webview's web process.
pub fn observe(window: &WebviewWindow, session: &Arc<MainWindowSession>) -> tauri::Result<()> {
    let target = window.clone();
    let session = session.clone();
    window.with_webview(move |webview| {
        webview
            .inner()
            .connect_web_process_terminated(move |_, reason| {
                let reason = GoneReason::from_webkit(reason);
                tracing::error!(reason = reason.as_str(), "Render process gone");
                let window = target.clone();
                let session = session.clone();
                // Off the GTK signal: the decision reads window state through Tauri.
                tauri::async_runtime::spawn(async move {
                    on_gone(&window, &session, reason);
                });
            });
    })
}

fn on_gone(window: &WebviewWindow, session: &Arc<MainWindowSession>, reason: GoneReason) {
    crate::streaming_priority::reset(window.app_handle());
    let reloaded_recently = session.renderer.record_gone(Instant::now());
    let policy = window.state::<MainWindowPolicy>();
    let context = GoneContext {
        darwin: cfg!(target_os = "macos"),
        quitting: policy.is_quitting(),
        hidden: !window.is_visible().unwrap_or(true),
        close_to_tray: policy.hides_on_close(&window.state::<Arc<DesktopConfig>>()),
        reloaded_recently,
    };
    match gone_action(reason, &context) {
        GoneAction::Quit => {
            tracing::error!(
                reason = reason.as_str(),
                reloaded_recently,
                "Quitting after renderer process termination"
            );
            window.app_handle().exit(0);
        }
        GoneAction::DeferReload => tracing::warn!(
            reason = reason.as_str(),
            "Deferring hidden main window renderer recovery until the window is shown"
        ),
        GoneAction::Reload => {
            session.renderer.lock().reloaded = true;
            reload(window, session, false);
        }
        GoneAction::Ignore => {}
    }
}

/// `reloadMainWindowRendererAfterGone`; `unless_loading` is the check
/// `recoverMainWindowRendererBeforeShow` adds.
fn reload(window: &WebviewWindow, session: &Arc<MainWindowSession>, unless_loading: bool) {
    let session = session.clone();
    let reloaded = window.with_webview(move |webview| {
        let view = webview.inner();
        if unless_loading && view.is_loading() {
            return;
        }
        session.renderer.lock().gone = false;
        tracing::warn!("Reloading main window renderer after termination");
        view.reload_bypass_cache();
    });
    if let Err(error) = reloaded {
        tracing::error!(%error, "Failed to reload the main window renderer");
    }
}

/// `recoverMainWindowRendererBeforeShow`: a renderer left dead while hidden reloads on show.
pub fn recover_before_show(window: &WebviewWindow, session: &Arc<MainWindowSession>) {
    if session.renderer.lock().gone {
        reload(window, session, true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> GoneContext {
        GoneContext {
            darwin: false,
            quitting: false,
            hidden: false,
            close_to_tray: false,
            reloaded_recently: false,
        }
    }

    #[test]
    fn crashes_reload_until_one_repeats_after_a_reload() {
        for reason in [
            GoneReason::Crashed,
            GoneReason::Oom,
            GoneReason::AbnormalExit,
        ] {
            assert_eq!(gone_action(reason, &context()), GoneAction::Reload);
            let repeated = GoneContext {
                reloaded_recently: true,
                ..context()
            };
            assert_eq!(gone_action(reason, &repeated), GoneAction::Quit);
        }
        assert_eq!(
            gone_action(GoneReason::Killed, &context()),
            GoneAction::Quit
        );
    }

    #[test]
    fn nothing_happens_while_quitting() {
        let quitting = GoneContext {
            quitting: true,
            reloaded_recently: true,
            ..context()
        };
        assert_eq!(
            gone_action(GoneReason::Killed, &quitting),
            GoneAction::Ignore
        );
    }

    #[test]
    fn a_hidden_macos_window_kept_for_the_dock_recovers_later() {
        let kept = GoneContext {
            darwin: true,
            hidden: true,
            close_to_tray: true,
            ..context()
        };
        assert_eq!(gone_action(GoneReason::Killed, &kept), GoneAction::Reload);
        assert_eq!(gone_action(GoneReason::Crashed, &kept), GoneAction::Reload);
        let repeated = GoneContext {
            reloaded_recently: true,
            ..kept
        };
        assert_eq!(
            gone_action(GoneReason::Crashed, &repeated),
            GoneAction::DeferReload
        );
        // Shown, the same window follows the general rule.
        let shown = GoneContext {
            hidden: false,
            ..kept
        };
        assert_eq!(gone_action(GoneReason::Killed, &shown), GoneAction::Quit);
    }

    #[test]
    fn a_repeat_counts_only_after_a_reload_within_the_window() {
        let tracker = Tracker::default();
        let start = Instant::now();
        assert!(!tracker.record_gone(start));
        tracker.lock().reloaded = true;
        assert!(tracker.record_gone(start + Duration::from_secs(29)));
        assert!(!tracker.record_gone(start + Duration::from_secs(60)));
        // A finished load clears the reload, so the next termination is a first one again.
        tracker.page_loaded();
        assert!(!tracker.record_gone(start + Duration::from_secs(61)));
    }
}
