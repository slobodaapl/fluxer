// SPDX-License-Identifier: AGPL-3.0-or-later

use std::sync::atomic::Ordering;

use super::super::test_support::{
    MemoryMarkers, cached_plan, document, loopback_plan, outbound, plans_with,
};
use super::*;

#[test]
fn instance_keys_lowercase_the_origin_and_drop_trailing_slashes() {
    assert_eq!(
        instance_key(" HTTPS://Chat.Example.TEST:443/Api//\n").as_deref(),
        Some("https://chat.example.test/Api")
    );
    assert_eq!(
        instance_key("http://127.0.0.1:8080/").as_deref(),
        Some("http://127.0.0.1:8080")
    );
    // An empty query or fragment serializes to nothing, as in the WHATWG getters.
    assert_eq!(
        instance_key("https://x.test/api?#").as_deref(),
        Some("https://x.test/api")
    );
    for refused in [
        "https://x.test/api?v=1",
        "https://x.test/api#top",
        "https://user@x.test/api",
        "https://:pw@x.test/api",
        "wss://x.test/api",
        "not a url",
    ] {
        assert_eq!(instance_key(refused), None, "{refused}");
    }
}

#[test]
fn trusted_origins_cover_every_http_endpoint_and_the_gateway_once() {
    let mut plan = loopback_plan(1);
    plan.endpoints = RuntimeEndpoints {
        api: "https://api.example.test/v1".to_owned(),
        api_public: "https://api.example.test/public".to_owned(),
        webapp: "https://web.example.test".to_owned(),
        media: "https://media.example.test:8443/m".to_owned(),
        static_cdn: "".to_owned(),
        upload_relay: Some("http://relay.example.test/".to_owned()),
        gateway: "wss://gateway.example.test:443/ws".to_owned(),
    };
    assert_eq!(
        trusted_http_origins(&plan),
        [
            "http://relay.example.test",
            "https://api.example.test",
            "https://gateway.example.test",
            "https://media.example.test:8443",
            "https://web.example.test",
        ]
    );
    plan.endpoints.gateway = "ws://gateway.example.test:9000".to_owned();
    assert!(trusted_http_origins(&plan).contains(&"http://gateway.example.test:9000".to_owned()));
}

#[test]
fn projection_routes_the_api_through_the_local_app_origin() {
    let plan = loopback_plan(4100);
    let projected = project(&plan);
    assert_eq!(projected.instance_key, "http://127.0.0.1:4100/api");
    assert_eq!(
        projected.api_endpoint,
        format!("{}/api/http%3A%2F%2F127.0.0.1%3A4100%2Fapi", origin())
    );
    assert_eq!(projected.remote_api_endpoint, "http://127.0.0.1:4100/api");
    assert_eq!(projected.document, plan.document);
    let serialized = serde_json::to_value(&projected).unwrap();
    assert_eq!(
        serialized.as_object().unwrap().keys().collect::<Vec<_>>(),
        [
            "instanceKey",
            "apiEndpoint",
            "remoteApiEndpoint",
            "document"
        ]
    );
}

#[test]
fn plans_come_from_projected_discovery() {
    let document = document("http://127.0.0.1:4200");
    let parsed = crate::instance::discovery::parse_instance_discovery_document(&document).unwrap();
    let projected = crate::instance::discovery::project_client_runtime_discovery(&parsed).unwrap();
    let plan = runtime_plan_from_discovery(document.clone(), &projected).unwrap();
    assert_eq!(plan, loopback_plan(4200));

    let mut unusable = projected;
    unusable.endpoints.api_client = "http://127.0.0.1:4200/api?x=1".to_owned();
    let error = runtime_plan_from_discovery(document, &unusable).unwrap_err();
    assert_eq!(error.name(), "InvalidLocalAppRuntimeAPIEndpointError");
    assert_eq!(
        error.message(),
        "Instance discovery advertised an unusable API endpoint: http://127.0.0.1:4200/api?x=1"
    );
}

fn plans() -> Arc<RuntimePlans> {
    plans_with(outbound(), Arc::new(MemoryMarkers::default()).markers())
}

#[tokio::test]
async fn the_cache_keeps_the_256_most_recently_used_plans() {
    let plans = plans();
    for port in 1..=256 {
        cached_plan(&plans, port).await;
    }
    let first = "http://127.0.0.1:1/api";
    assert!(plans.find_plan_for_route(first).is_some());
    cached_plan(&plans, 257).await;
    assert!(
        plans.find_plan_for_route(first).is_some(),
        "a lookup refreshes the plan"
    );
    assert!(
        plans
            .find_plan_for_route("http://127.0.0.1:2/api")
            .is_none(),
        "the least recently used plan is evicted"
    );
    assert!(
        plans
            .find_plan_for_route("http://127.0.0.1:257/api")
            .is_some()
    );

    // Re-caching a present plan refreshes it instead of evicting another.
    cached_plan(&plans, 3).await;
    cached_plan(&plans, 258).await;
    assert!(
        plans
            .find_plan_for_route("http://127.0.0.1:3/api")
            .is_some()
    );
    assert!(
        plans
            .find_plan_for_route("http://127.0.0.1:4/api")
            .is_none()
    );
}

