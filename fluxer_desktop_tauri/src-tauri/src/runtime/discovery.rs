// SPDX-License-Identifier: AGPL-3.0-or-later

//! Instance discovery for the desktop runtime, ported from DesktopRuntimeDiscovery.ts: fetch the
//! discovery document from the instance's candidate URLs, turn it into a runtime plan, register
//! the plan's origins, and remember the served document per API endpoint so an unreachable
//! instance still boots from its last answer.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use hyper::header::{ACCEPT, CONTENT_TYPE, HeaderValue};
use hyper::{HeaderMap, StatusCode};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use url::Url;

use super::errors::{is_host_resolution_failure, outbound_error};
use super::fetch::fetch_instance;
use super::markers::Markers;
use super::plans::{LocalAppRuntimePlan, runtime_plan_from_discovery, trusted_http_origins};
use crate::error::BridgeError;
use crate::instance::discovery::{
    DiscoveryError, discovery_candidate_urls, parse_instance_discovery_document,
    project_client_runtime_discovery, require_discovery_declares_its_own_origin,
};
use crate::instance::endpoint::{EndpointKind, normalize_instance_endpoint};
use crate::instance::official::official_client_api_endpoint_for_alias;
use crate::js::js_trim;
use crate::net::origin::normalize_http_network_origin;
use crate::net::outbound::{AddressRequirement, OutboundHttp};

const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(15);
const LAST_SERVED_DISCOVERY_MARKER_PREFIX: &str = "runtime_discovery.v1:";
const JSON_MIME_TYPE: &str = "application/json";
const DISCOVERY_FAILED_ERROR_NAME: &str = "DesktopRuntimeDiscoveryFailedError";
/// `DESKTOP_RUNTIME_DISCOVERY_UNREACHABLE_ERROR_NAME`; the renderer reports the instance as
/// unreachable when an error message carries it (BootstrapErrorKind.ts).
const DISCOVERY_UNREACHABLE_ERROR_NAME: &str = "DesktopRuntimeDiscoveryUnreachableError";

struct DiscoveryAttempt {
    url: String,
    reason: String,
}

fn described_failure(api_endpoint: &str, attempts: &[DiscoveryAttempt]) -> String {
    let described = attempts
        .iter()
        .map(|attempt| format!("{} ({})", attempt.url, attempt.reason))
        .collect::<Vec<_>>()
        .join("; ");
    format!("No usable instance discovery document was served for {api_endpoint}: {described}")
}

fn discovery_failed(api_endpoint: &str, attempts: &[DiscoveryAttempt]) -> BridgeError {
    BridgeError::new(
        DISCOVERY_FAILED_ERROR_NAME,
        described_failure(api_endpoint, attempts),
    )
}

fn discovery_unreachable(api_endpoint: &str, attempts: &[DiscoveryAttempt]) -> BridgeError {
    BridgeError::new(
        DISCOVERY_UNREACHABLE_ERROR_NAME,
        described_failure(api_endpoint, attempts),
    )
}

enum ResolveError {
    /// `DesktopRuntimeDiscoveryDeclaredElsewhereError`: every served document declared its API
    /// at another origin; `failed` is the error it reports if the declaration is not followed.
    DeclaredElsewhere {
        api_endpoint: String,
        declared: String,
        failed: BridgeError,
    },
    Failed(BridgeError),
}

impl From<BridgeError> for ResolveError {
    fn from(error: BridgeError) -> Self {
        Self::Failed(error)
    }
}

impl From<ResolveError> for BridgeError {
    fn from(error: ResolveError) -> Self {
        match error {
            ResolveError::DeclaredElsewhere { failed, .. } | ResolveError::Failed(failed) => failed,
        }
    }
}

/// Why a fetched document did not yield a plan.
enum PlanError {
    Discovery(DiscoveryError),
    Plan(BridgeError),
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Discovery(error) => error.fmt(f),
            Self::Plan(error) => f.write_str(error.message()),
        }
    }
}

/// A failed discovery fetch. `answered` is `InstanceAnsweredWithoutDiscoveryError`: the instance
/// is up but serves no discovery there, so the attempt does not count as unreachable.
struct FetchFailure {
    reason: String,
    answered: bool,
}

