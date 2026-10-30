// SPDX-License-Identifier: AGPL-3.0-or-later

//! Which gateway origins a renderer document may open native sockets to, ported from
//! `fluxer_desktop/src/main/GatewayOriginRegistry.ts`.
//!
//! A gateway origin is admitted when an instance (an anchor) advertises it as its gateway.
//! Anchors come from runtime plans (`record_anchor_gateway`), known instances, and the
//! target's own http(s) origin. Each document has a registration budget, charged before
//! resolution and never refunded.
//!
//! Electron keyed document state by `WebFrameMain` in a `WeakMap`; here it is keyed by
//! [`DocumentOwner`] and the transport releases it when the document goes away.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use futures_util::future::{BoxFuture, FutureExt, Shared};

use super::websocket_origin::{websocket_http_origin, websocket_origin};
use crate::documents::DocumentOwner;
use crate::error::BridgeError;
use crate::net::origin::normalize_http_network_origin;

const ADMISSION_TTL: Duration = Duration::from_secs(6 * 60 * 60);
const DISCOVERY_FAILURE_TTL: Duration = Duration::from_secs(60);
const REGISTRATIONS_PER_WINDOW: usize = 8;
const REGISTRATION_WINDOW: Duration = Duration::from_secs(60);
const REGISTRATIONS_PER_DOCUMENT: u32 = 64;
const REGISTRATION_MIN_INTERVAL: Duration = Duration::from_secs(1);
const ANCHOR_LIMIT: usize = 1024;
const RESOLUTION_CACHE_LIMIT: usize = 256;
const NOT_ADMITTED_ERROR: &str = "GatewayOriginNotAdmittedError";

/// `resolveAnchorGatewayOrigin`: the gateway endpoint an anchor's discovery advertises.
pub type ResolveAnchor =
    Box<dyn Fn(String) -> BoxFuture<'static, Result<String, String>> + Send + Sync>;
/// `additionalAnchorOrigins`: further anchor values, normalised by the registry.
pub type AdditionalAnchors = Box<dyn Fn() -> BoxFuture<'static, Vec<String>> + Send + Sync>;
pub type Clock = Box<dyn Fn() -> Instant + Send + Sync>;

/// `GatewayOriginNotAdmittedError`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OriginNotAdmitted(pub &'static str);

impl From<OriginNotAdmitted> for BridgeError {
    fn from(error: OriginNotAdmitted) -> Self {
        BridgeError::new(
            NOT_ADMITTED_ERROR,
            format!(
                "Gateway origin is not admitted for this renderer document: {}",
                error.0
            ),
        )
    }
}

type SharedResolution = Shared<BoxFuture<'static, Option<String>>>;
type SharedAdmission = Shared<BoxFuture<'static, Result<String, OriginNotAdmitted>>>;

struct AnchorResolution {
    expires_at: Instant,
    gateway_origin: Option<String>,
    /// Insertion order; the smallest is evicted first.
    sequence: u64,
}

#[derive(Default)]
struct DocumentState {
    admitted: HashMap<String, Instant>,
    in_flight: HashMap<String, SharedAdmission>,
    registration_times: VecDeque<Instant>,
    last_attempt_per_origin: HashMap<String, Instant>,
    registrations: u32,
}

impl DocumentState {
    /// `consumeRegistrationBudget`.
    fn consume_registration_budget(
        &mut self,
        target_origin: &str,
        now: Instant,
    ) -> Result<(), OriginNotAdmitted> {
        if self.registrations >= REGISTRATIONS_PER_DOCUMENT {
            return Err(OriginNotAdmitted(
                "this document exhausted its origin registration budget",
            ));
        }
        if self
            .last_attempt_per_origin
            .get(target_origin)
            .is_some_and(|&last| now.duration_since(last) < REGISTRATION_MIN_INTERVAL)
        {
            return Err(OriginNotAdmitted("this origin was attempted too recently"));
        }
        while self
            .registration_times
            .front()
            .is_some_and(|&first| now.duration_since(first) >= REGISTRATION_WINDOW)
        {
            self.registration_times.pop_front();
        }
        if self.registration_times.len() >= REGISTRATIONS_PER_WINDOW {
            return Err(OriginNotAdmitted(
                "this document registered too many origins in the last minute",
            ));
        }
        self.registration_times.push_back(now);
        self.last_attempt_per_origin
            .insert(target_origin.to_owned(), now);
        self.registrations += 1;
        Ok(())
    }
}

