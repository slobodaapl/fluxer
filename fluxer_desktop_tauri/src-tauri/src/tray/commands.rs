// SPDX-License-Identifier: AGPL-3.0-or-later

//! `tray-runtime-state-update` of IpcHandlers.ts: ungated, as in Electron, and dropped unless
//! the state is an object. The document token says which document the tray actions go to.

use serde_json::Value;
use tauri::{AppHandle, Webview};

#[tauri::command]
pub fn tray_runtime_state_update(
    app: AppHandle,
    webview: Webview,
    state: Option<Value>,
    document_token: String,
) {
    if let Some(state @ Value::Object(_)) = state {
        super::update_runtime_state(&app, &webview, document_token, &state);
    }
}
