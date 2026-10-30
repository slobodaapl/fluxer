// SPDX-License-Identifier: AGPL-3.0-or-later

//! Whether closing or minimizing the main window hides it instead, and whether the app keeps
//! running once no window is left: `shouldHideMainWindowOnClose`/`OnMinimize` and `isQuitting`
//! of fluxer_desktop/src/main/Window.ts, and `window-all-closed`/`before-quit` of main/index.ts.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{AppHandle, ExitRequestApi, Manager};

use super::MAIN_WINDOW_LABEL;
use super::session::MainWindowSlot;
use crate::config::DesktopConfig;

#[derive(Default)]
pub struct MainWindowPolicy {
    quitting: AtomicBool,
    /// `hasActiveDesktopTray()`.
    tray_active: AtomicBool,
}

impl MainWindowPolicy {
    pub fn is_quitting(&self) -> bool {
        self.quitting.load(Ordering::SeqCst)
    }

    /// For the tray, which reports whether its icon exists.
    pub fn set_tray_active(&self, active: bool) {
        self.tray_active.store(active, Ordering::SeqCst);
    }

    /// `shouldHideMainWindowOnClose`; `window-all-closed` keeps the app running on the same
    /// rule.
    pub fn hides_on_close(&self, config: &DesktopConfig) -> bool {
        let settings = config.window_behavior();
        cfg!(target_os = "macos")
            || (self.tray_active.load(Ordering::SeqCst)
                && settings.show_tray_icon
                && settings.close_to_tray)
    }

    /// `shouldHideMainWindowOnMinimize`.
    pub fn hides_on_minimize(&self, config: &DesktopConfig) -> bool {
        let settings = config.window_behavior();
        self.tray_active.load(Ordering::SeqCst)
            && settings.show_tray_icon
            && settings.minimize_to_tray
    }
}

/// `RunEvent::ExitRequested`. Without a code the last window closed (`window-all-closed`): the
/// app stays alive on macOS and, elsewhere, while the tray keeps it reachable. Any exit that
/// goes ahead is `before-quit`, which marks the app quitting; `AppHandle::exit` and `restart`
/// close no window through `CloseRequested`, so the main window's state is saved here.
pub fn on_exit_requested(app: &AppHandle, code: Option<i32>, api: &ExitRequestApi) {
    let policy = app.state::<MainWindowPolicy>();
    if code.is_none() && policy.hides_on_close(&app.state::<Arc<DesktopConfig>>()) {
        api.prevent_exit();
        if !cfg!(target_os = "macos") {
            tracing::info!(
                "All windows closed; keeping app alive because close-to-tray is enabled"
            );
        }
        return;
    }
    policy.quitting.store(true, Ordering::SeqCst);
    if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL)
        && let Some(session) = app.state::<MainWindowSlot>().current()
    {
        session.state.cancel_pending_save();
        session.state.save(&window);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config(settings: serde_json::Value) -> (tempfile::TempDir, DesktopConfig) {
        let directory = tempfile::tempdir().unwrap();
        let config = DesktopConfig::load(directory.path(), &[]);
        config.set_window_behavior(&settings);
        (directory, config)
    }

    #[test]
    fn hiding_needs_an_active_tray_outside_macos() {
        let policy = MainWindowPolicy::default();
        let (_directory, config) = config(json!({"minimizeToTray": true, "closeToTray": true}));
        assert_eq!(policy.hides_on_close(&config), cfg!(target_os = "macos"));
        assert!(!policy.hides_on_minimize(&config));
        policy.set_tray_active(true);
        assert!(policy.hides_on_close(&config));
        assert!(policy.hides_on_minimize(&config));
    }

    #[test]
    fn hiding_follows_the_tray_settings() {
        let policy = MainWindowPolicy::default();
        policy.set_tray_active(true);
        let (_directory, config) = config(json!({"closeToTray": false}));
        assert_eq!(policy.hides_on_close(&config), cfg!(target_os = "macos"));
        assert!(!policy.hides_on_minimize(&config));
        // Without the tray icon the tray behaviours are off whatever they were set to.
        config.set_window_behavior(&json!({
            "showTrayIcon": false, "closeToTray": true, "minimizeToTray": true
        }));
        assert_eq!(policy.hides_on_close(&config), cfg!(target_os = "macos"));
        assert!(!policy.hides_on_minimize(&config));
    }
}