#[derive(Default)]
struct State {
    /// Insertion-ordered set; re-seeding moves an origin to the back.
    anchor_origins: Vec<String>,
    anchor_resolutions: HashMap<String, AnchorResolution>,
    next_sequence: u64,
    anchor_in_flight: HashMap<String, SharedResolution>,
    documents: HashMap<DocumentOwner, DocumentState>,
}

impl State {
    fn seed_anchor_origin(&mut self, origin: String) {
        self.anchor_origins.retain(|existing| *existing != origin);
        self.anchor_origins.push(origin);
        if self.anchor_origins.len() > ANCHOR_LIMIT {
            let excess = self.anchor_origins.len() - ANCHOR_LIMIT;
            self.anchor_origins.drain(..excess);
        }
    }

    fn store_anchor_resolution(
        &mut self,
        candidate: String,
        gateway_origin: Option<String>,
        now: Instant,
    ) {
        let ttl = if gateway_origin.is_some() {
            ADMISSION_TTL
        } else {
            DISCOVERY_FAILURE_TTL
        };
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        self.anchor_resolutions.insert(
            candidate,
            AnchorResolution {
                expires_at: now + ttl,
                gateway_origin,
                sequence,
            },
        );
        while self.anchor_resolutions.len() > RESOLUTION_CACHE_LIMIT {
            let oldest = self
                .anchor_resolutions
                .iter()
                .min_by_key(|(_, resolution)| resolution.sequence)
                .map(|(candidate, _)| candidate.clone())
                .expect("an over-full cache has an oldest entry");
            self.anchor_resolutions.remove(&oldest);
        }
    }

    fn has_cached_anchor_for(&self, target_origin: &str, now: Instant) -> bool {
        self.anchor_origins.iter().any(|candidate| {
            self.anchor_resolutions
                .get(candidate)
                .is_some_and(|cached| {
                    cached.expires_at > now
                        && cached.gateway_origin.as_deref() == Some(target_origin)
                })
        })
    }
}

/// `anchorOrigin`.
fn anchor_origin(value: &str) -> Option<String> {
    normalize_http_network_origin(value).or_else(|| websocket_http_origin(value))
}

struct Inner {
    resolve_anchor_gateway: ResolveAnchor,
    additional_anchor_origins: AdditionalAnchors,
    now: Clock,
    state: Mutex<State>,
}

#[derive(Clone)]
pub struct GatewayOriginRegistry {
    inner: Arc<Inner>,
}

