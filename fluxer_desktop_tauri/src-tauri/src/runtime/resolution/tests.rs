// SPDX-License-Identifier: AGPL-3.0-or-later

use std::time::Duration;

use serde_json::json;

use super::super::test_support::{
    MemoryMarkers, Reply, TOKEN_A, TOKEN_B, TestServer, caller, document, loaded_documents,
    navigate, outbound, plans_with,
};
use super::*;

struct Fixture {
    documents: Arc<DocumentRegistry>,
    plans: Arc<RuntimePlans>,
    registry: Arc<ResolutionRegistry>,
}

fn fixture() -> Fixture {
    let documents = loaded_documents();
    let plans = plans_with(outbound(), Arc::new(MemoryMarkers::default()).markers());
    let registry = Arc::new(ResolutionRegistry::new(documents.clone(), plans.clone()));
    Fixture {
        documents,
        plans,
        registry,
    }
}

fn resolve_request(input: &str, request_id: &str) -> Value {
    json!({"input": input, "requestId": request_id})
}

fn pending_count(registry: &ResolutionRegistry) -> usize {
    registry.shared.state().pending.len()
}

/// Starts a resolution that stays pending until it is cancelled.
async fn start_hanging(
    fixture: &Fixture,
    server: &TestServer,
    token: &'static str,
    request_id: &str,
) -> tokio::task::JoinHandle<Result<DesktopRuntimePlan, BridgeError>> {
    let registry = fixture.registry.clone();
    let request = resolve_request(&server.api_endpoint(), request_id);
    let before = pending_count(&registry);
    let task = tokio::spawn(async move { registry.resolve(&caller(token), &request).await });
    for _ in 0..200 {
        if pending_count(&fixture.registry) > before {
            return task;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("resolution {request_id} never became pending");
}

async fn hanging_server() -> TestServer {
    let server = TestServer::start().await;
    server.route("/.well-known/fluxer", Reply::Hang);
    server
}

#[tokio::test]
async fn a_resolution_returns_the_projected_plan_and_caches_it() {
    let fixture = fixture();
    let server = TestServer::start().await;
    server.route("/.well-known/fluxer", Reply::json(&document(&server.base)));
    let plan = fixture
        .registry
        .resolve(
            &caller(TOKEN_A),
            &resolve_request(&server.api_endpoint(), "r1"),
        )
        .await
        .unwrap();
    assert_eq!(plan.instance_key, server.api_endpoint());
    assert_eq!(plan.remote_api_endpoint, server.api_endpoint());
    assert!(
        fixture
            .plans
            .find_plan_for_route(&plan.instance_key)
            .is_some()
    );
    assert_eq!(fixture.plans.active_plan(), None);
    assert_eq!(pending_count(&fixture.registry), 0);
}

#[tokio::test]
async fn malformed_requests_and_foreign_documents_are_refused() {
    let fixture = fixture();
    for request in [
        Value::Null,
        json!({"input": "x"}),
        json!({"input": "x", "requestId": "r", "extra": 1}),
        json!({"input": " ", "requestId": "r"}),
        json!({"input": "x", "requestId": "r".repeat(129)}),
        json!({"input": "x".repeat(8193), "requestId": "r"}),
    ] {
        let error = fixture
            .registry
            .resolve(&caller(TOKEN_A), &request)
            .await
            .unwrap_err();
        assert_eq!(error.name(), "InvalidDesktopRuntimeResolutionRequestError");
        assert_eq!(
            error.message(),
            "Desktop runtime config resolve request is malformed"
        );
    }
    let error = fixture
        .registry
        .resolve(
            &caller(TOKEN_B),
            &resolve_request("http://127.0.0.1:1/api", "r"),
        )
        .await
        .unwrap_err();
    assert_eq!(error.name(), "RendererDocumentCaptureError");
    let error = fixture
        .registry
        .cancel(&caller(TOKEN_A), &json!({"requestId": 1}))
        .unwrap_err();
    assert_eq!(
        error.message(),
        "Desktop runtime config resolution cancellation request is malformed"
    );
}

#[tokio::test]
async fn duplicates_are_refused_before_capacity() {
    let fixture = fixture();
    let server = hanging_server().await;
    let mut tasks = Vec::new();
    for index in 0..8 {
        tasks.push(start_hanging(&fixture, &server, TOKEN_A, &format!("r{index}")).await);
    }
    let duplicate = fixture
        .registry
        .resolve(
            &caller(TOKEN_A),
            &resolve_request(&server.api_endpoint(), "r0"),
        )
        .await
        .unwrap_err();
    assert_eq!(
        duplicate.name(),
        "DesktopRuntimeResolutionAlreadyExistsError"
    );
    assert_eq!(
        duplicate.message(),
        "Desktop runtime resolution r0 already exists"
    );
    let over = fixture
        .registry
        .resolve(
            &caller(TOKEN_A),
            &resolve_request(&server.api_endpoint(), "r8"),
        )
        .await
        .unwrap_err();
    assert_eq!(over.name(), "DesktopRuntimeResolutionCapacityError");

    fixture.registry.cleanup();
    assert_eq!(pending_count(&fixture.registry), 0);
    for task in tasks {
        let error = task.await.unwrap().unwrap_err();
        assert!(
            error
                .message()
                .contains("request was aborted by its caller")
        );
    }
}

#[tokio::test]
async fn only_the_owner_cancels_and_unknown_ids_are_ignored() {
    let fixture = fixture();
    let server = hanging_server().await;
    let task = start_hanging(&fixture, &server, TOKEN_A, "r1").await;
    assert_eq!(
        fixture
            .registry
            .cancel(&caller(TOKEN_A), &json!({"requestId": "unknown"})),
        Ok(())
    );
    let refused = fixture
        .registry
        .cancel(&caller(TOKEN_B), &json!({"requestId": "r1"}))
        .unwrap_err();
    assert_eq!(refused.name(), "DesktopRuntimeResolutionOwnershipError");
    assert_eq!(
        refused.message(),
        "Desktop runtime resolution r1 belongs to another renderer document"
    );
    assert_eq!(pending_count(&fixture.registry), 1);

    fixture
        .registry
        .cancel(&caller(TOKEN_A), &json!({"requestId": " r1 "}))
        .unwrap();
    let error = task.await.unwrap().unwrap_err();
    assert_eq!(error.name(), "DesktopRuntimeDiscoveryFailedError");
    assert_eq!(pending_count(&fixture.registry), 0);
}

#[tokio::test]
async fn navigating_away_aborts_the_document_resolutions() {
    let fixture = fixture();
    let server = hanging_server().await;
    let task = start_hanging(&fixture, &server, TOKEN_A, "r1").await;
    navigate(&fixture.documents, TOKEN_B);
    assert_eq!(pending_count(&fixture.registry), 0);
    let error = task.await.unwrap().unwrap_err();
    assert!(
        error
            .message()
            .contains("request was aborted by its caller")
    );
    // The id is free again for the next document.
    let next = start_hanging(&fixture, &server, TOKEN_B, "r1").await;
    fixture.registry.cleanup();
    assert!(next.await.unwrap().is_err());
}
