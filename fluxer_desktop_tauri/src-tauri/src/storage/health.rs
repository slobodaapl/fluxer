// SPDX-License-Identifier: AGPL-3.0-or-later

//! DesktopAppStoreHealth.ts: a state file records that the store has worked on this
//! machine, so a later failure to open it reads as a regression rather than a first run.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use super::private_file;

const DESKTOP_APP_STORE_STATE_FILE_NAME: &str = "account-store-state-v1.json";
const DESKTOP_APP_STORE_STATE_VERSION: u64 = 1;

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DesktopAppStoreState {
    pub version: u64,
    pub first_success_at: f64,
    pub last_success_at: f64,
    pub last_schema_version: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DesktopAppStoreHealth {
    Healthy,
    Unproven,
    Regressed,
}

impl DesktopAppStoreHealth {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Unproven => "unproven",
            Self::Regressed => "regressed",
        }
    }
}

pub(super) fn desktop_app_store_state_file(user_data_path: &Path) -> PathBuf {
    user_data_path.join(DESKTOP_APP_STORE_STATE_FILE_NAME)
}

fn read_timestamp(value: Option<&Value>) -> Option<f64> {
    value.and_then(Value::as_f64).filter(|value| *value > 0.0)
}

pub(super) fn read_desktop_app_store_state(user_data_path: &Path) -> Option<DesktopAppStoreState> {
    let contents = std::fs::read_to_string(desktop_app_store_state_file(user_data_path)).ok()?;
    let parsed: Value = serde_json::from_str(&contents).ok()?;
    let parsed = parsed.as_object()?;
    if parsed.get("version").and_then(Value::as_f64) != Some(DESKTOP_APP_STORE_STATE_VERSION as f64)
    {
        return None;
    }
    Some(DesktopAppStoreState {
        version: DESKTOP_APP_STORE_STATE_VERSION,
        first_success_at: read_timestamp(parsed.get("firstSuccessAt"))?,
        last_success_at: read_timestamp(parsed.get("lastSuccessAt"))?,
        last_schema_version: parsed
            .get("lastSchemaVersion")
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
    })
}

pub(super) fn record_desktop_app_store_success(
    user_data_path: &Path,
    now: f64,
    schema_version: i64,
) -> Option<DesktopAppStoreState> {
    let previous = read_desktop_app_store_state(user_data_path);
    let state = DesktopAppStoreState {
        version: DESKTOP_APP_STORE_STATE_VERSION,
        first_success_at: previous.map_or(now, |previous| previous.first_success_at),
        last_success_at: now,
        last_schema_version: schema_version as f64,
    };
    let encoded = serde_json::to_string(&state).ok()?;
    private_file::write(
        &desktop_app_store_state_file(user_data_path),
        &format!("{encoded}\n"),
    )
    .ok()?;
    Some(state)
}

pub(super) fn evaluate_desktop_app_store_health(
    available: bool,
    state: Option<&DesktopAppStoreState>,
) -> DesktopAppStoreHealth {
    if available {
        return DesktopAppStoreHealth::Healthy;
    }
    match state {
        None => DesktopAppStoreHealth::Unproven,
        Some(_) => DesktopAppStoreHealth::Regressed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_is_recorded_and_keeps_the_first_timestamp() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_desktop_app_store_state(dir.path()), None);
        let first = record_desktop_app_store_success(dir.path(), 100.0, 1).unwrap();
        assert_eq!(first.first_success_at, 100.0);
        let second = record_desktop_app_store_success(dir.path(), 200.0, 1).unwrap();
        assert_eq!(second.first_success_at, 100.0);
        assert_eq!(second.last_success_at, 200.0);
        assert_eq!(read_desktop_app_store_state(dir.path()), Some(second));
        let written = std::fs::read_to_string(desktop_app_store_state_file(dir.path())).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&written).unwrap(),
            serde_json::json!({"version": 1, "firstSuccessAt": 100.0, "lastSuccessAt": 200.0, "lastSchemaVersion": 1.0})
        );
    }

    #[test]
    fn malformed_state_reads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        let file = desktop_app_store_state_file(dir.path());
        for contents in [
            "not json",
            "[]",
            r#"{"version":2,"firstSuccessAt":1,"lastSuccessAt":1}"#,
            r#"{"version":1,"firstSuccessAt":0,"lastSuccessAt":1}"#,
        ] {
            std::fs::write(&file, contents).unwrap();
            assert_eq!(read_desktop_app_store_state(dir.path()), None, "{contents}");
        }
    }

    #[test]
    fn an_unavailable_store_regresses_only_after_a_recorded_success() {
        let state = DesktopAppStoreState {
            version: 1,
            first_success_at: 1.0,
            last_success_at: 1.0,
            last_schema_version: 1.0,
        };
        assert_eq!(
            evaluate_desktop_app_store_health(true, Some(&state)),
            DesktopAppStoreHealth::Healthy
        );
        assert_eq!(
            evaluate_desktop_app_store_health(false, None),
            DesktopAppStoreHealth::Unproven
        );
        assert_eq!(
            evaluate_desktop_app_store_health(false, Some(&state)),
            DesktopAppStoreHealth::Regressed
        );
    }
}
