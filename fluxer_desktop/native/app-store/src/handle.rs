// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::accounts::{self, AccountCompareAndSwap, AccountEntry, KnownInstanceEntry};
use crate::import::{self, AccountImport, EntryImport, PruneRequest};
use crate::model::{OpenOutcome, Store, StoreError};
use crate::scoped_storage::{self, ScopedEntry};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const MAX_PAYLOAD_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug)]
pub enum AppStoreError {
    Store(StoreError),
    Closed,
    Poisoned,
    PayloadTooLarge { limit: usize },
}

impl fmt::Display for AppStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store(error) => error.fmt(f),
            Self::Closed => f.write_str("app store is closed"),
            Self::Poisoned => f.write_str("app store lock was poisoned by an earlier panic"),
            Self::PayloadTooLarge { limit } => {
                write!(f, "app store payload exceeds {limit} bytes")
            }
        }
    }
}

impl std::error::Error for AppStoreError {}

impl From<StoreError> for AppStoreError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

type Result<T> = std::result::Result<T, AppStoreError>;

#[derive(Serialize)]
struct Initialization<'a> {
    path: &'a str,
    #[serde(rename = "schemaVersion")]
    schema_version: i32,
    #[serde(rename = "previousSchemaVersion")]
    previous_schema_version: i32,
    #[serde(rename = "appliedMigrations")]
    applied_migrations: u32,
    #[serde(rename = "quarantinedPath")]
    quarantined_path: Option<&'a str>,
    #[serde(rename = "quarantineReason")]
    quarantine_reason: Option<&'a str>,
}

#[derive(Deserialize)]
struct ClearStoreExceptRequest {
    store: String,
    #[serde(rename = "keysToKeep")]
    keys_to_keep: Vec<String>,
}

#[derive(Clone)]
pub struct AppStoreHandle {
    store: Arc<Mutex<Option<Store>>>,
    initialization: String,
}

impl AppStoreHandle {
    pub fn open(path: PathBuf, derivation_version: Option<&str>) -> Result<Self> {
        let store = Store::open(path, derivation_version)?;
        let initialization = describe(store.path().to_string_lossy().as_ref(), store.outcome())?;
        Ok(Self {
            store: Arc::new(Mutex::new(Some(store))),
            initialization,
        })
    }

    pub fn initialization(&self) -> &str {
        &self.initialization
    }

    pub fn close(&self) -> Result<()> {
        let mut guard = self.store.lock().map_err(|_| AppStoreError::Poisoned)?;
        *guard = None;
        Ok(())
    }

    fn with_store<T>(&self, job: impl FnOnce(&mut Store) -> Result<T>) -> Result<T> {
        let mut guard = self.store.lock().map_err(|_| AppStoreError::Poisoned)?;
        job(guard.as_mut().ok_or(AppStoreError::Closed)?)
    }

    pub fn get_metadata(&self, key: &str) -> Result<String> {
        self.with_store(|store| json(&store.metadata(key)?))
    }

    pub fn set_metadata(&self, key: &str, value: &str) -> Result<()> {
        self.with_store(|store| Ok(store.set_metadata(key, value)?))
    }

    pub fn get_all_accounts(&self) -> Result<String> {
        self.with_store(|store| json(&accounts::all_accounts(store.connection())?))
    }

    pub fn get_account(&self, storage_key: &str) -> Result<String> {
        self.with_store(|store| json(&accounts::account(store.connection(), storage_key)?))
    }

    pub fn upsert_account(&self, payload: &str) -> Result<()> {
        self.with_store(|store| {
            let entry: AccountEntry = parse(payload)?;
            Ok(accounts::upsert_account(store.connection(), &entry)?)
        })
    }

    pub fn compare_and_swap_account(&self, payload: &str) -> Result<bool> {
        self.with_store(|store| {
            let request: AccountCompareAndSwap = parse(payload)?;
            Ok(accounts::compare_and_swap_account(
                store.connection_mut(),
                &request,
            )?)
        })
    }

    pub fn delete_account(&self, storage_key: &str) -> Result<()> {
        self.with_store(|store| Ok(accounts::delete_account(store.connection(), storage_key)?))
    }

    pub fn import_accounts(&self, payload: &str) -> Result<String> {
        self.with_store(|store| {
            let request: AccountImport = parse(payload)?;
            json(&import::import_accounts(store, request)?)
        })
    }

    pub fn get_all_known_instances(&self) -> Result<String> {
        self.with_store(|store| json(&accounts::all_known_instances(store.connection())?))
    }

    pub fn upsert_known_instance(&self, payload: &str) -> Result<()> {
        self.with_store(|store| {
            let entry: KnownInstanceEntry = parse(payload)?;
            Ok(accounts::upsert_known_instance(store.connection(), &entry)?)
        })
    }

    pub fn delete_known_instance(&self, key: &str) -> Result<()> {
        self.with_store(|store| Ok(accounts::delete_known_instance(store.connection(), key)?))
    }

