// SPDX-License-Identifier: AGPL-3.0-or-later

//! Main-to-renderer events. Each webview attaches one channel at bridge start-up and every
//! event for that webview travels on it, the way `webContents.send` did under Electron.

use serde::Serialize;
use std::collections::HashMap;
use std::sync::Mutex;
use tauri::ipc::Channel;

#[derive(Serialize, Clone)]
pub struct BridgeEvent {
    name: &'static str,
    payload: serde_json::Value,
}

#[derive(Default)]
pub struct BridgeEvents {
    channels: Mutex<HashMap<String, Channel<BridgeEvent>>>,
}

impl BridgeEvents {
    /// True when the webview's channel took the event.
    pub fn emit(&self, webview_label: &str, name: &'static str, payload: impl Serialize) -> bool {
        let payload = match serde_json::to_value(payload) {
            Ok(payload) => payload,
            Err(error) => {
                tracing::error!(%error, name, "bridge event payload is not serialisable");
                return false;
            }
        };
        let mut channels = self.channels.lock().expect("bridge event channels lock");
        let Some(channel) = channels.get(webview_label) else {
            return false;
        };
        if let Err(error) = channel.send(BridgeEvent { name, payload }) {
            tracing::debug!(%error, webview_label, name, "dropping a detached bridge channel");
            channels.remove(webview_label);
            return false;
        }
        true
    }

    /// Drops the channel of a webview that is gone.
    pub fn detach(&self, webview_label: &str) {
        self.channels
            .lock()
            .expect("bridge event channels lock")
            .remove(webview_label);
    }
}

/// Replaces the webview's channel, so a reload re-attaches without leaking the old one.
#[tauri::command]
pub fn bridge_attach(
    webview: tauri::Webview,
    events: tauri::State<'_, BridgeEvents>,
    channel: Channel<BridgeEvent>,
) {
    events
        .channels
        .lock()
        .expect("bridge event channels lock")
        .insert(webview.label().to_owned(), channel);
}
