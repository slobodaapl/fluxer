// SPDX-License-Identifier: AGPL-3.0-or-later

//! Route parsing, target URLs, header policy and remote target admission of the proxy routes,
//! ported from LocalAppProxyHTTPPolicy.ts and `parseLocalAppRuntimeRoute` in
//! LocalAppRuntimePlans.ts.
//!
//! Not ported: blocking `X-Fluxer-Desktop-Protocol-Authorization` upstream. That header carried
//! Electron's per-frame protocol secret, which the Tauri shell has no counterpart for.

use std::borrow::Cow;

use percent_encoding::percent_decode_str;
use tauri::http::{HeaderMap, HeaderValue, Method, header};
use url::Url;

use crate::js::js_trim;
use crate::runtime::plans::{LocalAppRuntimePlan, http_origin_source};

const UPLOAD_ID_HEADER: &str = "x-fluxer-local-upload-id";
const REMOTE_PROXY_URL_PARAMETER: &str = "url";
const UPLOAD_RELAY_TOKEN_PARAMETER: &str = "t";
const UPLOAD_RELAY_PATH_PREFIX: &str = "/v1/relay";
const UPLOAD_RELAY_MULTIPART_PARAMETERS: &[&str] = &["uploadId", "partNumber"];
const UPLOAD_ID_MAX_LENGTH: usize = 128;
const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;
const NO_STORE_CACHE_CONTROL: &str = "no-store";
const PROXY_RESPONSE_DOCUMENT_POLICY: &str = "sandbox; default-src 'none'; frame-ancestors 'none'";

const HOP_BY_HOP_HEADERS: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

const API_REQUEST_BLOCKED_HEADERS: &[&str] = &[
    super::admission::PROTOCOL_AUTHORIZATION_HEADER,
    UPLOAD_ID_HEADER,
    "accept-encoding",
    "content-length",
    "cookie",
    "expect",
    "host",
    "origin",
    "proxy-connection",
    "referer",
];

const REMOTE_RESOURCE_REQUEST_BLOCKED_HEADERS: &[&str] =
    &["authorization", "x-fluxer-sudo-mode-jwt"];

/// The Electron blocklist, plus what acts on the local app document or origin rather than on the
/// proxied body, which Electron passed through: `clear-site-data`, `refresh`, `link`,
/// `service-worker-allowed`, the cross-origin and policy headers, and the reporting headers.
const RESPONSE_BLOCKED_HEADERS: &[&str] = &[
    "access-control-allow-credentials",
    "access-control-allow-headers",
    "access-control-allow-methods",
    "access-control-allow-origin",
    "access-control-expose-headers",
    "access-control-max-age",
    "clear-site-data",
    "content-encoding",
    "content-length",
    "content-security-policy",
    "content-security-policy-report-only",
    "cross-origin-embedder-policy",
    "cross-origin-opener-policy",
    "document-policy",
    "link",
    "nel",
    "origin-agent-cluster",
    "permissions-policy",
    "refresh",
    "report-to",
    "reporting-endpoints",
    "service-worker-allowed",
    "set-cookie",
    "set-cookie2",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheDefault {
    NoStore,
    Upstream,
}

/// `LocalAppRuntimeRoute`: the decoded instance key and the local path up to and including it.
#[derive(Debug, PartialEq, Eq)]
pub struct RuntimeRoute {
    pub runtime_key: String,
    pub local_path_prefix: String,
}

/// `parseLocalAppRuntimeRoute` over an already parsed URL path.
pub fn parse_runtime_route(path: &str, base_path: &str) -> Option<RuntimeRoute> {
    let rest = path.strip_prefix(base_path)?.strip_prefix('/')?;
    let key_end = rest.find('/').unwrap_or(rest.len());
    let encoded = &rest[..key_end];
    if encoded.is_empty() {
        return None;
    }
    let runtime_key = decode_uri_component(encoded)?;
    if js_trim(&runtime_key).is_empty() {
        return None;
    }
    Some(RuntimeRoute {
        runtime_key,
        local_path_prefix: path[..base_path.len() + 1 + key_end].to_owned(),
    })
}

/// `decodeURIComponent`, which throws on a `%` not followed by two hex digits and on bytes that
/// do not decode as UTF-8.
fn decode_uri_component(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut index = 0;
    while let Some(offset) = bytes[index..].iter().position(|&byte| byte == b'%') {
        let escape = index + offset;
        let well_formed = bytes
            .get(escape + 1..escape + 3)
            .is_some_and(|digits| digits.iter().all(u8::is_ascii_hexdigit));
        if !well_formed {
            return None;
        }
        index = escape + 3;
    }
    percent_decode_str(value)
        .decode_utf8()
        .ok()
        .map(|decoded| decoded.into_owned())
}

pub fn request_has_body(method: &Method) -> bool {
    method != Method::GET && method != Method::HEAD
}

/// `buildAPITargetURL`. Setting the path on the http(s) target re-parses it, so a backslash in
/// the local path becomes a separator and its dot segments resolve against the API base path.
pub fn api_target_url(
    api_endpoint: &str,
    request: &Url,
    local_path_prefix: &str,
) -> Option<String> {
    let mut target = Url::parse(&with_trailing_slash(api_endpoint)).ok()?;
    let base_path = normalize_api_endpoint_path(target.path()).to_owned();
    let suffix = api_target_suffix_path(
        request
            .path()
            .get(local_path_prefix.len()..)
            .unwrap_or_default(),
    );
    target.set_path(&format!("{base_path}{suffix}"));
    target.set_query(request.query().filter(|query| !query.is_empty()));
    Some(target.into())
}

fn with_trailing_slash(value: &str) -> Cow<'_, str> {
    if value.ends_with('/') {
        Cow::Borrowed(value)
    } else {
        Cow::Owned(format!("{value}/"))
    }
}

