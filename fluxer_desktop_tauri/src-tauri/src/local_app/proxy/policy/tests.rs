// SPDX-License-Identifier: AGPL-3.0-or-later

use tauri::http::{HeaderMap, HeaderValue, Method};
use url::Url;

use super::*;
use crate::local_app::url::origin;
use crate::runtime::test_support::loopback_plan;

fn local(path_and_query: &str) -> Url {
    Url::parse(&format!("{}{path_and_query}", origin())).unwrap()
}

fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (name, value) in pairs {
        map.append(*name, HeaderValue::from_str(value).unwrap());
    }
    map
}

fn names(map: &HeaderMap) -> Vec<&str> {
    let mut names: Vec<&str> = map.keys().map(|name| name.as_str()).collect();
    names.sort_unstable();
    names
}

fn route(path: &str, base: &str) -> Option<(String, String)> {
    parse_runtime_route(path, base).map(|route| (route.runtime_key, route.local_path_prefix))
}

#[test]
fn runtime_routes_decode_the_key_segment() {
    assert_eq!(
        route("/api/https%3A%2F%2Fchat.example%2Fapi/v1/users", "/api"),
        Some((
            "https://chat.example/api".to_owned(),
            "/api/https%3A%2F%2Fchat.example%2Fapi".to_owned()
        ))
    );
    assert_eq!(
        route("/proxy/key", "/proxy"),
        Some(("key".to_owned(), "/proxy/key".to_owned()))
    );
    assert_eq!(
        route("/api/key/", "/api"),
        Some(("key".to_owned(), "/api/key".to_owned()))
    );
    assert_eq!(
        route("/api/%E2%9C%93", "/api"),
        Some(("\u{2713}".to_owned(), "/api/%E2%9C%93".to_owned()))
    );
}

#[test]
fn runtime_routes_reject_missing_blank_and_malformed_keys() {
    for path in [
        "/api",
        "/api/",
        "/api//v1",
        "/apix/key",
        "/proxy/key",
        "/api/%20%09",
        "/api/%",
        "/api/%2",
        "/api/%zz",
        "/api/%FF",
        "/api/%C0%AF",
    ] {
        assert_eq!(route(path, "/api"), None, "{path}");
    }
}

#[test]
fn api_targets_append_the_local_suffix_to_the_endpoint_path() {
    let request = local("/api/key/v1/users/@me?with=1&b=%20");
    assert_eq!(
        api_target_url("https://chat.example/api", &request, "/api/key").as_deref(),
        Some("https://chat.example/api/v1/users/@me?with=1&b=%20")
    );
    assert_eq!(
        api_target_url("https://chat.example/api//", &request, "/api/key").as_deref(),
        Some("https://chat.example/api/v1/users/@me?with=1&b=%20")
    );
    assert_eq!(
        api_target_url("https://api.example", &request, "/api/key").as_deref(),
        Some("https://api.example/v1/users/@me?with=1&b=%20")
    );
    assert_eq!(
        api_target_url("https://api.example/", &local("/api/key"), "/api/key").as_deref(),
        Some("https://api.example/")
    );
    assert_eq!(
        api_target_url("https://api.example/api", &local("/api/key?"), "/api/key").as_deref(),
        Some("https://api.example/api/")
    );
}

#[test]
fn api_target_paths_are_reparsed_as_http_paths() {
    // Dot segments in the request resolve before routing, so they cannot leave the route.
    let request = local("/api/key/v1/../x");
    assert_eq!(request.path(), "/api/key/x");
    // A backslash is literal in the local URL but a separator in the http target, so its dot
    // segments climb out of the endpoint path, as with the WHATWG pathname setter.
    let request = local("/api/key/v1/..\\..\\x");
    assert_eq!(
        api_target_url("https://chat.example/api", &request, "/api/key").as_deref(),
        Some("https://chat.example/x")
    );
    // Special-scheme query encoding applies to the copied query.
    let request = local("/api/key/v1?q='a'");
    assert_eq!(
        api_target_url("https://chat.example/api", &request, "/api/key").as_deref(),
        Some("https://chat.example/api/v1?q=%27a%27")
    );
}

