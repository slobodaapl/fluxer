// SPDX-License-Identifier: AGPL-3.0-or-later

//! Runtime switch transactions, ported from DesktopRuntimeTransactionRegistry.ts.
//!
//! A preparation resolves a plan (pending), then holds it (prepared) until the renderer commits
//! it as the active plan (committed, provisional) and finally finalizes or rolls it back. Every
//! plan change bumps the activation revision, so a stale preparation cannot commit over a newer
//! activation and a stale commit cannot roll back over one.
//!
//! Each Electron handler ran to completion on one thread. Here every state step runs under one
//! lock acquisition; ownership checks against the document registry run outside that lock and
//! are re-validated against the record they were made for.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use serde::Serialize;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::plans::{DesktopRuntimePlan, LocalAppRuntimePlan, RuntimePlans, project};
use super::request::{
    REQUEST_ID_MAX_LENGTH, read_bounded_string, read_exact_record, read_revision,
};
use crate::documents::{Caller, DocumentOwner, DocumentRegistry, InvalidationReason, WatchGuard};
use crate::error::BridgeError;

const MAX_RUNTIME_PREPARATIONS: usize = 8;
const INSTANCE_KEY_MAX_LENGTH: usize = 8192;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const PREPARE_CONTEXT: &str = "Desktop runtime config prepare";
const DEACTIVATE_CONTEXT: &str = "Desktop runtime config deactivate";

const PREPARE_REQUEST_KEYS: &[&str] = &["preparationId", "instanceKey"];
const TRANSACTION_REQUEST_KEYS: &[&str] = &["preparationId", "instanceKey", "baseRevision"];
const COMMITTED_TRANSACTION_REQUEST_KEYS: &[&str] = &[
    "preparationId",
    "instanceKey",
    "baseRevision",
    "committedRevision",
];
const DEACTIVATE_REQUEST_KEYS: &[&str] = &["rendererActiveInstanceKey"];

fn malformed(context: &str) -> BridgeError {
    BridgeError::new(
        "InvalidDesktopRuntimeTransactionRequestError",
        format!("Desktop runtime transaction {context} is malformed"),
    )
}

fn not_found(preparation_id: &str) -> BridgeError {
    BridgeError::new(
        "DesktopRuntimePreparationNotFoundError",
        format!("Desktop runtime preparation {preparation_id} does not exist"),
    )
}

fn instance_mismatch(requested: &str, resolved: &str) -> BridgeError {
    BridgeError::new(
        "DesktopRuntimeInstanceMismatchError",
        format!(
            "Desktop runtime preparation requested {requested} but discovery resolved {resolved}"
        ),
    )
}

fn ownership(preparation_id: &str) -> BridgeError {
    BridgeError::new(
        "DesktopRuntimePreparationOwnershipError",
        format!(
            "Desktop runtime preparation {preparation_id} belongs to another renderer document"
        ),
    )
}

fn identity_mismatch(preparation_id: &str) -> BridgeError {
    BridgeError::new(
        "DesktopRuntimePreparationIdentityError",
        format!("Desktop runtime preparation {preparation_id} has a different instance identity"),
    )
}

fn not_in_state(preparation_id: &str, expected_state: &str) -> BridgeError {
    BridgeError::new(
        "DesktopRuntimePreparationStateError",
        format!("Desktop runtime preparation {preparation_id} is not {expected_state}"),
    )
}

fn superseded(preparation_id: &str, base_revision: u64, active_revision: u64) -> BridgeError {
    BridgeError::new(
        "DesktopRuntimePreparationSupersededError",
        format!(
            "Desktop runtime preparation {preparation_id} at revision {base_revision} was superseded by revision {active_revision}"
        ),
    )
}

/// `DesktopRuntimePreparation`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopRuntimePreparation {
    #[serde(flatten)]
    pub plan: DesktopRuntimePlan,
    pub preparation_id: String,
    pub base_revision: u64,
    pub base_active_instance_key: Option<String>,
}

