// SPDX-License-Identifier: AGPL-3.0-or-later

use ::url::Url;

pub const SCHEME: &str = "fluxer-app";
#[cfg(not(windows))]
pub const HOST: &str = "app";
pub const API_PATH_PREFIX: &str = "/api";
pub const REMOTE_PROXY_PATH_PREFIX: &str = "/proxy";
/// The shell's own voice debug event sink page, served to its window only (event_sink).
pub const VOICE_DEBUG_EVENT_SINK_PATH: &str = "/_desktop/voice-debug-event-sink";

#[cfg(not(windows))]
const ORIGIN: &str = "fluxer-app://app";
#[cfg(windows)]
const ORIGIN: &str = "https://fluxer-app.localhost";

#[cfg(not(windows))]
const EXPECTED_SCHEME: &str = SCHEME;
#[cfg(windows)]
const EXPECTED_SCHEME: &str = "https";

#[cfg(not(windows))]
const EXPECTED_HOST: &str = HOST;
#[cfg(windows)]
const EXPECTED_HOST: &str = "fluxer-app.localhost";

/// Origin the renderer document is served from. Tauri serves custom schemes as
/// `https://<scheme>.localhost` on Windows.
pub fn origin() -> &'static str {
    ORIGIN
}

pub fn parse_local_app_url(value: &str) -> Option<Url> {
    if value.is_empty() {
        return None;
    }
    let parsed = Url::parse(value).ok()?;
    if parsed.scheme() != EXPECTED_SCHEME {
        return None;
    }
    // The WHATWG parser lowercases special-scheme hosts but keeps non-special ones opaque.
    if !parsed.host_str()?.eq_ignore_ascii_case(EXPECTED_HOST) {
        return None;
    }
    if parsed.port().is_some() || !parsed.username().is_empty() || parsed.password().is_some() {
        return None;
    }
    Some(parsed)
}

pub fn is_local_app_url(value: &str) -> bool {
    parse_local_app_url(value).is_some()
}

pub fn is_reserved_proxy_path(pathname: &str) -> bool {
    [API_PATH_PREFIX, REMOTE_PROXY_PATH_PREFIX]
        .iter()
        .any(|prefix| {
            pathname == *prefix
                || pathname
                    .strip_prefix(prefix)
                    .is_some_and(|rest| rest.starts_with('/'))
        })
}

/// A local app URL outside the proxy routes and the shell's own page.
pub fn is_renderer_document_url(value: &str) -> bool {
    parse_local_app_url(value).is_some_and(|parsed| {
        !is_reserved_proxy_path(parsed.path()) && parsed.path() != VOICE_DEBUG_EVENT_SINK_PATH
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(windows))]
    #[test]
    fn custom_scheme_parses_with_host() {
        let parsed = Url::parse("fluxer-app://app/x").unwrap();
        assert_eq!(parsed.host_str(), Some("app"));
        assert_eq!(parsed.path(), "/x");
        assert_eq!(origin(), "fluxer-app://app");
    }

    #[cfg(not(windows))]
    #[test]
    fn accepts_local_app_urls() {
        assert!(is_local_app_url("fluxer-app://app/"));
        assert!(is_local_app_url("fluxer-app://app"));
        assert!(is_local_app_url("fluxer-app://app/channels/1?x=1#y"));
        assert!(is_local_app_url("FLUXER-APP://APP/"));
    }

    #[cfg(not(windows))]
    #[test]
    fn rejects_foreign_or_decorated_urls() {
        assert!(!is_local_app_url(""));
        assert!(!is_local_app_url("not a url"));
        assert!(!is_local_app_url("https://app/"));
        assert!(!is_local_app_url("fluxer-app://other/"));
        assert!(!is_local_app_url("fluxer-app://app:8080/"));
        assert!(!is_local_app_url("fluxer-app://user@app/"));
        assert!(!is_local_app_url("fluxer-app://user:pw@app/"));
        assert!(!is_local_app_url("fluxer-app://app.evil/"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_uses_https_localhost() {
        assert!(is_local_app_url("https://fluxer-app.localhost/x"));
        assert!(!is_local_app_url("fluxer-app://app/"));
        assert!(!is_local_app_url("https://fluxer-app.localhost:1/"));
        assert_eq!(origin(), "https://fluxer-app.localhost");
    }

    #[test]
    fn reserved_paths_are_not_renderer_documents() {
        let o = origin();
        assert!(is_renderer_document_url(&format!("{o}/")));
        assert!(is_renderer_document_url(&format!("{o}/channels/1")));
        assert!(is_renderer_document_url(&format!("{o}/apix")));
        assert!(is_renderer_document_url(&format!("{o}/proxying")));
        assert!(!is_renderer_document_url(&format!("{o}/api")));
        assert!(!is_renderer_document_url(&format!("{o}/api/v1/users")));
        assert!(!is_renderer_document_url(&format!("{o}/proxy")));
        assert!(!is_renderer_document_url(&format!("{o}/proxy/x?url=y")));
        assert!(!is_renderer_document_url("https://example.com/"));
    }
}