fn normalize_api_endpoint_path(path: &str) -> &str {
    if path == "/" || path.is_empty() {
        return "";
    }
    path.trim_end_matches('/')
}

fn api_target_suffix_path(suffix: &str) -> Cow<'_, str> {
    if suffix.is_empty() {
        return Cow::Borrowed("/");
    }
    if suffix.starts_with('/') {
        return Cow::Borrowed(suffix);
    }
    Cow::Owned(format!("/{suffix}"))
}

/// `remoteProxyTargetURL`: the first `url` query parameter, when it is an http(s) URL without
/// credentials.
pub fn remote_proxy_target_url(request: &Url) -> Option<String> {
    let (_, raw) = request
        .query_pairs()
        .find(|(name, _)| name == REMOTE_PROXY_URL_PARAMETER)?;
    if raw.is_empty() {
        return None;
    }
    let target = Url::parse(&raw).ok()?;
    if !is_http(&target) || has_credentials(&target) {
        return None;
    }
    Some(target.into())
}

pub fn is_supported_remote_resource_method(method: &Method) -> bool {
    method == Method::GET || method == Method::HEAD || method == Method::PUT
}

/// `isAllowedRemoteProxyTarget`: PUT may only reach the plan's upload relay; GET and HEAD may
/// reach any origin the plan serves the client from.
pub fn is_allowed_remote_proxy_target(
    method: &Method,
    target: &str,
    plan: &LocalAppRuntimePlan,
) -> bool {
    if method == Method::PUT {
        return is_allowed_upload_relay_target(target, plan.endpoints.upload_relay.as_deref());
    }
    is_allowed_remote_resource_target(target, plan)
}

fn is_allowed_remote_resource_target(target: &str, plan: &LocalAppRuntimePlan) -> bool {
    let Some(target_origin) = http_origin_source(target) else {
        return false;
    };
    let endpoints = &plan.endpoints;
    [
        &endpoints.api,
        &endpoints.api_public,
        &endpoints.webapp,
        &endpoints.media,
        &endpoints.static_cdn,
    ]
    .into_iter()
    .any(|source| http_origin_source(source).as_deref() == Some(target_origin.as_str()))
}

fn is_allowed_upload_relay_target(target: &str, upload_relay: Option<&str>) -> bool {
    let Some(upload_relay) = upload_relay.filter(|endpoint| !endpoint.is_empty()) else {
        return false;
    };
    let (Ok(target), Ok(endpoint)) = (Url::parse(target), Url::parse(upload_relay)) else {
        return false;
    };
    is_allowed_upload_relay_endpoint(&endpoint)
        && matches_upload_relay_endpoint_identity(&target, &endpoint)
        && has_allowed_upload_relay_path_and_token(&target, &endpoint)
}

fn is_allowed_upload_relay_endpoint(endpoint: &Url) -> bool {
    if !is_http(endpoint) || has_credentials(endpoint) || has_search(endpoint) || has_hash(endpoint)
    {
        return false;
    }
    if endpoint.path() != "/" && endpoint.path().ends_with('/') {
        return false;
    }
    !upload_relay_base_path(endpoint)
        .split('/')
        .any(|segment| segment == "relay")
}

fn matches_upload_relay_endpoint_identity(target: &Url, endpoint: &Url) -> bool {
    if target.scheme() != endpoint.scheme() {
        return false;
    }
    if has_credentials(target) || has_hash(target) {
        return false;
    }
    target.origin() == endpoint.origin()
}

