// SPDX-License-Identifier: AGPL-3.0-or-later

//! `/api/<instance key>/...`, ported from LocalAppAPIProxyRequestHandler.ts.

use tauri::http::{Request, Response, StatusCode};

use super::client::{API_ACCEPT_ENCODING, API_RESPONSE_BODY_LIMIT, ProxyFetch};
use super::policy::{
    CacheDefault, api_request_headers, api_target_url, proxy_response_headers,
    request_content_length, upload_id,
};
use super::{Proxy, ProxyRoute, failure_response, not_found_response, proxy_response};
use crate::runtime::plans::http_origin_source;

const UNKNOWN_RUNTIME_TEXT: &str = "Unknown local app API runtime";
const FAILURE_TEXT: &str = "Network error during API request";
/// `desktop.localApp.apiUnavailable` in MainI18n.ts.
const UNAVAILABLE_TEXT: &str = "No server is selected for this request.";

pub async fn handle(
    proxy: &Proxy,
    request: Request<Vec<u8>>,
    route: ProxyRoute,
) -> Response<Vec<u8>> {
    let plan = match proxy.plan_for_route(&route.runtime.runtime_key).await {
        Ok(Some(plan)) => plan,
        Ok(None) => return not_found_response(UNKNOWN_RUNTIME_TEXT),
        Err(error) => {
            tracing::warn!(%error, "Local app API proxy runtime plan lookup failed");
            return failure_response(StatusCode::BAD_GATEWAY, FAILURE_TEXT);
        }
    };
    let target = http_origin_source(&plan.endpoints.api).and_then(|_| {
        api_target_url(
            &plan.endpoints.api,
            &route.url,
            &route.runtime.local_path_prefix,
        )
    });
    let Some(target) = target else {
        tracing::warn!(instanceKey = %plan.instance_key, "Active instance API endpoint is not proxyable");
        return failure_response(StatusCode::SERVICE_UNAVAILABLE, UNAVAILABLE_TEXT);
    };
    let (parts, body) = request.into_parts();
    let upload_id = upload_id(&parts.headers);
    let fetch = ProxyFetch {
        target,
        headers: api_request_headers(
            &parts.headers,
            &parts.method,
            http_origin_source(&plan.endpoints.webapp).as_deref(),
        ),
        method: parts.method,
        body,
        accept_encoding: Some(API_ACCEPT_ENCODING),
        upload_id: upload_id.clone(),
        upload_total: request_content_length(&parts.headers),
        response_headers_deadline: None,
        response_body_limit: API_RESPONSE_BODY_LIMIT,
    };
    match proxy.fetch(fetch).await {
        Ok(response) => proxy_response(
            response.status,
            proxy_response_headers(&response.headers, CacheDefault::NoStore),
            response.body,
            response.held,
        ),
        Err(error) => {
            proxy.uploads.failure(upload_id.as_deref());
            tracing::warn!(%error, "Local app API proxy request failed");
            failure_response(StatusCode::BAD_GATEWAY, FAILURE_TEXT)
        }
    }
}
