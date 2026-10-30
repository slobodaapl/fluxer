// SPDX-License-Identifier: AGPL-3.0-or-later

//! `sanitizeDesktopConfig`: the load-time pass over `settings.json`. Unknown keys survive, as
//! the TS spread kept them; every known key is validated or dropped.

use serde_json::{Map, Value};

use super::js::is_blank;
use super::{global_shortcuts, product_names, troubleshooting, window_behavior};
use crate::build_channel::BuildChannel;

pub const CHROMIUM_SWITCHES_KEY: &str = "chromiumSwitches";
pub const WINDOW_BEHAVIOR_KEY: &str = "window_behavior";
pub const TROUBLESHOOTING_KEY: &str = "troubleshooting";
pub const THEME_ALLOWED_LOCAL_FILES_KEY: &str = "theme_allowed_local_files";
pub const APP_ORIGIN_KEY: &str = "app_origin";
pub const PREBOOT_THEME_KEY: &str = "preboot_theme";
pub const INSTANCE_PRODUCT_NAMES_KEY: &str = "instance_product_names";
pub const GLOBAL_SHORTCUTS_KEY: &str = "global_shortcuts";
pub const MAC_SCREEN_RECORDING_REQUESTED_KEY: &str = "mac_screen_recording_requested";
const LEGACY_APP_URL_KEY: &str = "app_url";
const PREBOOT_THEME_MAX_LENGTH: usize = 64;

/// `getOfficialAppOrigins`: the channel's legacy web app origin and its migrated origin.
fn official_app_origins(channel: BuildChannel) -> [&'static str; 2] {
    match channel {
        BuildChannel::Stable => ["https://web.fluxer.app", "https://fluxer.com"],
        BuildChannel::Canary => ["https://web.canary.fluxer.app", "https://canary.fluxer.com"],
        BuildChannel::Development => ["http://localhost:8088", "https://fluxer.com"],
    }
}

/// `PREBOOT_THEME_PATTERN`, `/^[a-z0-9][a-z0-9_-]{0,63}$/u`.
pub fn is_preboot_theme(value: &str) -> bool {
    let mut characters = value.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit())
        && value.len() <= PREBOOT_THEME_MAX_LENGTH
        && characters.all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || character == '_'
                || character == '-'
        })
}

/// `sanitizeChromiumSwitchesSetting`. The switches are kept for a later unit; nothing here
/// applies them.
fn sanitize_chromium_switches(value: &Value) -> Option<Value> {
    match value {
        Value::Array(items) => {
            let switches = non_blank_strings(items);
            (!switches.is_empty()).then_some(Value::Array(switches))
        }
        Value::Object(entries) => {
            let switches: Map<String, Value> = entries
                .iter()
                .filter(|(key, _)| !is_blank(key))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            (!switches.is_empty()).then_some(Value::Object(switches))
        }
        _ => None,
    }
}

/// The non-blank strings of `items`.
fn non_blank_strings(items: &[Value]) -> Vec<Value> {
    items
        .iter()
        .filter(|item| item.as_str().is_some_and(|item| !is_blank(item)))
        .cloned()
        .collect()
}

/// Sets `key` to `value` in place, or removes it, as the TS assignment and `delete` did.
fn put(config: &mut Map<String, Value>, key: &str, value: Option<Value>) {
    match value {
        Some(value) => {
            config.insert(key.to_owned(), value);
        }
        None => {
            config.shift_remove(key);
        }
    }
}

