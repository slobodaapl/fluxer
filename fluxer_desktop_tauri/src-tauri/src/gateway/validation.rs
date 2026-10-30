// SPDX-License-Identifier: AGPL-3.0-or-later

//! The gateway endpoint an anchor origin advertises, ported from `fetchValidatedFluxerInstance`
//! in `fluxer_desktop/src/main/FluxerInstanceValidation.ts` and the `fetchInstanceDiscovery`
//! path of `packages/instance_bootstrap/src/Discovery.ts` it calls.
//!
//! Electron also applied a 15 s deadline per candidate; the 5 s request timeout, which covers
//! the whole exchange, always ends a request first, so only that one is kept.

use std::time::Duration;

use hyper::header::{ACCEPT, CONTENT_TYPE, HeaderValue};
use hyper::{HeaderMap, StatusCode};
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::instance::discovery::{
    DiscoveryError, InstanceDiscovery, discovery_candidate_urls, parse_instance_discovery_document,
    require_discovery_declares_its_own_origin,
};
use crate::net::origin::{http_origin_of, normalize_http_network_origin};
use crate::net::outbound::OutboundHttp;
use crate::runtime::discovery::is_json_content_type;
use crate::runtime::fetch::fetch_instance;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const JSON_MIME_TYPE: &str = "application/json";

/// Why a candidate URL yielded no document. `stops` ends the candidate walk, as
/// `InstanceRequiresNewerClientError` and a 304 answer did.
struct CandidateFailure {
    reason: String,
    stops: bool,
}

impl CandidateFailure {
    fn skip(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            stops: false,
        }
    }
}

/// `requestInstanceDiscovery` without conditional headers or serving-origin adoption.
async fn request_discovery(
    outbound: &OutboundHttp,
    url: &str,
    anchor_origin: &str,
    cancel: &CancellationToken,
) -> Result<InstanceDiscovery, CandidateFailure> {
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static(JSON_MIME_TYPE));
    let response = fetch_instance(
        outbound,
        url,
        anchor_origin,
        headers,
        REQUEST_TIMEOUT,
        cancel,
    )
    .await
    .map_err(CandidateFailure::skip)?;
    if response.status == StatusCode::NOT_MODIFIED {
        return Err(CandidateFailure {
            reason: "the instance answered a conditional request it never received".to_owned(),
            stops: true,
        });
    }
    if response.status != StatusCode::OK {
        return Err(CandidateFailure::skip(format!(
            "responded HTTP {}",
            response.status.as_u16()
        )));
    }
    if !is_json_content_type(response.headers.get(CONTENT_TYPE)) {
        return Err(CandidateFailure::skip(
            "responded without a JSON content type",
        ));
    }
    let body = response.body.unwrap_or_default();
    let value: Value = serde_json::from_str(&String::from_utf8_lossy(&body))
        .map_err(|error| CandidateFailure::skip(error.to_string()))?;
    let document = parse_instance_discovery_document(&value).map_err(|error| CandidateFailure {
        stops: matches!(error, DiscoveryError::RequiresNewerClient(_)),
        reason: error.to_string(),
    })?;
    require_discovery_declares_its_own_origin(url, &document)
        .map_err(|error| CandidateFailure::skip(error.to_string()))?;
    Ok(document)
}

/// `readAdvertisedOrigins`: the http origins of every advertised service endpoint.
fn advertised_origins(document: &InstanceDiscovery) -> Result<Vec<String>, String> {
    let endpoints = &document.endpoints;
    let mut origins: Vec<String> = Vec::new();
    for endpoint in [
        Some(&endpoints.api),
        endpoints.api_client.as_ref(),
        endpoints.api_public.as_ref(),
        endpoints.media.as_ref(),
        endpoints.upload_relay.as_ref(),
        endpoints.static_cdn.as_ref(),
        endpoints.marketing.as_ref(),
        endpoints.docs.as_ref(),
        endpoints.admin.as_ref(),
        endpoints.invite.as_ref(),
        endpoints.gift.as_ref(),
        endpoints.webapp.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        let origin = Url::parse(endpoint)
            .ok()
            .as_ref()
            .and_then(http_origin_of)
            .ok_or_else(|| format!("advertised endpoint {endpoint} has no http origin"))?;
        if !origins.contains(&origin) {
            origins.push(origin);
        }
    }
    Ok(origins)
}

