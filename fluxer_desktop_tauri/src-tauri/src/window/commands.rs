// SPDX-License-Identifier: AGPL-3.0-or-later

//! The window channels of fluxer_desktop/src/main/IpcHandlers.ts and the first-content-painted
//! listener of Window.ts. Window controls act on the calling webview's window; attention
//! requests act on the main window, as Electron's did.

use std::sync::Arc;

use tauri::{AppHandle, Manager, State, UserAttentionType, Webview, WebviewWindow};

use super::policy::MainWindowPolicy;
use super::session::MainWindowSlot;
use super::{MAIN_WINDOW_LABEL, reveal};
use crate::config::DesktopConfig;

/// `window-minimize`: the main window hides to the tray when it minimizes there.
#[tauri::command]
pub fn window_minimize(
    window: WebviewWindow,
    policy: State<'_, MainWindowPolicy>,
    config: State<'_, Arc<DesktopConfig>>,
) -> tauri::Result<()> {
    if window.label() == MAIN_WINDOW_LABEL && policy.hides_on_minimize(&config) {
        window.hide()?;
        crate::tray::refresh_menu(window.app_handle());
        return Ok(());
    }
    window.minimize()
}

#[tauri::command]
pub fn window_toggle_maximize(window: WebviewWindow) -> tauri::Result<()> {
    if window.is_maximized()? {
        window.unmaximize()
    } else {
        window.maximize()
    }
}

/// `window-close`: a close request, which the main window may turn into a hide.
#[tauri::command]
pub fn window_close(window: WebviewWindow) -> tauri::Result<()> {
    window.close()
}

#[tauri::command]
pub fn window_is_maximized(window: WebviewWindow) -> bool {
    window.is_maximized().unwrap_or(false)
}

/// A press on a titlebar drag region (bridge/drag.ts); Chromium dragged `-webkit-app-region`
/// itself.
#[tauri::command]
pub fn window_start_dragging(window: WebviewWindow) -> tauri::Result<()> {
    window.start_dragging()
}

/// `set-zoom-factor`, which ignored anything but a positive factor.
#[tauri::command]
pub fn set_zoom_factor(webview: Webview, factor: f64) -> tauri::Result<()> {
    if factor > 0.0 && factor.is_finite() {
        webview.set_zoom(factor)?;
    }
    Ok(())
}

/// `desktop-window:first-content-painted`, accepted from the main window's document only.
#[tauri::command]
pub fn window_first_content_painted(
    app: AppHandle,
    webview: Webview,
    slot: State<'_, MainWindowSlot>,
) {
    if webview.label() != MAIN_WINDOW_LABEL {
        return;
    }
    if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL)
        && let Some(session) = slot.current()
    {
        reveal::content_painted(&window, &session);
    }
}

/// `window-flash` (`flashWindowForAttention`): none on macOS or while the main window has
/// focus. As with Electron's `flashFrame`, `persistent: false` stops a flash instead of
/// starting one.
#[tauri::command]
pub fn window_flash(app: AppHandle, persistent: bool) -> tauri::Result<()> {
    if cfg!(target_os = "macos") {
        return Ok(());
    }
    let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) else {
        return Ok(());
    };
    if window.is_focused()? {
        return Ok(());
    }
    window.request_user_attention(persistent.then_some(UserAttentionType::Critical))
}

/// `window-stop-flash` (`stopFlashingWindow`).
#[tauri::command]
pub fn window_stop_flash(app: AppHandle) -> tauri::Result<()> {
    match app.get_webview_window(MAIN_WINDOW_LABEL) {
        Some(window) => window.request_user_attention(None),
        None => Ok(()),
    }
}
