// SPDX-License-Identifier: AGPL-3.0-or-later

//! `popout:set-always-on-top` and `popout:focus` of fluxer_desktop/src/main/IpcHandlers.ts: the
//! Quick CSS editor key, the voice debug event sink key, or a voice popout's window name. A key
//! with no live window answers `false`. The bridge answers `false` for arguments of the wrong
//! type, as the handlers did.

use tauri::{AppHandle, State, WebviewWindow};

use super::Popouts;
use crate::event_sink;
use crate::window::show::focus_window;

fn window_for(app: &AppHandle, popouts: &Popouts, key: &str) -> Option<WebviewWindow> {
    if key == event_sink::POPOUT_KEY {
        return event_sink::window(app);
    }
    popouts.window(app, key)
}

#[tauri::command]
pub fn popout_set_always_on_top(
    app: AppHandle,
    popouts: State<'_, Popouts>,
    key: String,
    flag: bool,
) -> bool {
    let Some(window) = window_for(&app, &popouts, &key) else {
        return false;
    };
    match window.set_always_on_top(flag) {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(%error, key, "Failed to set a popout window always on top");
            false
        }
    }
}

#[tauri::command]
pub fn popout_focus(app: AppHandle, popouts: State<'_, Popouts>, key: String) -> bool {
    let Some(window) = window_for(&app, &popouts, &key) else {
        return false;
    };
    focus_window(&window);
    true
}
