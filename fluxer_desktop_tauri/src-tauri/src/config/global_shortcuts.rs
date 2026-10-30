// SPDX-License-Identifier: AGPL-3.0-or-later

//! The persisted `global_shortcuts` object of DesktopConfig.ts. Only stored here; the global
//! shortcut service that reads it is a later unit.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::js::{js_trim, utf16_length};

/// `GLOBAL_SHORTCUT_ACTIONS` in fluxer_desktop/src/common/GlobalShortcutActions.ts.
const GLOBAL_SHORTCUT_ACTIONS: [&str; 11] = [
    "voice_toggle_mute",
    "voice_toggle_deafen",
    "voice_push_to_talk",
    "voice_push_to_talk_priority",
    "voice_push_to_mute",
    "voice_priority_vad",
    "voice_toggle_vad",
    "voice_toggle_camera",
    "voice_switch_channel",
    "voice_disconnect",
    "system_open_account_switcher",
];
const DESCRIPTION_MAX_LENGTH: usize = 200;
const PREFERRED_TRIGGER_MAX_LENGTH: usize = 100;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum GlobalShortcutsPortalConsent {
    #[default]
    Unset,
    Granted,
    Declined,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GlobalShortcutAction {
    pub action: String,
    pub description: String,
    pub preferred_trigger: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GlobalShortcutsSettings {
    pub portal_consent: GlobalShortcutsPortalConsent,
    pub direct_input_enabled: bool,
    pub migrated: bool,
    pub last_actions: Vec<GlobalShortcutAction>,
}

/// `PersistedGlobalShortcutsSettings`.
#[derive(Serialize, Default)]
struct Persisted {
    #[serde(skip_serializing_if = "Option::is_none")]
    portal_consent: Option<GlobalShortcutsPortalConsent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    direct_input_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    migrated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_actions: Option<Vec<GlobalShortcutAction>>,
}

fn sanitize_action(item: &Value) -> Option<GlobalShortcutAction> {
    let item = item.as_object()?;
    let action = item.get("action")?.as_str()?;
    if !GLOBAL_SHORTCUT_ACTIONS.contains(&action) {
        return None;
    }
    let description = js_trim(item.get("description")?.as_str()?);
    let length = utf16_length(description);
    if length == 0 || length > DESCRIPTION_MAX_LENGTH {
        return None;
    }
    let preferred_trigger = item
        .get("preferredTrigger")
        .and_then(Value::as_str)
        .filter(|trigger| (1..=PREFERRED_TRIGGER_MAX_LENGTH).contains(&utf16_length(trigger)));
    Some(GlobalShortcutAction {
        action: action.to_owned(),
        description: description.to_owned(),
        preferred_trigger: preferred_trigger.map(str::to_owned),
    })
}

/// `sanitizePersistedGlobalShortcutActions`: the first valid entry per action.
fn sanitize_actions(value: Option<&Value>) -> Option<Vec<GlobalShortcutAction>> {
    let mut actions: Vec<GlobalShortcutAction> = Vec::new();
    for item in value?.as_array()? {
        let Some(action) = sanitize_action(item) else {
            continue;
        };
        if actions.iter().all(|seen| seen.action != action.action) {
            actions.push(action);
        }
    }
    (!actions.is_empty()).then_some(actions)
}

fn sanitize(value: &Value) -> Option<Persisted> {
    let object = value.as_object()?;
    let persisted = Persisted {
        portal_consent: object
            .get("portal_consent")
            .and_then(|consent| GlobalShortcutsPortalConsent::deserialize(consent).ok()),
        direct_input_enabled: object.get("direct_input_enabled").and_then(Value::as_bool),
        migrated: object.get("migrated").and_then(Value::as_bool),
        last_actions: sanitize_actions(object.get("last_actions")),
    };
    let empty = persisted.portal_consent.is_none()
        && persisted.direct_input_enabled.is_none()
        && persisted.migrated.is_none()
        && persisted.last_actions.is_none();
    (!empty).then_some(persisted)
}

fn to_value(persisted: &Persisted) -> Value {
    serde_json::to_value(persisted).expect("global shortcut settings serialise")
}

/// `sanitizePersistedGlobalShortcutsSettings`.
pub fn sanitize_persisted(value: &Value) -> Option<Value> {
    sanitize(value).as_ref().map(to_value)
}

/// `normalizeGlobalShortcutsSettings`.
pub fn normalize(source: Option<&Value>) -> GlobalShortcutsSettings {
    let persisted = source.and_then(sanitize).unwrap_or_default();
    GlobalShortcutsSettings {
        portal_consent: persisted.portal_consent.unwrap_or_default(),
        direct_input_enabled: persisted.direct_input_enabled.unwrap_or(false),
        migrated: persisted.migrated.unwrap_or(false),
        last_actions: persisted.last_actions.unwrap_or_default(),
    }
}

/// `serializeGlobalShortcutsSettings`.
pub fn serialize(settings: &GlobalShortcutsSettings) -> Value {
    to_value(&Persisted {
        portal_consent: Some(settings.portal_consent),
        direct_input_enabled: Some(settings.direct_input_enabled),
        migrated: Some(settings.migrated),
        last_actions: (!settings.last_actions.is_empty()).then(|| settings.last_actions.clone()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn defaults_when_absent_or_invalid() {
        assert_eq!(normalize(None), GlobalShortcutsSettings::default());
        assert_eq!(
            sanitize_persisted(&json!({"portal_consent": "maybe", "migrated": 1})),
            None
        );
    }

    #[test]
    fn actions_are_validated_trimmed_and_deduplicated() {
        let sanitized = sanitize_persisted(&json!({
            "portal_consent": "granted",
            "last_actions": [
                {"action": "voice_toggle_mute", "description": "  Mute  ", "preferredTrigger": "CTRL+M"},
                {"action": "voice_toggle_mute", "description": "Second"},
                {"action": "unknown", "description": "x"},
                {"action": "voice_disconnect", "description": "   "},
                {"action": "voice_disconnect", "description": "x".repeat(201)},
                {"action": "voice_toggle_deafen", "description": "Deafen", "preferredTrigger": "x".repeat(101)},
            ],
        }))
        .unwrap();
        assert_eq!(
            sanitized,
            json!({
                "portal_consent": "granted",
                "last_actions": [
                    {"action": "voice_toggle_mute", "description": "Mute", "preferredTrigger": "CTRL+M"},
                    {"action": "voice_toggle_deafen", "description": "Deafen", "preferredTrigger": null},
                ],
            })
        );
    }

    #[test]
    fn serialize_writes_every_flag_and_omits_empty_actions() {
        let settings = GlobalShortcutsSettings {
            portal_consent: GlobalShortcutsPortalConsent::Declined,
            direct_input_enabled: true,
            migrated: false,
            last_actions: Vec::new(),
        };
        let persisted = serialize(&settings);
        assert_eq!(
            persisted,
            json!({"portal_consent": "declined", "direct_input_enabled": true, "migrated": false})
        );
        assert_eq!(normalize(Some(&persisted)), settings);
    }
}
