// SPDX-License-Identifier: AGPL-3.0-or-later
//! Device-local preferences: classic Accessibility:motion and native in-app shortcuts.
use super::*;
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Shortcut {
    Switcher,
    Search,
    Settings,
    Mute,
    Deafen,
}
impl Shortcut {
    pub const ALL: [Self; 5] = [
        Self::Switcher,
        Self::Search,
        Self::Settings,
        Self::Mute,
        Self::Deafen,
    ];
    pub fn index(self) -> usize {
        self as usize
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Switcher => "Quick switcher",
            Self::Search => "Search messages",
            Self::Settings => "Open settings",
            Self::Mute => "Toggle microphone",
            Self::Deafen => "Toggle deafen",
        }
    }
}
/// Device-local window defaults mirror DesktopConfig.ts. These preferences
/// describe requested behavior; tray availability is resolved by the desktop.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct WindowBehavior {
    pub show_tray_icon: bool,
    pub close_to_tray: bool,
    pub start_minimized: bool,
    pub colored_tray_icon: bool,
}
impl Default for WindowBehavior {
    fn default() -> Self {
        Self {
            show_tray_icon: true,
            close_to_tray: true,
            start_minimized: false,
            colored_tray_icon: false,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WindowSetting {
    ShowTrayIcon,
    CloseToTray,
    StartMinimized,
    ColoredTrayIcon,
}
/// Native subset of the classic base themes. Unknown saved values safely fall back to Dark.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum BaseTheme {
    #[default]
    Dark,
    Coal,
    #[serde(other)]
    Unknown,
}
impl BaseTheme {
    pub fn background(self) -> u32 {
        if self == Self::Coal { 0x000000 } else { BG }
    }
    pub fn panel(self) -> u32 {
        if self == Self::Coal { 0x101010 } else { PANEL }
    }
    pub fn rail(self) -> u32 {
        if self == Self::Coal { 0x080808 } else { RAIL }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
struct Preferences {
    streamer: Option<streamer_mode::Preferences>,
    reduced_motion: bool,
    base_theme: BaseTheme,
    shortcuts: [String; 5],
    window_behavior: WindowBehavior,
    search_history: BTreeMap<String, Vec<String>>,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            streamer: None,
            reduced_motion: false,
            base_theme: BaseTheme::Dark,
            window_behavior: WindowBehavior::default(),
            search_history: BTreeMap::new(),
            shortcuts: ["ctrl-k", "ctrl-f", "ctrl-,", "ctrl-shift-m", "ctrl-shift-d"]
                .map(str::to_owned),
        }
    }
}
pub(super) struct NativePreferences {
    preferences: Preferences,
    path: Option<std::path::PathBuf>,
    pub recording: Option<Shortcut>,
    pub error: Option<String>,
    pub server_pending: bool,
    pub server_error: Option<String>,
    pub font_draft: Option<f32>,
}
fn path() -> Option<std::path::PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|p| std::path::PathBuf::from(p).join(".config")))
        .map(|p| p.join("fluxer-native/interface.json"))
}
fn valid(binding: &str) -> bool {
    let parts: Vec<_> = binding.split('-').collect();
    let Some(key) = parts.last() else {
        return false;
    };
    let modified = parts.iter().any(|p| matches!(*p, "ctrl" | "cmd" | "alt"));
    let function = key
        .strip_prefix('f')
        .and_then(|n| n.parse::<u8>().ok())
        .is_some_and(|n| (1..=24).contains(&n));
    Keystroke::parse(binding).is_ok_and(|key| keystroke(&key) == binding)
        && (modified || function)
        && !matches!(
            *key,
            "escape"
                | "tab"
                | "enter"
                | "f8"
                | "ctrl"
                | "control"
                | "alt"
                | "shift"
                | "cmd"
                | "meta"
                | "super"
                | "fn"
                | "capslock"
        )
        && !matches!(
            binding,
            "alt-f4"
                | "ctrl-q"
                | "cmd-q"
                | "ctrl-w"
                | "cmd-w"
                | "ctrl-a"
                | "cmd-a"
                | "ctrl-c"
                | "cmd-c"
                | "ctrl-v"
                | "cmd-v"
                | "ctrl-x"
                | "cmd-x"
                | "ctrl-z"
                | "cmd-z"
                | "ctrl-y"
                | "cmd-y"
                | "ctrl-shift-z"
                | "cmd-shift-z"
        )
}
fn validate(bindings: &[String; 5]) -> Result<(), String> {
    for (i, binding) in bindings.iter().enumerate() {
        if !valid(binding) {
            return Err("Use Ctrl, Alt or Command with a key. Escape, Tab, Enter, F8 and close/quit shortcuts are reserved.".into());
        }
        if bindings[..i].contains(binding) {
            return Err("That shortcut is already assigned to another action.".into());
        }
    }
    Ok(())
}
impl NativePreferences {
    pub fn new(demo: bool) -> Self {
        let path = (!demo).then(path).flatten();
        let mut error = None;
        let mut preferences = path
            .as_ref()
            .and_then(|p| match std::fs::read(p) {
                Ok(bytes) => match serde_json::from_slice::<Preferences>(&bytes) {
                    Ok(p) if validate(&p.shortcuts).is_ok() => Some(p),
                    _ => {
                        error = Some(
                            "Saved interface preferences are invalid; defaults are active.".into(),
                        );
                        None
                    }
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(_) => {
                    error = Some("Could not read interface preferences.".into());
                    None
                }
            })
            .unwrap_or_default();
        if preferences.streamer.is_none() {
            preferences.streamer = Some(streamer_mode::Preferences::from_legacy(
                !demo && desktop_notifications::legacy_streamer_enabled(),
            ));
        }
        Self {
            preferences,
            path,
            recording: None,
            error,
            server_pending: false,
            server_error: None,
            font_draft: None,
        }
    }
    pub fn streamer(&self) -> streamer_mode::Preferences {
        self.preferences.streamer.unwrap_or_default()
    }
    pub fn update_streamer(&mut self, update: impl FnOnce(&mut streamer_mode::Preferences)) {
        let mut next = self.preferences.clone();
        update(next.streamer.get_or_insert_with(Default::default));
        self.persist(next);
    }
    pub fn search_history(&self, key: &str) -> &[String] {
        self.preferences
            .search_history
            .get(key)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
    pub fn add_search_history(&mut self, key: String, query: &str) {
        let query = query.trim();
        if query.is_empty() {
            return;
        }
        let mut next = self.preferences.clone();
        let entries = next.search_history.entry(key).or_default();
        entries.retain(|entry| entry != query);
        entries.insert(0, query.to_owned());
        entries.truncate(10);
        self.persist(next);
    }
    pub fn clear_search_history(&mut self, key: &str) {
        let mut next = self.preferences.clone();
        next.search_history.remove(key);
        self.persist(next);
    }
    pub fn window_behavior(&self) -> WindowBehavior {
        self.preferences.window_behavior
    }
    pub fn set_window_behavior(&mut self, setting: WindowSetting, value: bool) {
        let mut next = self.preferences.clone();
        match setting {
            WindowSetting::ShowTrayIcon => next.window_behavior.show_tray_icon = value,
            WindowSetting::CloseToTray => next.window_behavior.close_to_tray = value,
            WindowSetting::StartMinimized => next.window_behavior.start_minimized = value,
            WindowSetting::ColoredTrayIcon => next.window_behavior.colored_tray_icon = value,
        }
        self.persist(next);
    }
    pub fn base_theme(&self) -> BaseTheme {
        match self.preferences.base_theme {
            BaseTheme::Unknown => BaseTheme::Dark,
            theme => theme,
        }
    }
    pub fn set_base_theme(&mut self, theme: BaseTheme) {
        let mut next = self.preferences.clone();
        next.base_theme = theme;
        self.persist(next);
    }
    pub fn reduced_motion(&self) -> bool {
        self.preferences.reduced_motion
    }
    fn action(&self, key: &Keystroke) -> Option<Shortcut> {
        let key = keystroke(key);
        if let Some(action) = Shortcut::ALL.into_iter().find(|a| self.binding(*a) == key) {
            return Some(action);
        }
        // Preserve established aliases while the primary binding remains at its
        // default. An explicitly assigned shortcut always wins over an alias.
        let action = match key.as_str() {
            "ctrl-t" | "cmd-t" | "cmd-k" => Shortcut::Switcher,
            "cmd-f" => Shortcut::Search,
            "cmd-," => Shortcut::Settings,
            _ => return None,
        };
        (self.binding(action) == Preferences::default().shortcuts[action.index()]).then_some(action)
    }
    pub fn binding(&self, action: Shortcut) -> &str {
        &self.preferences.shortcuts[action.index()]
    }
    fn persist(&mut self, next: Preferences) {
        let result = (|| -> std::io::Result<()> {
            if let Some(path) = &self.path {
                let parent = path.parent().unwrap();
                std::fs::create_dir_all(parent)?;
                let mut file = tempfile::NamedTempFile::new_in(parent)?;
                use std::io::Write;
                file.write_all(&serde_json::to_vec(&next)?)?;
                file.as_file().sync_all()?;
                file.persist(path).map_err(|e| e.error)?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.preferences = next;
                self.error = None
            }
            Err(_) => {
                self.error = Some(
                    "Could not save interface preferences; the previous values remain active."
                        .into(),
                )
            }
        }
    }
    pub fn toggle_motion(&mut self) {
        let mut p = self.preferences.clone();
        p.reduced_motion = !p.reduced_motion;
        self.persist(p)
    }
    pub fn assign(&mut self, action: Shortcut, binding: String) {
        let mut p = self.preferences.clone();
        p.shortcuts[action.index()] = binding;
        match validate(&p.shortcuts) {
            Ok(()) => {
                self.persist(p);
                if self.error.is_none() {
                    self.recording = None
                }
            }
            Err(e) => self.error = Some(e),
        }
    }
    pub fn reset(&mut self) {
        let mut p = self.preferences.clone();
        p.shortcuts = Preferences::default().shortcuts;
        self.persist(p);
        self.recording = None
    }
}
fn keystroke(key: &Keystroke) -> String {
    let mut parts = Vec::new();
    if key.modifiers.control {
        parts.push("ctrl")
    }
    if key.modifiers.alt {
        parts.push("alt")
    }
    if key.modifiers.shift {
        parts.push("shift")
    }
    if key.modifiers.platform {
        parts.push("cmd")
    }
    parts.push(&key.key);
    parts.join("-")
}
impl Fluxer {
    pub(super) fn interface_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.status_ui.is_open()
            || self.context_menu_open(cx)
            || self.profile_ui.is_open(cx)
            || self.image_viewer.is_open()
            || self.playback.modal.is_open()
            || self.message_links.is_open()
            || self.message_actions.is_blocking()
            || self.search_ui.confirm.is_some()
            || self.friends_ui.confirm.is_some()
            || self.emoji_target.is_some()
            || self.forwarding.is_some()
            || self.quick_switcher.open
        {
            if self.interface.recording.take().is_some() {
                cx.notify();
            }
            return;
        }
        if self.pane != Pane::Settings || self.settings_section != settings_view::Section::Keybinds
        {
            self.interface.recording = None;
        }
        if let Some(action) = self.interface.recording {
            if event.keystroke.key == "escape" {
                self.interface.recording = None;
                self.interface.error = None
            } else if !matches!(
                event.keystroke.key.as_str(),
                "control" | "ctrl" | "alt" | "shift" | "meta" | "super" | "cmd" | "fn" | "capslock"
            ) {
                self.interface.assign(action, keystroke(&event.keystroke));
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }
        let action = self.interface.action(&event.keystroke);
        if let Some(action) = action {
            if !event.is_held {
                match action {
                    Shortcut::Switcher => self.open_quick_switcher(window, cx),
                    Shortcut::Search => self.open_search(window, cx),
                    Shortcut::Settings => {
                        self.open_settings(settings_view::Section::Appearance, window, cx)
                    }
                    Shortcut::Mute => self.command(Command::SetMute(!self.state.voice.muted), cx),
                    Shortcut::Deafen => {
                        self.command(Command::SetDeaf(!self.state.voice.deafened), cx)
                    }
                }
            }
            cx.stop_propagation();
        }
    }
    pub(super) fn save_display_setting(
        &mut self,
        setting: fluxer_core::DisplaySetting,
        cx: &mut Context<Self>,
    ) {
        if self.interface.server_pending {
            return;
        }
        self.interface.server_pending = true;
        self.interface.server_error = None;
        self.command(Command::UpdateDisplaySetting(setting), cx);
        cx.notify();
    }
}
#[cfg(test)]
mod tests {
    use super::{
        BaseTheme, NativePreferences, Preferences, Shortcut, WindowBehavior, WindowSetting,
        keystroke, valid, validate,
    };
    use gpui::Keystroke;
    #[test]
    fn base_theme_persists_and_failed_save_preserves_active_theme() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("interface.json");
        let mut prefs = NativePreferences::new(true);
        prefs.path = Some(path.clone());
        prefs.set_base_theme(BaseTheme::Coal);
        let saved: Preferences = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved.base_theme, BaseTheme::Coal);
        assert_eq!(prefs.base_theme().background(), 0);
        prefs.path = Some(path.join("impossible"));
        prefs.set_base_theme(BaseTheme::Dark);
        assert_eq!(prefs.base_theme(), BaseTheme::Coal);
        assert!(prefs.error.is_some());
        let old: Preferences = serde_json::from_str("{}").unwrap();
        assert_eq!(old.base_theme, BaseTheme::Dark);
        let future: Preferences = serde_json::from_str(r#"{"base_theme":"future"}"#).unwrap();
        assert_eq!(future.base_theme, BaseTheme::Unknown);
    }
    #[test]
    fn shortcut_conflicts_and_reserved_keys_are_rejected() {
        let mut keys = Preferences::default().shortcuts;
        assert!(validate(&keys).is_ok());
        keys[1] = keys[0].clone();
        assert!(validate(&keys).is_err());
        for key in ["a", "escape", "ctrl-q", "f8", "ctrl-enter"] {
            assert!(!valid(key), "{key}");
        }
        for key in ["ctrl-shift-a", "alt-x", "f9"] {
            assert!(valid(key), "{key}");
        }
    }
    #[test]
    fn legacy_aliases_survive_defaults_but_yield_to_explicit_rebinding() {
        let mut p = NativePreferences::new(true);
        assert_eq!(
            p.action(&Keystroke::parse("ctrl-t").unwrap()),
            Some(Shortcut::Switcher)
        );
        assert_eq!(
            p.action(&Keystroke::parse("cmd-k").unwrap()),
            Some(Shortcut::Switcher)
        );
        p.assign(Shortcut::Search, "ctrl-t".into());
        assert_eq!(
            p.action(&Keystroke::parse("ctrl-t").unwrap()),
            Some(Shortcut::Search)
        );
        p.assign(Shortcut::Switcher, "ctrl-alt-k".into());
        assert_eq!(p.action(&Keystroke::parse("cmd-k").unwrap()), None);
    }
    #[test]
    fn failed_assignment_preserves_working_binding() {
        let mut p = NativePreferences::new(true);
        p.assign(Shortcut::Search, "ctrl-k".into());
        assert_eq!(p.binding(Shortcut::Search), "ctrl-f");
        assert!(p.error.is_some());
        p.assign(Shortcut::Search, "ctrl-shift-s".into());
        assert_eq!(p.binding(Shortcut::Search), "ctrl-shift-s");
        assert!(p.error.is_none());
    }
    #[test]
    fn preferences_persist_atomically_and_failed_save_keeps_previous_value() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("interface.json");
        let mut p = NativePreferences::new(true);
        p.path = Some(path.clone());
        p.assign(Shortcut::Search, "ctrl-shift-s".into());
        p.toggle_motion();
        let saved: Preferences = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(saved.reduced_motion);
        assert_eq!(saved.shortcuts[1], "ctrl-shift-s");
        p.path = Some(path.join("impossible"));
        p.toggle_motion();
        assert!(p.reduced_motion());
        assert!(p.error.is_some());
    }
    #[test]
    fn rebind_changes_the_action_selected_by_native_key_dispatch() {
        let mut p = NativePreferences::new(true);
        assert_eq!(
            p.action(&Keystroke::parse("ctrl-f").unwrap()),
            Some(Shortcut::Search)
        );
        p.assign(Shortcut::Search, "ctrl-shift-s".into());
        assert_eq!(p.action(&Keystroke::parse("ctrl-f").unwrap()), None);
        assert_eq!(
            p.action(&Keystroke::parse("ctrl-shift-s").unwrap()),
            Some(Shortcut::Search)
        );
        assert_eq!(
            keystroke(&Keystroke::parse("ctrl-shift-m").unwrap()),
            "ctrl-shift-m"
        );
        assert_eq!(
            p.action(&Keystroke::parse("ctrl-shift-m").unwrap()),
            Some(Shortcut::Mute)
        );
    }
    #[test]
    fn legacy_preferences_and_partial_window_settings_use_source_defaults() {
        let shortcuts = Preferences::default().shortcuts;
        let legacy = serde_json::json!({"reduced_motion":true,"shortcuts":shortcuts});
        let old: Preferences = serde_json::from_value(legacy).unwrap();
        assert!(old.reduced_motion);
        assert_eq!(old.shortcuts, shortcuts);
        assert_eq!(
            old.window_behavior,
            WindowBehavior {
                show_tray_icon: true,
                close_to_tray: true,
                start_minimized: false,
                colored_tray_icon: false
            }
        );
        let partial: Preferences =
            serde_json::from_value(serde_json::json!({"window_behavior":{"close_to_tray":false}}))
                .unwrap();
        assert_eq!(
            partial.window_behavior,
            WindowBehavior {
                close_to_tray: false,
                ..WindowBehavior::default()
            }
        );
    }
    #[test]
    fn window_settings_save_without_resetting_motion_shortcuts_or_other_window_flags() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("interface.json");
        let mut p = NativePreferences::new(true);
        p.path = Some(path.clone());
        p.toggle_motion();
        p.assign(Shortcut::Search, "ctrl-shift-s".into());
        p.set_window_behavior(WindowSetting::ShowTrayIcon, false);
        p.set_window_behavior(WindowSetting::CloseToTray, false);
        p.set_window_behavior(WindowSetting::StartMinimized, true);
        p.set_window_behavior(WindowSetting::ColoredTrayIcon, true);
        let saved: Preferences = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(
            saved.window_behavior,
            WindowBehavior {
                show_tray_icon: false,
                close_to_tray: false,
                start_minimized: true,
                colored_tray_icon: true
            }
        );
        assert_eq!(p.window_behavior(), saved.window_behavior);
        assert!(saved.reduced_motion);
        assert_eq!(saved.shortcuts[Shortcut::Search.index()], "ctrl-shift-s");
        p.reset();
        assert_eq!(p.window_behavior(), saved.window_behavior);
        assert!(p.reduced_motion());
    }
    #[test]
    fn failed_window_setting_save_keeps_every_previous_preference() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("interface.json");
        let mut p = NativePreferences::new(true);
        p.path = Some(path.clone());
        p.toggle_motion();
        p.assign(Shortcut::Search, "ctrl-shift-s".into());
        p.set_window_behavior(WindowSetting::ColoredTrayIcon, true);
        let before = p.window_behavior();
        let bytes = std::fs::read(&path).unwrap();
        p.path = Some(path.join("cannot-write-through-file"));
        p.set_window_behavior(WindowSetting::CloseToTray, false);
        assert_eq!(p.window_behavior(), before);
        assert!(p.reduced_motion());
        assert_eq!(p.binding(Shortcut::Search), "ctrl-shift-s");
        assert!(p.error.is_some());
        assert_eq!(
            std::fs::read(root.path().join("interface.json")).unwrap(),
            bytes
        );
    }
    #[test]
    fn streamer_preferences_use_device_defaults_and_persist_with_other_settings() {
        let p: Preferences = serde_json::from_str("{}").unwrap();
        assert!(p.streamer.is_none());
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("interface.json");
        let mut prefs = NativePreferences::new(true);
        prefs.path = Some(path.clone());
        prefs.toggle_motion();
        let enabled = super::streamer_mode::Preferences::from_legacy(true);
        prefs.update_streamer(|prefs| *prefs = enabled);
        let saved: Preferences = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved.streamer, Some(enabled));
        assert!(saved.reduced_motion);
    }

    #[test]
    fn preference_serialization_round_trips() {
        let p = Preferences {
            reduced_motion: true,
            ..Preferences::default()
        };
        let copy: Preferences = serde_json::from_slice(&serde_json::to_vec(&p).unwrap()).unwrap();
        assert!(copy.reduced_motion);
        assert_eq!(copy.shortcuts, p.shortcuts);
    }
}
