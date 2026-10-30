// SPDX-License-Identifier: AGPL-3.0-or-later

//! The request methods of DesktopAppStorage.ts: validate the payload, then run one queued
//! operation against the native store and shape its JSON the way the TS returned it.

use std::collections::HashSet;
use std::path::PathBuf;

use fluxer_app_store::AppStoreError;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::errors;
use super::private_file;
use super::service::DesktopAppStorage;
use super::validation::{
    AccountRecord, BATCH_ENTRIES_MAX, BATCH_KEYS_MAX, BATCH_RECORDS_MAX, ENTRY_VALUE_MAX_BYTES,
    MARKER_VALUE_MAX_BYTES, field, is_known_instance_record, record_bytes, require_account_record,
    require_array, require_boolean, require_bounded_string, require_finite_number,
    require_known_instance_record, require_lookup_key, require_marker, require_record,
};
use crate::error::BridgeError;

const ENTRY_STORE: &str = "app";
const DESKTOP_LEGACY_AUTHORITY_MARKER_KEY: &str = "legacy_import.authority";
const DESKTOP_LEGACY_AUTHORITY_MARKER_VALUE: &str = "committed.v1";

type Checked<T> = Result<T, BridgeError>;

/// `DesktopStorageEntry`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageEntry {
    pub key: String,
    pub value: String,
    pub updated_at: f64,
}

#[derive(Deserialize)]
struct StoredKnownInstance {
    record: Value,
}

#[derive(Deserialize)]
struct SkippedRecord {
    key: String,
    reason: String,
}

#[derive(Deserialize)]
struct EntryImportReport {
    skipped: Vec<SkippedRecord>,
}

struct PendingEntryWrite {
    scope: String,
    key: String,
    value: String,
    if_absent: bool,
}

/// The native store's JSON output, as the TS boundary's `JSON.parse` read it.
fn decode<T: DeserializeOwned>(output: Result<String, AppStoreError>) -> Checked<T> {
    serde_json::from_str(&output.map_err(errors::native)?).map_err(errors::native)
}

fn written(output: Result<(), AppStoreError>) -> Checked<()> {
    output.map_err(errors::native)
}

fn lookup_keys(value: &Value, context: &str, max: usize) -> Checked<Vec<String>> {
    require_array(value, context, max)?
        .iter()
        .enumerate()
        .map(|(index, key)| {
            require_lookup_key(key, &format!("{context}[{index}]")).map(str::to_owned)
        })
        .collect()
}

fn present_key(scope: &str, key: &str) -> String {
    format!("{scope}\0{key}")
}

impl DesktopAppStorage {
    pub async fn get_all_accounts(&self) -> Checked<Vec<AccountRecord>> {
        self.run(0, |handle, _| {
            let accounts: Vec<AccountRecord> = decode(handle.get_all_accounts())?;
            Ok(accounts
                .into_iter()
                .filter(|account| account.record.is_object())
                .collect())
        })
        .await
    }

    pub async fn get_account(&self, storage_key: &Value) -> Checked<Option<AccountRecord>> {
        let key = require_lookup_key(storage_key, "account storage key")?.to_owned();
        self.run(key.len(), move |handle, _| {
            let account: Option<AccountRecord> = decode(handle.get_account(&key))?;
            Ok(account.filter(|account| account.record.is_object()))
        })
        .await
    }

    pub async fn upsert_account(&self, account: &Value) -> Checked<()> {
        let entry = require_account_record(account, "account")?;
        let bytes = record_bytes(&entry.storage_key, &entry.record);
        let payload = json!(entry).to_string();
        self.run(bytes, move |handle, _| {
            written(handle.upsert_account(&payload))
        })
        .await
    }

    pub async fn compare_and_swap_account(&self, request: &Value) -> Checked<bool> {
        let context = "account compare-and-swap request";
        let payload = require_record(request, context)?;
        let expected =
            require_account_record(field(payload, "expected"), &format!("{context}.expected"))?;
        let replacement = require_account_record(
            field(payload, "replacement"),
            &format!("{context}.replacement"),
        )?;
        if expected.storage_key != replacement.storage_key {
            return Err(errors::request(
                context,
                "must use the same storageKey for expected and replacement records",
            ));
        }
        let bytes = record_bytes(&expected.storage_key, &expected.record)
            + record_bytes(&replacement.storage_key, &replacement.record);
        let payload = json!({"expected": expected, "replacement": replacement}).to_string();
        self.run(bytes, move |handle, _| {
            handle
                .compare_and_swap_account(&payload)
                .map_err(errors::native)
        })
        .await
    }

