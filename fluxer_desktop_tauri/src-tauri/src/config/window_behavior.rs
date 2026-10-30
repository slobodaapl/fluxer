// SPDX-License-Identifier: AGPL-3.0-or-later

//! `DesktopWindowBehaviorSettings`: the persisted `window_behavior` object of DesktopConfig.ts,
//! the update filter of IpcHandlers.ts, and the launch-time values Window.ts kept.

use serde::Serialize;
use serde_json::{Map, Value};

use super::js::{first_boolean, pick_booleans};

const MINIMIZE_TO_TRAY_STORAGE_KEY_V2: &str = "minimizeToTrayV2";
const CLOSE_TO_TRAY_STORAGE_KEY_V2: &str = "closeToTrayV2";

/// The persisted keys, in the order DesktopConfig.ts sanitised and wrote them.
const PERSISTED_KEYS: [&str; 11] = [
    "showTrayIcon",
    "coloredTrayIcon",
    "darkTaskbarBadge",
    "useNativeTitleBar",
    "rememberWindowState",
    "allowTransparency",
    "smoothScrolling",
    "middleClickAutoscroll",
    "startMinimized",
    MINIMIZE_TO_TRAY_STORAGE_KEY_V2,
    CLOSE_TO_TRAY_STORAGE_KEY_V2,
];

/// The keys `desktop-window-behavior-set` accepted (`normalizeDesktopWindowBehaviorUpdate`).
const UPDATE_KEYS: [&str; 11] = [
    "showTrayIcon",
    "coloredTrayIcon",
    "darkTaskbarBadge",
    "minimizeToTray",
    "closeToTray",
    "startMinimized",
    "useNativeTitleBar",
    "rememberWindowState",
    "allowTransparency",
    "smoothScrolling",
    "middleClickAutoscroll",
];

/// `DesktopWindowBehaviorSettings` in fluxer_app/src/types/electron.d.ts.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DesktopWindowBehaviorSettings {
    pub show_tray_icon: bool,
    pub colored_tray_icon: bool,
    pub dark_taskbar_badge: bool,
    pub minimize_to_tray: bool,
    pub close_to_tray: bool,
    pub start_minimized: bool,
    pub use_native_title_bar: bool,
    pub active_use_native_title_bar: bool,
    pub remember_window_state: bool,
    pub allow_transparency: bool,
    pub active_allow_transparency: bool,
    pub smooth_scrolling: bool,
    pub active_smooth_scrolling: bool,
    pub middle_click_autoscroll: bool,
    pub active_middle_click_autoscroll: bool,
}

impl DesktopWindowBehaviorSettings {
    /// The `active*` fields IpcHandlers.ts replaced with what the running window has.
    pub fn with_active(self, active: ActiveWindowBehavior) -> Self {
        Self {
            active_use_native_title_bar: active.use_native_title_bar,
            active_allow_transparency: active.allow_transparency,
            active_smooth_scrolling: active.smooth_scrolling,
            active_middle_click_autoscroll: active.middle_click_autoscroll,
            ..self
        }
    }
}

/// What a launch puts in effect, fixed when the main window is created (`initialUseNativeTitleBar`
/// and `initialAllowTransparency` in Window.ts, the Chromium switches for the other two).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActiveWindowBehavior {
    pub use_native_title_bar: bool,
    pub allow_transparency: bool,
    pub smooth_scrolling: bool,
    pub middle_click_autoscroll: bool,
}

impl ActiveWindowBehavior {
    /// `getEffectiveUseNativeTitleBar` and `allowTransparency` of Window.ts: the native title bar
    /// is used only without transparency and never on macOS, which always draws the renderer's
    /// titlebar under its traffic lights.
    ///
    /// Smooth scrolling follows its setting on Linux, where WebKitGTK has a switch for it; the
    /// other webviews keep their default (on). Middle-click autoscroll was a Blink feature that
    /// no webview here offers, so it is never in effect.
    pub fn for_launch(settings: &DesktopWindowBehaviorSettings) -> Self {
        Self {
            use_native_title_bar: !cfg!(target_os = "macos")
                && settings.use_native_title_bar
                && !settings.allow_transparency,
            allow_transparency: settings.allow_transparency,
            smooth_scrolling: !cfg!(target_os = "linux") || settings.smooth_scrolling,
            middle_click_autoscroll: false,
        }
    }
}

