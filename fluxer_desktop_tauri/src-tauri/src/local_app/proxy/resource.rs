// SPDX-License-Identifier: AGPL-3.0-or-later

//! `/proxy/<instance key>?url=<target>`, ported from LocalAppRemoteResourceRequestHandler.ts.
//!
//! Differs from Electron: the route answers on the local app origin, so a resource the
//! renderer could run or render as a document (script, style sheet, HTML, SVG, XML, XSLT,
//! WebAssembly) is served as `application/octet-stream`, and `nosniff` keeps it inert. SVG keeps
//! its type for requests that say they load an image, where it cannot run script.

use tauri::http::{HeaderMap, HeaderValue, Request, Response, StatusCode, header};

use super::client::{ProxyFetch, RESOURCE_RESPONSE_BODY_LIMIT, RESOURCE_RESPONSE_HEADERS_DEADLINE};
use super::policy::{
    CacheDefault, is_allowed_remote_proxy_target, is_supported_remote_resource_method,
    proxy_response_headers, remote_proxy_target_url, remote_resource_request_headers,
    request_content_length, upload_id,
};
use super::{
    Proxy, ProxyRoute, TEXT_CONTENT_TYPE, failure_response, not_found_response, proxy_response,
    text_response,
};
use crate::js::js_trim;

const UNKNOWN_RUNTIME_TEXT: &str = "Unknown local app resource runtime";
const FAILURE_TEXT: &str = "Network error during resource request";
const ALLOWED_METHODS: &str = "GET, HEAD, PUT";
const INERT_CONTENT_TYPE: &str = "application/octet-stream";
const SEC_FETCH_DEST_HEADER: &str = "sec-fetch-dest";
const IMAGE_DESTINATION: &str = "image";
const SVG_MEDIA_TYPE: &str = "image/svg+xml";
/// The WHATWG JavaScript MIME type essences.
const JAVASCRIPT_MEDIA_TYPES: &[&str] = &[
    "application/ecmascript",
    "application/javascript",
    "application/x-ecmascript",
    "application/x-javascript",
    "text/ecmascript",
    "text/javascript",
    "text/javascript1.0",
    "text/javascript1.1",
    "text/javascript1.2",
    "text/javascript1.3",
    "text/javascript1.4",
    "text/javascript1.5",
    "text/jscript",
    "text/livescript",
    "text/x-ecmascript",
    "text/x-javascript",
];
const ACTIVE_MEDIA_TYPES: &[&str] = &[
    "text/css",
    "text/html",
    "application/xhtml+xml",
    "image/svg+xml",
    "application/xml",
    "text/xml",
    "text/xsl",
    "application/wasm",
];

pub async fn handle(
    proxy: &Proxy,
    request: Request<Vec<u8>>,
    route: ProxyRoute,
) -> Response<Vec<u8>> {
    if !is_supported_remote_resource_method(request.method()) {
        return Response::builder()
            .status(StatusCode::METHOD_NOT_ALLOWED)
            .header(header::ALLOW, ALLOWED_METHODS)
            .header(header::CONTENT_TYPE, TEXT_CONTENT_TYPE)
            .body(b"Method not allowed".to_vec())
            .expect("static response parts are valid");
    }
    let Some(target) = remote_proxy_target_url(&route.url) else {
        return text_response(StatusCode::BAD_REQUEST, "Missing remote resource URL");
    };
    let plan = match proxy.plan_for_route(&route.runtime.runtime_key).await {
        Ok(Some(plan)) => plan,
        Ok(None) => return not_found_response(UNKNOWN_RUNTIME_TEXT),
        Err(error) => {
            tracing::warn!(%error, "Local app remote resource runtime plan lookup failed");
            return failure_response(StatusCode::BAD_GATEWAY, FAILURE_TEXT);
        }
    };
    if !is_allowed_remote_proxy_target(request.method(), &target, &plan) {
        tracing::warn!(instanceKey = %plan.instance_key, "Blocked a local app remote resource proxy request");
        return text_response(
            StatusCode::FORBIDDEN,
            "Remote resource is not allowed for the requested instance",
        );
    }
    let (parts, body) = request.into_parts();
    let upload_id = upload_id(&parts.headers);
    let image_request = is_image_request(&parts.headers);
    let fetch = ProxyFetch {
        target,
        headers: remote_resource_request_headers(&parts.headers),
        method: parts.method,
        body,
        accept_encoding: None,
        upload_id: upload_id.clone(),
        upload_total: request_content_length(&parts.headers),
        response_headers_deadline: Some(RESOURCE_RESPONSE_HEADERS_DEADLINE),
        response_body_limit: RESOURCE_RESPONSE_BODY_LIMIT,
    };
    match proxy.fetch(fetch).await {
        Ok(response) => {
            let mut headers = proxy_response_headers(&response.headers, CacheDefault::Upstream);
            if let Some(length) = response.headers.get(header::CONTENT_LENGTH)
                && !response.headers.contains_key(header::CONTENT_ENCODING)
            {
                headers.insert(header::CONTENT_LENGTH, length.clone());
            }
            neutralize_active_content_type(&mut headers, image_request);
            proxy_response(response.status, headers, response.body, response.held)
        }
        Err(error) => {
            proxy.uploads.failure(upload_id.as_deref());
            tracing::warn!(%error, "Local app remote resource proxy request failed");
            failure_response(StatusCode::BAD_GATEWAY, FAILURE_TEXT)
        }
    }
}

