// SPDX-License-Identifier: AGPL-3.0-or-later

//! The application menu of fluxer_desktop/src/main/Menu.ts. macOS gets the menu itself
//! ([`macos`]). Elsewhere Electron hid its menu bar and only the accelerators stayed live,
//! firing for keys the page had not `preventDefault`ed; the bridge listens for the same keys
//! (bridge/shortcuts.ts) and sends zoom and settings to the renderer itself, so only the window
//! actions below come here. No native menu is built there: a hidden GTK menu bar's accelerators
//! would not fire, and its role items would be disabled.

#[cfg(any(target_os = "macos", test))]
pub mod macos;

#[cfg(target_os = "linux")]
use std::sync::Arc;

use serde::Deserialize;
use tauri::{Manager, WebviewWindow};

#[cfg(target_os = "linux")]
use crate::config::DesktopConfig;

/// The menu items acting on a window: the `reload`, `forceReload` and `togglefullscreen` roles,
/// the developer tools item, and the `quit` role.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum WindowAction {
    ToggleDevtools,
    Reload,
    ForceReload,
    ToggleFullscreen,
    Quit,
}

/// Runs `action` on the window the menu targeted.
pub fn run(window: &WebviewWindow, action: WindowAction) {
    let result = match action {
        WindowAction::ToggleDevtools => {
            toggle_devtools(window);
            Ok(())
        }
        WindowAction::Reload => window.reload(),
        WindowAction::ForceReload => force_reload(window),
        WindowAction::ToggleFullscreen => window
            .is_fullscreen()
            .and_then(|fullscreen| window.set_fullscreen(!fullscreen)),
        WindowAction::Quit => {
            window.app_handle().exit(0);
            Ok(())
        }
    };
    if let Err(error) = result {
        tracing::warn!(%error, ?action, "Failed to run a menu action");
    }
}

/// A menu accelerator from the bridge, for the calling window.
#[tauri::command]
pub fn app_menu_action(window: WebviewWindow, action: WindowAction) {
    run(&window, action);
}

/// `toggleWindowDevTools`.
fn toggle_devtools(window: &WebviewWindow) {
    if window.is_devtools_open() {
        window.close_devtools();
    } else {
        open_devtools(window, false);
    }
}

/// `openWindowDevTools`: detached when asked to be or while the window allows transparency.
/// Only WebKitGTK takes the request; WebView2's tools always open in a window of their own, and
/// macOS opens them attached.
#[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
pub fn open_devtools(window: &WebviewWindow, force_detach: bool) {
    window.open_devtools();
    #[cfg(target_os = "linux")]
    if force_detach
        || window
            .state::<Arc<DesktopConfig>>()
            .active_window_behavior()
            .allow_transparency
    {
        let detached = window.with_webview(|webview| {
            use webkit2gtk::{WebInspectorExt, WebViewExt};
            if let Some(inspector) = webview.inner().inspector() {
                inspector.detach();
            }
        });
        if let Err(error) = detached {
            tracing::warn!(%error, "Failed to detach the developer tools");
        }
    }
}

/// `reloadIgnoringCache`.
fn force_reload(window: &WebviewWindow) -> tauri::Result<()> {
    window.with_webview(|webview| {
        #[cfg(target_os = "linux")]
        {
            use webkit2gtk::WebViewExt;
            webview.inner().reload_bypass_cache();
        }
        #[cfg(windows)]
        {
            use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2CallDevToolsProtocolMethodCompletedHandler;
            use windows::core::w;
            // SAFETY: COM calls on the live webview's controller.
            let reloaded = unsafe {
                webview.controller().CoreWebView2().and_then(|core| {
                    core.CallDevToolsProtocolMethod(
                        w!("Page.reload"),
                        w!(r#"{"ignoreCache":true}"#),
                        None::<&ICoreWebView2CallDevToolsProtocolMethodCompletedHandler>,
                    )
                })
            };
            if let Err(error) = reloaded {
                tracing::warn!(%error, "Failed to reload the window ignoring its cache");
            }
        }
        #[cfg(target_os = "macos")]
        {
            use objc2::msg_send;
            use objc2::runtime::AnyObject;
            let web_view = webview.inner().cast::<AnyObject>();
            // SAFETY: `inner` is the window's live WKWebView; `reloadFromOrigin` returns a
            // navigation the caller may ignore. Not verified on macOS.
            let _: *mut AnyObject = unsafe { msg_send![web_view, reloadFromOrigin] };
        }
    })
}
