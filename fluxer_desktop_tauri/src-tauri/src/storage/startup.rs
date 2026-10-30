// SPDX-License-Identifier: AGPL-3.0-or-later

//! The app-store start-up of fluxer_desktop/src/main/index.ts: open, judge health, record
//! telemetry, recover, and log the legacy import state.

use std::path::Path;
use std::sync::Arc;

use serde_json::Value;

use super::health::{
    DesktopAppStoreHealth, desktop_app_store_state_file, evaluate_desktop_app_store_health,
    read_desktop_app_store_state, record_desktop_app_store_success,
};
use super::service::{DesktopAppStorage, unix_millis};
use super::telemetry::{
    DesktopAccountStoreEvent, DesktopStorePermissionState, probe_desktop_store_permissions,
    record_desktop_account_store_event,
};
use crate::dialog::{self, MessageBoxOptions};
use crate::error::BridgeError;
use crate::i18n::MainI18n;

const DESKTOP_LEGACY_IMPORT_MARKER_KEY: &str = "legacy_import.v1";
const DESKTOP_LEGACY_IMPORT_FAILURE_MARKER_KEY: &str = "legacy_import.failures";
const DESKTOP_LEGACY_SESSION_MARKER_KEY: &str = "legacy_session.reconciled_at";
const DESKTOP_LEGACY_IMPORT_VERSION: f64 = 1.0;
const DESKTOP_LEGACY_IMPORT_PHASES: [&str; 4] = ["accounts", "entries", "session", "done"];
const DESKTOP_LEGACY_IMPORT_PHASE_DONE: &str = "done";

/// Opens the store under `user_data_path`. A store that worked here before and no longer
/// opens ends the process after a notice. Runs on the main thread, which the notice needs.
pub fn open_desktop_app_storage(user_data_path: &Path, i18n: &MainI18n) -> Arc<DesktopAppStorage> {
    let (storage, opened) = DesktopAppStorage::open(user_data_path);
    let status = storage.status();
    let state = read_desktop_app_store_state(user_data_path);
    let health = evaluate_desktop_app_store_health(status.available, state.as_ref());
    tracing::info!(
        available = status.available,
        schemaVersion = status.schema_version,
        storeFile = %opened.store_file.display(),
        quarantineReason = ?status.quarantine_reason,
        quarantinedFile = ?opened.quarantined_file,
        unavailableReason = ?status.unavailable_reason,
        health = health.as_str(),
        "[AppStore] Desktop app store opened"
    );
    if !status.available {
        record_desktop_account_store_event(DesktopAccountStoreEvent::FallbackToWeb);
        if health == DesktopAppStoreHealth::Regressed {
            report_unreadable_desktop_app_store(
                &opened.store_file,
                status.unavailable_reason.as_deref(),
                i18n,
            );
        }
    }
    if status.quarantined {
        record_desktop_account_store_event(DesktopAccountStoreEvent::Quarantined);
    }
    if probe_desktop_store_permissions(&opened.store_file) == DesktopStorePermissionState::Widened {
        record_desktop_account_store_event(DesktopAccountStoreEvent::PermissionHardeningFailed);
        tracing::warn!(
            storeFile = %opened.store_file.display(),
            "[AppStore] The account store is readable beyond this user and permission hardening did not hold"
        );
    }
    let recovery = tauri::async_runtime::block_on(storage.recover());
    if recovery.reseed_required {
        record_desktop_account_store_event(DesktopAccountStoreEvent::Quarantined);
        tracing::warn!(
            quarantineReason = ?recovery.status.quarantine_reason,
            "[AppStore] The previous store was quarantined and the renderer re-seeds the fresh one"
        );
    }
    tauri::async_runtime::block_on(record_desktop_app_store_import_state(
        &storage,
        user_data_path,
        status.schema_version,
    ));
    Arc::new(storage)
}