impl FetchFailure {
    fn unreachable(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            answered: false,
        }
    }

    fn answered(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            answered: true,
        }
    }
}

struct ServedDiscovery {
    url: String,
    document: Value,
}

struct ResolvedRuntimePlan {
    plan: LocalAppRuntimePlan,
    served: Option<ServedDiscovery>,
    remembered_requirement: Option<AddressRequirement>,
    attempts: Vec<DiscoveryAttempt>,
}

struct RememberedRuntimePlan {
    plan: LocalAppRuntimePlan,
    anchor_requirement: Option<AddressRequirement>,
}

/// `requireDesktopRuntimeAPIEndpoint`: the normalized absolute API endpoint, mapped to its
/// official canonical endpoint when it is an alias.
pub fn require_api_endpoint(input: &str) -> Result<String, BridgeError> {
    let normalized = normalize_instance_endpoint(input, EndpointKind::Api)
        .filter(|endpoint| endpoint.contains("://"))
        .ok_or_else(|| {
            BridgeError::new(
                "DesktopRuntimeEndpointInputError",
                format!(
                    "Desktop runtime endpoint input is not an absolute http(s) API endpoint: {input}"
                ),
            )
        })?;
    Ok(official_client_api_endpoint_for_alias(&normalized).map_or(normalized, str::to_owned))
}

/// `requireDesktopHTTPOrigin(new URL(apiEndpoint).origin)`.
fn anchor_origin(api_endpoint: &str) -> Result<String, BridgeError> {
    let origin = Url::parse(api_endpoint)
        .map(|url| url.origin().ascii_serialization())
        .unwrap_or_default();
    normalize_http_network_origin(&origin).ok_or_else(|| {
        BridgeError::new(
            "DesktopOutboundHTTPInvalidOriginError",
            format!("Desktop outbound HTTP origin is not a canonical http(s) origin: {origin}"),
        )
    })
}

fn marker_key(api_endpoint: &str) -> String {
    format!("{LAST_SERVED_DISCOVERY_MARKER_PREFIX}{api_endpoint}")
}

fn requirement_name(requirement: AddressRequirement) -> &'static str {
    match requirement {
        AddressRequirement::Any => "any",
        AddressRequirement::Public => "public",
    }
}

fn read_requirement(value: Option<&Value>) -> Option<AddressRequirement> {
    match value.and_then(Value::as_str) {
        Some("any") => Some(AddressRequirement::Any),
        Some("public") => Some(AddressRequirement::Public),
        _ => None,
    }
}

/// `runtimePlanFromServedDiscovery`: the document must declare the origin that served it.
fn runtime_plan_from_served_discovery(
    url: &str,
    document: Value,
) -> Result<LocalAppRuntimePlan, PlanError> {
    let parsed = parse_instance_discovery_document(&document).map_err(PlanError::Discovery)?;
    require_discovery_declares_its_own_origin(url, &parsed).map_err(PlanError::Discovery)?;
    let projected = project_client_runtime_discovery(&parsed).map_err(PlanError::Discovery)?;
    runtime_plan_from_discovery(document, &projected).map_err(PlanError::Plan)
}

pub(crate) fn is_json_content_type(value: Option<&HeaderValue>) -> bool {
    let Some(value) = value else {
        return false;
    };
    let header = String::from_utf8_lossy(value.as_bytes());
    let media_type = js_trim(header.split(';').next().unwrap_or_default()).to_lowercase();
    media_type == JSON_MIME_TYPE || media_type.ends_with("+json")
}