    pub async fn delete_account(&self, storage_key: &Value) -> Checked<()> {
        let key = require_lookup_key(storage_key, "account storage key")?.to_owned();
        self.run(key.len(), move |handle, _| {
            written(handle.delete_account(&key))
        })
        .await
    }

    pub async fn import_accounts(&self, request: &Value) -> Checked<Value> {
        let payload = require_record(request, "account import request")?;
        let records = require_array(
            field(payload, "records"),
            "account import request.records",
            BATCH_RECORDS_MAX,
        )?
        .iter()
        .enumerate()
        .map(|(index, value)| {
            require_account_record(value, &format!("account import request.records[{index}]"))
        })
        .collect::<Checked<Vec<_>>>()?;
        let marker = require_marker(field(payload, "marker"), "account import request.marker")?;
        let bytes = records
            .iter()
            .map(|record| record_bytes(&record.storage_key, &record.record))
            .sum();
        let payload = json!({"records": records, "marker": marker}).to_string();
        self.run(bytes, move |handle, _| {
            decode(handle.import_accounts(&payload))
        })
        .await
    }

    pub async fn prune_accounts(&self, request: &Value) -> Checked<Value> {
        let payload = require_record(request, "account prune request")?;
        let known_storage_keys = lookup_keys(
            field(payload, "knownStorageKeys"),
            "account prune request.knownStorageKeys",
            BATCH_KEYS_MAX,
        )?;
        let list_is_authoritative = require_boolean(
            field(payload, "listIsAuthoritative"),
            "account prune request.listIsAuthoritative",
        )?;
        let bytes = known_storage_keys.iter().map(String::len).sum();
        let payload = json!({
            "knownStorageKeys": known_storage_keys,
            "listIsAuthoritative": list_is_authoritative,
        })
        .to_string();
        self.run(bytes, move |handle, _| decode(handle.prune(&payload)))
            .await
    }

    pub async fn load_entries(&self, scope: &Value) -> Checked<Vec<StorageEntry>> {
        let scope = require_lookup_key(scope, "storage scope")?.to_owned();
        self.run(scope.len(), move |handle, clock| {
            let entries: Vec<StorageEntry> = decode(handle.get_entries(ENTRY_STORE, &scope))?;
            for entry in &entries {
                clock.observe(entry.updated_at);
            }
            Ok(entries)
        })
        .await
    }

    pub async fn get_entry(&self, scope: &Value, key: &Value) -> Checked<Option<StorageEntry>> {
        let scope = require_lookup_key(scope, "storage scope")?.to_owned();
        let key = require_lookup_key(key, "storage key")?.to_owned();
        self.run(scope.len() + key.len(), move |handle, clock| {
            let entry: Option<StorageEntry> = decode(handle.get_entry(ENTRY_STORE, &scope, &key))?;
            if let Some(entry) = &entry {
                clock.observe(entry.updated_at);
            }
            Ok(entry)
        })
        .await
    }

    pub async fn set_entry(&self, scope: &Value, key: &Value, value: &Value) -> Checked<()> {
        let scope = require_lookup_key(scope, "storage scope")?.to_owned();
        let key = require_lookup_key(key, "storage key")?.to_owned();
        let value =
            require_bounded_string(value, "storage value", ENTRY_VALUE_MAX_BYTES)?.to_owned();
        self.run(
            scope.len() + key.len() + value.len(),
            move |handle, clock| {
                let payload = json!({
                    "store": ENTRY_STORE,
                    "scope": scope,
                    "key": key,
                    "value": value,
                    "updatedAt": clock.next(),
                });
                written(handle.set_entry(&payload.to_string()))
            },
        )
        .await
    }

    pub async fn delete_entry(&self, scope: &Value, key: &Value) -> Checked<()> {
        let scope = require_lookup_key(scope, "storage scope")?.to_owned();
        let key = require_lookup_key(key, "storage key")?.to_owned();
        self.run(scope.len() + key.len(), move |handle, _| {
            written(handle.delete_entry(ENTRY_STORE, &scope, &key))
        })
        .await
    }

    pub async fn clear_all_for_scope(&self, scope: &Value) -> Checked<()> {
        let scope = require_lookup_key(scope, "storage scope")?.to_owned();
        self.run(scope.len(), move |handle, _| {
            written(handle.clear_scope(&scope))
        })
        .await
    }