/// `DesktopRuntimeCommit`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopRuntimeCommit {
    pub preparation_id: String,
    pub instance_key: String,
    pub base_revision: u64,
    pub revision: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AbortDisposition {
    Absent,
    Aborted,
    RolledBack,
}

/// `DesktopRuntimeAbort`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopRuntimeAbort {
    pub disposition: AbortDisposition,
    pub revision: u64,
    pub active_instance_key: Option<String>,
}

/// `DesktopRuntimeRollback`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopRuntimeRollback {
    pub revision: u64,
    pub active_instance_key: Option<String>,
}

/// `DesktopRuntimeDeactivation`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopRuntimeDeactivation {
    pub revision: u64,
    pub deactivated_instance_key: String,
}

/// `RuntimeTransactionIdentity`.
struct TransactionIdentity {
    preparation_id: String,
    instance_key: String,
    base_revision: u64,
}

/// `DesktopRuntimeFinalization`, the committed transaction identity.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopRuntimeFinalization {
    pub preparation_id: String,
    pub instance_key: String,
    pub base_revision: u64,
    pub committed_revision: u64,
}

struct PendingPreparation {
    entry: u64,
    owner: DocumentOwner,
    cancel: CancellationToken,
    watcher: WatchGuard,
}

enum PreparationState {
    Prepared,
    Committed {
        committed_revision: u64,
        previous_plan: Option<Arc<LocalAppRuntimePlan>>,
    },
}

struct Preparation {
    entry: u64,
    instance_key: String,
    base_revision: u64,
    base_active_instance_key: Option<String>,
    owner: DocumentOwner,
    plan: Arc<LocalAppRuntimePlan>,
    watcher: WatchGuard,
    state: PreparationState,
}

#[derive(Default)]
struct TransactionState {
    pending: HashMap<String, PendingPreparation>,
    preparations: HashMap<String, Preparation>,
    revision: u64,
    next_entry: u64,
}

impl TransactionState {
    fn require_admission(&self, preparation_id: &str) -> Result<(), BridgeError> {
        if self.pending.contains_key(preparation_id)
            || self.preparations.contains_key(preparation_id)
        {
            return Err(BridgeError::new(
                "DesktopRuntimePreparationAlreadyExistsError",
                format!("Desktop runtime preparation {preparation_id} already exists"),
            ));
        }
        if self.preparations.len() + self.pending.len() >= MAX_RUNTIME_PREPARATIONS {
            return Err(BridgeError::new(
                "DesktopRuntimePreparationCapacityError",
                "Desktop runtime preparation capacity is exhausted",
            ));
        }
        Ok(())
    }

    /// The record `preparation_id` names, pending first; a record keeps its entry number
    /// when it moves from pending to prepared.
    fn owner_of(&self, preparation_id: &str) -> Option<(u64, DocumentOwner)> {
        self.pending
            .get(preparation_id)
            .map(|pending| (pending.entry, pending.owner.clone()))
            .or_else(|| {
                self.preparations
                    .get(preparation_id)
                    .map(|preparation| (preparation.entry, preparation.owner.clone()))
            })
    }

    fn next_revision(&self) -> Result<u64, BridgeError> {
        if self.revision >= MAX_SAFE_INTEGER {
            return Err(BridgeError::new(
                "DesktopRuntimeActivationRevisionExhaustedError",
                "Desktop runtime activation revision is exhausted",
            ));
        }
        Ok(self.revision + 1)
    }

    /// `requirePreparation`: exists, belongs to the caller, and has the requested identity.
    fn require_preparation(
        &self,
        identity: &TransactionIdentity,
        caller_owns: bool,
    ) -> Result<&Preparation, BridgeError> {
        let preparation = self
            .preparations
            .get(&identity.preparation_id)
            .ok_or_else(|| not_found(&identity.preparation_id))?;
        if !caller_owns {
            return Err(ownership(&identity.preparation_id));
        }
        if preparation.instance_key != identity.instance_key
            || preparation.base_revision != identity.base_revision
        {
            return Err(identity_mismatch(&identity.preparation_id));
        }
        Ok(preparation)
    }

