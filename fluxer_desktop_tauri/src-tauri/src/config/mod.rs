// SPDX-License-Identifier: AGPL-3.0-or-later

//! The desktop config, the port of fluxer_desktop/src/common/DesktopConfig.ts: `settings.json`
//! with the same schema, loaded once at setup and rewritten (owner-only, write-then-rename)
//! after every change. It lives in Tauri's `app_data_dir`, not Electron's userData directory;
//! picking up an Electron install's settings is not part of this shell yet.

pub mod commands;
mod global_shortcuts;
pub(crate) mod js;
mod last_route;
pub mod launch;
mod product_names;
mod sanitize;
mod troubleshooting;
mod window_behavior;

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde_json::{Map, Value};

use self::global_shortcuts::GlobalShortcutsSettings;
use self::sanitize::{
    GLOBAL_SHORTCUTS_KEY, INSTANCE_PRODUCT_NAMES_KEY, MAC_SCREEN_RECORDING_REQUESTED_KEY,
    PREBOOT_THEME_KEY, THEME_ALLOWED_LOCAL_FILES_KEY, TROUBLESHOOTING_KEY, WINDOW_BEHAVIOR_KEY,
    is_preboot_theme, sanitize_desktop_config,
};
use self::troubleshooting::DesktopTroubleshootingSettings;
pub use self::window_behavior::ActiveWindowBehavior;
use self::window_behavior::DesktopWindowBehaviorSettings;
use crate::build_channel::BuildChannel;
use crate::storage::private_file;

const CONFIG_FILE_NAME: &str = "settings.json";

/// Values fixed for this run when the config loads.
struct Launch {
    window: ActiveWindowBehavior,
    /// `initialTrayDesired` in DesktopTray.ts.
    show_tray_icon: bool,
    /// `getLaunchDesktopTroubleshootingSettings`.
    troubleshooting: DesktopTroubleshootingSettings,
}

pub struct DesktopConfig {
    directory: PathBuf,
    values: Mutex<Map<String, Value>>,
    launch: Launch,
}