/// `reportUnreadableDesktopAppStore`: an error box with Quit and Show Store Folder (which
/// reveals the store file), then exit.
fn report_unreadable_desktop_app_store(
    store_file: &Path,
    reason: Option<&str>,
    i18n: &MainI18n,
) -> ! {
    tracing::error!(
        storeFile = %store_file.display(),
        reason,
        "[AppStore] The desktop app store worked here before and cannot be opened now"
    );
    let details =
        reason.map(|reason| i18n.t("desktop.appStore.unreadableDetails", &[("reason", reason)]));
    let detail = [
        Some(i18n.t("desktop.appStore.unreadableUnchanged", &[])),
        Some(i18n.t("desktop.appStore.unreadableAdvice", &[])),
        details,
    ]
    .into_iter()
    .flatten()
    .filter(|paragraph| !paragraph.is_empty())
    .collect::<Vec<_>>()
    .join("\n\n");
    let choice = dialog::show_message_box_sync(&MessageBoxOptions {
        title: i18n.t("desktop.appStore.unreadableTitle", &[]),
        message: i18n.t("desktop.appStore.unreadableMessage", &[]),
        detail,
        buttons: vec![
            i18n.t("desktop.appMenu.quit", &[]),
            i18n.t("desktop.appStore.showStoreFolder", &[]),
        ],
        default_id: 0,
        cancel_id: 0,
    });
    if choice == 1
        && let Err(error) = tauri_plugin_opener::reveal_item_in_dir(store_file)
    {
        tracing::error!(%error, "[AppStore] Failed to reveal the store folder");
    }
    std::process::exit(1);
}

async fn record_desktop_app_store_import_state(
    storage: &DesktopAppStorage,
    user_data_path: &Path,
    schema_version: i64,
) {
    let read = async {
        let phase = read_desktop_legacy_import_phase(
            read_marker(storage, DESKTOP_LEGACY_IMPORT_MARKER_KEY)
                .await?
                .as_deref(),
        );
        let failures = read_marker(storage, DESKTOP_LEGACY_IMPORT_FAILURE_MARKER_KEY).await?;
        let reconciled_at = read_marker(storage, DESKTOP_LEGACY_SESSION_MARKER_KEY).await?;
        Ok::<_, BridgeError>((phase, failures, reconciled_at))
    };
    match read.await {
        Ok((phase, failures, reconciled_at)) => {
            tracing::info!(
                phase,
                failures,
                reconciledAt = reconciled_at,
                stateFile = %desktop_app_store_state_file(user_data_path).display(),
                "[AppStore] Legacy import state"
            );
            if phase == Some(DESKTOP_LEGACY_IMPORT_PHASE_DONE) {
                record_desktop_app_store_success(user_data_path, unix_millis(), schema_version);
                record_desktop_account_store_event(DesktopAccountStoreEvent::Migrated);
            }
        }
        Err(error) => {
            tracing::warn!(%error, "[AppStore] Failed to read the legacy import state");
        }
    }
}

async fn read_marker(
    storage: &DesktopAppStorage,
    key: &str,
) -> Result<Option<String>, BridgeError> {
    storage.get_marker_value(&Value::from(key)).await
}

/// `readDesktopLegacyImportPhase` in packages/desktop_ipc/src/StorageContract.ts.
fn read_desktop_legacy_import_phase(value: Option<&str>) -> Option<&'static str> {
    let parsed: Value = serde_json::from_str(value?).ok()?;
    if parsed.get("version").and_then(Value::as_f64) != Some(DESKTOP_LEGACY_IMPORT_VERSION) {
        return None;
    }
    let phase = parsed.get("phase")?.as_str()?;
    DESKTOP_LEGACY_IMPORT_PHASES
        .into_iter()
        .find(|candidate| *candidate == phase)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_import_phase_needs_version_one_and_a_known_phase() {
        assert_eq!(read_desktop_legacy_import_phase(None), None);
        assert_eq!(read_desktop_legacy_import_phase(Some("not json")), None);
        assert_eq!(
            read_desktop_legacy_import_phase(Some(r#"{"version":2,"phase":"done"}"#)),
            None
        );
        assert_eq!(
            read_desktop_legacy_import_phase(Some(r#"{"version":1,"phase":"later"}"#)),
            None
        );
        assert_eq!(
            read_desktop_legacy_import_phase(Some(r#"{"version":1,"phase":"done","updatedAt":3}"#)),
            Some("done")
        );
    }

    #[test]
    fn a_finished_legacy_import_records_store_health() {
        let dir = tempfile::tempdir().unwrap();
        let i18n = MainI18n::load(dir.path());
        let storage = open_desktop_app_storage(dir.path(), &i18n);
        assert!(read_desktop_app_store_state(dir.path()).is_none());
        tauri::async_runtime::block_on(storage.set_marker_value(
            &Value::from(DESKTOP_LEGACY_IMPORT_MARKER_KEY),
            &Value::from(r#"{"version":1,"phase":"done","updatedAt":1}"#),
        ))
        .unwrap();
        drop(storage);
        let reopened = open_desktop_app_storage(dir.path(), &i18n);
        let state = read_desktop_app_store_state(dir.path()).expect("health state recorded");
        assert_eq!(
            state.last_schema_version,
            reopened.status().schema_version as f64
        );
    }
}
