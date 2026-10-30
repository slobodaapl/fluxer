// SPDX-License-Identifier: AGPL-3.0-or-later

//! The desktop config channels of IpcHandlers.ts and main/index.ts. Each command keeps the
//! gate Electron had: none for window behavior, troubleshooting, relaunch and the last route;
//! the local app origin for the preboot theme and instance product name reports, where
//! Electron compared `senderFrame.origin` with `DESKTOP_APP_ORIGIN`. Tauri reports the calling
//! webview's main-frame URL, not the sending frame's, so a subframe counts as its top document.
//!
//! A window behavior change re-applies the tray and the Windows taskbar badge, shows a hidden
//! main window once the tray icon is off and drops the saved bounds once they are no longer
//! remembered.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;
use tauri::{AppHandle, Manager, State, Webview};

use super::DesktopConfig;
use super::js::is_truthy;
use super::troubleshooting::DesktopTroubleshootingSettings;
use super::window_behavior::DesktopWindowBehaviorSettings;
use crate::local_app::url::is_local_app_url;
use crate::{tray, window};

type Config<'a> = State<'a, Arc<DesktopConfig>>;

static RELAUNCH_REQUESTED: AtomicBool = AtomicBool::new(false);

fn arg(value: Option<Value>) -> Value {
    value.unwrap_or_default()
}

fn is_local_app_caller(webview: &Webview) -> bool {
    webview
        .url()
        .is_ok_and(|url| is_local_app_url(url.as_str()))
}

/// `relaunchAndExit`. Tauri's restart re-executes the binary (the AppImage when running from
/// one) and ends this process, so the calling `invoke` never settles, as under Electron.
pub(crate) fn relaunch_and_exit(app: &AppHandle) {
    if RELAUNCH_REQUESTED.swap(true, Ordering::SeqCst) {
        tracing::warn!("Ignoring duplicate relaunch request");
        return;
    }
    app.restart();
}

#[tauri::command]
pub fn desktop_window_behavior_get(config: Config<'_>) -> DesktopWindowBehaviorSettings {
    config.window_behavior()
}

#[tauri::command]
pub fn desktop_window_behavior_set(
    app: AppHandle,
    config: Config<'_>,
    settings: Option<Value>,
) -> DesktopWindowBehaviorSettings {
    let next = config.set_window_behavior(&arg(settings));
    tray::apply_window_behavior(&app);
    #[cfg(windows)]
    crate::unread_badge::refresh_windows_overlay(&app);
    if !next.show_tray_icon
        && app
            .get_webview_window(window::MAIN_WINDOW_LABEL)
            .is_some_and(|main| !main.is_visible().unwrap_or(true))
    {
        window::show_main_window(&app);
    }
    if !next.remember_window_state {
        window::clear_saved_state(&app);
    }
    next
}

#[tauri::command]
pub fn desktop_window_behavior_pending_restart(config: Config<'_>) -> bool {
    config.window_behavior_pending_restart()
}

#[tauri::command]
pub fn desktop_app_relaunch(app: AppHandle) {
    relaunch_and_exit(&app);
}

#[tauri::command]
pub fn desktop_troubleshooting_get(config: Config<'_>) -> DesktopTroubleshootingSettings {
    config.launch_troubleshooting()
}

/// `setHardwareAccelerationDisabled[AndRestart]`. macOS always stores `false` and never
/// restarts. Electron applied the value as a Chromium switch at the next launch; here it is
/// only stored. The reply is the launch view, which a change does not alter.
#[tauri::command]
pub fn desktop_troubleshooting_set_disable_hardware_acceleration(
    app: AppHandle,
    config: Config<'_>,
    options: Option<Value>,
) -> DesktopTroubleshootingSettings {
    let options = arg(options);
    let disable = is_truthy(options.get("disable"));
    let is_macos = cfg!(target_os = "macos");
    config.set_troubleshooting(|settings| {
        settings.disable_hardware_acceleration = disable && !is_macos;
    });
    if is_macos {
        tracing::info!("Hardware acceleration setting ignored on macOS");
    } else {
        tracing::info!(disable, "Hardware acceleration setting persisted");
        if is_truthy(options.get("restart")) {
            relaunch_and_exit(&app);
        }
    }
    config.launch_troubleshooting()
}

/// `setModuleUpdatesDisabledAndRestart`: stores the value and always restarts.
#[tauri::command]
pub fn desktop_troubleshooting_set_disable_module_updates(
    app: AppHandle,
    config: Config<'_>,
    options: Option<Value>,
) -> DesktopTroubleshootingSettings {
    let disable = is_truthy(arg(options).get("disable"));
    config.set_troubleshooting(|settings| settings.disable_module_updates = disable);
    tracing::info!(disable, "Restarting to apply module updates change");
    relaunch_and_exit(&app);
    config.launch_troubleshooting()
}

#[tauri::command]
pub fn desktop_preboot_theme_report(webview: Webview, config: Config<'_>, theme: Option<Value>) {
    if let Some(Value::String(theme)) = theme
        && is_local_app_caller(&webview)
    {
        config.set_preboot_theme(&theme);
    }
}

#[tauri::command]
pub fn desktop_instance_product_name_report(
    webview: Webview,
    config: Config<'_>,
    report: Option<Value>,
) {
    if is_local_app_caller(&webview) {
        config.remember_instance_product_name(&arg(report));
    }
}

#[tauri::command]
pub fn desktop_last_route_report(config: Config<'_>, route_path: Option<Value>) {
    config.record_last_route(&arg(route_path));
}