impl DesktopConfig {
    /// `loadDesktopConfig`. A missing file is an empty config; an unreadable one is logged and
    /// treated as empty, and the next change overwrites it.
    pub fn load(directory: &Path, args: &[String]) -> Self {
        let path = directory.join(CONFIG_FILE_NAME);
        let values = match std::fs::read_to_string(&path) {
            Ok(raw) => match serde_json::from_str(&raw) {
                Ok(value) => {
                    tracing::info!(path = %path.display(), "Loaded desktop config");
                    sanitize_desktop_config(value, BuildChannel::current())
                }
                Err(error) => {
                    tracing::error!(%error, "Failed to load desktop config");
                    Map::new()
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Map::new(),
            Err(error) => {
                tracing::error!(%error, "Failed to load desktop config");
                Map::new()
            }
        };
        Self::with_values(directory.to_owned(), values, args)
    }

    fn with_values(directory: PathBuf, values: Map<String, Value>, args: &[String]) -> Self {
        let window = window_behavior::normalize(object(values.get(WINDOW_BEHAVIOR_KEY)));
        let troubleshooting = troubleshooting::normalize(object(values.get(TROUBLESHOOTING_KEY)))
            .for_launch(args, cfg!(target_os = "macos"));
        Self {
            directory,
            values: Mutex::new(values),
            launch: Launch {
                window: ActiveWindowBehavior::for_launch(&window),
                show_tray_icon: window.show_tray_icon,
                troubleshooting,
            },
        }
    }

    fn lock(&self) -> MutexGuard<'_, Map<String, Value>> {
        self.values.lock().expect("desktop config lock")
    }

    /// Applies `change` and saves when it reports a change, under one lock so writes land in
    /// the order they were made.
    fn edit(&self, change: impl FnOnce(&mut Map<String, Value>) -> bool) {
        let mut values = self.lock();
        if change(&mut values) {
            self.save(&values);
        }
    }

    /// `saveDesktopConfig`. A failed write is logged; the in-memory config keeps the change.
    fn save(&self, values: &Map<String, Value>) {
        let path = self.directory.join(CONFIG_FILE_NAME);
        let contents = serde_json::to_string_pretty(values).expect("desktop config serialises");
        let written = std::fs::create_dir_all(&self.directory)
            .and_then(|()| private_file::write(&path, &contents));
        match written {
            Ok(()) => tracing::debug!(path = %path.display(), "Saved desktop config"),
            Err(error) => tracing::error!(%error, "Failed to save desktop config"),
        }
    }

    pub fn active_window_behavior(&self) -> ActiveWindowBehavior {
        self.launch.window
    }

    fn stored_window_behavior(&self) -> DesktopWindowBehaviorSettings {
        window_behavior::normalize(object(self.lock().get(WINDOW_BEHAVIOR_KEY)))
    }

    /// `desktop-window-behavior-get`: the stored settings with this run's `active*` values.
    pub fn window_behavior(&self) -> DesktopWindowBehaviorSettings {
        self.stored_window_behavior()
            .with_active(self.launch.window)
    }

    /// `desktop-window-behavior-set`: the update filter of IpcHandlers.ts, then
    /// `setDesktopWindowBehaviorSettings`.
    pub fn set_window_behavior(&self, update: &Value) -> DesktopWindowBehaviorSettings {
        let update = window_behavior::sanitize_update(update);
        self.edit(|values| {
            let current = window_behavior::normalize(object(values.get(WINDOW_BEHAVIOR_KEY)));
            let next = window_behavior::apply_update(&current, update);
            values.insert(
                WINDOW_BEHAVIOR_KEY.to_owned(),
                window_behavior::serialize(&next),
            );
            true
        });
        self.window_behavior()
    }

    /// `initialTrayDesired` of DesktopTray.ts: `showTrayIcon` as this run started.
    pub fn launch_show_tray_icon(&self) -> bool {
        self.launch.show_tray_icon
    }

    /// `desktop-window-behavior-pending-restart`: the tray (Linux only, where it cannot be
    /// created or removed at runtime), the effective native title bar and transparency. The
    /// other `active*` values are not part of Electron's rule.
    pub fn window_behavior_pending_restart(&self) -> bool {
        let current = self.stored_window_behavior();
        let tray =
            cfg!(target_os = "linux") && current.show_tray_icon != self.launch.show_tray_icon;
        let next = ActiveWindowBehavior::for_launch(&current);
        tray || next.use_native_title_bar != self.launch.window.use_native_title_bar
            || next.allow_transparency != self.launch.window.allow_transparency
    }

    /// `getLaunchDesktopTroubleshootingSettings`.
    pub fn launch_troubleshooting(&self) -> DesktopTroubleshootingSettings {
        self.launch.troubleshooting
    }

    /// `setDesktopTroubleshootingSettings`.
    pub fn set_troubleshooting(&self, change: impl FnOnce(&mut DesktopTroubleshootingSettings)) {
        self.edit(|values| {
            let mut settings = troubleshooting::normalize(object(values.get(TROUBLESHOOTING_KEY)));
            change(&mut settings);
            values.insert(
                TROUBLESHOOTING_KEY.to_owned(),
                troubleshooting::serialize(&settings),
            );
            true
        });
    }

    /// `getPrebootTheme`.
    pub fn preboot_theme(&self) -> Option<String> {
        self.lock()
            .get(PREBOOT_THEME_KEY)
            .and_then(Value::as_str)
            .filter(|theme| is_preboot_theme(theme))
            .map(str::to_owned)
    }

    /// `setPrebootTheme`: ignores invalid tokens and saves only a change.
    pub fn set_preboot_theme(&self, theme: &str) {
        if !is_preboot_theme(theme) {
            return;
        }
        self.edit(|values| {
            if values.get(PREBOOT_THEME_KEY).and_then(Value::as_str) == Some(theme) {
                return false;
            }
            values.insert(
                PREBOOT_THEME_KEY.to_owned(),
                Value::String(theme.to_owned()),
            );
            true
        });
    }

    /// `getLaunchProductName`: the most recently reported instance's product name.
    pub fn launch_product_name(&self) -> Option<String> {
        product_names::sanitize(self.lock().get(INSTANCE_PRODUCT_NAMES_KEY))?
            .into_iter()
            .next()
            .map(|entry| entry.product_name)
    }

    /// `rememberInstanceProductName`.
    pub fn remember_instance_product_name(&self, report: &Value) {
        let Some(accepted) = product_names::read_report(report) else {
            return;
        };
        self.edit(|values| {
            let current =
                product_names::sanitize(values.get(INSTANCE_PRODUCT_NAMES_KEY)).unwrap_or_default();
            let Some(next) = product_names::remember(current, accepted) else {
                return false;
            };
            values.insert(
                INSTANCE_PRODUCT_NAMES_KEY.to_owned(),
                product_names::to_value(&next),
            );
            true
        });
    }

    /// `recordDesktopLastRoute`.
    pub fn record_last_route(&self, route_path: &Value) {
        last_route::record(&self.directory, route_path);
    }

    /// `resolveDesktopLandingUrl`.
    pub fn landing_url(&self) -> String {
        last_route::resolve_landing_url(&self.directory)
    }
}

/// Settings stored here for consumers that later units add: the global shortcut service, theme
/// local-file access and the macOS screen-recording prompt.
#[allow(dead_code)]
impl DesktopConfig {
    /// `getGlobalShortcutsSettings`.
    pub fn global_shortcuts(&self) -> GlobalShortcutsSettings {
        global_shortcuts::normalize(self.lock().get(GLOBAL_SHORTCUTS_KEY))
    }