/// `fetchDiscoveryDocument`.
async fn fetch_discovery_document(
    outbound: &OutboundHttp,
    url: &str,
    expected_origin: &str,
    cancel: &CancellationToken,
) -> Result<Value, FetchFailure> {
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static(JSON_MIME_TYPE));
    let response = fetch_instance(
        outbound,
        url,
        expected_origin,
        headers,
        DISCOVERY_TIMEOUT,
        cancel,
    )
    .await
    .map_err(FetchFailure::unreachable)?;
    if !response.status.is_success() {
        let reason = format!("responded HTTP {}", response.status.as_u16());
        return Err(if response.status >= StatusCode::INTERNAL_SERVER_ERROR {
            FetchFailure::unreachable(reason)
        } else {
            FetchFailure::answered(reason)
        });
    }
    if !is_json_content_type(response.headers.get(CONTENT_TYPE)) {
        return Err(FetchFailure::unreachable(
            "responded without a JSON content type",
        ));
    }
    let Some(body) = response.body else {
        return Err(FetchFailure::unreachable("responded without a body"));
    };
    // `JSON.parse(body.toString('utf8'))`: invalid UTF-8 decodes to U+FFFD, a BOM is not skipped.
    serde_json::from_str(&String::from_utf8_lossy(&body))
        .map_err(|_| FetchFailure::answered("responded with a malformed JSON body"))
}

pub struct RuntimeDiscovery {
    pub(super) outbound: Arc<OutboundHttp>,
    markers: Markers,
}

impl RuntimeDiscovery {
    pub fn new(outbound: Arc<OutboundHttp>, markers: Markers) -> Self {
        Self { outbound, markers }
    }

    /// `resolveDesktopRuntimePlan`. A document served at another origin's declaration is
    /// followed once.
    pub async fn resolve(
        &self,
        input: &str,
        cancel: &CancellationToken,
    ) -> Result<LocalAppRuntimePlan, BridgeError> {
        match self.resolve_at(input, cancel, None).await {
            Err(ResolveError::DeclaredElsewhere {
                api_endpoint,
                declared,
                ..
            }) => {
                tracing::info!(
                    input,
                    declaredApiEndpoint = %declared,
                    "Instance discovery declared another API origin, resolving that instance"
                );
                Ok(self
                    .resolve_at(&declared, cancel, Some(api_endpoint))
                    .await?)
            }
            result => Ok(result?),
        }
    }

    async fn resolve_at(
        &self,
        input: &str,
        cancel: &CancellationToken,
        followed_from: Option<String>,
    ) -> Result<LocalAppRuntimePlan, ResolveError> {
        let api_endpoint = require_api_endpoint(input)?;
        let anchor = anchor_origin(&api_endpoint)?;
        let resolved = self
            .fetch_runtime_plan(&api_endpoint, &anchor, cancel, followed_from.is_some())
            .await?;
        let requirement = match self
            .outbound
            .register_anchored_origins(
                &anchor,
                &trusted_http_origins(&resolved.plan),
                resolved.remembered_requirement,
            )
            .await
        {
            Ok(requirement) => requirement,
            Err(error) if resolved.served.is_none() && is_host_resolution_failure(&error) => {
                return Err(discovery_unreachable(&api_endpoint, &resolved.attempts).into());
            }
            Err(error) => return Err(outbound_error(&error).into()),
        };
        if let Some(served) = &resolved.served {
            let marker = json!({
                "url": served.url,
                "document": served.document,
                "anchorRequirement": requirement_name(requirement),
            })
            .to_string();
            if let Some(from) = followed_from.filter(|from| *from != api_endpoint) {
                self.remember_last_served_discovery(&from, marker.clone());
            }
            self.remember_last_served_discovery(&api_endpoint, marker);
        }
        Ok(resolved.plan)
    }

    /// `restoreRememberedRuntimePlan`: the plan remembered for `instance_key`, its origins
    /// registered again under the remembered requirement.
    pub async fn restore_remembered_runtime_plan(
        &self,
        instance_key: &str,
    ) -> Result<Option<LocalAppRuntimePlan>, BridgeError> {
        let Ok(api_endpoint) = require_api_endpoint(instance_key) else {
            return Ok(None);
        };
        let Some(remembered) = self.read_last_served_discovery(&api_endpoint).await else {
            return Ok(None);
        };
        if remembered.plan.instance_key != instance_key {
            return Ok(None);
        }
        self.outbound
            .register_anchored_origins(
                &anchor_origin(&api_endpoint)?,
                &trusted_http_origins(&remembered.plan),
                remembered.anchor_requirement,
            )
            .await
            .map_err(|error| outbound_error(&error))?;
        Ok(Some(remembered.plan))
    }