pub fn sanitize_desktop_config(value: Value, channel: BuildChannel) -> Map<String, Value> {
    let Value::Object(mut config) = value else {
        return Map::new();
    };
    config.shift_remove(LEGACY_APP_URL_KEY);
    let string = |config: &Map<String, Value>, key: &str, valid: &dyn Fn(&str) -> bool| {
        config
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| valid(value))
            .map(|value| Value::String(value.to_owned()))
    };
    let app_origin = string(&config, APP_ORIGIN_KEY, &|origin| {
        official_app_origins(channel).contains(&origin)
    });
    put(&mut config, APP_ORIGIN_KEY, app_origin);
    let switches =
        sanitize_chromium_switches(config.get(CHROMIUM_SWITCHES_KEY).unwrap_or(&Value::Null));
    put(&mut config, CHROMIUM_SWITCHES_KEY, switches);
    let window_behavior = window_behavior::sanitize_persisted(
        config.get(WINDOW_BEHAVIOR_KEY).unwrap_or(&Value::Null),
    );
    put(
        &mut config,
        WINDOW_BEHAVIOR_KEY,
        window_behavior.map(Value::Object),
    );
    let troubleshooting = troubleshooting::sanitize_persisted(
        config.get(TROUBLESHOOTING_KEY).unwrap_or(&Value::Null),
    );
    put(
        &mut config,
        TROUBLESHOOTING_KEY,
        troubleshooting.map(Value::Object),
    );
    let shortcuts = global_shortcuts::sanitize_persisted(
        config.get(GLOBAL_SHORTCUTS_KEY).unwrap_or(&Value::Null),
    );
    put(&mut config, GLOBAL_SHORTCUTS_KEY, shortcuts);
    let requested = (config.get(MAC_SCREEN_RECORDING_REQUESTED_KEY) == Some(&Value::Bool(true)))
        .then_some(Value::Bool(true));
    put(&mut config, MAC_SCREEN_RECORDING_REQUESTED_KEY, requested);
    let files = config
        .get(THEME_ALLOWED_LOCAL_FILES_KEY)
        .and_then(Value::as_array)
        .map(|items| Value::Array(non_blank_strings(items)));
    put(&mut config, THEME_ALLOWED_LOCAL_FILES_KEY, files);
    let theme = string(&config, PREBOOT_THEME_KEY, &is_preboot_theme);
    put(&mut config, PREBOOT_THEME_KEY, theme);
    let names = product_names::sanitize(config.get(INSTANCE_PRODUCT_NAMES_KEY))
        .map(|entries| product_names::to_value(&entries));
    put(&mut config, INSTANCE_PRODUCT_NAMES_KEY, names);
    config
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sanitize(value: Value) -> Value {
        Value::Object(sanitize_desktop_config(value, BuildChannel::Stable))
    }

    #[test]
    fn non_object_files_load_empty() {
        for value in [json!(null), json!([1]), json!("x"), json!(3)] {
            assert_eq!(sanitize(value), json!({}));
        }
    }

    #[test]
    fn unknown_keys_survive_and_legacy_or_invalid_ones_are_dropped() {
        let sanitized = sanitize(json!({
            "future": {"a": 1},
            "app_url": "https://web.fluxer.app",
            "app_origin": "https://evil.example",
            "chromiumSwitches": ["  ", 3],
            "window_behavior": {"showTrayIcon": "no"},
            "troubleshooting": [],
            "global_shortcuts": {},
            "mac_screen_recording_requested": false,
            "preboot_theme": "Dark",
            "instance_product_names": [],
        }));
        assert_eq!(sanitized, json!({"future": {"a": 1}}));
    }

    #[test]
    fn valid_values_are_kept_in_place() {
        let input = json!({
            "app_origin": "https://fluxer.com",
            "chromiumSwitches": {"": 1, "enable-features": "X"},
            "theme_allowed_local_files": ["/a.css", " ", 4],
            "window_behavior": {"closeToTrayV2": false, "junk": true},
            "troubleshooting": {"disableHardwareAcceleration": true},
            "mac_screen_recording_requested": true,
            "preboot_theme": "dark_1-x",
            "instance_product_names": [{"instance_key": "k", "product_name": " Acme "}],
        });
        let sanitized = sanitize(input);
        assert_eq!(
            sanitized,
            json!({
                "app_origin": "https://fluxer.com",
                "chromiumSwitches": {"enable-features": "X"},
                "theme_allowed_local_files": ["/a.css"],
                "window_behavior": {"closeToTrayV2": false},
                "troubleshooting": {"disableHardwareAcceleration": true},
                "mac_screen_recording_requested": true,
                "preboot_theme": "dark_1-x",
                "instance_product_names": [{"instance_key": "k", "product_name": "Acme"}],
            })
        );
        assert_eq!(
            sanitized
                .as_object()
                .unwrap()
                .keys()
                .next()
                .map(String::as_str),
            Some("app_origin")
        );
        // An empty file list is kept, as `Array.isArray` + `filter` kept it.
        assert_eq!(
            sanitize(json!({"theme_allowed_local_files": [" "]})),
            json!({"theme_allowed_local_files": []})
        );
    }

    #[test]
    fn app_origins_are_per_channel() {
        let canary = json!({"app_origin": "https://canary.fluxer.com"});
        assert_eq!(sanitize(canary.clone()), json!({}));
        assert_eq!(
            Value::Object(sanitize_desktop_config(
                canary.clone(),
                BuildChannel::Canary
            )),
            canary
        );
    }

    #[test]
    fn preboot_theme_tokens() {
        for valid in ["dark", "0", "a-b_c", &"a".repeat(64)] {
            assert!(is_preboot_theme(valid), "{valid}");
        }
        for invalid in [
            "",
            "-dark",
            "_a",
            "Dark",
            "da rk",
            "dark>",
            "é",
            &"a".repeat(65),
        ] {
            assert!(!is_preboot_theme(invalid), "{invalid}");
        }
    }
}