#[test]
fn remote_targets_come_from_the_first_url_parameter() {
    let target = |query: &str| remote_proxy_target_url(&local(&format!("/proxy/key{query}")));
    assert_eq!(
        target("?url=https%3A%2F%2Fmedia.example%2Fa.png%3Fx%3D1").as_deref(),
        Some("https://media.example/a.png?x=1")
    );
    assert_eq!(
        target("?url=https://a.example/1&url=https://b.example/2").as_deref(),
        Some("https://a.example/1")
    );
    assert_eq!(
        target("?url=https://a.example/a+b").as_deref(),
        Some("https://a.example/a%20b")
    );
    for query in [
        "",
        "?url=",
        "?other=https://a.example/",
        "?url=not%20a%20url",
        "?url=ftp://a.example/",
        "?url=fluxer-app://app/",
        "?url=https://user@a.example/",
        "?url=https://user:pw@a.example/",
    ] {
        assert_eq!(target(query), None, "{query}");
    }
}

#[test]
fn api_request_headers_drop_hop_by_hop_and_ambient_headers() {
    let incoming = headers(&[
        ("connection", "keep-alive"),
        ("keep-alive", "timeout=5"),
        ("proxy-authenticate", "x"),
        ("proxy-authorization", "x"),
        ("te", "trailers"),
        ("trailer", "x"),
        ("transfer-encoding", "chunked"),
        ("upgrade", "h2c"),
        ("x-fluxer-local-upload-id", "abc"),
        ("accept-encoding", "zstd"),
        ("content-length", "5"),
        ("cookie", "a=b"),
        ("expect", "100-continue"),
        ("host", "app"),
        ("origin", "fluxer-app://app"),
        ("proxy-connection", "close"),
        ("referer", "fluxer-app://app/"),
        ("authorization", "token"),
        ("x-fluxer-sudo-mode-jwt", "jwt"),
        ("content-type", "application/json"),
        ("x-custom", "1"),
        ("x-custom", "2"),
    ]);
    let forwarded = api_request_headers(&incoming, &Method::GET, Some("https://web.example"));
    assert_eq!(
        names(&forwarded),
        [
            "authorization",
            "content-type",
            "x-custom",
            "x-fluxer-sudo-mode-jwt"
        ]
    );
    assert_eq!(forwarded.get_all("x-custom").iter().count(), 2);

    let resource = remote_resource_request_headers(&incoming);
    assert_eq!(names(&resource), ["content-type", "x-custom"]);
}

#[test]
fn requests_with_a_body_carry_the_web_app_origin() {
    let incoming = headers(&[("origin", "fluxer-app://app")]);
    for method in [Method::GET, Method::HEAD] {
        assert!(
            api_request_headers(&incoming, &method, Some("https://web.example"))
                .get("origin")
                .is_none()
        );
    }
    for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
        assert_eq!(
            api_request_headers(&incoming, &method, Some("https://web.example"))
                .get("origin")
                .unwrap(),
            "https://web.example"
        );
    }
    assert!(
        api_request_headers(&incoming, &Method::POST, None)
            .get("origin")
            .is_none()
    );
}

