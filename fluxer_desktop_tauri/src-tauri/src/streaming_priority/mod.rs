// SPDX-License-Identifier: AGPL-3.0-or-later

//! Streaming priority while the user shares their screen: fluxer_desktop/src/main/
//! StreamingPriority.ts. Acquisitions are counted; the first keeps the display awake
//! ([`display_sleep`]) until the count is back at zero, or until the main window's renderer goes
//! or navigates to another document (`resetStreamingPriority`).
//!
//! Electron also lifted background throttling for the calling page (no Tauri webview exposes
//! that at runtime) and, on Windows, raised process and GPU scheduling priority, which this
//! shell does not do; the diagnostics keep Electron's shape and report those parts as
//! unsupported on every platform.

mod display_sleep;

use std::collections::HashSet;
use std::sync::Arc;

use serde_json::{Value, json};
use tauri::{AppHandle, Manager, Webview};

use self::display_sleep::DisplaySleepBlocker;
use crate::build_channel::BuildChannel;
use crate::desktop_info::node_platform;

const GPU_SCHEDULING_PRIORITY_ENV: &str = "FLUXER_STREAMING_GPU_SCHEDULING_PRIORITY";
/// `os.constants.priority.PRIORITY_ABOVE_NORMAL`.
const STREAMING_PRIORITY: i32 = -7;
const GPU_PRIORITY_REFRESH_INTERVAL_MS: u64 = 20_000;

/// `resolveGpuSchedulingPriority`: `high` unless the variable says `realtime` or turns it off.
fn gpu_scheduling_priority(raw: Option<&str>) -> Option<&'static str> {
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return Some("high");
    };
    match raw.to_ascii_lowercase().as_str() {
        "realtime" | "real-time" => Some("realtime"),
        "high" => Some("high"),
        "off" | "disabled" | "none" => None,
        _ => {
            tracing::warn!(
                env = GPU_SCHEDULING_PRIORITY_ENV,
                value = raw,
                "[StreamingPriority] Ignoring invalid GPU scheduling priority override"
            );
            Some("high")
        }
    }
}

#[derive(Default)]
struct Inner {
    ref_count: u32,
    /// The running blocker and Electron's id for it.
    blocker: Option<(u32, DisplaySleepBlocker)>,
    next_blocker_id: u32,
    /// The webviews that acquired since the count was last zero.
    tracked: HashSet<String>,
    last_acquire: Option<Value>,
    last_restore: Option<Value>,
}

pub struct StreamingPriority {
    gpu_priority: Option<&'static str>,
    inner: tokio::sync::Mutex<Inner>,
}

impl Default for StreamingPriority {
    fn default() -> Self {
        let raw = std::env::var(GPU_SCHEDULING_PRIORITY_ENV).ok();
        Self {
            gpu_priority: gpu_scheduling_priority(raw.as_deref()),
            inner: tokio::sync::Mutex::default(),
        }
    }
}

impl StreamingPriority {
    /// The `lastAcquire` Electron recorded where it could not raise GPU priority.
    fn gpu_acquire_attempt(&self) -> Value {
        match self.gpu_priority {
            None => json!({
                "status": "disabled",
                "priorityClass": null,
                "targets": [],
                "elevatedProcessIds": [],
                "skippedProcessIds": [],
                "failedProcessIds": [],
                "detail": format!("{GPU_SCHEDULING_PRIORITY_ENV} disabled GPU scheduling priority"),
            }),
            Some(priority) => json!({
                "status": "unsupported-platform",
                "priorityClass": priority,
                "targets": [],
                "elevatedProcessIds": [],
                "skippedProcessIds": [],
                "failedProcessIds": [],
                "detail": format!(
                    "GPU scheduling priority is not available in this shell; current platform is {}",
                    node_platform()
                ),
            }),
        }
    }