/// Fetches `origin`'s discovery document, registers the origins it advertises as anchored to
/// it, and returns its gateway endpoint.
pub async fn fetch_validated_gateway_endpoint(
    outbound: &OutboundHttp,
    origin: &str,
    cancel: &CancellationToken,
) -> Result<String, String> {
    let anchor_origin = normalize_http_network_origin(origin)
        .filter(|canonical| canonical == origin)
        .ok_or_else(|| format!("{origin} is not a canonical http(s) origin"))?;
    let mut attempts: Vec<String> = Vec::new();
    let mut document = None;
    for url in discovery_candidate_urls(&anchor_origin) {
        match request_discovery(outbound, &url, &anchor_origin, cancel).await {
            Ok(served) => {
                document = Some(served);
                break;
            }
            Err(failure) if failure.stops => {
                return Err(format!(
                    "Instance discovery document served by {anchor_origin} is unusable: {}",
                    failure.reason
                ));
            }
            Err(failure) => attempts.push(format!("{url} ({})", failure.reason)),
        }
    }
    let Some(document) = document else {
        return Err(format!(
            "No discovery document was served by any candidate: {}",
            attempts.join(", ")
        ));
    };
    let origins = advertised_origins(&document)?;
    outbound
        .register_anchored_origins(&anchor_origin, &origins, None)
        .await
        .map_err(|error| error.to_string())?;
    Ok(document.endpoints.gateway)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::test_support::{Reply, TestServer, document, outbound};
    use serde_json::json;
    use std::sync::Arc;

    async fn served(reply: Reply, at: &str) -> TestServer {
        let server = TestServer::start().await;
        server.route(at, reply);
        server
    }

    async fn validate(server: &TestServer) -> (Result<String, String>, Arc<OutboundHttp>) {
        let outbound = outbound();
        let result =
            fetch_validated_gateway_endpoint(&outbound, &server.base, &CancellationToken::new())
                .await;
        (result, outbound)
    }

    #[tokio::test]
    async fn returns_the_gateway_and_registers_the_advertised_origins() {
        let server = TestServer::start().await;
        server.route("/.well-known/fluxer", Reply::json(&document(&server.base)));
        let (result, outbound) = validate(&server).await;
        let port = server.base.rsplit(':').next().unwrap();
        assert_eq!(result.unwrap(), format!("ws://127.0.0.1:{port}/gateway"));
        assert_eq!(
            outbound.registered_cleartext_origins(),
            vec![server.base.clone()]
        );
    }

    #[tokio::test]
    async fn falls_back_to_the_conventional_api_path() {
        let server = TestServer::start().await;
        server.route(
            "/api/.well-known/fluxer",
            Reply::json(&document(&server.base)),
        );
        let (result, _) = validate(&server).await;
        assert!(result.unwrap().ends_with("/gateway"));
        assert_eq!(
            server.requests(),
            vec!["/.well-known/fluxer", "/api/.well-known/fluxer"]
        );
    }

    #[tokio::test]
    async fn refuses_non_ok_status_and_non_json_answers() {
        let server = TestServer::start().await;
        server.route(
            "/.well-known/fluxer",
            Reply::Fixed(
                203,
                vec![("Content-Type", "application/json".to_owned())],
                document(&server.base).to_string().into_bytes(),
            ),
        );
        server.route(
            "/api/.well-known/fluxer",
            Reply::Fixed(
                200,
                vec![("Content-Type", "text/html".to_owned())],
                document(&server.base).to_string().into_bytes(),
            ),
        );
        let (result, outbound) = validate(&server).await;
        let error = result.unwrap_err();
        assert!(error.contains("(responded HTTP 203)"), "{error}");
        assert!(
            error.contains("(responded without a JSON content type)"),
            "{error}"
        );
        assert!(outbound.registered_cleartext_origins().is_empty());
    }

    #[tokio::test]
    async fn refuses_a_document_declaring_another_origin() {
        let server = TestServer::start().await;
        server.route(
            "/.well-known/fluxer",
            Reply::json(&document("http://127.0.0.2:1")),
        );
        let (result, outbound) = validate(&server).await;
        assert!(result.is_err());
        assert!(outbound.registered_cleartext_origins().is_empty());
    }

    #[tokio::test]
    async fn a_newer_generation_stops_the_candidate_walk() {
        let mut newer = document("http://placeholder");
        newer["codename"] = json!("future");
        let server = served(Reply::json(&newer), "/.well-known/fluxer").await;
        server.route(
            "/api/.well-known/fluxer",
            Reply::json(&document(&server.base)),
        );
        let (result, _) = validate(&server).await;
        assert!(result.is_err());
        assert_eq!(server.requests(), vec!["/.well-known/fluxer"]);
    }

    #[tokio::test]
    async fn refuses_an_oversized_document() {
        let mut large = document("http://placeholder");
        large["padding"] = json!("x".repeat(1024 * 1024));
        let server = served(Reply::json(&large), "/.well-known/fluxer").await;
        let (result, _) = validate(&server).await;
        assert!(result.unwrap_err().contains("exceeds 1048576 bytes"));
    }
}