    /// The committed revision and previous plan of a preparation committed at `committed_revision`.
    fn require_committed(
        preparation: &Preparation,
        preparation_id: &str,
        committed_revision: u64,
    ) -> Result<Option<Arc<LocalAppRuntimePlan>>, BridgeError> {
        match &preparation.state {
            PreparationState::Committed {
                committed_revision: revision,
                previous_plan,
            } if *revision == committed_revision => Ok(previous_plan.clone()),
            _ => Err(not_in_state(preparation_id, "committed")),
        }
    }

    /// `findProvisionalCommit` over the preparations `include` selects: the one committed at
    /// the current revision, as its id and previous plan.
    fn provisional_commit(
        &self,
        include: impl Fn(&Preparation) -> bool,
    ) -> Option<(String, Option<Arc<LocalAppRuntimePlan>>)> {
        self.preparations
            .iter()
            .find_map(|(id, preparation)| match &preparation.state {
                PreparationState::Committed {
                    committed_revision,
                    previous_plan,
                } if *committed_revision == self.revision && include(preparation) => {
                    Some((id.clone(), previous_plan.clone()))
                }
                _ => None,
            })
    }

    /// The returned watcher must be dropped after the lock is released.
    fn dispose(&mut self, preparation_id: &str) -> Option<WatchGuard> {
        self.preparations
            .remove(preparation_id)
            .map(|preparation| preparation.watcher)
    }

    /// Removes and aborts the pending preparation if it is still `entry`.
    fn dispose_pending(&mut self, preparation_id: &str, entry: u64) -> Option<WatchGuard> {
        if self
            .pending
            .get(preparation_id)
            .is_none_or(|pending| pending.entry != entry)
        {
            return None;
        }
        let pending = self.pending.remove(preparation_id)?;
        pending.cancel.cancel();
        Some(pending.watcher)
    }
}

struct Shared {
    plans: Arc<RuntimePlans>,
    state: Mutex<TransactionState>,
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, TransactionState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn activate(
        &self,
        state: &mut TransactionState,
        plan: Arc<LocalAppRuntimePlan>,
    ) -> Result<u64, BridgeError> {
        let revision = state.next_revision()?;
        self.plans.activate(plan)?;
        state.revision = revision;
        Ok(revision)
    }

    fn restore(
        &self,
        state: &mut TransactionState,
        plan: Option<Arc<LocalAppRuntimePlan>>,
    ) -> Result<DesktopRuntimeRollback, BridgeError> {
        let revision = state.next_revision()?;
        let active_instance_key = plan.as_ref().map(|plan| plan.instance_key.clone());
        match plan {
            None => self.plans.deactivate()?,
            Some(plan) => self.plans.activate(plan)?,
        }
        state.revision = revision;
        Ok(DesktopRuntimeRollback {
            revision,
            active_instance_key,
        })
    }

    fn abort_result(
        &self,
        state: &TransactionState,
        disposition: AbortDisposition,
    ) -> DesktopRuntimeAbort {
        DesktopRuntimeAbort {
            disposition,
            revision: state.revision,
            active_instance_key: self
                .plans
                .active_plan()
                .map(|plan| plan.instance_key.clone()),
        }
    }

    fn dispose_pending(&self, preparation_id: &str, entry: u64) {
        let watcher = self.state().dispose_pending(preparation_id, entry);
        drop(watcher);
    }

    /// `handleOwnerInvalidated`: rolls back the owner's provisional commit, then drops every
    /// record the owner's document held.
    fn owner_invalidated(
        &self,
        owner: &DocumentOwner,
        preparation_id: &str,
        reason: InvalidationReason,
    ) {
        let mut released = Vec::new();
        let mut state = self.state();
        if let Some((_, previous_plan)) =
            state.provisional_commit(|preparation| preparation.owner == *owner)
            && let Err(error) = self.restore(&mut state, previous_plan)
        {
            tracing::error!(
                preparationId = preparation_id,
                ?reason,
                %error,
                "Failed to roll back a committed runtime after renderer invalidation"
            );
        }
        let owned: Vec<String> = state
            .preparations
            .iter()
            .filter(|(_, preparation)| preparation.owner == *owner)
            .map(|(id, _)| id.clone())
            .collect();
        released.extend(owned.iter().filter_map(|id| state.dispose(id)));
        let pending: Vec<(String, u64)> = state
            .pending
            .iter()
            .filter(|(_, pending)| pending.owner == *owner)
            .map(|(id, pending)| (id.clone(), pending.entry))
            .collect();
        released.extend(
            pending
                .iter()
                .filter_map(|(id, entry)| state.dispose_pending(id, *entry)),
        );
    }
}

