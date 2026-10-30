// SPDX-License-Identifier: AGPL-3.0-or-later

//! `native-locale-set` of IpcHandlers.ts: ungated, as in Electron. A payload without a string
//! locale and a strings object is dropped, and so is every non-string entry.

use serde_json::Value;
use tauri::{AppHandle, State};

use super::MainI18n;
use crate::tray;

#[tauri::command]
pub fn native_locale_set(app: AppHandle, i18n: State<'_, MainI18n>, payload: Option<Value>) {
    let Some(Value::Object(payload)) = payload else {
        return;
    };
    let (Some(Value::String(locale)), Some(Value::Object(strings))) =
        (payload.get("locale"), payload.get("strings"))
    else {
        return;
    };
    let strings = strings
        .iter()
        .filter(|(_, value)| value.is_string())
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    if i18n.set(locale, strings) {
        // `onLocaleChange` subscribers.
        tray::locale_changed(&app);
        #[cfg(windows)]
        crate::jump_list::locale_changed(&app);
        #[cfg(target_os = "macos")]
        {
            crate::menu::macos::locale_changed(&app);
            crate::dock_menu::refresh(&app);
        }
    }
}