    /// `setGlobalShortcutsSettings`; `change` edits the current settings.
    pub fn update_global_shortcuts(
        &self,
        change: impl FnOnce(&mut GlobalShortcutsSettings),
    ) -> GlobalShortcutsSettings {
        self.edit(|values| {
            let mut settings = global_shortcuts::normalize(values.get(GLOBAL_SHORTCUTS_KEY));
            change(&mut settings);
            values.insert(
                GLOBAL_SHORTCUTS_KEY.to_owned(),
                global_shortcuts::serialize(&settings),
            );
            true
        });
        self.global_shortcuts()
    }

    /// `getAllowedThemeLocalFiles`.
    pub fn allowed_theme_local_files(&self) -> Vec<String> {
        allowed_theme_local_files(&self.lock())
    }

    /// `addAllowedThemeLocalFiles`: the union, deduplicated and sorted by UTF-16 code units as
    /// `Array.prototype.sort` does.
    pub fn add_allowed_theme_local_files(&self, paths: &[String]) -> Vec<String> {
        self.edit(|values| {
            let mut files = allowed_theme_local_files(values);
            for path in paths {
                if !js::is_blank(path) && !files.contains(path) {
                    files.push(path.clone());
                }
            }
            files.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
            values.insert(THEME_ALLOWED_LOCAL_FILES_KEY.to_owned(), Value::from(files));
            true
        });
        self.allowed_theme_local_files()
    }

    /// `clearAllowedThemeLocalFiles`.
    pub fn clear_allowed_theme_local_files(&self) {
        self.edit(|values| {
            values.shift_remove(THEME_ALLOWED_LOCAL_FILES_KEY);
            true
        });
    }

    /// `hasRequestedMacScreenRecording`.
    pub fn has_requested_mac_screen_recording(&self) -> bool {
        self.lock().get(MAC_SCREEN_RECORDING_REQUESTED_KEY) == Some(&Value::Bool(true))
    }

    /// `markMacScreenRecordingRequested`.
    pub fn mark_mac_screen_recording_requested(&self) {
        self.edit(|values| {
            if values.get(MAC_SCREEN_RECORDING_REQUESTED_KEY) == Some(&Value::Bool(true)) {
                return false;
            }
            values.insert(
                MAC_SCREEN_RECORDING_REQUESTED_KEY.to_owned(),
                Value::Bool(true),
            );
            true
        });
    }
}

fn object(value: Option<&Value>) -> Option<&Map<String, Value>> {
    value?.as_object()
}

fn allowed_theme_local_files(values: &Map<String, Value>) -> Vec<String> {
    values
        .get(THEME_ALLOWED_LOCAL_FILES_KEY)
        .and_then(Value::as_array)
        .map(|files| {
            files
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
