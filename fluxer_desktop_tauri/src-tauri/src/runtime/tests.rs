// SPDX-License-Identifier: AGPL-3.0-or-later

use serde_json::json;

use super::plans::trusted_http_origins;
use super::test_support::{
    MemoryMarkers, TOKEN_A, cached_plan, caller, loaded_documents, loopback_plan, outbound,
};
use super::*;

fn config() -> RuntimeConfig {
    RuntimeConfig::new(
        outbound(),
        Arc::new(MemoryMarkers::default()).markers(),
        loaded_documents(),
    )
}

#[tokio::test]
async fn plans_are_checked_against_the_exposed_outbound_client() {
    let config = config();
    let plan = Arc::new(loopback_plan(5100));
    assert!(config.plans().cache(plan.clone()).is_err());
    config
        .outbound()
        .register_anchored_origins("http://127.0.0.1:5100", &trusted_http_origins(&plan), None)
        .await
        .unwrap();
    config.plans().cache(plan.clone()).unwrap();
    assert_eq!(
        config.plans().find_plan_for_route(&plan.instance_key),
        Some(plan)
    );
}

#[tokio::test]
async fn cleanup_rolls_back_a_provisional_commit() {
    let config = config();
    let plan = cached_plan(&config.plans(), 5101).await;
    let owner = caller(TOKEN_A);
    let key = plan.instance_key.clone();
    config
        .prepare(&owner, &json!({"preparationId": "a", "instanceKey": key}))
        .await
        .unwrap();
    config
        .commit(
            &owner,
            &json!({"preparationId": "a", "instanceKey": key, "baseRevision": 0}),
        )
        .unwrap();
    assert_eq!(config.plans().active_plan(), Some(plan));
    config.cleanup();
    assert_eq!(config.plans().active_plan(), None);
    let abort = config
        .abort(&owner, &json!({"preparationId": "a"}))
        .unwrap();
    assert_eq!(
        serde_json::to_value(abort).unwrap(),
        json!({"disposition": "absent", "revision": 0, "activeInstanceKey": null})
    );
}
