// SPDX-License-Identifier: AGPL-3.0-or-later

//! Native strings, the port of fluxer_desktop/src/main/MainI18n.ts. The renderer pushes its
//! translations of the shell's strings (`setNativeLocale`); the English `FALLBACK_STRINGS` of
//! MainI18n.ts, read from that file at build time, fill every key it leaves out. The last push
//! is kept in `native-strings.json` in the app data directory (owner-only) and loaded at
//! start-up, so the tray is translated before the renderer runs.
//!
//! The locale only decides whether a push changed anything: Electron formatted the badge count
//! with it, while this shell writes plain digits.

pub mod commands;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde_json::{Map, Value};

use crate::build_channel::BuildChannel;
use crate::storage::private_file;

include!(concat!(env!("OUT_DIR"), "/fallback_strings.rs"));

const STORAGE_FILE_NAME: &str = "native-strings.json";
const DEFAULT_LOCALE: &str = "en-US";
const APP_NAME_PLACEHOLDER: &str = "appName";

struct Strings {
    locale: String,
    /// `currentStrings`: the fallback strings with the renderer's on top.
    strings: HashMap<String, String>,
}

pub struct MainI18n {
    directory: PathBuf,
    current: Mutex<Strings>,
}

fn merged(strings: &Map<String, Value>) -> HashMap<String, String> {
    let mut merged: HashMap<String, String> = FALLBACK_STRINGS
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect();
    for (key, value) in strings {
        if let Value::String(value) = value {
            merged.insert(key.clone(), value.clone());
        }
    }
    merged
}

impl MainI18n {
    /// `initializeMainI18n`: the persisted strings when the file holds a locale and a strings
    /// object, the fallback strings otherwise.
    pub fn load(directory: &Path) -> Self {
        let persisted = std::fs::read_to_string(directory.join(STORAGE_FILE_NAME))
            .ok()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok());
        let current = match persisted.as_ref().and_then(|value| {
            Some((value.get("locale")?.as_str()?, value.get("strings")?.as_object()?))
        }) {
            Some((locale, strings)) => {
                tracing::info!(
                    locale,
                    count = strings.len(),
                    "Loaded persisted native strings"
                );
                Strings {
                    locale: locale.to_owned(),
                    strings: merged(strings),
                }
            }
            None => Strings {
                locale: DEFAULT_LOCALE.to_owned(),
                strings: merged(&Map::new()),
            },
        };
        Self {
            directory: directory.to_owned(),
            current: Mutex::new(current),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Strings> {
        self.current.lock().expect("native strings lock")
    }

    /// `setNativeStrings`. `strings` holds string values only. True when the strings changed, in
    /// which case they were saved (a failed write is logged) and the shell's strings refresh.
    pub fn set(&self, locale: &str, strings: Map<String, Value>) -> bool {
        let next = merged(&strings);
        {
            let mut current = self.lock();
            if current.locale == locale && current.strings == next {
                return false;
            }
            current.locale = locale.to_owned();
            current.strings = next;
        }
        let payload = serde_json::json!({"locale": locale, "strings": strings});
        let written = std::fs::create_dir_all(&self.directory).and_then(|()| {
            private_file::write(
                &self.directory.join(STORAGE_FILE_NAME),
                &payload.to_string(),
            )
        });
        if let Err(error) = written {
            tracing::warn!(%error, "Failed to persist native strings");
        }
        true
    }

    /// `t`: the current template for `key` (the key itself when there is none) with each
    /// `{name}` that `vars` or `appName` names replaced. Other placeholders stay as written.
    pub fn t(&self, key: &str, vars: &[(&str, &str)]) -> String {
        let current = self.lock();
        let template = current.strings.get(key).map_or(key, String::as_str);
        interpolate(template, vars)
    }
}

/// The `/\{(\w+)\}/g` replacement of `t`; `vars` win over `appName`.
fn interpolate(template: &str, vars: &[(&str, &str)]) -> String {
    let lookup = |name: &str| {
        vars.iter()
            .find(|(candidate, _)| *candidate == name)
            .map(|(_, value)| *value)
            .or_else(|| (name == APP_NAME_PLACEHOLDER).then(|| BuildChannel::current().app_name()))
    };
    let mut resolved = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        resolved.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let name_length = after
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(after.len());
        let name = &after[..name_length];
        match lookup(name) {
            Some(value) if name_length > 0 && after[name_length..].starts_with('}') => {
                resolved.push_str(value);
                rest = &after[name_length + 1..];
            }
            _ => {
                resolved.push('{');
                rest = after;
            }
        }
    }
    resolved.push_str(rest);
    resolved
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn strings(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn missing_keys_fall_back_to_english_then_to_the_key() {
        let directory = tempfile::tempdir().unwrap();
        let i18n = MainI18n::load(directory.path());
        assert_eq!(i18n.t("desktop.tray.openSettings", &[]), "Open settings");
        i18n.set("de", strings(json!({"desktop.tray.status": "Status (de)"})));
        assert_eq!(i18n.t("desktop.tray.status", &[]), "Status (de)");
        // A push without a key keeps the English text for it.
        assert_eq!(i18n.t("desktop.tray.openSettings", &[]), "Open settings");
        assert_eq!(i18n.t("desktop.no.such.key", &[]), "desktop.no.such.key");
    }

    #[test]
    fn placeholders_take_vars_then_the_app_name_and_unknown_ones_stay() {
        let app = BuildChannel::current().app_name();
        assert_eq!(
            interpolate("Quit {appName}", &[]),
            format!("Quit {app}")
        );
        assert_eq!(
            interpolate("{appName} {version}", &[("appName", "X")]),
            "X {version}"
        );
        assert_eq!(
            interpolate("{{channel}} {} {9x} {a-b}", &[("channel", "c"), ("9x", "n")]),
            "{c} {} n {a-b}"
        );
        assert_eq!(interpolate("{unterminated", &[("unterminated", "v")]), "{unterminated");
    }

    #[test]
    fn a_push_persists_and_a_repeat_is_not_a_change() {
        let directory = tempfile::tempdir().unwrap();
        let i18n = MainI18n::load(directory.path());
        let push = strings(json!({"desktop.tray.quit": "Beenden"}));
        assert!(i18n.set("de-DE", push.clone()));
        assert!(!i18n.set("de-DE", push.clone()));
        // The same strings under another locale are a change.
        assert!(i18n.set("de-AT", push));
        let reloaded = MainI18n::load(directory.path());
        assert_eq!(reloaded.lock().locale, "de-AT");
        assert_eq!(reloaded.t("desktop.tray.quit", &[]), "Beenden");
        // Pushing the English text explicitly equals the fallback-only state.
        let fresh = tempfile::tempdir().unwrap();
        let english = MainI18n::load(fresh.path());
        assert!(!english.set(
            DEFAULT_LOCALE,
            strings(json!({"desktop.tray.quit": "Quit {appName}"}))
        ));
    }

    #[test]
    fn an_unusable_file_loads_the_fallback_strings() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join(STORAGE_FILE_NAME),
            r#"{"locale": 3, "strings": {}}"#,
        )
        .unwrap();
        let i18n = MainI18n::load(directory.path());
        assert_eq!(i18n.lock().locale, DEFAULT_LOCALE);
        assert_eq!(i18n.t("desktop.tray.statusDnd", &[]), "Do not disturb");
    }
}
