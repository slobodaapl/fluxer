// SPDX-License-Identifier: AGPL-3.0-or-later

//! The popout windows Window.ts tracked: `voicePopoutWindows` by window name and the one
//! `quickCssEditorWindow`, plus the children that exist but have not been named yet.

use std::collections::{HashMap, HashSet};

use super::classify::{PopoutKind, QUICK_CSS_EDITOR_KEY};

const LABEL_PREFIX: &str = "popout-";

#[derive(Default)]
pub struct Registry {
    next_id: u64,
    unnamed: HashSet<String>,
    /// Window name to label.
    voice: HashMap<String, String>,
    editor: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Naming {
    /// The child closed before its name came back.
    Gone,
    /// The child is tracked under its name; `replaced` held that name before and is closed, as
    /// `trackVoicePopoutWindow` and `trackQuickCssEditorWindow` closed the previous window.
    Named { replaced: Option<String> },
}

impl Registry {
    /// Voice popouts and unnamed children, the count the capacity check reads.
    pub fn open_children(&self) -> usize {
        self.voice.len() + self.unnamed.len()
    }

    /// A fresh label for a child about to be created. Labels are never reused.
    pub fn add_unnamed(&mut self) -> String {
        self.next_id += 1;
        let label = format!("{LABEL_PREFIX}{}", self.next_id);
        self.unnamed.insert(label.clone());
        label
    }

    #[cfg(not(target_os = "linux"))]
    pub fn is_unnamed(&self, label: &str) -> bool {
        self.unnamed.contains(label)
    }

    pub fn name(&mut self, label: &str, kind: &PopoutKind) -> Naming {
        if !self.unnamed.remove(label) {
            return Naming::Gone;
        }
        let replaced = match kind {
            PopoutKind::Voice(name) => self.voice.insert(name.clone(), label.to_owned()),
            PopoutKind::QuickCssEditor => self.editor.replace(label.to_owned()),
        };
        Naming::Named { replaced }
    }

    /// The window's `closed` listener: whatever the label was tracked as is dropped.
    pub fn forget(&mut self, label: &str) {
        self.unnamed.remove(label);
        self.voice.retain(|_, tracked| tracked != label);
        if self.editor.as_deref() == Some(label) {
            self.editor = None;
        }
    }

    /// The window `popoutSetAlwaysOnTop` and `popoutFocus` act on for `key`.
    pub fn label_for(&self, key: &str) -> Option<&str> {
        if key == QUICK_CSS_EDITOR_KEY {
            return self.editor.as_deref();
        }
        self.voice.get(key).map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TILE: &str = "fluxer-voice-popout:tile:camera:a";
    const CALL: &str = "fluxer-voice-popout:call:1";

    fn voice(name: &str) -> PopoutKind {
        PopoutKind::Voice(name.to_owned())
    }

    #[test]
    fn unnamed_children_hold_capacity_until_named_or_closed() {
        let mut registry = Registry::default();
        let first = registry.add_unnamed();
        let second = registry.add_unnamed();
        assert_ne!(first, second);
        assert_eq!(registry.open_children(), 2);
        assert_eq!(
            registry.name(&first, &voice(TILE)),
            Naming::Named { replaced: None }
        );
        assert_eq!(registry.open_children(), 2);
        // The editor does not count against the voice popout capacity.
        assert_eq!(
            registry.name(&second, &PopoutKind::QuickCssEditor),
            Naming::Named { replaced: None }
        );
        assert_eq!(registry.open_children(), 1);
        let third = registry.add_unnamed();
        registry.forget(&third);
        assert_eq!(registry.open_children(), 1);
        assert_eq!(registry.name(&third, &voice(CALL)), Naming::Gone);
        assert_eq!(registry.label_for(CALL), None);
    }

    #[test]
    fn a_second_window_of_a_name_replaces_the_first() {
        let mut registry = Registry::default();
        let old = registry.add_unnamed();
        registry.name(&old, &voice(TILE));
        let new = registry.add_unnamed();
        assert_eq!(
            registry.name(&new, &voice(TILE)),
            Naming::Named {
                replaced: Some(old.clone())
            }
        );
        // The replaced window's close leaves the new one tracked.
        registry.forget(&old);
        assert_eq!(registry.label_for(TILE), Some(new.as_str()));
        assert_eq!(registry.open_children(), 1);

        let editor = registry.add_unnamed();
        registry.name(&editor, &PopoutKind::QuickCssEditor);
        let next_editor = registry.add_unnamed();
        assert_eq!(
            registry.name(&next_editor, &PopoutKind::QuickCssEditor),
            Naming::Named {
                replaced: Some(editor.clone())
            }
        );
        registry.forget(&editor);
        assert_eq!(
            registry.label_for(QUICK_CSS_EDITOR_KEY),
            Some(next_editor.as_str())
        );
        registry.forget(&next_editor);
        assert_eq!(registry.label_for(QUICK_CSS_EDITOR_KEY), None);
    }

    #[test]
    fn keys_resolve_to_tracked_windows_only() {
        let mut registry = Registry::default();
        let unnamed = registry.add_unnamed();
        assert_eq!(registry.label_for(TILE), None);
        assert_eq!(registry.label_for(QUICK_CSS_EDITOR_KEY), None);
        registry.name(&unnamed, &voice(TILE));
        assert_eq!(registry.label_for(TILE), Some(unnamed.as_str()));
        assert_eq!(registry.label_for(CALL), None);
        assert_eq!(registry.label_for(""), None);
    }
}
