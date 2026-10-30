// SPDX-License-Identifier: AGPL-3.0-or-later

//! `trayState` of DesktopTray.ts: what the renderer last reported for the tray menu.
//!
//! Electron `Object.assign`ed whatever the renderer sent; here each field takes only a value of
//! its type (`null` clears the nullable ones), so a malformed field is ignored instead of
//! stored. `hidePersonalInformation` only renamed accounts in the account submenu, which this
//! shell does not have, so it is not kept.

use serde::Serialize;
use serde_json::Value;

/// `TrayPresenceStatus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PresenceStatus {
    Online,
    Idle,
    Dnd,
    Invisible,
}

impl PresenceStatus {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "online" => Some(Self::Online),
            "idle" => Some(Self::Idle),
            "dnd" => Some(Self::Dnd),
            "invisible" => Some(Self::Invisible),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrayRuntimeState {
    pub voice_connected: bool,
    pub voice_channel_label: Option<String>,
    pub self_mute: bool,
    pub self_deaf: bool,
    pub presence_status: Option<PresenceStatus>,
    pub build_info: Option<String>,
}

fn boolean(update: &Value, key: &str, field: &mut bool) {
    if let Some(value) = update.get(key).and_then(Value::as_bool) {
        *field = value;
    }
}

fn nullable_string(update: &Value, key: &str, field: &mut Option<String>) {
    match update.get(key) {
        Some(Value::String(value)) => *field = Some(value.clone()),
        Some(Value::Null) => *field = None,
        _ => {}
    }
}

impl TrayRuntimeState {
    /// Applies a `Partial<TrayRuntimeStatePayload>`; the build info when it changed to a
    /// string, which Electron logged.
    pub fn apply(&mut self, update: &Value) -> Option<String> {
        let previous_build_info = self.build_info.clone();
        boolean(update, "voiceConnected", &mut self.voice_connected);
        nullable_string(update, "voiceChannelLabel", &mut self.voice_channel_label);
        boolean(update, "selfMute", &mut self.self_mute);
        boolean(update, "selfDeaf", &mut self.self_deaf);
        match update.get("presenceStatus") {
            Some(Value::String(status)) => self.presence_status = PresenceStatus::parse(status),
            Some(Value::Null) => self.presence_status = None,
            _ => {}
        }
        nullable_string(update, "buildInfo", &mut self.build_info);
        self.build_info
            .clone()
            .filter(|_| update.get("buildInfo").is_some_and(Value::is_string))
            .filter(|info| previous_build_info.as_ref() != Some(info))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn partial_updates_keep_other_fields_and_null_clears() {
        let mut state = TrayRuntimeState::default();
        state.apply(&json!({"voiceConnected": true, "voiceChannelLabel": "lobby", "presenceStatus": "idle"}));
        state.apply(&json!({"selfMute": true}));
        assert_eq!(
            state,
            TrayRuntimeState {
                voice_connected: true,
                voice_channel_label: Some("lobby".to_owned()),
                self_mute: true,
                self_deaf: false,
                presence_status: Some(PresenceStatus::Idle),
                build_info: None,
            }
        );
        state.apply(&json!({"voiceChannelLabel": null, "presenceStatus": null}));
        assert_eq!(state.voice_channel_label, None);
        assert_eq!(state.presence_status, None);
    }

    #[test]
    fn mistyped_fields_are_ignored() {
        let mut state = TrayRuntimeState::default();
        state.apply(&json!({"voiceConnected": "yes", "selfDeaf": 1, "buildInfo": 5}));
        assert_eq!(state, TrayRuntimeState::default());
        state.apply(&json!({"presenceStatus": "busy"}));
        assert_eq!(state.presence_status, None);
    }

    #[test]
    fn only_a_new_build_info_is_reported() {
        let mut state = TrayRuntimeState::default();
        assert_eq!(state.apply(&json!({"buildInfo": "a"})).as_deref(), Some("a"));
        assert_eq!(state.apply(&json!({"buildInfo": "a"})), None);
        assert_eq!(state.apply(&json!({"selfMute": true})), None);
        assert_eq!(state.apply(&json!({"buildInfo": "b"})).as_deref(), Some("b"));
    }
}
