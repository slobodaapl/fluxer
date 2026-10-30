// SPDX-License-Identifier: AGPL-3.0-or-later

//! Showing, focusing and hiding app windows: `showWindow`, `focusWindow` and `hideWindow` of
//! fluxer_desktop/src/main/Window.ts with `hideWindowLeavingFullScreen` of MacFullScreenHide.ts.
//! The tray, notifications and second-instance handling show the main window through
//! [`show_main_window`].
//!
//! tao drops `set_focus` while a window is hidden or minimized, and on Linux `show` and
//! `unminimize` only queue a request (the minimized state clears when the window manager
//! answers). A focus asked for right after them is retried once, [`FOCUS_RETRY_DELAY`] later
//! from a runtime thread, by which point the queued show has run and the window manager has had
//! a round trip to restore the window. A single retry cannot loop.

use std::time::Duration;

use tauri::{AppHandle, Manager, WebviewWindow};

use super::MAIN_WINDOW_LABEL;
use super::session::{MainWindowSession, MainWindowSlot};
use super::state;

const FOCUS_RETRY_DELAY: Duration = Duration::from_millis(100);
/// How long macOS keeps the window on every Space while it is pulled to the current one.
#[cfg(target_os = "macos")]
const ALL_WORKSPACES_DURATION: Duration = Duration::from_millis(250);

/// Focuses `window`, retrying once when `was_hidden_or_minimized` may have dropped the call.
fn focus(window: &WebviewWindow, was_hidden_or_minimized: bool) {
    if let Err(error) = window.set_focus() {
        tracing::warn!(%error, label = window.label(), "Failed to focus the window");
        return;
    }
    if !was_hidden_or_minimized {
        return;
    }
    let window = window.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FOCUS_RETRY_DELAY).await;
        if !window.is_focused().unwrap_or(true) {
            let _ = window.set_focus();
        }
    });
}

fn is_hidden_or_minimized(window: &WebviewWindow) -> bool {
    !window.is_visible().unwrap_or(true) || window.is_minimized().unwrap_or(false)
}

/// `show()` and `focus()` of the main window, followed by its `show` listeners.
pub fn show_and_focus(window: &WebviewWindow, session: &MainWindowSession) {
    let was_hidden_or_minimized = is_hidden_or_minimized(window);
    if let Err(error) = window.show() {
        tracing::warn!(%error, "Failed to show the main window");
        return;
    }
    session.shown(window);
    crate::tray::refresh_menu(window.app_handle());
    #[cfg(windows)]
    crate::unread_badge::refresh_windows_overlay(window.app_handle());
    focus(window, was_hidden_or_minimized);
}

/// `showWindow`. While the reveal gate is still holding a hidden window, the request only makes
/// the reveal show it.
pub fn show_main_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) else {
        return;
    };
    let Some(session) = app.state::<MainWindowSlot>().current() else {
        return;
    };
    if !session.reveal_gate().is_settled() && !window.is_visible().unwrap_or(true) {
        session.request_show();
        return;
    }
    #[cfg(target_os = "macos")]
    session
        .fullscreen_hide_pending
        .store(false, std::sync::atomic::Ordering::SeqCst);
    if window.is_minimized().unwrap_or(false)
        && let Err(error) = window.unminimize()
    {
        tracing::warn!(%error, "Failed to restore the main window");
    }
    state::ensure_on_screen(&window);
    #[cfg(target_os = "linux")]
    super::renderer::recover_before_show(&window, &session);
    #[cfg(target_os = "macos")]
    show_on_current_space(app, &window);
    show_and_focus(&window, &session);
}

/// The macOS half of `showWindow`: the dock icon back, the app activated (`app.focus({steal:
/// true})`), and the window briefly on every Space so it shows on the current one. Tauri has no
/// `visibleOnFullScreen` option. Not verified on macOS.
#[cfg(target_os = "macos")]
fn show_on_current_space(app: &AppHandle, window: &WebviewWindow) {
    if let Err(error) = app.set_dock_visibility(true) {
        tracing::warn!(%error, "Failed to show dock");
    }
    if let Err(error) = app.show() {
        tracing::warn!(%error, "Failed to focus app");
    }
    if let Err(error) = window.set_visible_on_all_workspaces(true) {
        tracing::warn!(%error, "Failed to set visible on all workspaces");
        return;
    }
    let window = window.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(ALL_WORKSPACES_DURATION).await;
        if let Err(error) = window.set_visible_on_all_workspaces(false) {
            tracing::warn!(%error, "Failed to disable visible on all workspaces");
        }
    });
}

/// `focusWindow`, for the popout and event sink windows. Tauri has no `moveTop`; focusing
/// raises the window.
pub fn focus_window(window: &WebviewWindow) {
    let was_hidden_or_minimized = is_hidden_or_minimized(window);
    if window.is_minimized().unwrap_or(false)
        && let Err(error) = window.unminimize()
    {
        tracing::warn!(%error, label = window.label(), "Failed to restore the window");
    }
    if !window.is_visible().unwrap_or(true)
        && let Err(error) = window.show()
    {
        tracing::warn!(%error, label = window.label(), "Failed to show the window");
    }
    focus(window, was_hidden_or_minimized);
}

/// `hideWindow`, for the tray.
pub fn hide_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL)
        && let Some(session) = app.state::<MainWindowSlot>().current()
    {
        hide(&window, &session);
    }
}

/// `hideWindowLeavingFullScreen`: a fullscreen macOS window leaves fullscreen first and hides
/// once it has (`finish_fullscreen_hide`); elsewhere the window hides at once. The main
/// window's `hide` listener refreshes the tray menu.
#[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
pub fn hide(window: &WebviewWindow, session: &MainWindowSession) {
    #[cfg(target_os = "macos")]
    if window.is_fullscreen().unwrap_or(false) {
        if !session
            .fullscreen_hide_pending
            .swap(true, std::sync::atomic::Ordering::SeqCst)
            && let Err(error) = window.set_fullscreen(false)
        {
            tracing::warn!(%error, "Failed to leave fullscreen before hiding");
        }
        return;
    }
    if let Err(error) = window.hide() {
        tracing::warn!(%error, "Failed to hide the main window");
    }
    crate::tray::refresh_menu(window.app_handle());
}

/// The `leave-full-screen` listener of a pending hide. Tauri has no fullscreen events, so this
/// runs on every resize and hides once the window is out of fullscreen. Not verified on macOS.
#[cfg(target_os = "macos")]
pub fn finish_fullscreen_hide(window: &WebviewWindow, session: &MainWindowSession) {
    use std::sync::atomic::Ordering;
    if !session.fullscreen_hide_pending.load(Ordering::SeqCst)
        || window.is_fullscreen().unwrap_or(true)
    {
        return;
    }
    session
        .fullscreen_hide_pending
        .store(false, Ordering::SeqCst);
    if let Err(error) = window.hide() {
        tracing::warn!(%error, "Failed to hide the main window");
    }
    crate::tray::refresh_menu(window.app_handle());
}