    pub fn get_entries(&self, store: &str, scope: &str) -> Result<String> {
        self.with_store(|app_store| {
            json(&scoped_storage::scope_entries(
                app_store.connection(),
                store,
                scope,
            )?)
        })
    }

    pub fn get_entry(&self, store: &str, scope: &str, key: &str) -> Result<String> {
        self.with_store(|app_store| {
            json(&scoped_storage::entry(
                app_store.connection(),
                store,
                scope,
                key,
            )?)
        })
    }

    pub fn set_entry(&self, payload: &str) -> Result<()> {
        self.with_store(|store| {
            let entry: ScopedEntry = parse(payload)?;
            Ok(scoped_storage::set_entry(store.connection(), &entry)?)
        })
    }

    pub fn delete_entry(&self, store: &str, scope: &str, key: &str) -> Result<()> {
        self.with_store(|app_store| {
            Ok(scoped_storage::delete_entry(
                app_store.connection(),
                store,
                scope,
                key,
            )?)
        })
    }

    pub fn clear_scope(&self, scope: &str) -> Result<()> {
        self.with_store(|store| Ok(scoped_storage::clear_scope(store.connection(), scope)?))
    }

    pub fn clear_store_except(&self, payload: &str) -> Result<()> {
        self.with_store(|store| {
            let request: ClearStoreExceptRequest = parse(payload)?;
            Ok(scoped_storage::clear_store_except(
                store.connection(),
                &request.store,
                &request.keys_to_keep,
            )?)
        })
    }

    pub fn import_entries(&self, payload: &str) -> Result<String> {
        self.with_store(|store| {
            let request: EntryImport = parse(payload)?;
            json(&import::import_entries(store, request)?)
        })
    }

    pub fn prune(&self, payload: &str) -> Result<String> {
        self.with_store(|store| {
            let request: PruneRequest = parse(payload)?;
            json(&import::prune(store, request)?)
        })
    }
}

fn describe(path: &str, outcome: &OpenOutcome) -> Result<String> {
    json(&Initialization {
        path,
        schema_version: outcome.schema_version,
        previous_schema_version: outcome.previous_schema_version,
        applied_migrations: outcome.applied_migrations,
        quarantined_path: outcome.quarantined_path.as_deref(),
        quarantine_reason: outcome.quarantine_reason,
    })
}

fn json<T: Serialize>(value: &T) -> Result<String> {
    Ok(serde_json::to_string(value).map_err(StoreError::from)?)
}

fn parse<T: DeserializeOwned>(payload: &str) -> Result<T> {
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err(AppStoreError::PayloadTooLarge {
            limit: MAX_PAYLOAD_BYTES,
        });
    }
    Ok(serde_json::from_str(payload).map_err(StoreError::from)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("fluxer_app_store_{name}_{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            Self(dir)
        }

        fn db(&self) -> PathBuf {
            self.0.join("app-store.sqlite")
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(Path::new(&self.0));
        }
    }

    #[test]
    fn account_roundtrip() {
        let dir = TempDir::new("roundtrip");
        let handle = AppStoreHandle::open(dir.db(), None).unwrap();
        let initialization: serde_json::Value =
            serde_json::from_str(handle.initialization()).unwrap();
        assert!(initialization["schemaVersion"].is_number());

        let payload = r#"{"storageKey":"acct-1","record":{"userId":"42","lastActive":7}}"#;
        handle.upsert_account(payload).unwrap();
        let fetched: serde_json::Value =
            serde_json::from_str(&handle.get_account("acct-1").unwrap()).unwrap();
        assert_eq!(fetched["storageKey"], "acct-1");
        assert_eq!(fetched["record"]["userId"], "42");
        let all: serde_json::Value =
            serde_json::from_str(&handle.get_all_accounts().unwrap()).unwrap();
        assert_eq!(all.as_array().unwrap().len(), 1);

        let clone = handle.clone();
        clone.delete_account("acct-1").unwrap();
        assert_eq!(handle.get_account("acct-1").unwrap(), "null");
    }

    #[test]
    fn oversized_payload_is_refused() {
        let dir = TempDir::new("oversize");
        let handle = AppStoreHandle::open(dir.db(), None).unwrap();
        let payload = " ".repeat(MAX_PAYLOAD_BYTES + 1);
        let error = handle.upsert_account(&payload).unwrap_err();
        assert!(matches!(error, AppStoreError::PayloadTooLarge { .. }));
        assert_eq!(
            error.to_string(),
            format!("app store payload exceeds {MAX_PAYLOAD_BYTES} bytes")
        );
    }

    #[test]
    fn calls_after_close_report_closed() {
        let dir = TempDir::new("closed");
        let handle = AppStoreHandle::open(dir.db(), None).unwrap();
        let clone = handle.clone();
        handle.close().unwrap();
        let error = clone.get_all_accounts().unwrap_err();
        assert!(matches!(error, AppStoreError::Closed));
        assert_eq!(error.to_string(), "app store is closed");
    }

    #[test]
    fn handle_is_send_sync() {
        fn assert_send_sync<T: Send + Sync + Clone>() {}
        assert_send_sync::<AppStoreHandle>();
    }
}
