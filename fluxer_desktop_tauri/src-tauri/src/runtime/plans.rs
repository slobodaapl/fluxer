// SPDX-License-Identifier: AGPL-3.0-or-later

//! Local app runtime plans, ported from LocalAppRuntimePlans.ts, DesktopRuntimePlanProjection.ts
//! and the plan half of LocalAppProtocol.ts (cache, activation, remembered restore, admission).
//!
//! Not ported: `recordGatewayOriginAnchor`, which the gateway origin registry unit owns.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use futures_util::FutureExt;
use futures_util::future::{BoxFuture, Shared};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use serde::Serialize;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use url::Url;

use super::discovery::RuntimeDiscovery;
use super::errors::outbound_error;
use crate::error::BridgeError;
use crate::gateway::websocket_origin::websocket_http_origin;
use crate::instance::discovery::ClientRuntimeDiscovery;
use crate::js::js_trim;
use crate::local_app::url::{API_PATH_PREFIX, origin};

const RUNTIME_PLAN_CACHE_MAX_ENTRIES: usize = 256;

/// The characters `encodeURIComponent` leaves alone besides ASCII alphanumerics.
const URI_COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'!')
    .remove(b'~')
    .remove(b'*')
    .remove(b'\'')
    .remove(b'(')
    .remove(b')');

#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeEndpoints {
    /// The API endpoint the client talks to (`endpoints.api_client`).
    pub api: String,
    pub api_public: String,
    pub webapp: String,
    pub media: String,
    pub static_cdn: String,
    pub upload_relay: Option<String>,
    pub gateway: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LocalAppRuntimePlan {
    pub instance_key: String,
    /// The discovery document exactly as the instance served it.
    pub document: Value,
    pub endpoints: RuntimeEndpoints,
}

/// `DesktopRuntimePlan`, the renderer's view of a plan.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopRuntimePlan {
    pub instance_key: String,
    pub api_endpoint: String,
    pub remote_api_endpoint: String,
    pub document: Value,
}

/// `localAppRuntimeInstanceKey`: lowercased origin plus the path without trailing slashes, for
/// an http(s) endpoint without credentials, query or fragment.
pub fn instance_key(api_endpoint: &str) -> Option<String> {
    let url = Url::parse(js_trim(api_endpoint)).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some_and(|password| !password.is_empty())
        || url.query().is_some_and(|query| !query.is_empty())
        || url.fragment().is_some_and(|fragment| !fragment.is_empty())
    {
        return None;
    }
    let origin = url.origin().ascii_serialization().to_lowercase();
    Some(format!("{origin}{}", url.path().trim_end_matches('/')))
}

/// `localAppRuntimePlanFromDiscovery`, over an already parsed and projected document.
pub fn runtime_plan_from_discovery(
    document: Value,
    discovery: &ClientRuntimeDiscovery,
) -> Result<LocalAppRuntimePlan, BridgeError> {
    let endpoints = &discovery.endpoints;
    let instance_key = instance_key(&endpoints.api_client).ok_or_else(|| {
        BridgeError::new(
            "InvalidLocalAppRuntimeAPIEndpointError",
            format!(
                "Instance discovery advertised an unusable API endpoint: {}",
                endpoints.api_client
            ),
        )
    })?;
    Ok(LocalAppRuntimePlan {
        instance_key,
        document,
        endpoints: RuntimeEndpoints {
            api: endpoints.api_client.clone(),
            api_public: endpoints.api_public.clone(),
            webapp: endpoints.webapp.clone(),
            media: endpoints.media.clone(),
            static_cdn: endpoints.static_cdn.clone(),
            upload_relay: endpoints.upload_relay.clone(),
            gateway: endpoints.gateway.clone(),
        },
    })
}

/// `httpOriginSource`: the WHATWG origin of a non-empty http(s) URL.
pub fn http_origin_source(value: &str) -> Option<String> {
    let url = Url::parse(value).ok()?;
    matches!(url.scheme(), "http" | "https").then(|| url.origin().ascii_serialization())
}