/// At least one `Sec-Fetch-Dest`, and every one `image`.
fn is_image_request(headers: &HeaderMap) -> bool {
    let mut destinations = headers.get_all(SEC_FETCH_DEST_HEADER).iter().peekable();
    destinations.peek().is_some()
        && destinations.all(|value| value.as_bytes() == IMAGE_DESTINATION.as_bytes())
}

/// Replaces every `Content-Type` when any value names active content or is not visible ASCII.
fn neutralize_active_content_type(headers: &mut HeaderMap, image_request: bool) {
    let active = headers.get_all(header::CONTENT_TYPE).iter().any(|value| {
        value
            .to_str()
            .map_or(true, |value| is_active_content_type(value, image_request))
    });
    if active {
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static(INERT_CONTENT_TYPE),
        );
    }
}

fn is_active_content_type(value: &str, image_request: bool) -> bool {
    let essence = js_trim(value.split(';').next().unwrap_or_default()).to_ascii_lowercase();
    if image_request && essence == SVG_MEDIA_TYPE {
        return false;
    }
    JAVASCRIPT_MEDIA_TYPES.contains(&essence.as_str())
        || essence.ends_with("+javascript")
        || ACTIVE_MEDIA_TYPES.contains(&essence.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn neutralized(values: &[&str]) -> Vec<String> {
        neutralized_for(values, false)
    }

    fn neutralized_for(values: &[&str], image_request: bool) -> Vec<String> {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append(
                header::CONTENT_TYPE,
                HeaderValue::from_bytes(value.as_bytes()).unwrap(),
            );
        }
        neutralize_active_content_type(&mut headers, image_request);
        headers
            .get_all(header::CONTENT_TYPE)
            .iter()
            .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned())
            .collect()
    }

    #[test]
    fn active_content_types_become_octet_streams() {
        for value in [
            "text/javascript",
            "application/javascript; charset=utf-8",
            "Application/ECMAScript",
            "text/ecmascript",
            "application/x-javascript",
            "application/vnd.test+javascript",
            "text/css",
            " text/html ;charset=utf-8",
            "application/xhtml+xml",
            "image/svg+xml",
            "application/xml",
            "text/xml",
            "text/xsl",
            "application/wasm",
        ] {
            assert_eq!(neutralized(&[value]), [INERT_CONTENT_TYPE], "{value}");
        }
        assert_eq!(
            neutralized(&["image/png", "text/html"]),
            [INERT_CONTENT_TYPE]
        );
        assert_eq!(neutralized(&["text/html\u{ff}"]), [INERT_CONTENT_TYPE]);
        assert_eq!(neutralized(&["image/png\u{ff}"]), [INERT_CONTENT_TYPE]);
    }

    #[test]
    fn passive_content_types_pass_through() {
        for value in [
            "image/png",
            "video/mp4",
            "audio/ogg; codecs=opus",
            "application/json",
            "text/plain; charset=utf-8",
            "application/octet-stream",
        ] {
            assert_eq!(neutralized(&[value]), [value], "{value}");
        }
        assert!(neutralized(&[]).is_empty());
    }

    fn destinations(values: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append(SEC_FETCH_DEST_HEADER, HeaderValue::from_str(value).unwrap());
        }
        headers
    }

    #[test]
    fn image_requests_need_every_destination_to_be_image() {
        assert!(is_image_request(&destinations(&["image"])));
        assert!(is_image_request(&destinations(&["image", "image"])));
        assert!(!is_image_request(&destinations(&[])));
        assert!(!is_image_request(&destinations(&["image", "empty"])));
        assert!(!is_image_request(&destinations(&["Image"])));
        assert!(!is_image_request(&destinations(&["document"])));
    }

    #[test]
    fn svg_keeps_its_type_for_image_requests_only() {
        assert_eq!(neutralized_for(&["image/svg+xml"], true), ["image/svg+xml"]);
        assert_eq!(
            neutralized_for(&["Image/SVG+XML; charset=utf-8"], true),
            ["Image/SVG+XML; charset=utf-8"]
        );
        assert_eq!(
            neutralized_for(&["image/svg+xml"], false),
            [INERT_CONTENT_TYPE]
        );
        for value in ["text/html", "text/javascript", "application/xml"] {
            assert_eq!(
                neutralized_for(&[value], true),
                [INERT_CONTENT_TYPE],
                "{value}"
            );
        }
    }
}
