// SPDX-License-Identifier: AGPL-3.0-or-later

//! Renderer-requested runtime resolutions, ported from DesktopRuntimeResolutionRegistry.ts.
//!
//! Electron ran each step of a resolution on one thread; here the duplicate and capacity checks
//! run again under the lock that inserts the entry, since the owner capture between them runs
//! unlocked. The document registry is never called with this registry's lock held.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::plans::{DesktopRuntimePlan, RuntimePlans, project};
use super::request::{REQUEST_ID_MAX_LENGTH, read_bounded_string, read_exact_record};
use crate::documents::{Caller, DocumentOwner, DocumentRegistry, WatchGuard};
use crate::error::BridgeError;

const MAX_PENDING_RESOLUTIONS: usize = 8;
const RESOLVE_INPUT_MAX_LENGTH: usize = 8192;
const RESOLVE_CONTEXT: &str = "Desktop runtime config resolve";

fn malformed(context: &str) -> BridgeError {
    BridgeError::new(
        "InvalidDesktopRuntimeResolutionRequestError",
        format!("Desktop runtime config {context} is malformed"),
    )
}

struct PendingResolution {
    entry: u64,
    owner: DocumentOwner,
    cancel: CancellationToken,
    watcher: WatchGuard,
}

#[derive(Default)]
struct ResolutionState {
    pending: HashMap<String, PendingResolution>,
    next_entry: u64,
}

impl ResolutionState {
    fn require_admission(&self, request_id: &str) -> Result<(), BridgeError> {
        if self.pending.contains_key(request_id) {
            return Err(BridgeError::new(
                "DesktopRuntimeResolutionAlreadyExistsError",
                format!("Desktop runtime resolution {request_id} already exists"),
            ));
        }
        if self.pending.len() >= MAX_PENDING_RESOLUTIONS {
            return Err(BridgeError::new(
                "DesktopRuntimeResolutionCapacityError",
                "Desktop runtime config resolution capacity is exhausted",
            ));
        }
        Ok(())
    }

    /// Removes the entry at `request_id` (only if it is `entry`, when given) and aborts it. The
    /// returned watcher must be dropped after the lock is released.
    fn dispose(&mut self, request_id: &str, entry: Option<u64>) -> Option<WatchGuard> {
        let pending = self.pending.get(request_id)?;
        if entry.is_some_and(|entry| entry != pending.entry) {
            return None;
        }
        let pending = self.pending.remove(request_id)?;
        pending.cancel.cancel();
        Some(pending.watcher)
    }
}

#[derive(Default)]
struct Shared {
    state: Mutex<ResolutionState>,
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, ResolutionState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn dispose(&self, request_id: &str, entry: Option<u64>) {
        let watcher = self.state().dispose(request_id, entry);
        drop(watcher);
    }
}

pub struct ResolutionRegistry {
    documents: Arc<DocumentRegistry>,
    plans: Arc<RuntimePlans>,
    shared: Arc<Shared>,
}

impl ResolutionRegistry {
    pub fn new(documents: Arc<DocumentRegistry>, plans: Arc<RuntimePlans>) -> Self {
        Self {
            documents,
            plans,
            shared: Arc::default(),
        }
    }

    pub async fn resolve(
        &self,
        caller: &Caller,
        request: &Value,
    ) -> Result<DesktopRuntimePlan, BridgeError> {
        let record = read_exact_record(request, &["input", "requestId"])
            .ok_or_else(|| malformed("resolve request"))?;
        let input = read_bounded_string(&record["input"], RESOLVE_INPUT_MAX_LENGTH);
        let request_id = read_bounded_string(&record["requestId"], REQUEST_ID_MAX_LENGTH);
        let (Some(input), Some(request_id)) = (input, request_id) else {
            return Err(malformed("resolve request"));
        };
        self.shared.state().require_admission(&request_id)?;
        let owner = self.documents.capture(caller, RESOLVE_CONTEXT)?;
        let cancel = self.plans.shutdown_child_token();
        let entry = self.insert(&request_id, &owner, &cancel)?;
        let result = async {
            let plan = self.plans.discovery().resolve(&input, &cancel).await?;
            self.documents.require_current(&owner, RESOLVE_CONTEXT)?;
            let plan = Arc::new(plan);
            self.plans.cache(plan.clone())?;
            Ok(project(&plan))
        }
        .await;
        self.shared.dispose(&request_id, Some(entry));
        result
    }

    fn insert(
        &self,
        request_id: &str,
        owner: &DocumentOwner,
        cancel: &CancellationToken,
    ) -> Result<u64, BridgeError> {
        let entry = {
            let mut state = self.shared.state();
            state.next_entry += 1;
            state.next_entry
        };
        let weak: Weak<Shared> = Arc::downgrade(&self.shared);
        let watched_id = request_id.to_owned();
        let watcher = self.documents.watch(owner, RESOLVE_CONTEXT, move |_| {
            if let Some(shared) = weak.upgrade() {
                shared.dispose(&watched_id, Some(entry));
            }
        })?;
        let mut state = self.shared.state();
        if let Err(error) = state.require_admission(request_id) {
            drop(state);
            drop(watcher);
            return Err(error);
        }
        state.pending.insert(
            request_id.to_owned(),
            PendingResolution {
                entry,
                owner: owner.clone(),
                cancel: cancel.clone(),
                watcher,
            },
        );
        drop(state);
        // An invalidation between `watch` and the insert found no entry to dispose.
        if !self.documents.is_current(owner) {
            self.shared.dispose(request_id, Some(entry));
        }
        Ok(entry)
    }

    pub fn cancel(&self, caller: &Caller, request: &Value) -> Result<(), BridgeError> {
        let request_id = read_exact_record(request, &["requestId"])
            .and_then(|record| read_bounded_string(&record["requestId"], REQUEST_ID_MAX_LENGTH))
            .ok_or_else(|| malformed("resolution cancellation request"))?;
        let Some((entry, owner)) = self
            .shared
            .state()
            .pending
            .get(&request_id)
            .map(|pending| (pending.entry, pending.owner.clone()))
        else {
            return Ok(());
        };
        if !self.documents.matches(&owner, caller) {
            return Err(BridgeError::new(
                "DesktopRuntimeResolutionOwnershipError",
                format!(
                    "Desktop runtime resolution {request_id} belongs to another renderer document"
                ),
            ));
        }
        self.shared.dispose(&request_id, Some(entry));
        Ok(())
    }

    pub fn cleanup(&self) {
        let watchers: Vec<WatchGuard> = {
            let mut state = self.shared.state();
            let ids: Vec<String> = state.pending.keys().cloned().collect();
            ids.iter()
                .filter_map(|request_id| state.dispose(request_id, None))
                .collect()
        };
        drop(watchers);
    }
}

#[cfg(test)]
mod tests;