#[test]
fn response_headers_drop_cors_cookies_encoding_policy_and_document_controls() {
    let upstream = headers(&[
        ("connection", "close"),
        ("transfer-encoding", "chunked"),
        ("access-control-allow-credentials", "true"),
        ("access-control-allow-headers", "*"),
        ("access-control-allow-methods", "*"),
        ("access-control-allow-origin", "*"),
        ("access-control-expose-headers", "*"),
        ("access-control-max-age", "1"),
        ("content-encoding", "gzip"),
        ("content-length", "10"),
        ("content-security-policy", "default-src *"),
        ("content-security-policy-report-only", "default-src *"),
        ("set-cookie", "a=b"),
        ("set-cookie2", "a=b"),
        ("clear-site-data", "\"*\""),
        ("refresh", "0; url=https://evil.test/"),
        ("link", "<https://evil.test/sw.js>; rel=serviceworker"),
        ("service-worker-allowed", "/"),
        ("origin-agent-cluster", "?1"),
        ("cross-origin-opener-policy", "same-origin"),
        ("cross-origin-embedder-policy", "require-corp"),
        ("permissions-policy", "camera=*"),
        ("document-policy", "force-load-at-top"),
        ("report-to", "{}"),
        ("nel", "{}"),
        ("reporting-endpoints", "default=\"https://evil.test/\""),
        ("x-content-type-options", "other"),
        ("content-type", "application/json"),
        ("x-ratelimit-remaining", "4"),
    ]);
    let api = proxy_response_headers(&upstream, CacheDefault::NoStore);
    assert_eq!(
        names(&api),
        [
            "cache-control",
            "content-security-policy",
            "content-type",
            "x-content-type-options",
            "x-ratelimit-remaining"
        ]
    );
    assert_eq!(api["cache-control"], "no-store");
    assert_eq!(
        api["content-security-policy"],
        "sandbox; default-src 'none'; frame-ancestors 'none'"
    );
    assert_eq!(api.get_all("x-content-type-options").iter().count(), 1);
    assert_eq!(api["x-content-type-options"], "nosniff");

    let resource = proxy_response_headers(&upstream, CacheDefault::Upstream);
    assert!(resource.get("cache-control").is_none());
    let cached = headers(&[("cache-control", "max-age=60")]);
    assert_eq!(
        proxy_response_headers(&cached, CacheDefault::NoStore)["cache-control"],
        "max-age=60"
    );
}

#[test]
fn upload_ids_match_the_renderer_pattern() {
    let id = |value: &str| upload_id(&headers(&[("x-fluxer-local-upload-id", value)]));
    assert_eq!(id("aZ09_-").as_deref(), Some("aZ09_-"));
    assert_eq!(
        id(&"a".repeat(128)).as_deref(),
        Some("a".repeat(128).as_str())
    );
    for value in ["", "a b", "a.b", "a/b", &"a".repeat(129)] {
        assert_eq!(id(value), None, "{value}");
    }
    assert_eq!(upload_id(&HeaderMap::new()), None);
    let repeated = headers(&[
        ("x-fluxer-local-upload-id", "a"),
        ("x-fluxer-local-upload-id", "b"),
    ]);
    assert_eq!(upload_id(&repeated), None);
}

#[test]
fn content_lengths_are_trimmed_safe_integers() {
    let length = |value: &str| request_content_length(&headers(&[("content-length", value)]));
    assert_eq!(length("0"), Some(0));
    assert_eq!(length(" 42 "), Some(42));
    assert_eq!(length("007"), Some(7));
    assert_eq!(length("9007199254740991"), Some(9_007_199_254_740_991));
    for value in [
        "",
        "-1",
        "1.5",
        "1e3",
        "0x10",
        "9007199254740992",
        "99999999999999999999",
    ] {
        assert_eq!(length(value), None, "{value}");
    }
    assert_eq!(request_content_length(&HeaderMap::new()), None);
}

#[test]
fn remote_reads_may_reach_the_plan_client_origins_only() {
    let plan = loopback_plan(5300);
    let allowed = |target: &str| is_allowed_remote_proxy_target(&Method::GET, target, &plan);
    assert!(allowed("http://127.0.0.1:5300/media/a.png"));
    assert!(allowed("http://127.0.0.1:5300/anything?x=1"));
    assert!(is_allowed_remote_proxy_target(
        &Method::HEAD,
        "http://127.0.0.1:5300/a",
        &plan
    ));
    assert!(!allowed("http://127.0.0.1:5301/media/a.png"));
    assert!(!allowed("https://127.0.0.1:5300/media/a.png"));
    assert!(!allowed("http://localhost:5300/media/a.png"));
    assert!(!allowed("not a url"));
}

