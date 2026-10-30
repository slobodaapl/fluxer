// SPDX-License-Identifier: AGPL-3.0-or-later

//! The lifecycle and operation queue of DesktopAppStorage.ts. Operations run one at a time in
//! arrival order on the blocking pool, within the same pending-count and pending-byte limits.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use fluxer_app_store::AppStoreHandle;
use serde::{Deserialize, Serialize};

use super::errors;
use super::health::desktop_app_store_state_file;
use crate::error::BridgeError;

const DESKTOP_APP_STORE_FILE_NAME: &str = "desktop-app-store.sqlite3";
const DESKTOP_APP_STORE_DERIVATION_VERSION: &str = "1";
const DESKTOP_AUTHORITY_SENTINEL_FILE_NAME: &str = "desktop-app-authority-v1";
const SCHEMA_VERSION_MARKER: &str = "schema.version";

const PENDING_OPERATIONS_MAX: usize = 64;
const PENDING_INPUT_BYTES_MAX: usize = 128 * 1024 * 1024;

/// `DesktopStoreStatus`.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreStatus {
    pub available: bool,
    pub authority_expected: bool,
    pub schema_version: i64,
    pub quarantined: bool,
    pub quarantine_reason: Option<String>,
    pub unavailable_reason: Option<String>,
}

/// `DesktopAppStorageOpenResult`, without the status the storage already carries.
pub(super) struct OpenedStore {
    pub store_file: PathBuf,
    pub quarantined_file: Option<String>,
}

pub(super) struct Recovery {
    pub status: StoreStatus,
    pub reseed_required: bool,
}

/// The `initialization` JSON of the native store.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Initialization {
    schema_version: i64,
    quarantined_path: Option<String>,
    quarantine_reason: Option<String>,
}

/// `Date.now()`.
pub(super) fn unix_millis() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_millis() as f64)
}

/// Entry timestamps: wall-clock milliseconds, strictly increasing across writes.
#[derive(Default)]
pub(super) struct Clock(Mutex<f64>);

impl Clock {
    pub fn next(&self) -> f64 {
        let now = unix_millis();
        let mut stamp = self.lock();
        *stamp = if now > *stamp { now } else { *stamp + 1.0 };
        *stamp
    }

    pub fn observe(&self, value: f64) {
        let mut stamp = self.lock();
        if value > *stamp {
            *stamp = value;
        }
    }

    fn lock(&self) -> MutexGuard<'_, f64> {
        self.0.lock().expect("desktop app store clock lock")
    }
}

struct State {
    handle: Option<AppStoreHandle>,
    status: StoreStatus,
    accepting: bool,
    pending: usize,
    pending_bytes: usize,
}

pub struct DesktopAppStorage {
    authority_sentinel: PathBuf,
    state: Mutex<State>,
    /// Fair, so operations start in the order they were admitted.
    queue: tokio::sync::Mutex<()>,
    clock: Arc<Clock>,
}

/// A pending operation's share of the limits, released when the operation settles.
struct Admission<'a> {
    storage: &'a DesktopAppStorage,
    bytes: usize,
}

impl Drop for Admission<'_> {
    fn drop(&mut self) {
        let mut state = self.storage.lock();
        state.pending -= 1;
        state.pending_bytes -= self.bytes;
    }
}

impl DesktopAppStorage {
    /// `new DesktopAppStorage(...)` followed by `open()`.
    pub(super) fn open(user_data_path: &Path) -> (Self, OpenedStore) {
        let store_file = user_data_path.join(DESKTOP_APP_STORE_FILE_NAME);
        let authority_sentinel = user_data_path.join(DESKTOP_AUTHORITY_SENTINEL_FILE_NAME);
        let authority_expected =
            authority_sentinel.exists() || desktop_app_store_state_file(user_data_path).exists();
        let unopened = |reason: String| StoreStatus {
            available: false,
            authority_expected,
            schema_version: 0,
            quarantined: false,
            quarantine_reason: None,
            unavailable_reason: Some(reason),
        };
        let opened = AppStoreHandle::open(
            store_file.clone(),
            Some(DESKTOP_APP_STORE_DERIVATION_VERSION),
        )
        .map_err(|error| error.to_string())
        .and_then(|handle| {
            serde_json::from_str::<Initialization>(handle.initialization())
                .map(|initialization| (handle, initialization))
                .map_err(|error| error.to_string())
        });
        let (state, quarantined_file) = match opened {
            Ok((handle, initialization)) => (
                State {
                    handle: Some(handle),
                    status: StoreStatus {
                        available: true,
                        authority_expected,
                        schema_version: initialization.schema_version,
                        quarantined: initialization.quarantined_path.is_some(),
                        quarantine_reason: initialization.quarantine_reason,
                        unavailable_reason: None,
                    },
                    accepting: true,
                    pending: 0,
                    pending_bytes: 0,
                },
                initialization.quarantined_path,
            ),
            Err(reason) => (
                State {
                    handle: None,
                    status: unopened(reason),
                    accepting: false,
                    pending: 0,
                    pending_bytes: 0,
                },
                None,
            ),
        };
        let storage = Self {
            authority_sentinel,
            state: Mutex::new(state),
            queue: tokio::sync::Mutex::new(()),
            clock: Arc::default(),
        };
        (
            storage,
            OpenedStore {
                store_file,
                quarantined_file,
            },
        )
    }

