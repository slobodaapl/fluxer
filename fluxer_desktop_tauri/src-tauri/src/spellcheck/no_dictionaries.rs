// SPDX-License-Identifier: AGPL-3.0-or-later

//! macOS and Windows: no spell checking engine is ported yet, so nothing is installed and
//! nothing loads. The service then lists no languages and resolves every state to `off`.

pub struct Installed {
    pub tag: String,
}

pub fn discover() -> Vec<Installed> {
    Vec::new()
}

#[derive(Default)]
pub struct Dictionaries {}

impl Dictionaries {
    pub fn sync(&mut self, _wanted: &[&Installed]) {}

    pub fn apply_personal(&mut self, _words: &[String]) {}

    pub fn misspelling(&self, _word: &str) -> Option<Vec<String>> {
        None
    }
}