pub struct TransactionRegistry {
    documents: Arc<DocumentRegistry>,
    shared: Arc<Shared>,
}

impl TransactionRegistry {
    /// `initialize`: revision 1 when a plan is already active, else 0.
    pub fn new(documents: Arc<DocumentRegistry>, plans: Arc<RuntimePlans>) -> Self {
        let revision = u64::from(plans.active_plan().is_some());
        Self {
            documents,
            shared: Arc::new(Shared {
                plans,
                state: Mutex::new(TransactionState {
                    revision,
                    ..TransactionState::default()
                }),
            }),
        }
    }

    pub async fn prepare(
        &self,
        caller: &Caller,
        request: &Value,
    ) -> Result<DesktopRuntimePreparation, BridgeError> {
        let record = read_exact_record(request, PREPARE_REQUEST_KEYS)
            .ok_or_else(|| malformed("prepare request"))?;
        let preparation_id = read_bounded_string(&record["preparationId"], REQUEST_ID_MAX_LENGTH);
        let instance_key = read_bounded_string(&record["instanceKey"], INSTANCE_KEY_MAX_LENGTH);
        let (Some(preparation_id), Some(instance_key)) = (preparation_id, instance_key) else {
            return Err(malformed("prepare request"));
        };
        self.shared.state().require_admission(&preparation_id)?;
        let owner = self.documents.capture(caller, PREPARE_CONTEXT)?;
        let cancel = self.shared.plans.shutdown_child_token();
        let entry = self.insert_pending(&preparation_id, &owner, &cancel)?;
        let result = self
            .prepare_pending(&preparation_id, &instance_key, entry, &owner, &cancel)
            .await;
        if result.is_err() {
            self.shared.dispose_pending(&preparation_id, entry);
        }
        result
    }

    fn insert_pending(
        &self,
        preparation_id: &str,
        owner: &DocumentOwner,
        cancel: &CancellationToken,
    ) -> Result<u64, BridgeError> {
        let entry = {
            let mut state = self.shared.state();
            state.next_entry += 1;
            state.next_entry
        };
        let weak: Weak<Shared> = Arc::downgrade(&self.shared);
        let watched_id = preparation_id.to_owned();
        let watched_owner = owner.clone();
        let watched_cancel = cancel.clone();
        let watcher = self
            .documents
            .watch(owner, PREPARE_CONTEXT, move |reason| {
                watched_cancel.cancel();
                if let Some(shared) = weak.upgrade() {
                    shared.owner_invalidated(&watched_owner, &watched_id, reason);
                }
            })?;
        let mut state = self.shared.state();
        if let Err(error) = state.require_admission(preparation_id) {
            drop(state);
            drop(watcher);
            return Err(error);
        }
        state.pending.insert(
            preparation_id.to_owned(),
            PendingPreparation {
                entry,
                owner: owner.clone(),
                cancel: cancel.clone(),
                watcher,
            },
        );
        drop(state);
        // An invalidation between `watch` and the insert found no entry to dispose.
        if !self.documents.is_current(owner) {
            self.shared.dispose_pending(preparation_id, entry);
        }
        Ok(entry)
    }