impl GatewayOriginRegistry {
    pub fn new(
        resolve_anchor_gateway: ResolveAnchor,
        additional_anchor_origins: AdditionalAnchors,
        now: Clock,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                resolve_anchor_gateway,
                additional_anchor_origins,
                now,
                state: Mutex::default(),
            }),
        }
    }

    /// `recordAnchorGateway`: a runtime plan's API endpoint vouches for its gateway.
    pub fn record_anchor_gateway(&self, anchor_value: &str, gateway_endpoint: &str) {
        let Some(origin) = anchor_origin(anchor_value) else {
            return;
        };
        let now = self.now();
        let mut state = self.state();
        state.seed_anchor_origin(origin.clone());
        if let Some(gateway_origin) = websocket_origin(gateway_endpoint) {
            state.store_anchor_resolution(origin, Some(gateway_origin), now);
        }
    }

    pub fn release_document(&self, document: &DocumentOwner) {
        self.state().documents.remove(document);
    }

    #[cfg(test)]
    pub fn tracks_document(&self, document: &DocumentOwner) -> bool {
        self.state().documents.contains_key(document)
    }

    pub fn cleanup(&self) {
        let mut state = self.state();
        state.anchor_resolutions.clear();
        state.anchor_in_flight.clear();
    }

    /// `admit`: resolves to the admitted ws(s) origin of `url` for `document`.
    pub async fn admit(
        &self,
        document: &DocumentOwner,
        url: &str,
    ) -> Result<String, OriginNotAdmitted> {
        let target_origin =
            websocket_origin(url).ok_or(OriginNotAdmitted("the target has no WebSocket origin"))?;
        let admission = {
            let now = self.now();
            let mut state = self.state();
            let document_state = state.documents.entry(document.clone()).or_default();
            if document_state
                .admitted
                .get(&target_origin)
                .is_some_and(|&until| until > now)
            {
                return Ok(target_origin);
            }
            if let Some(in_flight) = document_state.in_flight.get(&target_origin) {
                in_flight.clone()
            } else {
                document_state.admitted.remove(&target_origin);
                document_state.consume_registration_budget(&target_origin, now)?;
                let admission = self
                    .clone()
                    .resolve_admission(document.clone(), target_origin.clone(), url.to_owned())
                    .boxed()
                    .shared();
                document_state
                    .in_flight
                    .insert(target_origin, admission.clone());
                admission
            }
        };
        admission.await
    }

    /// `admitAnchored`: `url` is the gateway the account's instance (`anchor`) advertises.
    /// Its consumer, the account engine, is not ported yet.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the account engine port calls it")
    )]
    pub async fn admit_anchored(
        &self,
        url: &str,
        anchor: &str,
    ) -> Result<String, OriginNotAdmitted> {
        let target_origin =
            websocket_origin(url).ok_or(OriginNotAdmitted("the target has no WebSocket origin"))?;
        let anchor_value =
            anchor_origin(anchor).ok_or(OriginNotAdmitted("the account has no instance origin"))?;
        self.state().seed_anchor_origin(anchor_value.clone());
        match self.resolve_anchor(anchor_value).await {
            None => Err(OriginNotAdmitted(
                "could not reach the account instance to confirm its gateway",
            )),
            Some(gateway_origin) if gateway_origin != target_origin => Err(OriginNotAdmitted(
                "the account instance does not advertise it as its gateway",
            )),
            Some(_) => Ok(target_origin),
        }
    }

    async fn resolve_admission(
        self,
        document: DocumentOwner,
        target_origin: String,
        url: String,
    ) -> Result<String, OriginNotAdmitted> {
        let result = self.find_advertising_anchor(&target_origin, &url).await;
        let now = self.now();
        let mut state = self.state();
        // A released document's state is gone; the admission still answers its caller.
        if let Some(document_state) = state.documents.get_mut(&document) {
            document_state.in_flight.remove(&target_origin);
            if result.is_ok() {
                document_state
                    .admitted
                    .insert(target_origin.clone(), now + ADMISSION_TTL);
            }
        }
        result.map(|()| target_origin)
    }

    async fn find_advertising_anchor(
        &self,
        target_origin: &str,
        url: &str,
    ) -> Result<(), OriginNotAdmitted> {
        if self
            .state()
            .has_cached_anchor_for(target_origin, self.now())
        {
            return Ok(());
        }
        for candidate in self.anchor_candidates(url).await {
            if self.resolve_anchor(candidate).await.as_deref() == Some(target_origin) {
                return Ok(());
            }
        }
        Err(OriginNotAdmitted(
            "no reachable instance advertises it as its gateway",
        ))
    }

    /// Seeded anchors, then additional anchors up to the anchor limit, then the target's own
    /// http(s) origin, without duplicates.
    async fn anchor_candidates(&self, url: &str) -> Vec<String> {
        let mut candidates = self.state().anchor_origins.clone();
        let mut seen: HashSet<String> = candidates.iter().cloned().collect();
        for value in (self.inner.additional_anchor_origins)().await {
            if let Some(origin) = anchor_origin(&value)
                && seen.insert(origin.clone())
            {
                candidates.push(origin);
            }
            if candidates.len() >= ANCHOR_LIMIT {
                break;
            }
        }
        if let Some(origin) = websocket_http_origin(url)
            && seen.insert(origin.clone())
        {
            candidates.push(origin);
        }
        candidates
    }

    /// `resolveAnchor`: the gateway origin `candidate` advertises, cached for its TTL and
    /// shared between concurrent callers. Failures resolve to `None`.
    async fn resolve_anchor(&self, candidate: String) -> Option<String> {
        let resolution = {
            let now = self.now();
            let mut state = self.state();
            if let Some(cached) = state.anchor_resolutions.get(&candidate)
                && cached.expires_at > now
            {
                return cached.gateway_origin.clone();
            }
            if let Some(in_flight) = state.anchor_in_flight.get(&candidate) {
                in_flight.clone()
            } else {
                let registry = self.clone();
                let key = candidate.clone();
                let resolution = async move {
                    let gateway_origin =
                        match (registry.inner.resolve_anchor_gateway)(key.clone()).await {
                            Ok(endpoint) => websocket_origin(&endpoint),
                            Err(reason) => {
                                tracing::debug!(anchor = %key, %reason, "gateway anchor did not resolve");
                                None
                            }
                        };
                    let now = registry.now();
                    let mut state = registry.state();
                    state.store_anchor_resolution(key.clone(), gateway_origin.clone(), now);
                    state.anchor_in_flight.remove(&key);
                    gateway_origin
                }
                .boxed()
                .shared();
                state.anchor_in_flight.insert(candidate, resolution.clone());
                resolution
            }
        };
        resolution.await
    }

    fn now(&self) -> Instant {
        (self.inner.now)()
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests;