fn has_allowed_upload_relay_path_and_token(target: &Url, endpoint: &Url) -> bool {
    let relay_key_prefix = format!(
        "{}{UPLOAD_RELAY_PATH_PREFIX}/",
        upload_relay_base_path(endpoint)
    );
    if !target.path().starts_with(&relay_key_prefix)
        || target.path().len() == relay_key_prefix.len()
    {
        return false;
    }
    has_allowed_upload_relay_query(target)
}

fn has_allowed_upload_relay_query(target: &Url) -> bool {
    let mut token = None;
    for (name, value) in target.query_pairs() {
        if name == UPLOAD_RELAY_TOKEN_PARAMETER {
            if token.is_some() {
                return false;
            }
            token = Some(value);
            continue;
        }
        if UPLOAD_RELAY_MULTIPART_PARAMETERS.contains(&name.as_ref()) {
            continue;
        }
        return false;
    }
    token.is_some_and(|token| !token.is_empty())
}

fn upload_relay_base_path(endpoint: &Url) -> &str {
    let path = endpoint.path();
    if path == "/" {
        return "";
    }
    path.strip_suffix("/v1/relay").unwrap_or(path)
}

fn is_http(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
}

fn has_credentials(url: &Url) -> bool {
    !url.username().is_empty() || url.password().is_some_and(|password| !password.is_empty())
}

fn has_search(url: &Url) -> bool {
    url.query().is_some_and(|query| !query.is_empty())
}

fn has_hash(url: &Url) -> bool {
    url.fragment().is_some_and(|fragment| !fragment.is_empty())
}

/// `Headers.get` of a header sent once; repeated values join with `, ` there, which no caller's
/// pattern accepts.
fn single_header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?;
    if values.next().is_some() {
        return None;
    }
    value.to_str().ok()
}

/// `readLocalAppUploadId`: `^[A-Za-z0-9_-]{1,128}$`.
pub fn upload_id(headers: &HeaderMap) -> Option<String> {
    let value = single_header(headers, UPLOAD_ID_HEADER)?;
    let valid = !value.is_empty()
        && value.len() <= UPLOAD_ID_MAX_LENGTH
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-');
    valid.then(|| value.to_owned())
}

/// `readRequestContentLength`: a trimmed `^\d+$` value that is a safe integer.
pub fn request_content_length(headers: &HeaderMap) -> Option<u64> {
    let value = js_trim(single_header(headers, header::CONTENT_LENGTH.as_str())?);
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value
        .parse::<u64>()
        .ok()
        .filter(|&length| length <= MAX_SAFE_INTEGER)
}

fn copy_allowed_headers(headers: &HeaderMap, blocked: impl Fn(&str) -> bool) -> HeaderMap {
    let mut next = HeaderMap::new();
    for (name, value) in headers {
        if !blocked(name.as_str()) {
            next.append(name.clone(), value.clone());
        }
    }
    next
}

fn is_api_request_blocked(name: &str) -> bool {
    HOP_BY_HOP_HEADERS.contains(&name) || API_REQUEST_BLOCKED_HEADERS.contains(&name)
}

/// `buildAPIRequestHeaders`: requests with a body carry the instance web app's origin.
pub fn api_request_headers(
    headers: &HeaderMap,
    method: &Method,
    origin: Option<&str>,
) -> HeaderMap {
    let mut next = copy_allowed_headers(headers, is_api_request_blocked);
    if let Some(origin) = origin
        && request_has_body(method)
        && let Ok(value) = HeaderValue::from_str(origin)
    {
        next.insert(header::ORIGIN, value);
    }
    next
}

/// `buildRemoteResourceRequestHeaders`: as the API headers, minus credentials.
pub fn remote_resource_request_headers(headers: &HeaderMap) -> HeaderMap {
    copy_allowed_headers(headers, |name| {
        is_api_request_blocked(name) || REMOTE_RESOURCE_REQUEST_BLOCKED_HEADERS.contains(&name)
    })
}

/// `buildProxyResponseHeaders`.
pub fn proxy_response_headers(headers: &HeaderMap, cache_default: CacheDefault) -> HeaderMap {
    let mut next = copy_allowed_headers(headers, |name| {
        HOP_BY_HOP_HEADERS.contains(&name) || RESPONSE_BLOCKED_HEADERS.contains(&name)
    });
    if cache_default == CacheDefault::NoStore && !next.contains_key(header::CACHE_CONTROL) {
        next.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(NO_STORE_CACHE_CONTROL),
        );
    }
    next.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(PROXY_RESPONSE_DOCUMENT_POLICY),
    );
    next.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    next
}

#[cfg(test)]
mod tests;