    async fn prepare_pending(
        &self,
        preparation_id: &str,
        instance_key: &str,
        entry: u64,
        owner: &DocumentOwner,
        cancel: &CancellationToken,
    ) -> Result<DesktopRuntimePreparation, BridgeError> {
        let plans = &self.shared.plans;
        let plan = match plans.find_plan_for_route(instance_key) {
            Some(plan) => plan,
            None => {
                let plan = plans.discovery().resolve(instance_key, cancel).await?;
                if plan.instance_key != instance_key {
                    return Err(instance_mismatch(instance_key, &plan.instance_key));
                }
                Arc::new(plan)
            }
        };
        self.documents.require_current(owner, PREPARE_CONTEXT)?;
        let mut state = self.shared.state();
        if state
            .pending
            .get(preparation_id)
            .is_none_or(|pending| pending.entry != entry)
        {
            return Err(not_found(preparation_id));
        }
        if plan.instance_key != instance_key {
            return Err(instance_mismatch(instance_key, &plan.instance_key));
        }
        let Some(pending) = state.pending.remove(preparation_id) else {
            return Err(not_found(preparation_id));
        };
        let base_active_instance_key = plans
            .active_plan()
            .map(|active| active.instance_key.clone());
        let preparation = DesktopRuntimePreparation {
            plan: project(&plan),
            preparation_id: preparation_id.to_owned(),
            base_revision: state.revision,
            base_active_instance_key: base_active_instance_key.clone(),
        };
        let base_revision = state.revision;
        state.preparations.insert(
            preparation_id.to_owned(),
            Preparation {
                entry,
                instance_key: instance_key.to_owned(),
                base_revision,
                base_active_instance_key,
                owner: pending.owner,
                plan,
                watcher: pending.watcher,
                state: PreparationState::Prepared,
            },
        );
        Ok(preparation)
    }

    /// Locks the state once the record `preparation_id` names is the same before and after
    /// asking the document registry whether `caller` owns it, so the registry is never called
    /// under the lock. The flag answers for the record the returned state holds, if any.
    fn lock_owned(
        &self,
        caller: &Caller,
        preparation_id: &str,
    ) -> (MutexGuard<'_, TransactionState>, bool) {
        let mut observed = self.shared.state().owner_of(preparation_id);
        loop {
            let caller_owns = observed
                .as_ref()
                .is_some_and(|(_, owner)| self.documents.matches(owner, caller));
            let state = self.shared.state();
            let current = state.owner_of(preparation_id);
            if current.as_ref().map(|(entry, _)| *entry)
                == observed.as_ref().map(|(entry, _)| *entry)
            {
                return (state, caller_owns);
            }
            drop(state);
            observed = current;
        }
    }

    pub fn commit(
        &self,
        caller: &Caller,
        request: &Value,
    ) -> Result<DesktopRuntimeCommit, BridgeError> {
        let identity = transaction_identity(request)?;
        let (mut state, caller_owns) = self.lock_owned(caller, &identity.preparation_id);
        let preparation = state.require_preparation(&identity, caller_owns)?;
        if !matches!(preparation.state, PreparationState::Prepared) {
            return Err(not_in_state(&identity.preparation_id, "prepared"));
        }
        let entry = preparation.entry;
        if state.preparations.values().any(|existing| {
            existing.entry != entry && matches!(existing.state, PreparationState::Committed { .. })
        }) {
            return Err(BridgeError::new(
                "DesktopRuntimeCommitInProgressError",
                format!(
                    "Desktop runtime preparation {} cannot commit while another runtime commit is provisional",
                    identity.preparation_id
                ),
            ));
        }
        if preparation.base_revision != state.revision {
            return Err(superseded(
                &identity.preparation_id,
                preparation.base_revision,
                state.revision,
            ));
        }
        let previous_plan = self.shared.plans.active_plan();
        if previous_plan
            .as_ref()
            .map(|plan| plan.instance_key.as_str())
            != preparation.base_active_instance_key.as_deref()
        {
            return Err(identity_mismatch(&identity.preparation_id));
        }
        let plan = preparation.plan.clone();
        let committed_revision = self.shared.activate(&mut state, plan)?;
        if let Some(preparation) = state.preparations.get_mut(&identity.preparation_id) {
            preparation.state = PreparationState::Committed {
                committed_revision,
                previous_plan,
            };
        }
        Ok(DesktopRuntimeCommit {
            preparation_id: identity.preparation_id,
            instance_key: identity.instance_key,
            base_revision: identity.base_revision,
            revision: committed_revision,
        })
    }