    /// Probes the opened store once; a failure takes it out of service.
    pub(super) async fn recover(&self) -> Recovery {
        if self.lock().handle.is_none() {
            return Recovery {
                status: self.status(),
                reseed_required: false,
            };
        }
        let probe = self
            .run(SCHEMA_VERSION_MARKER.len(), |handle, _| {
                handle
                    .get_metadata(SCHEMA_VERSION_MARKER)
                    .map_err(errors::native)
            })
            .await;
        let mut state = self.lock();
        if let Err(error) = probe {
            state.accepting = false;
            state.status.available = false;
            state.status.unavailable_reason = Some(error.message().to_owned());
            return Recovery {
                status: state.status.clone(),
                reseed_required: false,
            };
        }
        Recovery {
            status: state.status.clone(),
            reseed_required: state.status.quarantined,
        }
    }

    pub fn status(&self) -> StoreStatus {
        self.lock().status.clone()
    }

    pub(super) fn is_open(&self) -> bool {
        self.lock().handle.is_some()
    }

    pub(super) fn authority_sentinel(&self) -> &Path {
        &self.authority_sentinel
    }

    pub(super) fn set_authority_expected(&self, expected: bool) {
        self.lock().status.authority_expected = expected;
    }

    /// `runOperation`: admits the operation against the limits, waits for its turn, and runs
    /// it on the blocking pool.
    pub(super) async fn run<T, F>(&self, input_bytes: usize, operation: F) -> Result<T, BridgeError>
    where
        T: Send + 'static,
        F: FnOnce(&AppStoreHandle, &Clock) -> Result<T, BridgeError> + Send + 'static,
    {
        let _admission = self.admit(input_bytes)?;
        let _turn = self.queue.lock().await;
        let handle = {
            let state = self.lock();
            match (&state.handle, state.accepting) {
                (Some(handle), true) => handle.clone(),
                _ => return Err(errors::shutdown()),
            }
        };
        let clock = Arc::clone(&self.clock);
        tauri::async_runtime::spawn_blocking(move || operation(&handle, &clock))
            .await
            .map_err(errors::native)?
    }

    fn admit(&self, input_bytes: usize) -> Result<Admission<'_>, BridgeError> {
        let mut state = self.lock();
        if state.handle.is_none() {
            let reason = state
                .status
                .unavailable_reason
                .as_deref()
                .unwrap_or("the store did not open");
            return Err(errors::unavailable(reason));
        }
        if !state.accepting {
            return Err(errors::shutdown());
        }
        if state.pending >= PENDING_OPERATIONS_MAX {
            return Err(errors::capacity(
                "concurrent operations",
                PENDING_OPERATIONS_MAX,
            ));
        }
        if state.pending_bytes + input_bytes > PENDING_INPUT_BYTES_MAX {
            return Err(errors::capacity(
                "pending request bytes",
                PENDING_INPUT_BYTES_MAX,
            ));
        }
        state.pending += 1;
        state.pending_bytes += input_bytes;
        Ok(Admission {
            storage: self,
            bytes: input_bytes,
        })
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("desktop app store state lock")
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    pub fn open_in(dir: &Path) -> DesktopAppStorage {
        let (storage, _) = DesktopAppStorage::open(dir);
        storage
    }

