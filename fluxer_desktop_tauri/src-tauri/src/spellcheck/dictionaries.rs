// SPDX-License-Identifier: AGPL-3.0-or-later

//! Loaded system Hunspell dictionaries, after `syncLoadedHunspell`, `applyPersonalDictionary`,
//! `checkWords` and `suggestWord` in Spellcheck.ts. Only UTF-8 dictionaries load
//! (fluxer_system_hunspell refuses others); Electron fell back to its bundled copy of the
//! language for those, which this shell does not have.

use std::collections::BTreeSet;

use fluxer_system_hunspell::Hunspell;
pub use fluxer_system_hunspell::SystemDictionary as Installed;

/// `BUILTIN_ALLOWLIST`.
const BUILTIN_ALLOWLIST: [&str; 3] = ["Fluxer", "fluxer", "Hampus"];
/// The suggestion cap of the Electron context menu and of its native Hunspell adapter.
const MAX_SUGGESTIONS: usize = 12;

pub fn discover() -> Vec<Installed> {
    fluxer_system_hunspell::discover_system_dictionaries()
}

#[derive(Default)]
pub struct Dictionaries {
    loaded: Vec<(String, Hunspell)>,
    /// Personal words added to every loaded dictionary (`sessionPersonalApplied`).
    personal: BTreeSet<String>,
}

impl Dictionaries {
    /// Drops the dictionaries no longer wanted and loads the missing ones, each with the
    /// built-in allowlist and the personal words already applied.
    pub fn sync(&mut self, wanted: &[&Installed]) {
        self.loaded
            .retain(|(tag, _)| wanted.iter().any(|dict| dict.tag == *tag));
        if self.loaded.is_empty() {
            self.personal.clear();
        }
        for dict in wanted {
            if self.loaded.iter().any(|(tag, _)| *tag == dict.tag) {
                continue;
            }
            match Hunspell::new(&dict.aff_path, &dict.dic_path) {
                Ok(mut hunspell) => {
                    for word in BUILTIN_ALLOWLIST
                        .iter()
                        .copied()
                        .chain(self.personal.iter().map(String::as_str))
                    {
                        let _ = hunspell.add(word);
                    }
                    tracing::info!(tag = %dict.tag, aff = %dict.aff_path.display(), "loaded a system Hunspell dictionary");
                    self.loaded.push((dict.tag.clone(), hunspell));
                }
                Err(error) => {
                    tracing::warn!(tag = %dict.tag, aff = %dict.aff_path.display(), %error, "a system Hunspell dictionary failed to load");
                }
            }
        }
    }

    /// Brings every loaded dictionary's personal words to `words`.
    pub fn apply_personal(&mut self, words: &[String]) {
        if self.loaded.is_empty() {
            return;
        }
        let desired: BTreeSet<String> = words.iter().cloned().collect();
        for (_, hunspell) in &mut self.loaded {
            for word in self.personal.difference(&desired) {
                let _ = hunspell.remove(word);
            }
            for word in desired.difference(&self.personal) {
                let _ = hunspell.add(word);
            }
        }
        self.personal = desired;
    }

    /// `None` when a loaded dictionary accepts `word` or none is loaded; otherwise up to 12
    /// suggestions merged across the dictionaries in load order.
    pub fn misspelling(&self, word: &str) -> Option<Vec<String>> {
        if self.loaded.is_empty()
            || self
                .loaded
                .iter()
                .any(|(_, hunspell)| hunspell.spell(word).unwrap_or(false))
        {
            return None;
        }
        let mut suggestions: Vec<String> = Vec::new();
        for (_, hunspell) in &self.loaded {
            for suggestion in hunspell
                .suggest(word, Some(MAX_SUGGESTIONS as u32))
                .unwrap_or_default()
            {
                if !suggestions.contains(&suggestion) {
                    suggestions.push(suggestion);
                }
            }
            if suggestions.len() >= MAX_SUGGESTIONS {
                break;
            }
        }
        suggestions.truncate(MAX_SUGGESTIONS);
        Some(suggestions)
    }
}
