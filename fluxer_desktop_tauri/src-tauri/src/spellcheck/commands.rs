// SPDX-License-Identifier: AGPL-3.0-or-later

//! The spellcheck channels of Spellcheck.ts. The state-changing commands are synchronous so
//! they run on the main thread in the order the renderer sent them, as `ipcMain` handlers did;
//! dictionary loading happens in `Spellcheck::schedule_apply`, off that thread.
//! `spellcheck-replace-misspelling` is handled in the bridge, which knows the word's range.

use std::sync::Arc;

use serde_json::Value;
use tauri::{AppHandle, Manager, State, Webview};

use super::languages::CatalogueEntry;
use super::state::SpellcheckState;
use super::{Broadcast, Spellcheck, installed_tags};

type Service<'a> = State<'a, Arc<Spellcheck>>;

#[tauri::command]
pub fn spellcheck_set_state(
    app: AppHandle,
    webview: Webview,
    spellcheck: Service<'_>,
    patch: Option<Value>,
) -> SpellcheckState {
    let state = spellcheck.set_state(&patch.unwrap_or_default());
    spellcheck.schedule_apply(
        app,
        webview.label().to_owned(),
        Broadcast {
            state: true,
            resolved: true,
        },
    );
    state
}

#[tauri::command]
pub fn spellcheck_get_available_languages(spellcheck: Service<'_>) -> Vec<String> {
    spellcheck.available_languages(&installed_tags())
}

#[tauri::command]
pub fn spellcheck_get_bundled_dictionaries() -> Vec<CatalogueEntry> {
    Spellcheck::catalogue(&installed_tags())
}

#[tauri::command]
pub fn spellcheck_add_word_to_dictionary(
    app: AppHandle,
    webview: Webview,
    spellcheck: Service<'_>,
    word: Option<Value>,
) {
    if spellcheck.add_word(word.as_ref().and_then(Value::as_str).unwrap_or_default()) {
        spellcheck.schedule_apply(
            app,
            webview.label().to_owned(),
            Broadcast {
                state: true,
                resolved: false,
            },
        );
    }
}

/// `language`: the dictionary base the bridge detected in the composer text, or `None`.
#[tauri::command]
pub fn spellcheck_update_autodetect_language(
    app: AppHandle,
    webview: Webview,
    spellcheck: Service<'_>,
    language: Option<String>,
) {
    if spellcheck.update_detected_language(language.as_deref(), &installed_tags()) {
        spellcheck.schedule_apply(
            app,
            webview.label().to_owned(),
            Broadcast {
                state: false,
                resolved: true,
            },
        );
    }
}

/// The suggestions for a misspelled `word` (at most 12), or `None` when it is accepted.
#[tauri::command]
pub async fn spellcheck_check_word(
    app: AppHandle,
    word: String,
) -> tauri::Result<Option<Vec<String>>> {
    let spellcheck = app.state::<Arc<Spellcheck>>().inner().clone();
    tauri::async_runtime::spawn_blocking(move || spellcheck.misspelling(&word)).await
}