    async fn fetch_runtime_plan(
        &self,
        api_endpoint: &str,
        anchor: &str,
        cancel: &CancellationToken,
        followed: bool,
    ) -> Result<ResolvedRuntimePlan, ResolveError> {
        let candidates = discovery_candidate_urls(api_endpoint);
        let mut attempts = Vec::new();
        let mut declared_api_endpoint: Option<String> = None;
        let mut instance_unreachable = !candidates.is_empty();
        for url in candidates {
            let document =
                match fetch_discovery_document(&self.outbound, &url, anchor, cancel).await {
                    Ok(document) => document,
                    Err(failure) => {
                        instance_unreachable &= !cancel.is_cancelled() && !failure.answered;
                        attempts.push(DiscoveryAttempt {
                            url,
                            reason: failure.reason,
                        });
                        continue;
                    }
                };
            match runtime_plan_from_served_discovery(&url, document.clone()) {
                Ok(plan) => {
                    return Ok(ResolvedRuntimePlan {
                        plan,
                        served: Some(ServedDiscovery { url, document }),
                        remembered_requirement: None,
                        attempts,
                    });
                }
                Err(error) => {
                    instance_unreachable = false;
                    if let PlanError::Discovery(DiscoveryError::OriginMismatch {
                        declared, ..
                    }) = &error
                    {
                        declared_api_endpoint.get_or_insert_with(|| declared.clone());
                    }
                    attempts.push(DiscoveryAttempt {
                        url,
                        reason: error.to_string(),
                    });
                }
            }
        }
        if !instance_unreachable {
            let failed = discovery_failed(api_endpoint, &attempts);
            return Err(match declared_api_endpoint {
                Some(declared) if !followed => ResolveError::DeclaredElsewhere {
                    api_endpoint: api_endpoint.to_owned(),
                    declared,
                    failed,
                },
                _ => ResolveError::Failed(failed),
            });
        }
        let Some(last_served) = self.read_last_served_discovery(api_endpoint).await else {
            return Err(discovery_unreachable(api_endpoint, &attempts).into());
        };
        tracing::warn!(
            apiEndpoint = api_endpoint,
            attempts = %described_failure(api_endpoint, &attempts),
            "Instance discovery is unreachable, using the last served discovery document"
        );
        Ok(ResolvedRuntimePlan {
            plan: last_served.plan,
            served: None,
            remembered_requirement: last_served.anchor_requirement,
            attempts,
        })
    }

    /// Writes in the background; a failed write is only logged.
    fn remember_last_served_discovery(&self, api_endpoint: &str, marker: String) {
        let write = self.markers.set(marker_key(api_endpoint), marker);
        let api_endpoint = api_endpoint.to_owned();
        tokio::spawn(async move {
            if let Err(error) = write.await {
                tracing::warn!(apiEndpoint = %api_endpoint, %error, "Failed to remember the served discovery document");
            }
        });
    }

    async fn read_last_served_discovery(
        &self,
        api_endpoint: &str,
    ) -> Option<RememberedRuntimePlan> {
        let failed = |error: &dyn fmt::Display| {
            tracing::warn!(apiEndpoint = api_endpoint, %error, "Failed to read the last served discovery document");
        };
        let value = match self.markers.get(marker_key(api_endpoint)).await {
            Ok(value) => value?,
            Err(error) => {
                failed(&error);
                return None;
            }
        };
        let remembered: Value = match serde_json::from_str(&value) {
            Ok(remembered) => remembered,
            Err(error) => {
                failed(&error);
                return None;
            }
        };
        let url = remembered.get("url")?.as_str()?;
        let document = remembered.get("document").cloned().unwrap_or_default();
        match runtime_plan_from_served_discovery(url, document) {
            Ok(plan) => Some(RememberedRuntimePlan {
                plan,
                anchor_requirement: read_requirement(remembered.get("anchorRequirement")),
            }),
            Err(error) => {
                failed(&error);
                None
            }
        }
    }
}

#[cfg(test)]
mod tests;
