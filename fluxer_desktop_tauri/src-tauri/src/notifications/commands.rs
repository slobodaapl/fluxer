// SPDX-License-Identifier: AGPL-3.0-or-later

//! The notification commands. `show-notification` was the only gated one in Electron
//! (`requirePrivilegedRendererDocumentSender`, which threw); the document token stands in for
//! the sender frame, as for the unread badge.

use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Manager, State, Webview};

use super::{NotificationOptions, Notifications};
use crate::documents::{Caller, DocumentRegistry};
use crate::error::BridgeError;

const SHOW_CHANNEL: &str = "show-notification";

#[derive(Serialize)]
pub struct NotificationResult {
    id: String,
}

#[tauri::command]
pub async fn show_notification(
    app: AppHandle,
    webview: Webview,
    documents: State<'_, Arc<DocumentRegistry>>,
    payload: NotificationOptions,
    document_token: String,
) -> Result<NotificationResult, BridgeError> {
    documents
        .capture_for_read(&Caller::new(&webview, document_token), SHOW_CHANNEL)
        .map_err(|_| {
            BridgeError::new(
                "UntrustedRendererDocumentSenderError",
                format!(
                    "{SHOW_CHANNEL} is only reachable from a trusted top-level renderer document"
                ),
            )
        })?;
    let id = super::show(&app, payload).await?;
    Ok(NotificationResult { id })
}

#[tauri::command]
pub async fn notification_sound_allowed(app: AppHandle) -> bool {
    app.state::<Notifications>().allows_sound().await
}

/// Electron looked non-string ids up and found nothing.
#[tauri::command]
pub fn close_notification(app: AppHandle, id: Value) {
    if let Some(id) = id.as_str() {
        super::close(&app, id);
    }
}

#[tauri::command]
pub fn close_notifications(app: AppHandle, ids: Vec<Value>) {
    for id in ids.iter().filter_map(Value::as_str) {
        super::close(&app, id);
    }
}
