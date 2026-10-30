// SPDX-License-Identifier: AGPL-3.0-or-later

//! `SpellcheckState` and the patch rules of Spellcheck.ts (`normalizeState`, `sanitizeEngine`,
//! `applyLaunchSpellcheckMode`). Electron kept the state in memory per session and never wrote
//! it to disk: the renderer persists it as a synced user setting and pushes it on start-up.

use std::collections::HashSet;

use serde::Serialize;
use serde_json::Value;

use super::launch::LaunchMode;
use crate::js::js_trim;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    Auto,
    Hunspell,
    System,
}

impl Engine {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(Self::Auto),
            "hunspell" => Some(Self::Hunspell),
            "system" => Some(Self::System),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpellcheckState {
    pub enabled: bool,
    pub engine: Engine,
    pub auto_detect: bool,
    pub languages: Vec<String>,
    pub personal_dictionary: Vec<String>,
}

impl Default for SpellcheckState {
    fn default() -> Self {
        Self {
            enabled: true,
            engine: Engine::Auto,
            auto_detect: true,
            languages: Vec::new(),
            personal_dictionary: Vec::new(),
        }
    }
}

/// `patch.<name> ?? prev.<name>`: a missing or `null` field keeps the previous value.
fn field<'a>(patch: &'a Value, name: &str) -> Option<&'a Value> {
    patch.get(name).filter(|value| !value.is_null())
}

impl SpellcheckState {
    /// `normalizeState(prev, patch)`. A non-boolean `enabled` or `autoDetect` keeps the
    /// previous value; Electron stored whatever non-null value arrived and read it as truthy.
    pub fn patched(&self, patch: &Value) -> Self {
        let boolean = |name, previous| {
            field(patch, name)
                .and_then(Value::as_bool)
                .unwrap_or(previous)
        };
        let engine = match field(patch, "engine") {
            None => self.engine,
            Some(value) => value
                .as_str()
                .and_then(Engine::parse)
                .unwrap_or(Engine::Auto),
        };
        let languages = match patch.get("languages").and_then(Value::as_array) {
            Some(items) => items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
            None => self.languages.clone(),
        };
        let personal_dictionary = match patch.get("personalDictionary").and_then(Value::as_array) {
            Some(items) => items
                .iter()
                .map(|item| item.as_str().unwrap_or_default().to_owned())
                .collect(),
            None => self.personal_dictionary.clone(),
        };
        let mut seen_languages = HashSet::new();
        let mut seen_words = HashSet::new();
        Self {
            enabled: boolean("enabled", self.enabled),
            engine,
            auto_detect: boolean("autoDetect", self.auto_detect),
            languages: languages
                .into_iter()
                .filter(|tag| !tag.is_empty() && seen_languages.insert(tag.to_lowercase()))
                .collect(),
            personal_dictionary: personal_dictionary
                .iter()
                .map(|word| js_trim(word))
                .filter(|word| !word.is_empty() && seen_words.insert(*word))
                .map(str::to_owned)
                .collect(),
        }
    }

    /// `applyLaunchSpellcheckMode`.
    pub fn with_launch_mode(self, mode: LaunchMode) -> Self {
        match mode {
            LaunchMode::Default => self,
            LaunchMode::Off => Self {
                enabled: false,
                ..self
            },
            LaunchMode::Engine(engine) => Self {
                enabled: true,
                engine,
                ..self
            },
        }
    }
}