    pub async fn clear_all_except(&self, keys_to_keep: &Value) -> Checked<()> {
        let keys = lookup_keys(keys_to_keep, "storage retained keys", BATCH_KEYS_MAX)?;
        let bytes = keys.iter().map(String::len).sum();
        let payload = json!({"store": ENTRY_STORE, "keysToKeep": keys}).to_string();
        self.run(bytes, move |handle, _| {
            written(handle.clear_store_except(&payload))
        })
        .await
    }

    pub async fn set_many_entries(&self, writes: &Value) -> Checked<()> {
        let requested = require_array(writes, "storage writes", BATCH_ENTRIES_MAX)?
            .iter()
            .enumerate()
            .map(|(index, value)| {
                let context = format!("storage writes[{index}]");
                let record = require_record(value, &context)?;
                Ok(PendingEntryWrite {
                    scope: require_lookup_key(field(record, "scope"), &format!("{context}.scope"))?
                        .to_owned(),
                    key: require_lookup_key(field(record, "key"), &format!("{context}.key"))?
                        .to_owned(),
                    value: require_bounded_string(
                        field(record, "value"),
                        &format!("{context}.value"),
                        ENTRY_VALUE_MAX_BYTES,
                    )?
                    .to_owned(),
                    if_absent: field(record, "ifAbsent") == &Value::Bool(true),
                })
            })
            .collect::<Checked<Vec<_>>>()?;
        let bytes = requested
            .iter()
            .map(|write| write.scope.len() + write.key.len() + write.value.len())
            .sum();
        self.run(bytes, move |handle, clock| {
            let mut present = HashSet::new();
            let scopes: HashSet<&str> = requested
                .iter()
                .filter(|write| write.if_absent)
                .map(|write| write.scope.as_str())
                .collect();
            for scope in scopes {
                let entries: Vec<StorageEntry> = decode(handle.get_entries(ENTRY_STORE, scope))?;
                present.extend(entries.iter().map(|entry| present_key(scope, &entry.key)));
            }
            let entries: Vec<Value> = requested
                .into_iter()
                .filter(|write| {
                    !(write.if_absent && present.contains(&present_key(&write.scope, &write.key)))
                })
                .map(|write| {
                    json!({
                        "store": ENTRY_STORE,
                        "scope": write.scope,
                        "key": write.key,
                        "value": write.value,
                        "updatedAt": clock.next(),
                    })
                })
                .collect();
            if entries.is_empty() {
                return Ok(());
            }
            let report: EntryImportReport =
                decode(handle.import_entries(&json!({"entries": entries}).to_string()))?;
            if report.skipped.is_empty() {
                return Ok(());
            }
            let refusals: Vec<String> = report
                .skipped
                .iter()
                .map(|entry| format!("{}: {}", entry.key, entry.reason))
                .collect();
            Err(errors::write_refused(&refusals))
        })
        .await
    }

