// SPDX-License-Identifier: AGPL-3.0-or-later

//! `system-idle-time-ms` of IpcHandlers.ts: `powerMonitor.getSystemIdleTime()` in milliseconds.
//! Electron always answered, in whole seconds. Here the answer keeps its millisecond resolution,
//! and a session with no idle source rejects: fluxer_app/src/features/ui/state/Idle.ts then falls
//! back to in-page activity and asks again a minute later, where a 0 would have marked the user
//! active for good.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
use self::linux as platform;
#[cfg(target_os = "macos")]
use self::macos as platform;
#[cfg(windows)]
use self::windows as platform;

use std::sync::Arc;

use crate::error::BridgeError;

/// The platform's idle source; on Linux it remembers which backend answers.
#[derive(Default)]
pub struct SystemIdle(platform::SystemIdle);

#[tauri::command]
pub async fn system_idle_time_ms(
    idle: tauri::State<'_, Arc<SystemIdle>>,
) -> Result<u64, BridgeError> {
    let idle = idle.inner().clone();
    tauri::async_runtime::spawn_blocking(move || idle.0.idle_time_ms())
        .await
        .map_err(|error| error.to_string())
        .flatten()
        .map_err(|message| BridgeError::new("Error", message))
}