/// `runtimePlanTrustedHTTPOrigins`: every http origin the plan talks to, deduplicated and sorted.
pub fn trusted_http_origins(plan: &LocalAppRuntimePlan) -> Vec<String> {
    let endpoints = &plan.endpoints;
    let mut origins: Vec<String> = [
        Some(endpoints.api.as_str()),
        Some(endpoints.api_public.as_str()),
        Some(endpoints.media.as_str()),
        endpoints.upload_relay.as_deref(),
        Some(endpoints.static_cdn.as_str()),
        Some(endpoints.webapp.as_str()),
    ]
    .into_iter()
    .flatten()
    .filter_map(http_origin_source)
    .chain(websocket_http_origin(&endpoints.gateway))
    .collect();
    origins.sort();
    origins.dedup();
    origins
}

/// `projectDesktopRuntimePlan`: the plan as the renderer reaches it through the local `/api` route.
pub fn project(plan: &LocalAppRuntimePlan) -> DesktopRuntimePlan {
    DesktopRuntimePlan {
        instance_key: plan.instance_key.clone(),
        api_endpoint: format!(
            "{}{API_PATH_PREFIX}/{}",
            origin(),
            utf8_percent_encode(&plan.instance_key, URI_COMPONENT)
        ),
        remote_api_endpoint: plan.endpoints.api.clone(),
        document: plan.document.clone(),
    }
}

fn shutdown_admission_error() -> BridgeError {
    BridgeError::new(
        "LocalAppProtocolShutdownAdmissionError",
        "Local app protocol is shutting down",
    )
}

/// Least recently used plans by instance key; a hit moves the plan to the back.
#[derive(Default)]
struct PlanCache {
    order: VecDeque<String>,
    plans: HashMap<String, Arc<LocalAppRuntimePlan>>,
}

impl PlanCache {
    fn touch(&mut self, key: &str) -> Option<Arc<LocalAppRuntimePlan>> {
        let plan = self.plans.get(key)?.clone();
        self.order.retain(|candidate| candidate != key);
        self.order.push_back(key.to_owned());
        Some(plan)
    }

    fn insert(&mut self, plan: Arc<LocalAppRuntimePlan>) {
        let key = plan.instance_key.clone();
        if self.plans.remove(&key).is_some() {
            self.order.retain(|candidate| *candidate != key);
        }
        if self.plans.len() >= RUNTIME_PLAN_CACHE_MAX_ENTRIES
            && let Some(oldest) = self.order.pop_front()
        {
            self.plans.remove(&oldest);
        }
        self.order.push_back(key.clone());
        self.plans.insert(key, plan);
    }
}

type Restoration =
    Shared<BoxFuture<'static, Result<Option<Arc<LocalAppRuntimePlan>>, BridgeError>>>;

struct PlansState {
    accepting: bool,
    active: Option<Arc<LocalAppRuntimePlan>>,
    cache: PlanCache,
    restorations: HashMap<String, Restoration>,
}

impl PlansState {
    fn find(&mut self, key: &str) -> Option<Arc<LocalAppRuntimePlan>> {
        if let Some(active) = self.active.as_ref().filter(|plan| plan.instance_key == key) {
            return Some(active.clone());
        }
        self.cache.touch(key)
    }
}

/// The runtime plans and admission state the local app protocol owned in Electron.
/// Told about every plan that is cached or activated, after the plans lock is released.
/// `recordGatewayOriginAnchor` in LocalAppProtocol.ts is the one Electron consumer.
pub type PlanObserver = Box<dyn Fn(&LocalAppRuntimePlan) + Send + Sync>;

pub struct RuntimePlans {
    discovery: Arc<RuntimeDiscovery>,
    state: Mutex<PlansState>,
    shutdown: CancellationToken,
    observer: OnceLock<PlanObserver>,
}

impl RuntimePlans {
    pub fn new(discovery: Arc<RuntimeDiscovery>) -> Self {
        Self {
            discovery,
            state: Mutex::new(PlansState {
                accepting: true,
                active: None,
                cache: PlanCache::default(),
                restorations: HashMap::new(),
            }),
            shutdown: CancellationToken::new(),
            observer: OnceLock::new(),
        }
    }

    /// Installs the plan observer; there is exactly one, set once during startup.
    pub fn set_observer(&self, observer: PlanObserver) {
        if self.observer.set(observer).is_err() {
            panic!("the runtime plan observer is already installed");
        }
    }

    fn notify(&self, plan: &LocalAppRuntimePlan) {
        if let Some(observer) = self.observer.get() {
            observer(plan);
        }
    }

    pub fn discovery(&self) -> &Arc<RuntimeDiscovery> {
        &self.discovery
    }