/// `sanitizePersistedDesktopWindowBehaviorSettings`.
pub fn sanitize_persisted(value: &Value) -> Option<Map<String, Value>> {
    pick_booleans(value, &PERSISTED_KEYS)
}

/// `normalizeDesktopWindowBehaviorUpdate`.
pub fn sanitize_update(value: &Value) -> Map<String, Value> {
    pick_booleans(value, &UPDATE_KEYS).unwrap_or_default()
}

/// `normalizeDesktopWindowBehaviorSettings`. `source` is either the persisted object (with the
/// V2 tray keys) or a full settings object merged with an update.
pub fn normalize(source: Option<&Map<String, Value>>) -> DesktopWindowBehaviorSettings {
    let read = |keys: &[&str], fallback: bool| first_boolean(source, keys, fallback);
    let use_native_title_bar = read(&["useNativeTitleBar"], false);
    let allow_transparency = read(&["allowTransparency"], false);
    let smooth_scrolling = read(&["smoothScrolling"], true);
    let middle_click_autoscroll = read(&["middleClickAutoscroll"], false);
    let mut settings = DesktopWindowBehaviorSettings {
        show_tray_icon: read(&["showTrayIcon"], true),
        colored_tray_icon: read(&["coloredTrayIcon"], false),
        dark_taskbar_badge: read(&["darkTaskbarBadge"], false),
        minimize_to_tray: read(&["minimizeToTray", MINIMIZE_TO_TRAY_STORAGE_KEY_V2], false),
        close_to_tray: read(&["closeToTray", CLOSE_TO_TRAY_STORAGE_KEY_V2], true),
        start_minimized: read(&["startMinimized"], false),
        use_native_title_bar,
        active_use_native_title_bar: read(&["activeUseNativeTitleBar"], use_native_title_bar),
        remember_window_state: read(&["rememberWindowState"], true),
        allow_transparency,
        active_allow_transparency: read(&["activeAllowTransparency"], allow_transparency),
        smooth_scrolling,
        active_smooth_scrolling: read(&["activeSmoothScrolling"], smooth_scrolling),
        middle_click_autoscroll,
        active_middle_click_autoscroll: read(
            &["activeMiddleClickAutoscroll"],
            middle_click_autoscroll,
        ),
    };
    if !settings.show_tray_icon {
        settings.minimize_to_tray = false;
        settings.close_to_tray = false;
        settings.start_minimized = false;
    }
    settings
}

/// `serializeDesktopWindowBehaviorSettings`.
pub fn serialize(settings: &DesktopWindowBehaviorSettings) -> Value {
    let values = [
        settings.show_tray_icon,
        settings.colored_tray_icon,
        settings.dark_taskbar_badge,
        settings.use_native_title_bar,
        settings.remember_window_state,
        settings.allow_transparency,
        settings.smooth_scrolling,
        settings.middle_click_autoscroll,
        settings.start_minimized,
        settings.minimize_to_tray,
        settings.close_to_tray,
    ];
    Value::Object(
        PERSISTED_KEYS
            .iter()
            .zip(values)
            .map(|(key, value)| ((*key).to_owned(), Value::Bool(value)))
            .collect(),
    )
}