    pub async fn import_entries(&self, request: &Value) -> Checked<Value> {
        let payload = require_record(request, "storage import request")?;
        let entries = require_array(
            field(payload, "entries"),
            "storage import request.entries",
            BATCH_ENTRIES_MAX,
        )?
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let context = format!("storage import request.entries[{index}]");
            let record = require_record(value, &context)?;
            let scope = require_lookup_key(field(record, "scope"), &format!("{context}.scope"))?;
            let key = require_lookup_key(field(record, "key"), &format!("{context}.key"))?;
            let value = require_bounded_string(
                field(record, "value"),
                &format!("{context}.value"),
                ENTRY_VALUE_MAX_BYTES,
            )?;
            let updated_at =
                require_finite_number(field(record, "updatedAt"), &format!("{context}.updatedAt"))?;
            Ok((
                scope.to_owned(),
                key.to_owned(),
                value.to_owned(),
                updated_at,
            ))
        })
        .collect::<Checked<Vec<_>>>()?;
        let marker = require_marker(field(payload, "marker"), "storage import request.marker")?;
        let bytes = entries
            .iter()
            .map(|(scope, key, value, _)| scope.len() + key.len() + value.len())
            .sum();
        self.run(bytes, move |handle, clock| {
            for (_, _, _, updated_at) in &entries {
                clock.observe(*updated_at);
            }
            let entries: Vec<Value> = entries
                .into_iter()
                .map(|(scope, key, value, updated_at)| {
                    json!({
                        "store": ENTRY_STORE,
                        "scope": scope,
                        "key": key,
                        "value": value,
                        "updatedAt": updated_at,
                    })
                })
                .collect();
            let payload = json!({"entries": entries, "marker": marker});
            decode(handle.import_entries(&payload.to_string()))
        })
        .await
    }

    pub async fn get_marker_value(&self, key: &Value) -> Checked<Option<String>> {
        let key = require_lookup_key(key, "store marker key")?.to_owned();
        self.run(key.len(), move |handle, _| {
            decode(handle.get_metadata(&key))
        })
        .await
    }

    pub async fn set_marker_value(&self, key: &Value, value: &Value) -> Checked<()> {
        let key = require_lookup_key(key, "store marker key")?.to_owned();
        let value =
            require_bounded_string(value, "store marker value", MARKER_VALUE_MAX_BYTES)?.to_owned();
        let authority = key == DESKTOP_LEGACY_AUTHORITY_MARKER_KEY;
        let committed = value == DESKTOP_LEGACY_AUTHORITY_MARKER_VALUE;
        let cleared = value.is_empty();
        self.run(key.len() + value.len(), move |handle, _| {
            written(handle.set_metadata(&key, &value))
        })
        .await?;
        if !authority {
            return Ok(());
        }
        if committed {
            let sentinel = self.authority_sentinel().to_owned();
            blocking_file_op(move || private_file::write(&sentinel, "1\n")).await?;
            self.set_authority_expected(true);
            return Ok(());
        }
        if cleared {
            let sentinel = self.authority_sentinel().to_owned();
            blocking_file_op(move || remove_if_present(sentinel)).await?;
            self.set_authority_expected(false);
        }
        Ok(())
    }

    /// Marker read for shell code (runtime discovery). Like the Electron callers that skipped a
    /// missing store, a store that never opened reads as no marker.
    pub async fn get_marker(&self, key: &str) -> Checked<Option<String>> {
        if !self.is_open() {
            return Ok(None);
        }
        self.get_marker_value(&Value::String(key.to_owned())).await
    }

    /// Marker write for shell code; a store that never opened skips the write.
    pub async fn set_marker(&self, key: &str, value: &str) -> Checked<()> {
        if !self.is_open() {
            return Ok(());
        }
        self.set_marker_value(
            &Value::String(key.to_owned()),
            &Value::String(value.to_owned()),
        )
        .await
    }

    pub async fn get_all_known_instances(&self) -> Checked<Vec<Value>> {
        self.run(0, |handle, _| {
            let stored: Vec<StoredKnownInstance> = decode(handle.get_all_known_instances())?;
            Ok(stored
                .into_iter()
                .map(|instance| instance.record)
                .filter(is_known_instance_record)
                .collect())
        })
        .await
    }

    pub async fn upsert_known_instance(&self, instance: &Value) -> Checked<()> {
        let key = require_known_instance_record(instance, "known instance")?.to_owned();
        let bytes = record_bytes(&key, instance);
        let payload = json!({"key": key, "record": instance}).to_string();
        self.run(bytes, move |handle, _| {
            written(handle.upsert_known_instance(&payload))
        })
        .await
    }

    pub async fn delete_known_instance(&self, instance_key: &Value) -> Checked<()> {
        let key = require_lookup_key(instance_key, "known instance key")?.to_owned();
        self.run(key.len(), move |handle, _| {
            written(handle.delete_known_instance(&key))
        })
        .await
    }
}

async fn blocking_file_op(
    operation: impl FnOnce() -> std::io::Result<()> + Send + 'static,
) -> Checked<()> {
    tauri::async_runtime::spawn_blocking(operation)
        .await
        .map_err(errors::native)?
        .map_err(errors::native)
}