    /// A token cancelled by `shutdown` or by the caller, like
    /// `AbortSignal.any([controller.signal, protocol.getShutdownSignal()])`.
    pub fn shutdown_child_token(&self) -> CancellationToken {
        self.shutdown.child_token()
    }

    /// Stops admitting plan changes and aborts every discovery tied to the shutdown token.
    pub fn shutdown(&self) {
        self.state().accepting = false;
        self.shutdown.cancel();
    }

    pub fn active_plan(&self) -> Option<Arc<LocalAppRuntimePlan>> {
        self.state().active.clone()
    }

    /// `findPlanForRoute`: the active plan or a cached one, never a remembered restore.
    pub fn find_plan_for_route(&self, key: &str) -> Option<Arc<LocalAppRuntimePlan>> {
        self.state().find(key)
    }

    /// `cacheRuntimePlan`.
    pub fn cache(&self, plan: Arc<LocalAppRuntimePlan>) -> Result<(), BridgeError> {
        self.cache_locked(&mut self.state(), plan.clone())?;
        self.notify(&plan);
        Ok(())
    }

    /// `activateRuntimePlan`: caches the plan and makes it the active one.
    pub fn activate(&self, plan: Arc<LocalAppRuntimePlan>) -> Result<(), BridgeError> {
        {
            let mut state = self.state();
            self.cache_locked(&mut state, plan.clone())?;
            tracing::info!(instanceKey = %plan.instance_key, "Activated a local app runtime plan");
            state.active = Some(plan.clone());
        }
        self.notify(&plan);
        Ok(())
    }

    /// `deactivateRuntimePlan`: no plan is active; the old one stays cached.
    pub fn deactivate(&self) -> Result<(), BridgeError> {
        let mut state = self.state();
        if !state.accepting {
            return Err(shutdown_admission_error());
        }
        state.active = None;
        tracing::info!("Deactivated the local app runtime plan");
        Ok(())
    }

    fn cache_locked(
        &self,
        state: &mut PlansState,
        plan: Arc<LocalAppRuntimePlan>,
    ) -> Result<(), BridgeError> {
        if !state.accepting {
            return Err(shutdown_admission_error());
        }
        self.discovery
            .outbound
            .require_registered_origins(&trusted_http_origins(&plan))
            .map_err(|error| outbound_error(&error))?;
        state.cache.insert(plan);
        Ok(())
    }

    fn state(&self) -> MutexGuard<'_, PlansState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl RuntimePlans {
    /// `planForRoute`: a known plan, else the remembered one for `key`, restored once for every
    /// concurrent caller and cached (never activated).
    pub async fn plan_for_route(
        self: &Arc<Self>,
        key: &str,
    ) -> Result<Option<Arc<LocalAppRuntimePlan>>, BridgeError> {
        let restoration = {
            let mut state = self.state();
            if let Some(known) = state.find(key) {
                return Ok(Some(known));
            }
            let entry = state.restorations.entry(key.to_owned());
            entry.or_insert_with(|| self.spawn_restoration(key)).clone()
        };
        restoration.await
    }

    /// Runs the restore as its own task so its entry is cleared even if every waiter leaves.
    /// Called with the state lock held, so the entry exists before the task can remove it.
    fn spawn_restoration(self: &Arc<Self>, key: &str) -> Restoration {
        let plans = self.clone();
        let key = key.to_owned();
        let task = tokio::spawn(async move {
            let restored = plans.restore(&key).await;
            plans.state().restorations.remove(&key);
            restored
        });
        async move {
            task.await.unwrap_or_else(|error| {
                Err(BridgeError::new(
                    "Error",
                    format!("Plan restore failed: {error}"),
                ))
            })
        }
        .boxed()
        .shared()
    }

    async fn restore(&self, key: &str) -> Result<Option<Arc<LocalAppRuntimePlan>>, BridgeError> {
        let plan = match self.discovery.restore_remembered_runtime_plan(key).await {
            Ok(Some(plan)) => Arc::new(plan),
            Ok(None) => return Ok(None),
            Err(error) => {
                tracing::warn!(instanceKey = key, %error, "Failed to restore a remembered local app runtime plan");
                return Ok(None);
            }
        };
        {
            let mut state = self.state();
            if !state.accepting {
                return Ok(None);
            }
            if let Some(known) = state.find(key) {
                return Ok(Some(known));
            }
            self.cache_locked(&mut state, plan.clone())?;
        }
        self.notify(&plan);
        Ok(Some(plan))
    }
}

#[cfg(test)]
mod tests;
