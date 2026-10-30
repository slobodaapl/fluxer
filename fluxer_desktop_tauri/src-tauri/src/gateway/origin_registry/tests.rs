// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::FutureExt;
use tokio::sync::Notify;

use super::*;
use crate::documents::DocumentOwner;
use crate::runtime::test_support::{TOKEN_A, TOKEN_B, caller, loaded_documents, navigate};

const TARGET: &str = "wss://gateway.chat.example/?v=9";
const TARGET_ORIGIN: &str = "wss://gateway.chat.example";

#[derive(Default)]
struct Instances {
    /// Anchor origin to the gateway endpoint its discovery advertises.
    advertised: Mutex<HashMap<String, String>>,
    known: Mutex<Vec<String>>,
    calls: Mutex<Vec<String>>,
    /// When set, every resolution waits for a notification.
    gate: Mutex<Option<Arc<Notify>>>,
    offset: Mutex<Duration>,
}

impl Instances {
    fn advertise(&self, anchor: &str, gateway: &str) {
        self.advertised
            .lock()
            .unwrap()
            .insert(anchor.to_owned(), gateway.to_owned());
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn advance(&self, by: Duration) {
        *self.offset.lock().unwrap() += by;
    }
}

fn registry(instances: &Arc<Instances>) -> GatewayOriginRegistry {
    let start = Instant::now();
    let (resolving, known, clock) = (instances.clone(), instances.clone(), instances.clone());
    GatewayOriginRegistry::new(
        Box::new(move |anchor| {
            let instances = resolving.clone();
            async move {
                instances.calls.lock().unwrap().push(anchor.clone());
                let gate = instances.gate.lock().unwrap().clone();
                if let Some(gate) = gate {
                    gate.notified().await;
                }
                instances
                    .advertised
                    .lock()
                    .unwrap()
                    .get(&anchor)
                    .cloned()
                    .ok_or_else(|| "unreachable".to_owned())
            }
            .boxed()
        }),
        Box::new(move || {
            let known = known.known.lock().unwrap().clone();
            async move { known }.boxed()
        }),
        Box::new(move || start + *clock.offset.lock().unwrap()),
    )
}

fn owner(token: &str) -> (Arc<crate::documents::DocumentRegistry>, DocumentOwner) {
    let documents = loaded_documents();
    if token != TOKEN_A {
        navigate(&documents, token);
    }
    let owner = documents.capture(&caller(token), "Test").unwrap();
    (documents, owner)
}

fn refused(reason: &'static str) -> Result<String, OriginNotAdmitted> {
    Err(OriginNotAdmitted(reason))
}

#[tokio::test]
async fn admits_a_gateway_its_own_origin_advertises_and_caches_the_admission() {
    let instances = Arc::new(Instances::default());
    instances.advertise(
        "https://gateway.chat.example",
        "wss://gateway.chat.example/gateway",
    );
    let registry = registry(&instances);
    let (_documents, document) = owner(TOKEN_A);

    assert_eq!(
        registry.admit(&document, TARGET).await.unwrap(),
        TARGET_ORIGIN
    );
    assert_eq!(
        registry.admit(&document, TARGET).await.unwrap(),
        TARGET_ORIGIN
    );
    assert_eq!(instances.calls(), vec!["https://gateway.chat.example"]);
    assert_eq!(registry.state().documents[&document].registrations, 1);

    // The admission lasts six hours, then the origin must be registered again.
    instances.advance(ADMISSION_TTL);
    assert_eq!(
        registry.admit(&document, TARGET).await.unwrap(),
        TARGET_ORIGIN
    );
    assert_eq!(registry.state().documents[&document].registrations, 2);
}

#[tokio::test]
async fn walks_seeded_then_known_anchors_then_the_target_in_order() {
    let instances = Arc::new(Instances::default());
    *instances.known.lock().unwrap() = vec![
        "chat.example".to_owned(),
        "https://seeded.example".to_owned(),
        "not an origin/".to_owned(),
    ];
    instances.advertise("https://chat.example", "wss://gateway.chat.example");
    let registry = registry(&instances);
    registry
        .state()
        .seed_anchor_origin(anchor_origin("https://seeded.example/api").unwrap());
    let (_documents, document) = owner(TOKEN_A);

    assert!(registry.admit(&document, TARGET).await.is_ok());
    assert_eq!(
        instances.calls(),
        vec!["https://seeded.example", "https://chat.example"]
    );

    let (_other_documents, other) = owner(TOKEN_B);
    assert_eq!(
        registry.admit(&other, "wss://elsewhere.example/").await,
        refused("no reachable instance advertises it as its gateway")
    );
    // Cached resolutions are reused; only the target's own origin is new.
    assert_eq!(
        instances.calls()[2..],
        ["https://elsewhere.example".to_owned()]
    );
}

#[tokio::test]
async fn a_recorded_plan_admits_its_gateway_without_discovery() {
    let instances = Arc::new(Instances::default());
    let registry = registry(&instances);
    registry.record_anchor_gateway("https://chat.example/api", "wss://gateway.chat.example/gw");
    let (_documents, document) = owner(TOKEN_A);

    assert!(registry.admit(&document, TARGET).await.is_ok());
    assert!(instances.calls().is_empty());

    registry.cleanup();
    let (_other_documents, other) = owner(TOKEN_B);
    assert!(registry.admit(&other, TARGET).await.is_err());
    assert_eq!(
        instances.calls(),
        vec!["https://chat.example", "https://gateway.chat.example"]
    );
}

#[tokio::test]
async fn failed_resolutions_are_retried_after_a_minute() {
    let instances = Arc::new(Instances::default());
    let registry = registry(&instances);
    let (_documents, document) = owner(TOKEN_A);

    assert!(registry.admit(&document, TARGET).await.is_err());
    instances.advance(Duration::from_secs(1));
    assert!(registry.admit(&document, TARGET).await.is_err());
    assert_eq!(instances.calls().len(), 1);

    instances.advance(DISCOVERY_FAILURE_TTL);
    instances.advertise("https://gateway.chat.example", TARGET);
    assert!(registry.admit(&document, TARGET).await.is_ok());
    assert_eq!(instances.calls().len(), 2);
}

#[tokio::test]
async fn an_origin_is_attempted_at_most_once_a_second() {
    let instances = Arc::new(Instances::default());
    let registry = registry(&instances);
    let (_documents, document) = owner(TOKEN_A);

    assert!(registry.admit(&document, TARGET).await.is_err());
    instances.advance(Duration::from_millis(999));
    assert_eq!(
        registry.admit(&document, TARGET).await,
        refused("this origin was attempted too recently")
    );
    instances.advance(Duration::from_millis(1));
    assert_eq!(
        registry.admit(&document, TARGET).await,
        refused("no reachable instance advertises it as its gateway")
    );
}

#[tokio::test]
async fn a_document_registers_at_most_eight_origins_a_minute() {
    let instances = Arc::new(Instances::default());
    let registry = registry(&instances);
    let (_documents, document) = owner(TOKEN_A);
    let target = |index: usize| format!("wss://g{index}.example/");

    for index in 0..8 {
        assert!(registry.admit(&document, &target(index)).await.is_err());
    }
    // Failed registrations stay charged.
    assert_eq!(
        registry.admit(&document, &target(8)).await,
        refused("this document registered too many origins in the last minute")
    );
    instances.advance(REGISTRATION_WINDOW);
    assert_eq!(
        registry.admit(&document, &target(8)).await,
        refused("no reachable instance advertises it as its gateway")
    );
}

#[tokio::test]
async fn a_document_registers_at_most_sixty_four_origins_until_released() {
    let instances = Arc::new(Instances::default());
    let registry = registry(&instances);
    let (_documents, document) = owner(TOKEN_A);

    for index in 0..64 {
        if index % 8 == 0 {
            instances.advance(REGISTRATION_WINDOW);
        }
        assert!(
            registry
                .admit(&document, &format!("wss://g{index}.example/"))
                .await
                .is_err()
        );
    }
    instances.advance(REGISTRATION_WINDOW);
    assert_eq!(
        registry.admit(&document, "wss://g64.example/").await,
        refused("this document exhausted its origin registration budget")
    );

    registry.release_document(&document);
    assert_eq!(
        registry.admit(&document, "wss://g64.example/").await,
        refused("no reachable instance advertises it as its gateway")
    );
}

#[tokio::test]
async fn concurrent_admissions_share_one_registration_and_resolution() {
    let instances = Arc::new(Instances::default());
    let gate = Arc::new(Notify::new());
    *instances.gate.lock().unwrap() = Some(gate.clone());
    instances.advertise("https://gateway.chat.example", TARGET);
    let registry = registry(&instances);
    let (_documents, document) = owner(TOKEN_A);

    let first = registry.admit(&document, TARGET);
    let second = registry.admit(&document, TARGET);
    let release = async {
        while instances.calls().is_empty() {
            tokio::task::yield_now().await;
        }
        gate.notify_one();
    };
    let (first, second, ()) = tokio::join!(first, second, release);
    assert_eq!(first.unwrap(), TARGET_ORIGIN);
    assert_eq!(second.unwrap(), TARGET_ORIGIN);
    assert_eq!(instances.calls().len(), 1);
    let state = registry.state();
    assert_eq!(state.documents[&document].registrations, 1);
    assert!(state.documents[&document].in_flight.is_empty());
    assert!(state.anchor_in_flight.is_empty());
}

#[tokio::test]
async fn the_oldest_resolution_is_evicted_past_the_cache_limit() {
    let instances = Arc::new(Instances::default());
    let registry = registry(&instances);
    for index in 0..=RESOLUTION_CACHE_LIMIT {
        registry.record_anchor_gateway(
            &format!("https://a{index}.example"),
            &format!("wss://g{index}.example"),
        );
    }
    instances.advertise("https://a0.example", "wss://g0.example");
    instances.advertise("https://a1.example", "wss://g1.example");
    let (_documents, document) = owner(TOKEN_A);

    assert!(registry.admit(&document, "wss://g1.example/").await.is_ok());
    assert!(instances.calls().is_empty());
    assert!(registry.admit(&document, "wss://g0.example/").await.is_ok());
    assert_eq!(instances.calls(), vec!["https://a0.example"]);
}

#[tokio::test]
async fn the_oldest_anchor_is_dropped_past_the_anchor_limit() {
    let instances = Arc::new(Instances::default());
    let registry = registry(&instances);
    for index in 0..=ANCHOR_LIMIT {
        registry
            .state()
            .seed_anchor_origin(format!("https://a{index}.example"));
    }
    // Re-seeding moves an anchor to the back instead of duplicating it.
    registry
        .state()
        .seed_anchor_origin("https://a1.example".to_owned());
    let candidates = registry.anchor_candidates(TARGET).await;
    assert_eq!(candidates.len(), ANCHOR_LIMIT + 1);
    assert_eq!(candidates[0], "https://a2.example");
    assert_eq!(candidates[ANCHOR_LIMIT - 1], "https://a1.example");
    assert_eq!(candidates[ANCHOR_LIMIT], "https://gateway.chat.example");
    assert!(!candidates.contains(&"https://a0.example".to_owned()));
}

#[tokio::test]
async fn anchored_admission_requires_the_account_instance_to_advertise_the_gateway() {
    let instances = Arc::new(Instances::default());
    instances.advertise("https://chat.example", "wss://gateway.chat.example");
    let registry = registry(&instances);

    assert_eq!(
        registry
            .admit_anchored(TARGET, "https://chat.example/api")
            .await
            .unwrap(),
        TARGET_ORIGIN
    );
    assert_eq!(
        registry
            .admit_anchored("wss://other.example/", "chat.example")
            .await,
        refused("the account instance does not advertise it as its gateway")
    );
    assert_eq!(
        registry
            .admit_anchored(TARGET, "https://down.example")
            .await,
        refused("could not reach the account instance to confirm its gateway")
    );
    assert_eq!(
        registry.admit_anchored(TARGET, "").await,
        refused("the account has no instance origin")
    );
    assert_eq!(
        registry.admit_anchored("not a url", "chat.example").await,
        refused("the target has no WebSocket origin")
    );
}
