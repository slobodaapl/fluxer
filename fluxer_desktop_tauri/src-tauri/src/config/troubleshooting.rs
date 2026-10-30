// SPDX-License-Identifier: AGPL-3.0-or-later

//! `DesktopTroubleshootingSettings`: the persisted `troubleshooting` object of DesktopConfig.ts
//! and the launch view of DesktopDebugInfo.ts.
//!
//! Electron applied `disableHardwareAcceleration` as a Chromium switch before ready and
//! `disableModuleUpdates` to its module updater. This shell persists and reports both; applying
//! them to the webview is a later unit.

use serde::Serialize;
use serde_json::{Map, Value, json};

use super::js::{first_boolean, pick_booleans};
use super::launch;

const KEYS: [&str; 2] = ["disableHardwareAcceleration", "disableModuleUpdates"];

/// `DesktopTroubleshootingSettings` in fluxer_app/src/types/electron.d.ts.
#[derive(Serialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DesktopTroubleshootingSettings {
    pub disable_hardware_acceleration: bool,
    pub disable_module_updates: bool,
}

impl DesktopTroubleshootingSettings {
    /// `resolveEffectiveDesktopTroubleshootingSettings`: the stored values plus the launch flags.
    pub fn for_launch(self, args: &[String], is_macos: bool) -> Self {
        Self {
            disable_hardware_acceleration: !is_macos
                && (self.disable_hardware_acceleration
                    || launch::disables_hardware_acceleration(args)),
            disable_module_updates: self.disable_module_updates
                || launch::disables_module_updates(args),
        }
    }
}

/// `sanitizePersistedDesktopTroubleshootingSettings`.
pub fn sanitize_persisted(value: &Value) -> Option<Map<String, Value>> {
    pick_booleans(value, &KEYS)
}

/// `normalizeDesktopTroubleshootingSettings`.
pub fn normalize(source: Option<&Map<String, Value>>) -> DesktopTroubleshootingSettings {
    DesktopTroubleshootingSettings {
        disable_hardware_acceleration: first_boolean(source, &[KEYS[0]], false),
        disable_module_updates: first_boolean(source, &[KEYS[1]], false),
    }
}

/// `serializeDesktopTroubleshootingSettings`.
pub fn serialize(settings: &DesktopTroubleshootingSettings) -> Value {
    json!({
        KEYS[0]: settings.disable_hardware_acceleration,
        KEYS[1]: settings.disable_module_updates,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn defaults_and_round_trip() {
        assert_eq!(normalize(None), DesktopTroubleshootingSettings::default());
        let settings = DesktopTroubleshootingSettings {
            disable_hardware_acceleration: true,
            disable_module_updates: false,
        };
        let persisted = serialize(&settings);
        assert_eq!(
            persisted.to_string(),
            r#"{"disableHardwareAcceleration":true,"disableModuleUpdates":false}"#
        );
        assert_eq!(normalize(sanitize_persisted(&persisted).as_ref()), settings);
        assert_eq!(
            sanitize_persisted(&json!({"disableModuleUpdates": "1"})),
            None
        );
    }

    #[test]
    fn launch_flags_force_the_switches_and_macos_keeps_acceleration() {
        let stored = DesktopTroubleshootingSettings::default();
        let launched = stored.for_launch(&args(&["app", "--fluxer-disable-gpu"]), false);
        assert!(launched.disable_hardware_acceleration);
        assert!(!launched.disable_module_updates);
        assert!(
            stored
                .for_launch(&args(&["app", "--fluxer-safe-mode"]), false)
                .disable_hardware_acceleration
        );
        let stored = DesktopTroubleshootingSettings {
            disable_hardware_acceleration: true,
            disable_module_updates: false,
        };
        let launched = stored.for_launch(&args(&["app", "--disable-module-updates=1"]), true);
        assert!(!launched.disable_hardware_acceleration);
        assert!(launched.disable_module_updates);
    }
}
