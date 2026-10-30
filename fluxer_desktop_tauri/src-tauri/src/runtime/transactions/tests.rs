// SPDX-License-Identifier: AGPL-3.0-or-later

//! The transaction state table of DesktopRuntimeTransactionRegistry.ts, row by row.

use std::time::Duration;

use serde_json::json;

use super::super::test_support::{
    MemoryMarkers, Reply, TOKEN_A, TOKEN_B, TestServer, cached_plan, caller, document,
    loaded_documents, navigate, outbound, plans_with,
};
use super::*;

const P1: u16 = 5001;
const P2: u16 = 5002;
const P3: u16 = 5003;

struct Fixture {
    documents: Arc<DocumentRegistry>,
    plans: Arc<RuntimePlans>,
    registry: Arc<TransactionRegistry>,
}

impl Fixture {
    async fn new() -> Self {
        let documents = loaded_documents();
        let plans = plans_with(outbound(), Arc::new(MemoryMarkers::default()).markers());
        for port in [P1, P2, P3] {
            cached_plan(&plans, port).await;
        }
        let registry = Arc::new(TransactionRegistry::new(documents.clone(), plans.clone()));
        Self {
            documents,
            plans,
            registry,
        }
    }

    fn revision(&self) -> u64 {
        self.registry.shared.state().revision
    }

    fn set_revision(&self, revision: u64) {
        self.registry.shared.state().revision = revision;
    }

    fn records(&self) -> (usize, usize) {
        let state = self.registry.shared.state();
        (state.pending.len(), state.preparations.len())
    }

    fn active_key(&self) -> Option<String> {
        self.plans
            .active_plan()
            .map(|plan| plan.instance_key.clone())
    }

    async fn prepare(&self, id: &str, port: u16) -> Result<DesktopRuntimePreparation, BridgeError> {
        self.registry
            .prepare(&caller(TOKEN_A), &prepare_request(id, &key(port)))
            .await
    }

    fn commit(&self, id: &str, port: u16, base: u64) -> Result<DesktopRuntimeCommit, BridgeError> {
        self.registry
            .commit(&caller(TOKEN_A), &transaction(id, &key(port), base))
    }

    fn finalize(
        &self,
        id: &str,
        port: u16,
        base: u64,
        committed: u64,
    ) -> Result<DesktopRuntimeFinalization, BridgeError> {
        self.registry.finalize(
            &caller(TOKEN_A),
            &committed_transaction(id, &key(port), base, committed),
        )
    }

    fn rollback(
        &self,
        id: &str,
        port: u16,
        base: u64,
        committed: u64,
    ) -> Result<DesktopRuntimeRollback, BridgeError> {
        self.registry.rollback(
            &caller(TOKEN_A),
            &committed_transaction(id, &key(port), base, committed),
        )
    }

    fn abort(&self, id: &str) -> Result<DesktopRuntimeAbort, BridgeError> {
        self.registry
            .abort(&caller(TOKEN_A), &json!({"preparationId": id}))
    }

