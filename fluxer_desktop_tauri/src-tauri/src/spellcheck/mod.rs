// SPDX-License-Identifier: AGPL-3.0-or-later

//! Spell checking, the port of fluxer_desktop/src/main/Spellcheck.ts.
//!
//! On Linux Electron disabled Chromium's spellchecker and checked words with Hunspell, loading
//! the system dictionary for each wanted language first. Here WebKitGTK's own spell checking
//! (Enchant) draws the underlines for the same system dictionaries, and fluxer_system_hunspell
//! answers the context menu's "is this word misspelled, and what are the suggestions". The
//! resolved engine is reported as `hunspell`, as Electron reported it on Linux; the renderer
//! keys its personal dictionary settings and its reload prompt on that mode.
//!
//! Differences from Electron:
//! - no bundled dictionaries, so the `hunspell` engine (bundled dictionaries only) loads
//!   nothing and resolves to `off`, and the catalogue lists the installed system dictionaries;
//! - Enchant does not see the personal dictionary or the built-in allowlist, so WebKit still
//!   underlines those words while the context menu treats them as correct;
//! - `spellcheckSetState` replies before the dictionaries are applied; the
//!   `spellcheck-state-changed` and `spellcheck-engine-resolved` events follow the apply;
//! - an apply that loads nothing is not retried (Electron retried failed downloads);
//! - macOS and Windows have no engine yet: no languages, every state resolves to `off`.
//!
//! Electron gated none of these channels; Tauri only admits IPC from the local app.

pub mod commands;
#[cfg(target_os = "linux")]
mod dictionaries;
#[cfg(not(target_os = "linux"))]
#[path = "no_dictionaries.rs"]
mod dictionaries;
mod languages;
mod launch;
mod state;
#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::sync::{Arc, Mutex, MutexGuard};

use serde::Serialize;
use serde_json::{Value, json};
use tauri::{AppHandle, Manager};

use self::dictionaries::{Dictionaries, Installed};
use self::languages::CatalogueEntry;
use self::launch::LaunchMode;
use self::state::{Engine, SpellcheckState};
use crate::events::BridgeEvents;

const STATE_CHANGED_EVENT: &str = "spellcheck-state-changed";
const ENGINE_RESOLVED_EVENT: &str = "spellcheck-engine-resolved";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum ResolvedMode {
    Hunspell,
    Off,
}

/// `SpellcheckResolvedEngineInfo` in DesktopBridgeTypes.ts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedEngine {
    mode: ResolvedMode,
    /// The installed dictionaries in use, which are also WebKit's spell checking languages.
    hunspell_langs: Vec<String>,
    system_langs: Vec<String>,
}

/// Which events an apply sends to the calling webview.
#[derive(Clone, Copy, Debug)]
pub struct Broadcast {
    pub state: bool,
    pub resolved: bool,
}

#[derive(Default)]
struct Session {
    state: SpellcheckState,
    /// The dictionary chosen for the language auto-detected in the composer.
    auto_detected: Option<String>,
}

pub struct Spellcheck {
    launch_mode: LaunchMode,
    preferred: Vec<String>,
    session: Mutex<Session>,
    dictionaries: Mutex<Dictionaries>,
}

fn tags(installed: &[Installed]) -> Vec<String> {
    installed.iter().map(|dict| dict.tag.clone()).collect()
}

impl Spellcheck {
    pub fn new(args: &[String]) -> Self {
        let env = |name: &str| std::env::var(name).ok();
        let launch_mode = launch::launch_mode(args, env).unwrap_or_else(|error| {
            tracing::error!(%error, "ignoring the spellcheck launch option");
            LaunchMode::Default
        });
        if launch_mode != LaunchMode::Default {
            tracing::warn!(?launch_mode, "spellcheck launch diagnostic override active");
        }
        Self::with(launch_mode, languages::preferred_languages(env))
    }

    fn with(launch_mode: LaunchMode, preferred: Vec<String>) -> Self {
        Self {
            launch_mode,
            preferred,
            session: Mutex::new(Session {
                state: SpellcheckState::default().with_launch_mode(launch_mode),
                auto_detected: None,
            }),
            dictionaries: Mutex::new(Dictionaries::default()),
        }
    }