    pub fn abort(
        &self,
        caller: &Caller,
        request: &Value,
    ) -> Result<DesktopRuntimeAbort, BridgeError> {
        let preparation_id = read_exact_record(request, &["preparationId"])
            .and_then(|record| read_bounded_string(&record["preparationId"], REQUEST_ID_MAX_LENGTH))
            .ok_or_else(|| malformed("abort request"))?;
        let mut released = Vec::new();
        let (mut state, caller_owns) = self.lock_owned(caller, &preparation_id);
        if let Some(entry) = state
            .pending
            .get(&preparation_id)
            .map(|pending| pending.entry)
        {
            if !caller_owns {
                return Err(ownership(&preparation_id));
            }
            released.extend(state.dispose_pending(&preparation_id, entry));
            return Ok(self.shared.abort_result(&state, AbortDisposition::Aborted));
        }
        let Some(preparation) = state.preparations.get(&preparation_id) else {
            return Ok(self.shared.abort_result(&state, AbortDisposition::Absent));
        };
        if !caller_owns {
            return Err(ownership(&preparation_id));
        }
        let committed = match &preparation.state {
            PreparationState::Prepared => None,
            PreparationState::Committed {
                committed_revision,
                previous_plan,
            } => Some((*committed_revision, previous_plan.clone())),
        };
        let Some((committed_revision, previous_plan)) = committed else {
            released.extend(state.dispose(&preparation_id));
            return Ok(self.shared.abort_result(&state, AbortDisposition::Aborted));
        };
        if committed_revision != state.revision {
            released.extend(state.dispose(&preparation_id));
            return Err(superseded(
                &preparation_id,
                committed_revision,
                state.revision,
            ));
        }
        self.shared.restore(&mut state, previous_plan)?;
        released.extend(state.dispose(&preparation_id));
        Ok(self
            .shared
            .abort_result(&state, AbortDisposition::RolledBack))
    }

    pub fn finalize(
        &self,
        caller: &Caller,
        request: &Value,
    ) -> Result<DesktopRuntimeFinalization, BridgeError> {
        let identity = committed_transaction_identity(request)?;
        let (mut state, caller_owns) = self.lock_owned(caller, &identity.preparation_id);
        let preparation = state.require_preparation(&identity.transaction(), caller_owns)?;
        TransactionState::require_committed(
            preparation,
            &identity.preparation_id,
            identity.committed_revision,
        )?;
        let released = state.dispose(&identity.preparation_id);
        drop(state);
        drop(released);
        Ok(identity)
    }

    pub fn rollback(
        &self,
        caller: &Caller,
        request: &Value,
    ) -> Result<DesktopRuntimeRollback, BridgeError> {
        let identity = committed_transaction_identity(request)?;
        let mut released = Vec::new();
        let (mut state, caller_owns) = self.lock_owned(caller, &identity.preparation_id);
        let preparation = state.require_preparation(&identity.transaction(), caller_owns)?;
        let previous_plan = TransactionState::require_committed(
            preparation,
            &identity.preparation_id,
            identity.committed_revision,
        )?;
        if identity.committed_revision != state.revision {
            released.extend(state.dispose(&identity.preparation_id));
            return Err(superseded(
                &identity.preparation_id,
                identity.committed_revision,
                state.revision,
            ));
        }
        let rollback = self.shared.restore(&mut state, previous_plan)?;
        released.extend(state.dispose(&identity.preparation_id));
        Ok(rollback)
    }

