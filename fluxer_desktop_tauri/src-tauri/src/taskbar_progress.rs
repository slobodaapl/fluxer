// SPDX-License-Identifier: AGPL-3.0-or-later

//! `taskbar-progress-set` of IpcHandlers.ts: upload progress on the main window's taskbar entry
//! (Windows), the dock (macOS, not verified) or the launcher entry (Linux, see
//! [`crate::launcher_entry`]). Ungated, as in Electron. A missing or non-number fraction is -1,
//! an unknown mode `normal`; mode `none` or a negative fraction removes the bar, anything else
//! is clamped to 0..1. Linux has no modes, as under Electron.

use serde_json::Value;
use tauri::{AppHandle, Manager};
#[cfg(not(target_os = "linux"))]
use tauri::window::{ProgressBarState, ProgressBarStatus};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Normal,
    Indeterminate,
    Error,
    Paused,
    None,
}

impl Mode {
    fn parse(value: Option<&Value>) -> Self {
        match value.and_then(Value::as_str) {
            Some("indeterminate") => Self::Indeterminate,
            Some("error") => Self::Error,
            Some("paused") => Self::Paused,
            Some("none") => Self::None,
            _ => Self::Normal,
        }
    }
}

/// What `setTaskbarProgress` asks of the window: `None` removes the bar.
fn requested(payload: &Value) -> Option<(f64, Mode)> {
    let fraction = payload.get("fraction").and_then(Value::as_f64).unwrap_or(-1.0);
    let mode = Mode::parse(payload.get("mode"));
    (mode != Mode::None && fraction >= 0.0).then(|| (fraction.clamp(0.0, 1.0), mode))
}

#[cfg(not(target_os = "linux"))]
fn progress_bar_state(requested: Option<(f64, Mode)>) -> ProgressBarState {
    let Some((fraction, mode)) = requested else {
        return ProgressBarState {
            status: Some(ProgressBarStatus::None),
            progress: None,
        };
    };
    let status = match mode {
        Mode::Normal | Mode::None => ProgressBarStatus::Normal,
        Mode::Indeterminate => ProgressBarStatus::Indeterminate,
        Mode::Error => ProgressBarStatus::Error,
        Mode::Paused => ProgressBarStatus::Paused,
    };
    // Electron's Windows taskbar took `static_cast<int>(fraction * 100)` of 100.
    ProgressBarState {
        status: Some(status),
        progress: Some((fraction * 100.0) as u64),
    }
}

/// The main window must exist, as under Electron, even where the bar is app-wide.
#[tauri::command]
pub fn taskbar_progress_set(app: AppHandle, payload: Option<Value>) {
    let Some(payload @ Value::Object(_)) = payload else {
        return;
    };
    let requested = requested(&payload);
    let window = app.get_webview_window(crate::window::MAIN_WINDOW_LABEL);
    #[cfg(target_os = "linux")]
    if window.is_some() {
        app.state::<crate::launcher_entry::LauncherEntry>()
            .set_progress(requested.map_or(-1.0, |(fraction, _)| fraction));
    }
    #[cfg(not(target_os = "linux"))]
    if let Some(window) = window
        && let Err(error) = window.set_progress_bar(progress_bar_state(requested))
    {
        tracing::debug!(%error, "Failed to set the taskbar progress");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fractions_clamp_and_negative_or_none_remove_the_bar() {
        assert_eq!(
            requested(&json!({"fraction": 0.4, "mode": "paused"})),
            Some((0.4, Mode::Paused))
        );
        assert_eq!(requested(&json!({"fraction": 1.7})), Some((1.0, Mode::Normal)));
        assert_eq!(requested(&json!({"fraction": 0, "mode": "bogus"})), Some((0.0, Mode::Normal)));
        assert_eq!(requested(&json!({"fraction": -0.01})), None);
        assert_eq!(requested(&json!({"fraction": 0.5, "mode": "none"})), None);
        // A non-number fraction counts as -1.
        assert_eq!(requested(&json!({"fraction": "0.5"})), None);
        assert_eq!(requested(&json!({})), None);
    }
}