    fn session(&self) -> MutexGuard<'_, Session> {
        self.session.lock().expect("spellcheck session lock")
    }

    /// `spellcheck-set-state`.
    pub fn set_state(&self, patch: &Value) -> SpellcheckState {
        let mut session = self.session();
        session.state = session
            .state
            .patched(patch)
            .with_launch_mode(self.launch_mode);
        session.state.clone()
    }

    /// `spellcheck-add-word-to-dictionary`; `false` for the empty word, which Electron ignored.
    pub fn add_word(&self, word: &str) -> bool {
        if word.is_empty() {
            return false;
        }
        let mut session = self.session();
        let mut words = session.state.personal_dictionary.clone();
        words.push(word.to_owned());
        session.state = session
            .state
            .patched(&json!({ "personalDictionary": words }))
            .with_launch_mode(self.launch_mode);
        true
    }

    /// `spellcheck:update-autodetect-text` once the bridge has detected `base` (a dictionary
    /// base such as `de`, or `None`). `true` when the auto-detected dictionary changed.
    pub fn update_detected_language(&self, base: Option<&str>, installed: &[String]) -> bool {
        let mut session = self.session();
        if !session.state.enabled || !session.state.auto_detect {
            return false;
        }
        let next = base
            .and_then(|base| languages::choose_tag_for_language(base, &self.preferred, installed));
        if session.auto_detected == next {
            return false;
        }
        session.auto_detected = next;
        true
    }

    /// `listAvailableSpellcheckLanguages` on Linux, without the bundled catalogue.
    pub fn available_languages(&self, installed: &[String]) -> Vec<String> {
        let engine = match self.launch_mode {
            LaunchMode::Engine(engine) => engine,
            LaunchMode::Default | LaunchMode::Off => self.session().state.engine,
        };
        if engine == Engine::Hunspell {
            return Vec::new();
        }
        installed.to_vec()
    }

    pub fn catalogue(installed: &[String]) -> Vec<CatalogueEntry> {
        let mut entries: Vec<CatalogueEntry> = installed
            .iter()
            .map(|tag| languages::catalogue_entry(tag))
            .collect();
        entries.sort_by(|a, b| a.tag.cmp(&b.tag));
        entries
    }

    /// The context menu's check: `None` when spell checking is off or the word is accepted.
    pub fn misspelling(&self, word: &str) -> Option<Vec<String>> {
        if !self.session().state.enabled {
            return None;
        }
        self.dictionaries
            .lock()
            .expect("spellcheck dictionaries lock")
            .misspelling(word)
    }

    /// `applyStateToWebContents` for the current state. The dictionaries lock is held across
    /// `then`, so overlapping applies hand their results on in the order they ran.
    fn apply(&self, installed: &[Installed], then: impl FnOnce(&SpellcheckState, &ResolvedEngine)) {
        let mut dictionaries = self
            .dictionaries
            .lock()
            .expect("spellcheck dictionaries lock");
        let (state, detected) = {
            let session = self.session();
            (session.state.clone(), session.auto_detected.clone())
        };
        let installed_tags = tags(installed);
        let mut wanted: Vec<&Installed> = Vec::new();
        if state.enabled && state.engine != Engine::Hunspell {
            let mut seen = HashSet::new();
            for tag in languages::requested_languages(&state, detected.as_deref(), &self.preferred)
            {
                if let Some(found) = languages::match_installed(&tag, &installed_tags)
                    && seen.insert(found.to_owned())
                    && let Some(dict) = installed.iter().find(|dict| dict.tag == found)
                {
                    wanted.push(dict);
                }
            }
        }
        dictionaries.sync(&wanted);
        dictionaries.apply_personal(&state.personal_dictionary);
        let resolved = if wanted.is_empty() {
            if state.enabled {
                tracing::warn!(engine = ?state.engine, "no system dictionary serves the wanted spellcheck languages");
            }
            ResolvedEngine {
                mode: ResolvedMode::Off,
                hunspell_langs: Vec::new(),
                system_langs: Vec::new(),
            }
        } else {
            ResolvedEngine {
                mode: ResolvedMode::Hunspell,
                hunspell_langs: wanted.iter().map(|dict| dict.tag.clone()).collect(),
                system_langs: Vec::new(),
            }
        };
        then(&state, &resolved);
    }

    /// Applies the current state off the main thread, configures the webview's spell checking
    /// and sends the requested events to it.
    pub fn schedule_apply(self: &Arc<Self>, app: AppHandle, label: String, broadcast: Broadcast) {
        let spellcheck = self.clone();
        tauri::async_runtime::spawn_blocking(move || {
            spellcheck.apply(&dictionaries::discover(), |state, resolved| {
                #[cfg(target_os = "linux")]
                if let Some(window) = app.get_webview_window(&label)
                    && let Err(error) = crate::linux_webview::apply_spell_checking(
                        &window,
                        &resolved.hunspell_langs,
                    )
                {
                    tracing::warn!(%error, "configuring WebKit spell checking failed");
                }
                let events = app.state::<BridgeEvents>();
                if broadcast.state {
                    events.emit(&label, STATE_CHANGED_EVENT, state);
                }
                if broadcast.resolved {
                    events.emit(&label, ENGINE_RESOLVED_EVENT, resolved);
                }
            });
        });
    }
}

/// The installed dictionary tags, read afresh like Electron's `discoverSystemDictionaries()`.
fn installed_tags() -> Vec<String> {
    tags(&dictionaries::discover())
}