    pub fn deactivate(
        &self,
        caller: &Caller,
        request: &Value,
    ) -> Result<DesktopRuntimeDeactivation, BridgeError> {
        self.documents.capture(caller, DEACTIVATE_CONTEXT)?;
        let renderer_active_instance_key = read_exact_record(request, DEACTIVATE_REQUEST_KEYS)
            .ok_or_else(|| malformed("deactivate request"))
            .and_then(|record| {
                read_bounded_string(
                    &record["rendererActiveInstanceKey"],
                    INSTANCE_KEY_MAX_LENGTH,
                )
                .ok_or_else(|| malformed("deactivate request"))
            })?;
        let mut state = self.shared.state();
        if !state.pending.is_empty() || !state.preparations.is_empty() {
            return Err(BridgeError::new(
                "DesktopRuntimeDeactivationInProgressError",
                "Desktop runtime cannot deactivate while another runtime transition is active",
            ));
        }
        let plans = &self.shared.plans;
        let Some(active) = plans
            .active_plan()
            .filter(|active| active.instance_key == renderer_active_instance_key)
        else {
            return Err(BridgeError::new(
                "DesktopRuntimeDeactivationIdentityError",
                "Desktop runtime deactivation does not match the renderer runtime identity",
            ));
        };
        let revision = state.next_revision()?;
        plans.deactivate()?;
        state.revision = revision;
        Ok(DesktopRuntimeDeactivation {
            revision,
            deactivated_instance_key: active.instance_key.clone(),
        })
    }

    pub fn cleanup(&self) {
        let mut released = Vec::new();
        let mut state = self.shared.state();
        if let Some((preparation_id, previous_plan)) = state.provisional_commit(|_| true)
            && let Err(error) = self.shared.restore(&mut state, previous_plan)
        {
            tracing::error!(
                preparationId = %preparation_id,
                %error,
                "Failed to roll back a provisional runtime during transaction cleanup"
            );
        }
        released.extend(
            state
                .preparations
                .drain()
                .map(|(_, preparation)| preparation.watcher),
        );
        released.extend(state.pending.drain().map(|(_, pending)| {
            pending.cancel.cancel();
            pending.watcher
        }));
        state.revision = 0;
    }
}

impl DesktopRuntimeFinalization {
    fn transaction(&self) -> TransactionIdentity {
        TransactionIdentity {
            preparation_id: self.preparation_id.clone(),
            instance_key: self.instance_key.clone(),
            base_revision: self.base_revision,
        }
    }
}

/// `requireTransactionIdentity`.
fn transaction_identity(request: &Value) -> Result<TransactionIdentity, BridgeError> {
    let record = read_exact_record(request, TRANSACTION_REQUEST_KEYS)
        .ok_or_else(|| malformed("transaction request"))?;
    let preparation_id = read_bounded_string(&record["preparationId"], REQUEST_ID_MAX_LENGTH);
    let instance_key = read_bounded_string(&record["instanceKey"], INSTANCE_KEY_MAX_LENGTH);
    let base_revision = read_revision(&record["baseRevision"], true);
    let (Some(preparation_id), Some(instance_key), Some(base_revision)) =
        (preparation_id, instance_key, base_revision)
    else {
        return Err(malformed("transaction request"));
    };
    Ok(TransactionIdentity {
        preparation_id,
        instance_key,
        base_revision,
    })
}

/// `requireCommittedTransactionIdentity`.
fn committed_transaction_identity(
    request: &Value,
) -> Result<DesktopRuntimeFinalization, BridgeError> {
    let record = read_exact_record(request, COMMITTED_TRANSACTION_REQUEST_KEYS)
        .ok_or_else(|| malformed("committed transaction request"))?;
    let preparation_id = read_bounded_string(&record["preparationId"], REQUEST_ID_MAX_LENGTH);
    let instance_key = read_bounded_string(&record["instanceKey"], INSTANCE_KEY_MAX_LENGTH);
    let base_revision = read_revision(&record["baseRevision"], true);
    let committed_revision = read_revision(&record["committedRevision"], false);
    let (Some(preparation_id), Some(instance_key), Some(base_revision), Some(committed_revision)) = (
        preparation_id,
        instance_key,
        base_revision,
        committed_revision,
    ) else {
        return Err(malformed("committed transaction request"));
    };
    Ok(DesktopRuntimeFinalization {
        preparation_id,
        instance_key,
        base_revision,
        committed_revision,
    })
}

#[cfg(test)]
mod tests;