#[tokio::test]
async fn plans_need_registered_origins() {
    let plans = plans();
    let error = plans.cache(Arc::new(loopback_plan(4300))).unwrap_err();
    assert_eq!(error.name(), "DesktopOutboundHTTPOriginNotRegisteredError");
    let error = plans.activate(Arc::new(loopback_plan(4300))).unwrap_err();
    assert_eq!(error.name(), "DesktopOutboundHTTPOriginNotRegisteredError");
    assert!(
        plans
            .find_plan_for_route("http://127.0.0.1:4300/api")
            .is_none()
    );
    assert!(plans.active_plan().is_none());
}

#[tokio::test]
async fn activation_caches_and_deactivation_keeps_the_plan_cached() {
    let plans = plans();
    let plan = cached_plan(&plans, 4400).await;
    plans.activate(plan.clone()).unwrap();
    assert_eq!(plans.active_plan(), Some(plan.clone()));
    plans.deactivate().unwrap();
    assert_eq!(plans.active_plan(), None);
    assert_eq!(plans.find_plan_for_route(&plan.instance_key), Some(plan));
}

#[tokio::test]
async fn shutdown_refuses_plan_changes_and_cancels_child_tokens() {
    let plans = plans();
    let plan = cached_plan(&plans, 4500).await;
    let token = plans.shutdown_child_token();
    plans.shutdown();
    assert!(token.is_cancelled());
    for error in [
        plans.cache(plan.clone()).unwrap_err(),
        plans.activate(plan.clone()).unwrap_err(),
        plans.deactivate().unwrap_err(),
    ] {
        assert_eq!(error.name(), "LocalAppProtocolShutdownAdmissionError");
        assert_eq!(error.message(), "Local app protocol is shutting down");
    }
}

fn remember(markers: &MemoryMarkers, api_endpoint: &str, base: &str) {
    markers.values.lock().unwrap().insert(
        format!("runtime_discovery.v1:{api_endpoint}"),
        json_marker(base),
    );
}

fn json_marker(base: &str) -> String {
    serde_json::json!({
        "url": format!("{base}/.well-known/fluxer"),
        "document": document(base),
        "anchorRequirement": "any",
    })
    .to_string()
}

#[tokio::test]
async fn routes_restore_a_remembered_plan_once_without_activating_it() {
    let markers = Arc::new(MemoryMarkers::default());
    let plans = plans_with(outbound(), markers.markers());
    let base = "http://127.0.0.1:4600";
    let key = format!("{base}/api");
    remember(&markers, &key, base);

    let (first, second) = tokio::join!(plans.plan_for_route(&key), plans.plan_for_route(&key));
    let first = first.unwrap().unwrap();
    assert_eq!(*first, loopback_plan(4600));
    assert!(Arc::ptr_eq(&first, &second.unwrap().unwrap()));
    assert_eq!(
        markers.reads.load(Ordering::SeqCst),
        1,
        "one restore for both"
    );
    assert_eq!(plans.active_plan(), None);
    assert_eq!(plans.find_plan_for_route(&key), Some(first));
    plans
        .discovery()
        .outbound
        .require_registered_origins(&[base.to_owned()])
        .unwrap();
}

#[tokio::test]
async fn routes_without_a_matching_remembered_plan_resolve_to_none() {
    let markers = Arc::new(MemoryMarkers::default());
    let plans = plans_with(outbound(), markers.markers());
    assert_eq!(
        plans.plan_for_route("http://127.0.0.1:4700/api").await,
        Ok(None)
    );
    // A marker whose document belongs to another instance key is ignored.
    remember(
        &markers,
        "http://127.0.0.1:4701/api",
        "http://127.0.0.1:4702",
    );
    assert_eq!(
        plans.plan_for_route("http://127.0.0.1:4701/api").await,
        Ok(None)
    );
    // Unparseable markers are ignored too.
    markers.values.lock().unwrap().insert(
        "runtime_discovery.v1:http://127.0.0.1:4703/api".to_owned(),
        "{".to_owned(),
    );
    assert_eq!(
        plans.plan_for_route("http://127.0.0.1:4703/api").await,
        Ok(None)
    );
    assert_eq!(plans.plan_for_route("not a key").await, Ok(None));
}