/// `normalize({...current, ...update})` as `setDesktopWindowBehaviorSettings` computed it.
pub fn apply_update(
    current: &DesktopWindowBehaviorSettings,
    update: Map<String, Value>,
) -> DesktopWindowBehaviorSettings {
    let mut merged = match serde_json::to_value(current) {
        Ok(Value::Object(merged)) => merged,
        _ => unreachable!("window behavior settings serialise to an object"),
    };
    merged.extend(update);
    normalize(Some(&merged))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn defaults() -> DesktopWindowBehaviorSettings {
        normalize(None)
    }

    #[test]
    fn defaults_match_desktop_config() {
        assert_eq!(
            serde_json::to_value(defaults()).unwrap(),
            json!({
                "showTrayIcon": true,
                "coloredTrayIcon": false,
                "darkTaskbarBadge": false,
                "minimizeToTray": false,
                "closeToTray": true,
                "startMinimized": false,
                "useNativeTitleBar": false,
                "activeUseNativeTitleBar": false,
                "rememberWindowState": true,
                "allowTransparency": false,
                "activeAllowTransparency": false,
                "smoothScrolling": true,
                "activeSmoothScrolling": true,
                "middleClickAutoscroll": false,
                "activeMiddleClickAutoscroll": false,
            })
        );
    }

    #[test]
    fn persisted_tray_keys_use_the_v2_names() {
        let persisted = sanitize_persisted(&json!({
            "minimizeToTray": true,
            "minimizeToTrayV2": true,
            "closeToTrayV2": false,
            "useNativeTitleBar": "yes",
        }))
        .unwrap();
        assert_eq!(
            Value::Object(persisted.clone()),
            json!({"minimizeToTrayV2": true, "closeToTrayV2": false})
        );
        let settings = normalize(Some(&persisted));
        assert!(settings.minimize_to_tray);
        assert!(!settings.close_to_tray);
        assert!(!settings.use_native_title_bar);
    }

    #[test]
    fn hiding_the_tray_icon_turns_off_tray_behaviour() {
        let settings = normalize(
            json!({"showTrayIcon": false, "minimizeToTrayV2": true, "closeToTrayV2": true, "startMinimized": true})
                .as_object(),
        );
        assert!(!settings.minimize_to_tray && !settings.close_to_tray && !settings.start_minimized);
    }

    #[test]
    fn update_filter_drops_active_v2_and_non_boolean_fields() {
        let update = sanitize_update(&json!({
            "closeToTray": false,
            "activeUseNativeTitleBar": true,
            "minimizeToTrayV2": true,
            "smoothScrolling": 0,
        }));
        assert_eq!(Value::Object(update), json!({"closeToTray": false}));
        assert!(sanitize_update(&json!("closeToTray")).is_empty());
    }

    #[test]
    fn update_round_trips_through_the_persisted_shape() {
        let updated = apply_update(
            &defaults(),
            sanitize_update(&json!({"minimizeToTray": true, "useNativeTitleBar": true})),
        );
        assert!(updated.minimize_to_tray && updated.use_native_title_bar);
        let persisted = serialize(&updated);
        assert_eq!(
            persisted.as_object().unwrap().keys().collect::<Vec<_>>(),
            PERSISTED_KEYS.iter().collect::<Vec<_>>()
        );
        assert_eq!(persisted["minimizeToTrayV2"], json!(true));
        let reloaded = normalize(sanitize_persisted(&persisted).as_ref());
        assert_eq!(reloaded, normalize(persisted.as_object()));
        assert!(reloaded.minimize_to_tray && reloaded.use_native_title_bar);
        assert!(reloaded.active_use_native_title_bar);
    }

    #[test]
    fn active_values_come_from_the_launch() {
        let stored = normalize(
            json!({"useNativeTitleBar": true, "allowTransparency": false, "smoothScrolling": false})
                .as_object(),
        );
        let reported = stored.with_active(ActiveWindowBehavior::for_launch(&stored));
        assert_eq!(
            reported.active_use_native_title_bar,
            !cfg!(target_os = "macos")
        );
        assert!(!reported.active_allow_transparency);
        assert_eq!(reported.active_smooth_scrolling, !cfg!(target_os = "linux"));
        assert!(!reported.active_middle_click_autoscroll);
    }

    #[test]
    fn transparency_turns_the_native_title_bar_off() {
        let stored =
            normalize(json!({"useNativeTitleBar": true, "allowTransparency": true}).as_object());
        let active = ActiveWindowBehavior::for_launch(&stored);
        assert!(!active.use_native_title_bar);
        assert!(active.allow_transparency);
    }
}
