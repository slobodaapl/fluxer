// SPDX-License-Identifier: AGPL-3.0-or-later

//! Launch at login, the port of fluxer_desktop/src/main/Autostart.ts: the five `autostart-*`
//! channels. The login launch passes `--autostart` (except a macOS login item, which is told
//! apart by its launch event).
//!
//! Each platform registers under this shell's own identity, which Electron's cleanup cannot
//! mistake for its own: Linux writes `<desktop id>.desktop` in the autostart directory (the XDG
//! Background portal under Flatpak), Windows a `HKCU\...\Run` value named after the
//! AppUserModelID, macOS registers the app as a login item (not verified on macOS).
//!
//! `autostart-initialized-v2` in the app data directory records that the renderer has made its
//! first-run choice; the development channel always reads as initialized. The first-run default
//! (enable unless Flatpak or portable) is the renderer's decision
//! (fluxer_app/src/features/platform/utils/Autostart.ts), so these channels stay faithful and a
//! stable or canary build turns autostart on at its first run, as Electron did.
//!
//! Electron's portable mode is not ported (`getDesktopInfo` reports `portable: false`), so
//! neither is its "enable does nothing, enabled reads false" rule.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(windows, test))]
mod windows;

#[cfg(target_os = "linux")]
use self::linux as platform;
#[cfg(target_os = "macos")]
use self::macos as platform;
#[cfg(windows)]
use self::windows as platform;

use std::path::Path;

use tauri::{AppHandle, Manager};

use crate::build_channel::BuildChannel;
use crate::error::BridgeError;

#[cfg(target_os = "macos")]
pub use self::macos::opened_at_login;

/// `AUTOSTART_LAUNCH_ARG`.
#[cfg(any(target_os = "linux", windows, test))]
const AUTOSTART_ARG: &str = "--autostart";
const INITIALIZED_FILE: &str = "autostart-initialized-v2";
/// The JS `Error` the renderer's `invoke` rejected with.
const ERROR_NAME: &str = "Error";

fn failed(message: impl Into<String>) -> BridgeError {
    BridgeError::new(ERROR_NAME, message)
}

fn data_dir(app: &AppHandle) -> Result<std::path::PathBuf, BridgeError> {
    app.path()
        .app_data_dir()
        .map_err(|error| failed(error.to_string()))
}

fn is_initialized(channel: BuildChannel, directory: &Path) -> bool {
    channel == BuildChannel::Development || directory.join(INITIALIZED_FILE).exists()
}

/// `registerAutostartHandlers`' start-up pass: a Linux entry whose program went away is
/// rewritten for this one.
#[cfg(target_os = "linux")]
pub fn repair_on_launch(app: &AppHandle) {
    linux::repair(app);
}

#[tauri::command]
pub async fn autostart_enable(app: AppHandle) -> Result<(), BridgeError> {
    platform::enable(&app).await.map_err(failed)
}

#[tauri::command]
pub async fn autostart_disable(app: AppHandle) -> Result<(), BridgeError> {
    platform::disable(&app).await.map_err(failed)
}

#[tauri::command]
pub fn autostart_is_enabled(app: AppHandle) -> bool {
    platform::is_enabled(&app)
}

#[tauri::command]
pub fn autostart_is_initialized(app: AppHandle) -> Result<bool, BridgeError> {
    Ok(is_initialized(BuildChannel::current(), &data_dir(&app)?))
}

/// `markInitialized`: a failed write is logged, not reported.
#[tauri::command]
pub fn autostart_mark_initialized(app: AppHandle) -> Result<(), BridgeError> {
    if let Err(error) = std::fs::write(data_dir(&app)?.join(INITIALIZED_FILE), "1") {
        tracing::error!(%error, "Failed to mark autostart initialized");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_marker_file_initializes_outside_development() {
        let directory = tempfile::tempdir().unwrap();
        assert!(is_initialized(BuildChannel::Development, directory.path()));
        for channel in [BuildChannel::Stable, BuildChannel::Canary] {
            assert!(!is_initialized(channel, directory.path()), "{channel:?}");
        }
        std::fs::write(directory.path().join(INITIALIZED_FILE), "1").unwrap();
        assert!(is_initialized(BuildChannel::Stable, directory.path()));
    }
}