fn relay_plan(upload_relay: &str) -> LocalAppRuntimePlan {
    let mut plan = loopback_plan(5400);
    plan.endpoints.upload_relay = Some(upload_relay.to_owned());
    plan
}

fn put_allowed(upload_relay: &str, target: &str) -> bool {
    is_allowed_remote_proxy_target(&Method::PUT, target, &relay_plan(upload_relay))
}

const RELAY: &str = "https://relay.example/upload";
const RELAY_TARGET: &str = "https://relay.example/upload/v1/relay/key";

#[test]
fn uploads_reach_relay_keys_with_one_token() {
    assert!(put_allowed(RELAY, &format!("{RELAY_TARGET}?t=tok")));
    assert!(put_allowed(
        RELAY,
        &format!("{RELAY_TARGET}?uploadId=u&partNumber=1&t=tok&partNumber=2")
    ));
    assert!(put_allowed(
        "https://relay.example/upload/v1/relay",
        &format!("{RELAY_TARGET}?t=tok")
    ));
    assert!(put_allowed(
        "https://relay.example",
        "https://relay.example/v1/relay/key/part?t=tok"
    ));
    assert!(put_allowed(
        "https://relay.example/",
        "https://relay.example/v1/relay/key?t=tok"
    ));
}

#[test]
fn upload_relay_queries_need_exactly_one_non_empty_token_and_nothing_else() {
    for query in [
        "",
        "?",
        "?t=",
        "?t=a&t=b",
        "?t=a&t=",
        "?uploadId=u",
        "?t=a&x=1",
        "?t=a&T=b",
        "?t=a&uploadid=u",
        "?t=a&url=x",
    ] {
        assert!(
            !put_allowed(RELAY, &format!("{RELAY_TARGET}{query}")),
            "{query}"
        );
    }
}

#[test]
fn upload_relay_paths_must_be_keys_under_the_relay_prefix() {
    for target in [
        "https://relay.example/upload/v1/relay/?t=a",
        "https://relay.example/upload/v1/relay?t=a",
        "https://relay.example/upload/v1/relayx/key?t=a",
        "https://relay.example/v1/relay/key?t=a",
        "https://relay.example/other/v1/relay/key?t=a",
    ] {
        assert!(!put_allowed(RELAY, target), "{target}");
    }
}

#[test]
fn upload_relay_targets_must_share_the_relay_identity() {
    for target in [
        "http://relay.example/upload/v1/relay/key?t=a",
        "https://relay.example:8443/upload/v1/relay/key?t=a",
        "https://other.example/upload/v1/relay/key?t=a",
        "https://user@relay.example/upload/v1/relay/key?t=a",
        "https://user:pw@relay.example/upload/v1/relay/key?t=a",
        "https://relay.example/upload/v1/relay/key?t=a#frag",
        "not a url",
    ] {
        assert!(!put_allowed(RELAY, target), "{target}");
    }
}

#[test]
fn upload_relay_endpoints_must_be_plain_http_bases() {
    let target = format!("{RELAY_TARGET}?t=a");
    for endpoint in [
        "",
        "not a url",
        "ftp://relay.example/upload",
        "https://user@relay.example/upload",
        "https://relay.example/upload?x=1",
        "https://relay.example/upload#x",
        "https://relay.example/upload/",
        "https://relay.example/relay",
        "https://relay.example/relay/upload",
    ] {
        assert!(!put_allowed(endpoint, &target), "{endpoint}");
    }
    let mut plan = loopback_plan(5400);
    plan.endpoints.upload_relay = None;
    assert!(!is_allowed_remote_proxy_target(
        &Method::PUT,
        &target,
        &plan
    ));
}

#[test]
fn only_get_head_and_put_reach_remote_resources() {
    for method in [Method::GET, Method::HEAD, Method::PUT] {
        assert!(is_supported_remote_resource_method(&method));
    }
    for method in [Method::POST, Method::DELETE, Method::PATCH, Method::OPTIONS] {
        assert!(!is_supported_remote_resource_method(&method));
    }
}