    /// `acquireStreamingPriority`.
    async fn acquire(&self, webview_label: &str) {
        let mut inner = self.inner.lock().await;
        inner.tracked.insert(webview_label.to_owned());
        inner.ref_count += 1;
        if inner.ref_count == 1 {
            match DisplaySleepBlocker::start(BuildChannel::current().app_name()).await {
                Ok(blocker) => {
                    let id = inner.next_blocker_id;
                    inner.next_blocker_id += 1;
                    inner.blocker = Some((id, blocker));
                }
                Err(error) => {
                    tracing::warn!(%error, "[StreamingPriority] Failed to start power save blocker");
                }
            }
            tracing::info!(ref_count = inner.ref_count, "[StreamingPriority] Acquired");
        }
        inner.last_acquire = Some(self.gpu_acquire_attempt());
    }

    /// The count is zero: everything acquisition started stops.
    async fn restore(inner: &mut Inner) {
        inner.tracked.clear();
        if let Some((_, blocker)) = inner.blocker.take() {
            blocker.stop().await;
        }
        inner.last_restore = Some(json!({
            "status": "no-active-priority",
            "processIds": [],
            "restoredProcessIds": [],
            "failedProcessIds": [],
        }));
    }

    /// `releaseStreamingPriority`.
    async fn release(&self) {
        let mut inner = self.inner.lock().await;
        if inner.ref_count == 0 {
            return;
        }
        inner.ref_count -= 1;
        if inner.ref_count == 0 {
            Self::restore(&mut inner).await;
            tracing::info!("[StreamingPriority] Released");
        }
    }

    /// `resetStreamingPriority`.
    async fn reset(&self) {
        let mut inner = self.inner.lock().await;
        if inner.ref_count == 0 {
            return;
        }
        inner.ref_count = 0;
        Self::restore(&mut inner).await;
        tracing::info!("[StreamingPriority] Reset");
    }

    /// `getStreamingPriorityDiagnostics`.
    async fn diagnostics(&self) -> Value {
        let inner = self.inner.lock().await;
        let blocker_id = inner.blocker.as_ref().map(|(id, _)| *id);
        json!({
            "refCount": inner.ref_count,
            "powerSaveBlocker": {"active": blocker_id.is_some(), "id": blocker_id},
            "processPriority": {
                "supported": false,
                "streamingPriority": STREAMING_PRIORITY,
                "savedPriority": null,
                "elevated": false,
                "elevatedProcesses": [],
            },
            "gpuScheduling": {
                "supported": false,
                "priorityClass": self.gpu_priority,
                "env": GPU_SCHEDULING_PRIORITY_ENV,
                "nativeModuleStatus": if self.gpu_priority.is_some() {
                    "unsupported-platform"
                } else {
                    "disabled"
                },
                "nativeModuleLoadErrorDetail": null,
                "refreshActive": false,
                "refreshIntervalMs": GPU_PRIORITY_REFRESH_INTERVAL_MS,
                "trackedWebContents": inner.tracked.len(),
                "elevatedProcesses": [],
                "lastAcquire": inner.last_acquire,
                "lastRestore": inner.last_restore,
            },
        })
    }
}

fn priority(app: &AppHandle) -> Arc<StreamingPriority> {
    app.state::<Arc<StreamingPriority>>().inner().clone()
}

/// `resetStreamingPriority` for the main window's navigation and renderer listeners.
pub fn reset(app: &AppHandle) {
    let priority = priority(app);
    tauri::async_runtime::spawn(async move { priority.reset().await });
}

#[tauri::command]
pub async fn streaming_priority_acquire(app: AppHandle, webview: Webview) {
    priority(&app).acquire(webview.label()).await;
}

#[tauri::command]
pub async fn streaming_priority_release(app: AppHandle) {
    priority(&app).release().await;
}

#[tauri::command]
pub async fn streaming_priority_get_diagnostics(app: AppHandle) -> Value {
    priority(&app).diagnostics().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gpu_priority_override_accepts_electrons_spellings() {
        assert_eq!(gpu_scheduling_priority(None), Some("high"));
        assert_eq!(gpu_scheduling_priority(Some("  ")), Some("high"));
        assert_eq!(gpu_scheduling_priority(Some(" Real-Time ")), Some("realtime"));
        assert_eq!(gpu_scheduling_priority(Some("OFF")), None);
        assert_eq!(gpu_scheduling_priority(Some("none")), None);
        assert_eq!(gpu_scheduling_priority(Some("turbo")), Some("high"));
    }
}