    /// Starts a preparation for an uncached instance whose discovery never answers.
    async fn start_pending(
        &self,
        id: &str,
    ) -> (
        TestServer,
        tokio::task::JoinHandle<Result<DesktopRuntimePreparation, BridgeError>>,
    ) {
        let server = TestServer::start().await;
        server.route("/.well-known/fluxer", Reply::Hang);
        let registry = self.registry.clone();
        let request = prepare_request(id, &server.api_endpoint());
        let task = tokio::spawn(async move { registry.prepare(&caller(TOKEN_A), &request).await });
        for _ in 0..200 {
            if self.records().0 > 0 {
                return (server, task);
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("preparation {id} never became pending");
    }
}

fn key(port: u16) -> String {
    format!("http://127.0.0.1:{port}/api")
}

fn prepare_request(id: &str, instance_key: &str) -> Value {
    json!({"preparationId": id, "instanceKey": instance_key})
}

fn transaction(id: &str, instance_key: &str, base: u64) -> Value {
    json!({"preparationId": id, "instanceKey": instance_key, "baseRevision": base})
}

fn committed_transaction(id: &str, instance_key: &str, base: u64, committed: u64) -> Value {
    json!({
        "preparationId": id,
        "instanceKey": instance_key,
        "baseRevision": base,
        "committedRevision": committed,
    })
}

fn assert_error(result: Result<impl std::fmt::Debug, BridgeError>, name: &str, message: &str) {
    let error = result.unwrap_err();
    assert_eq!((error.name(), error.message()), (name, message));
}

// ABSENT -> prepare -> PREPARED

#[tokio::test]
async fn prepare_holds_a_cached_plan_at_the_current_revision() {
    let fixture = Fixture::new().await;
    let preparation = fixture.prepare("a", P1).await.unwrap();
    let plan = fixture.plans.find_plan_for_route(&key(P1)).unwrap();
    assert_eq!(
        serde_json::to_value(&preparation).unwrap(),
        json!({
            "instanceKey": key(P1),
            "apiEndpoint": project(&plan).api_endpoint,
            "remoteApiEndpoint": key(P1),
            "document": plan.document,
            "preparationId": "a",
            "baseRevision": 0,
            "baseActiveInstanceKey": null,
        })
    );
    assert_eq!(fixture.records(), (0, 1));
    assert_eq!(fixture.active_key(), None);
}

#[tokio::test]
async fn prepare_refuses_duplicates_capacity_and_foreign_documents() {
    let fixture = Fixture::new().await;
    for index in 0..8 {
        fixture.prepare(&format!("p{index}"), P1).await.unwrap();
    }
    assert_error(
        fixture.prepare("p0", P1).await,
        "DesktopRuntimePreparationAlreadyExistsError",
        "Desktop runtime preparation p0 already exists",
    );
    assert_error(
        fixture.prepare("p8", P1).await,
        "DesktopRuntimePreparationCapacityError",
        "Desktop runtime preparation capacity is exhausted",
    );
    fixture.abort("p0").unwrap();
    let foreign = fixture
        .registry
        .prepare(&caller(TOKEN_B), &prepare_request("p8", &key(P1)))
        .await
        .unwrap_err();
    assert_eq!(foreign.name(), "RendererDocumentCaptureError");
    for request in [
        json!({"preparationId": "x"}),
        json!({"preparationId": "x", "instanceKey": ""}),
        json!({"preparationId": "x", "instanceKey": key(P1), "baseRevision": 0}),
    ] {
        assert_error(
            fixture.registry.prepare(&caller(TOKEN_A), &request).await,
            "InvalidDesktopRuntimeTransactionRequestError",
            "Desktop runtime transaction prepare request is malformed",
        );
    }
}

#[tokio::test]
async fn a_failed_discovery_leaves_no_record() {
    let fixture = Fixture::new().await;
    let server = TestServer::start().await;
    let error = fixture
        .registry
        .prepare(
            &caller(TOKEN_A),
            &prepare_request("a", &server.api_endpoint()),
        )
        .await
        .unwrap_err();
    assert_eq!(error.name(), "DesktopRuntimeDiscoveryFailedError");
    assert_eq!(fixture.records(), (0, 0));
    fixture.prepare("a", P1).await.unwrap();
}

#[tokio::test]
async fn prepare_refuses_discovery_of_another_instance() {
    let fixture = Fixture::new().await;
    let server = TestServer::start().await;
    let mut served = document(&server.base);
    let moved = format!("{}/api/v2", server.base);
    served["endpoints"]["api"] = json!(moved);
    served["endpoints"]["api_client"] = json!(moved);
    server.route("/.well-known/fluxer", Reply::json(&served));
    let requested = server.api_endpoint();
    assert_error(
        fixture
            .registry
            .prepare(&caller(TOKEN_A), &prepare_request("a", &requested))
            .await,
        "DesktopRuntimeInstanceMismatchError",
        &format!(
            "Desktop runtime preparation requested {requested} but discovery resolved {moved}"
        ),
    );
    assert_eq!(fixture.records(), (0, 0));
}

// PENDING -> abort / owner invalidated

#[tokio::test]
async fn the_owner_aborts_a_pending_preparation() {
    let fixture = Fixture::new().await;
    let (_server, task) = fixture.start_pending("a").await;
    assert_error(
        fixture
            .registry
            .abort(&caller(TOKEN_B), &json!({"preparationId": "a"})),
        "DesktopRuntimePreparationOwnershipError",
        "Desktop runtime preparation a belongs to another renderer document",
    );
    assert_eq!(
        fixture.abort("a").unwrap(),
        DesktopRuntimeAbort {
            disposition: AbortDisposition::Aborted,
            revision: 0,
            active_instance_key: None,
        }
    );
    assert!(task.await.unwrap().is_err());
    assert_eq!(fixture.records(), (0, 0));
}

// PREPARED -> commit -> COMMITTED

#[tokio::test]
async fn commit_activates_the_plan_at_the_next_revision() {
    let fixture = Fixture::new().await;
    fixture.prepare("a", P1).await.unwrap();
    assert_eq!(
        serde_json::to_value(fixture.commit("a", P1, 0).unwrap()).unwrap(),
        json!({"preparationId": "a", "instanceKey": key(P1), "baseRevision": 0, "revision": 1})
    );
    assert_eq!(fixture.revision(), 1);
    assert_eq!(fixture.active_key(), Some(key(P1)));
    assert_error(
        fixture.commit("a", P1, 0),
        "DesktopRuntimePreparationStateError",
        "Desktop runtime preparation a is not prepared",
    );
}

#[tokio::test]
async fn refused_commits_leave_the_preparation_prepared() {
    let fixture = Fixture::new().await;
    fixture.prepare("a", P1).await.unwrap();
    assert_error(
        fixture.commit("missing", P1, 0),
        "DesktopRuntimePreparationNotFoundError",
        "Desktop runtime preparation missing does not exist",
    );
    assert_error(
        fixture
            .registry
            .commit(&caller(TOKEN_B), &transaction("a", &key(P1), 0)),
        "DesktopRuntimePreparationOwnershipError",
        "Desktop runtime preparation a belongs to another renderer document",
    );
    for (port, base) in [(P2, 0), (P1, 1)] {
        assert_error(
            fixture.commit("a", port, base),
            "DesktopRuntimePreparationIdentityError",
            "Desktop runtime preparation a has a different instance identity",
        );
    }
    for request in [
        json!({"preparationId": "a", "instanceKey": key(P1)}),
        json!({"preparationId": "a", "instanceKey": key(P1), "baseRevision": -1}),
        json!({"preparationId": "a", "instanceKey": key(P1), "baseRevision": 0.5}),
    ] {
        assert_error(
            fixture.registry.commit(&caller(TOKEN_A), &request),
            "InvalidDesktopRuntimeTransactionRequestError",
            "Desktop runtime transaction transaction request is malformed",
        );
    }
    assert_eq!(fixture.revision(), 0);
    fixture.commit("a", P1, 0).unwrap();
}

#[tokio::test]
async fn only_one_commit_is_provisional_at_a_time() {
    let fixture = Fixture::new().await;
    fixture.prepare("a", P1).await.unwrap();
    fixture.prepare("b", P2).await.unwrap();
    fixture.commit("a", P1, 0).unwrap();
    assert_error(
        fixture.commit("b", P2, 0),
        "DesktopRuntimeCommitInProgressError",
        "Desktop runtime preparation b cannot commit while another runtime commit is provisional",
    );
    fixture.finalize("a", P1, 0, 1).unwrap();
    // B's base revision is now stale.
    assert_error(
        fixture.commit("b", P2, 0),
        "DesktopRuntimePreparationSupersededError",
        "Desktop runtime preparation b at revision 0 was superseded by revision 1",
    );
    assert_eq!(
        fixture.abort("b").unwrap().disposition,
        AbortDisposition::Aborted
    );
}

#[tokio::test]
async fn commit_refuses_an_active_plan_changed_behind_the_preparation() {
    let fixture = Fixture::new().await;
    fixture.prepare("a", P1).await.unwrap();
    let other = fixture.plans.find_plan_for_route(&key(P2)).unwrap();
    fixture.plans.activate(other).unwrap();
    assert_error(
        fixture.commit("a", P1, 0),
        "DesktopRuntimePreparationIdentityError",
        "Desktop runtime preparation a has a different instance identity",
    );
    assert_eq!(fixture.active_key(), Some(key(P2)));
}

#[tokio::test]
async fn commit_refuses_an_exhausted_revision() {
    let fixture = Fixture::new().await;
    fixture.set_revision(MAX_SAFE_INTEGER);
    fixture.prepare("a", P1).await.unwrap();
    assert_error(
        fixture.commit("a", P1, MAX_SAFE_INTEGER),
        "DesktopRuntimeActivationRevisionExhaustedError",
        "Desktop runtime activation revision is exhausted",
    );
    assert_eq!(fixture.active_key(), None);
    assert_eq!(
        fixture.abort("a").unwrap().disposition,
        AbortDisposition::Aborted
    );
}

// PREPARED -> abort / finalize / rollback

#[tokio::test]
async fn a_prepared_preparation_aborts_but_cannot_finalize_or_roll_back() {
    let fixture = Fixture::new().await;
    fixture.prepare("a", P1).await.unwrap();
    for result in [
        fixture.finalize("a", P1, 0, 1).map(drop),
        fixture.rollback("a", P1, 0, 1).map(drop),
    ] {
        assert_error(
            result,
            "DesktopRuntimePreparationStateError",
            "Desktop runtime preparation a is not committed",
        );
    }
    assert_eq!(
        fixture.abort("a").unwrap(),
        DesktopRuntimeAbort {
            disposition: AbortDisposition::Aborted,
            revision: 0,
            active_instance_key: None,
        }
    );
    assert_eq!(
        fixture.abort("a").unwrap().disposition,
        AbortDisposition::Absent
    );
}

// COMMITTED -> finalize / rollback / abort

#[tokio::test]
async fn finalize_keeps_the_committed_plan_active() {
    let fixture = Fixture::new().await;
    fixture.prepare("a", P1).await.unwrap();
    fixture.commit("a", P1, 0).unwrap();
    assert_error(
        fixture.finalize("a", P1, 0, 2),
        "DesktopRuntimePreparationStateError",
        "Desktop runtime preparation a is not committed",
    );
    assert_error(
        fixture.registry.finalize(
            &caller(TOKEN_A),
            &committed_transaction("a", &key(P1), 0, 0),
        ),
        "InvalidDesktopRuntimeTransactionRequestError",
        "Desktop runtime transaction committed transaction request is malformed",
    );
    assert_eq!(
        serde_json::to_value(fixture.finalize("a", P1, 0, 1).unwrap()).unwrap(),
        committed_transaction("a", &key(P1), 0, 1)
    );
    assert_eq!(fixture.records(), (0, 0));
    assert_eq!(fixture.revision(), 1);
    assert_eq!(fixture.active_key(), Some(key(P1)));
}

#[tokio::test]
async fn rollback_restores_the_previous_plan_at_the_next_revision() {
    let fixture = Fixture::new().await;
    fixture.prepare("a", P2).await.unwrap();
    fixture.commit("a", P2, 0).unwrap();
    fixture.finalize("a", P2, 0, 1).unwrap();
    let preparation = fixture.prepare("b", P1).await.unwrap();
    assert_eq!(preparation.base_active_instance_key, Some(key(P2)));
    fixture.commit("b", P1, 1).unwrap();
    assert_eq!(
        fixture.rollback("b", P1, 1, 2).unwrap(),
        DesktopRuntimeRollback {
            revision: 3,
            active_instance_key: Some(key(P2)),
        }
    );
    assert_eq!(fixture.active_key(), Some(key(P2)));
    assert_eq!(fixture.records(), (0, 0));
}

#[tokio::test]
async fn abort_rolls_back_a_provisional_commit() {
    let fixture = Fixture::new().await;
    fixture.prepare("a", P1).await.unwrap();
    fixture.commit("a", P1, 0).unwrap();
    assert_eq!(
        fixture.abort("a").unwrap(),
        DesktopRuntimeAbort {
            disposition: AbortDisposition::RolledBack,
            revision: 2,
            active_instance_key: None,
        }
    );
    assert_eq!(fixture.active_key(), None);
    assert_eq!(fixture.records(), (0, 0));
}

/// Defensive rows: no public operation moves the revision past a provisional commit, so these
/// move it directly.
#[tokio::test]
async fn a_superseded_commit_aborts_without_rolling_back() {
    let fixture = Fixture::new().await;
    fixture.prepare("a", P1).await.unwrap();
    fixture.commit("a", P1, 0).unwrap();
    fixture.set_revision(5);
    assert_error(
        fixture.abort("a"),
        "DesktopRuntimePreparationSupersededError",
        "Desktop runtime preparation a at revision 1 was superseded by revision 5",
    );
    assert_eq!(
        fixture.abort("a").unwrap().disposition,
        AbortDisposition::Absent
    );
    assert_eq!(fixture.active_key(), Some(key(P1)));
}

#[tokio::test]
async fn a_superseded_commit_cannot_roll_back() {
    let fixture = Fixture::new().await;
    fixture.prepare("a", P1).await.unwrap();
    fixture.commit("a", P1, 0).unwrap();
    fixture.set_revision(7);
    assert_error(
        fixture.rollback("a", P1, 0, 1),
        "DesktopRuntimePreparationSupersededError",
        "Desktop runtime preparation a at revision 1 was superseded by revision 7",
    );
    assert_eq!(fixture.records(), (0, 0));
    assert_eq!(fixture.active_key(), Some(key(P1)));
    assert_eq!(fixture.revision(), 7);
}

#[tokio::test]
async fn aborting_nothing_reports_the_current_runtime() {
    let fixture = Fixture::new().await;
    fixture.prepare("a", P1).await.unwrap();
    fixture.commit("a", P1, 0).unwrap();
    assert_eq!(
        fixture.abort("missing").unwrap(),
        DesktopRuntimeAbort {
            disposition: AbortDisposition::Absent,
            revision: 1,
            active_instance_key: Some(key(P1)),
        }
    );
    assert_error(
        fixture
            .registry
            .abort(&caller(TOKEN_A), &json!({"preparationId": ""})),
        "InvalidDesktopRuntimeTransactionRequestError",
        "Desktop runtime transaction abort request is malformed",
    );
}

// Owner invalidated

#[tokio::test]
async fn navigation_rolls_back_the_documents_provisional_commit() {
    let fixture = Fixture::new().await;
    fixture.prepare("a", P1).await.unwrap();
    fixture.prepare("b", P2).await.unwrap();
    fixture.commit("a", P1, 0).unwrap();
    let (_server, pending) = fixture.start_pending("c").await;
    assert_eq!(fixture.records(), (1, 2));

    navigate(&fixture.documents, TOKEN_B);
    assert_eq!(fixture.records(), (0, 0));
    assert_eq!(fixture.active_key(), None);
    assert_eq!(fixture.revision(), 2);
    assert!(pending.await.unwrap().is_err());
    let next = caller(TOKEN_B);
    assert_eq!(
        fixture
            .registry
            .abort(&next, &json!({"preparationId": "a"}))
            .unwrap()
            .disposition,
        AbortDisposition::Absent
    );
}

#[tokio::test]
async fn navigation_after_finalize_keeps_the_active_plan() {
    let fixture = Fixture::new().await;
    fixture.prepare("a", P1).await.unwrap();
    fixture.commit("a", P1, 0).unwrap();
    fixture.finalize("a", P1, 0, 1).unwrap();
    navigate(&fixture.documents, TOKEN_B);
    assert_eq!(fixture.active_key(), Some(key(P1)));
    assert_eq!(fixture.revision(), 1);
}

// Deactivate

#[tokio::test]
async fn deactivate_captures_before_reading_the_request() {
    let fixture = Fixture::new().await;
    let error = fixture
        .registry
        .deactivate(&caller(TOKEN_B), &Value::Null)
        .unwrap_err();
    assert_eq!(error.name(), "RendererDocumentCaptureError");
    assert_error(
        fixture.registry.deactivate(&caller(TOKEN_A), &Value::Null),
        "InvalidDesktopRuntimeTransactionRequestError",
        "Desktop runtime transaction deactivate request is malformed",
    );
}

#[tokio::test]
async fn deactivate_needs_a_quiet_registry_and_the_renderer_identity() {
    let fixture = Fixture::new().await;
    let deactivate = |port: u16| {
        fixture.registry.deactivate(
            &caller(TOKEN_A),
            &json!({"rendererActiveInstanceKey": key(port)}),
        )
    };
    assert_error(
        deactivate(P1),
        "DesktopRuntimeDeactivationIdentityError",
        "Desktop runtime deactivation does not match the renderer runtime identity",
    );
    fixture.prepare("a", P1).await.unwrap();
    assert_error(
        deactivate(P1),
        "DesktopRuntimeDeactivationInProgressError",
        "Desktop runtime cannot deactivate while another runtime transition is active",
    );
    fixture.commit("a", P1, 0).unwrap();
    fixture.finalize("a", P1, 0, 1).unwrap();
    assert_error(
        deactivate(P2),
        "DesktopRuntimeDeactivationIdentityError",
        "Desktop runtime deactivation does not match the renderer runtime identity",
    );
    assert_eq!(
        deactivate(P1).unwrap(),
        DesktopRuntimeDeactivation {
            revision: 2,
            deactivated_instance_key: key(P1),
        }
    );
    assert_eq!(fixture.active_key(), None);
    assert!(fixture.plans.find_plan_for_route(&key(P1)).is_some());
}

// Cleanup

#[tokio::test]
async fn cleanup_rolls_back_and_forgets_everything() {
    let fixture = Fixture::new().await;
    fixture.prepare("a", P1).await.unwrap();
    fixture.commit("a", P1, 0).unwrap();
    let (_server, pending) = fixture.start_pending("b").await;
    fixture.registry.cleanup();
    assert_eq!(fixture.records(), (0, 0));
    assert_eq!(fixture.revision(), 0);
    assert_eq!(fixture.active_key(), None);
    assert!(pending.await.unwrap().is_err());
}

#[tokio::test]
async fn a_registry_created_over_an_active_plan_starts_at_revision_one() {
    let fixture = Fixture::new().await;
    let plan = fixture.plans.find_plan_for_route(&key(P3)).unwrap();
    fixture.plans.activate(plan).unwrap();
    let registry = TransactionRegistry::new(fixture.documents.clone(), fixture.plans.clone());
    assert_eq!(registry.shared.state().revision, 1);
}