/// `fs.rm(path, {force: true})`.
fn remove_if_present(path: PathBuf) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::service::tests::open_in;
    use tauri::async_runtime::block_on;

    fn rejection<T: std::fmt::Debug>(result: Checked<T>) -> BridgeError {
        result.unwrap_err()
    }

    #[test]
    fn accounts_round_trip_through_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let storage = open_in(dir.path());
        let account = json!({"storageKey": "acct-1", "record": {"userId": "42", "lastActive": 7, "z": 1, "a": 2}});
        block_on(storage.upsert_account(&account)).unwrap();
        let stored = block_on(storage.get_account(&json!("acct-1")))
            .unwrap()
            .unwrap();
        assert_eq!(json!(stored), account);
        let keys: Vec<&String> = stored.record.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            ["userId", "lastActive", "z", "a"],
            "record key order is kept"
        );
        assert_eq!(
            block_on(storage.get_all_accounts()).unwrap(),
            vec![stored.clone()]
        );
        assert_eq!(
            block_on(storage.get_account(&json!("missing"))).unwrap(),
            None
        );

        let replacement =
            json!({"storageKey": "acct-1", "record": {"userId": "42", "lastActive": 8}});
        let stale = json!({"expected": replacement, "replacement": replacement});
        assert!(!block_on(storage.compare_and_swap_account(&stale)).unwrap());
        let swap = json!({"expected": account, "replacement": replacement});
        assert!(block_on(storage.compare_and_swap_account(&swap)).unwrap());
        let mismatched =
            json!({"expected": account, "replacement": {"storageKey": "acct-2", "record": {}}});
        assert_eq!(
            rejection(block_on(storage.compare_and_swap_account(&mismatched))).message(),
            "Desktop app store request is invalid: account compare-and-swap request must use the same storageKey for expected and replacement records"
        );

        let report = block_on(storage.import_accounts(&json!({
            "records": [{"storageKey": "acct-2", "record": {"userId": "7"}}],
            "marker": {"key": "legacy_import.v1", "value": "accounts"},
        })))
        .unwrap();
        assert_eq!(report["imported"], 1);
        assert_eq!(report["skipped"], json!([]));
        assert_eq!(
            block_on(storage.get_marker_value(&json!("legacy_import.v1")))
                .unwrap()
                .as_deref(),
            Some("accounts")
        );

        let pruned = block_on(storage.prune_accounts(&json!({
            "knownStorageKeys": ["acct-1"],
            "listIsAuthoritative": true,
        })))
        .unwrap();
        assert_eq!(pruned, json!({"pruned": ["acct-2"], "refusedReason": null}));
        block_on(storage.delete_account(&json!("acct-1"))).unwrap();
        assert_eq!(block_on(storage.get_all_accounts()).unwrap(), vec![]);
    }

    #[test]
    fn entries_round_trip_with_increasing_stamps() {
        let dir = tempfile::tempdir().unwrap();
        let storage = open_in(dir.path());
        block_on(storage.set_entry(&json!("acct-1"), &json!("k1"), &json!("v1"))).unwrap();
        block_on(storage.set_entry(&json!("acct-1"), &json!("k2"), &json!("v2"))).unwrap();
        let first = block_on(storage.get_entry(&json!("acct-1"), &json!("k1")))
            .unwrap()
            .unwrap();
        let loaded = block_on(storage.load_entries(&json!("acct-1"))).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!((first.key.as_str(), first.value.as_str()), ("k1", "v1"));
        assert!(
            loaded
                .iter()
                .all(|entry| entry.key != "k2" || entry.updated_at > first.updated_at)
        );

        block_on(storage.set_many_entries(&json!([
            {"scope": "acct-1", "key": "k1", "value": "ignored", "ifAbsent": true},
            {"scope": "acct-1", "key": "k3", "value": "v3", "ifAbsent": true},
            {"scope": "acct-2", "key": "k1", "value": "other"},
        ])))
        .unwrap();
        let value = |scope: &str, key: &str| {
            block_on(storage.get_entry(&json!(scope), &json!(key)))
                .unwrap()
                .map(|entry| entry.value)
        };
        assert_eq!(value("acct-1", "k1").as_deref(), Some("v1"));
        assert_eq!(value("acct-1", "k3").as_deref(), Some("v3"));
        assert_eq!(value("acct-2", "k1").as_deref(), Some("other"));

        let report = block_on(storage.import_entries(&json!({
            "entries": [{"scope": "acct-3", "key": "k", "value": "v", "updatedAt": 5}],
            "marker": null,
        })))
        .unwrap();
        assert_eq!(report, json!({"imported": 1, "skipped": []}));
        let imported = block_on(storage.get_entry(&json!("acct-3"), &json!("k")))
            .unwrap()
            .unwrap();
        assert_eq!(imported.updated_at, 5.0);

        block_on(storage.delete_entry(&json!("acct-1"), &json!("k3"))).unwrap();
        assert_eq!(value("acct-1", "k3"), None);
        block_on(storage.clear_all_for_scope(&json!("acct-2"))).unwrap();
        assert_eq!(value("acct-2", "k1"), None);
        block_on(storage.clear_all_except(&json!(["k2"]))).unwrap();
        let remaining = block_on(storage.load_entries(&json!("acct-1"))).unwrap();
        assert_eq!(
            remaining
                .iter()
                .map(|entry| entry.key.as_str())
                .collect::<Vec<_>>(),
            ["k2"]
        );
    }

    #[test]
    fn invalid_requests_are_refused_before_the_store_is_touched() {
        let dir = tempfile::tempdir().unwrap();
        let storage = open_in(dir.path());
        let error = rejection(block_on(
            storage.set_many_entries(&json!([{"scope": "s", "key": "", "value": "v"}])),
        ));
        assert_eq!(error.name(), "DesktopAppStoreRequestError");
        assert_eq!(
            error.message(),
            "Desktop app store request is invalid: storage writes[0].key must be a non-empty NUL-free string"
        );
        let error = rejection(block_on(storage.import_entries(
            &json!({"entries": [{"scope": "s", "key": "k", "value": "v"}]}),
        )));
        assert_eq!(
            error.message(),
            "Desktop app store request is invalid: storage import request.entries[0].updatedAt must be a finite number"
        );
        let error = rejection(block_on(storage.get_account(&Value::Null)));
        assert_eq!(
            error.message(),
            "Desktop app store request is invalid: account storage key must be a string"
        );
        let error = rejection(block_on(
            storage.prune_accounts(&json!({"knownStorageKeys": []})),
        ));
        assert_eq!(
            error.message(),
            "Desktop app store request is invalid: account prune request.listIsAuthoritative must be a boolean"
        );
    }

    #[test]
    fn markers_round_trip_and_the_authority_marker_drives_the_sentinel() {
        let dir = tempfile::tempdir().unwrap();
        let storage = open_in(dir.path());
        assert_eq!(
            block_on(storage.get_marker("runtime.discovery")).unwrap(),
            None
        );
        block_on(storage.set_marker("runtime.discovery", "{\"url\":\"x\"}")).unwrap();
        assert_eq!(
            block_on(storage.get_marker("runtime.discovery"))
                .unwrap()
                .as_deref(),
            Some("{\"url\":\"x\"}")
        );

        let sentinel = dir.path().join("desktop-app-authority-v1");
        block_on(storage.set_marker(
            DESKTOP_LEGACY_AUTHORITY_MARKER_KEY,
            DESKTOP_LEGACY_AUTHORITY_MARKER_VALUE,
        ))
        .unwrap();
        assert_eq!(std::fs::read_to_string(&sentinel).unwrap(), "1\n");
        assert!(storage.status().authority_expected);
        block_on(storage.set_marker(DESKTOP_LEGACY_AUTHORITY_MARKER_KEY, "")).unwrap();
        assert!(!sentinel.exists());
        assert!(!storage.status().authority_expected);
        block_on(storage.set_marker(DESKTOP_LEGACY_AUTHORITY_MARKER_KEY, "")).unwrap();

        let error = rejection(block_on(storage.set_marker("schema.version", "9")));
        assert_eq!(error.name(), "Error");
        assert_eq!(error.message(), "store metadata key is reserved");
    }

    #[test]
    fn a_store_that_never_opened_has_no_markers_but_refuses_requests() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("file");
        std::fs::write(&blocker, "").unwrap();
        let storage = open_in(&blocker);
        assert_eq!(block_on(storage.get_marker("k")).unwrap(), None);
        block_on(storage.set_marker("k", "v")).unwrap();
        let error = rejection(block_on(storage.get_marker_value(&json!("k"))));
        assert_eq!(error.name(), "DesktopAppStoreUnavailableError");
    }

    #[test]
    fn known_instances_round_trip_and_unusable_records_are_hidden() {
        let dir = tempfile::tempdir().unwrap();
        let storage = open_in(dir.path());
        let instance = json!({"instanceKey": "i1", "domain": "example.test", "displayName": "Example", "lastUsed": 3, "extra": true});
        block_on(storage.upsert_known_instance(&instance)).unwrap();
        assert_eq!(
            block_on(storage.get_all_known_instances()).unwrap(),
            vec![instance]
        );
        block_on(storage.delete_known_instance(&json!("i1"))).unwrap();
        assert_eq!(
            block_on(storage.get_all_known_instances()).unwrap(),
            Vec::<Value>::new()
        );
    }
}