    #[test]
    fn opens_a_fresh_store_in_the_data_dir() {
        let dir = tempfile::tempdir().unwrap();
        let (storage, opened) = DesktopAppStorage::open(dir.path());
        assert_eq!(
            opened.store_file,
            dir.path().join("desktop-app-store.sqlite3")
        );
        assert!(opened.store_file.exists());
        assert_eq!(opened.quarantined_file, None);
        let status = storage.status();
        assert!(status.available);
        assert!(!status.authority_expected);
        assert!(!status.quarantined);
        assert_eq!(status.unavailable_reason, None);
        let recovery = tauri::async_runtime::block_on(storage.recover());
        assert!(recovery.status.available);
        assert!(!recovery.reseed_required);
    }

    #[test]
    fn a_corrupt_store_is_quarantined_and_needs_a_reseed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(DESKTOP_APP_STORE_FILE_NAME),
            vec![0x5a; 8192],
        )
        .unwrap();
        let (storage, opened) = DesktopAppStorage::open(dir.path());
        assert!(opened.quarantined_file.is_some());
        let status = storage.status();
        assert!(status.available && status.quarantined);
        assert_eq!(status.quarantine_reason.as_deref(), Some("corrupt"));
        let recovery = tauri::async_runtime::block_on(storage.recover());
        assert!(recovery.reseed_required);
    }

    #[test]
    fn an_unopenable_store_reports_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("file");
        std::fs::write(&blocker, "").unwrap();
        let (storage, _) = DesktopAppStorage::open(&blocker);
        let status = storage.status();
        assert!(!status.available);
        assert!(status.unavailable_reason.is_some());
        let error = tauri::async_runtime::block_on(storage.run(0, |_, _| Ok(()))).unwrap_err();
        assert_eq!(error.name(), "DesktopAppStoreUnavailableError");
        assert_eq!(
            error.message(),
            format!(
                "Desktop app store is unavailable: {}",
                status.unavailable_reason.unwrap()
            )
        );
        let recovery = tauri::async_runtime::block_on(storage.recover());
        assert!(!recovery.reseed_required);
    }

    #[test]
    fn existing_sentinel_or_state_file_expects_authority() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(DESKTOP_AUTHORITY_SENTINEL_FILE_NAME), "1\n").unwrap();
        assert!(open_in(dir.path()).status().authority_expected);
        let other = tempfile::tempdir().unwrap();
        std::fs::write(desktop_app_store_state_file(other.path()), "{}").unwrap();
        assert!(open_in(other.path()).status().authority_expected);
    }

    #[test]
    fn admission_is_bounded_by_pending_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let storage = open_in(dir.path());
        let error =
            tauri::async_runtime::block_on(storage.run(PENDING_INPUT_BYTES_MAX + 1, |_, _| Ok(())))
                .unwrap_err();
        assert_eq!(error.name(), "DesktopAppStoreCapacityError");
        assert_eq!(
            error.message(),
            "Desktop app store pending request bytes is limited to 134217728"
        );
        tauri::async_runtime::block_on(storage.run(PENDING_INPUT_BYTES_MAX, |_, _| Ok(())))
            .unwrap();
    }

    #[test]
    fn admission_is_bounded_by_concurrent_operations() {
        let dir = tempfile::tempdir().unwrap();
        let storage = open_in(dir.path());
        let held: Vec<_> = (0..PENDING_OPERATIONS_MAX)
            .map(|_| storage.admit(0).unwrap())
            .collect();
        let error = storage.admit(0).err().unwrap();
        assert_eq!(
            error.message(),
            "Desktop app store concurrent operations is limited to 64"
        );
        drop(held);
        assert!(storage.admit(0).is_ok());
    }

    #[test]
    fn a_failed_probe_takes_the_store_out_of_service() {
        let dir = tempfile::tempdir().unwrap();
        let storage = open_in(dir.path());
        storage.lock().handle.as_ref().unwrap().close().unwrap();
        let recovery = tauri::async_runtime::block_on(storage.recover());
        assert!(!recovery.status.available);
        assert_eq!(
            recovery.status.unavailable_reason.as_deref(),
            Some("app store is closed")
        );
        let error = tauri::async_runtime::block_on(storage.run(0, |_, _| Ok(()))).unwrap_err();
        assert_eq!(error.name(), "DesktopAppStoreShutdownError");
    }

    #[test]
    fn stamps_increase_past_observed_values() {
        let clock = Clock::default();
        let first = clock.next();
        assert!(clock.next() > first);
        clock.observe(first + 1.0e9);
        assert_eq!(clock.next(), first + 1.0e9 + 1.0);
    }
}
