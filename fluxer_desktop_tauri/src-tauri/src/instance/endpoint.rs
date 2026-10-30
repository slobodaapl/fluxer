// SPDX-License-Identifier: AGPL-3.0-or-later

//! Instance endpoint canonicalisation, ported from `NetworkOrigin.ts` and
//! `EndpointNormalization.ts` in `packages/instance_bootstrap`.

use std::borrow::Cow;

use url::Url;

use crate::js::is_js_whitespace;

pub const NETWORK_ENDPOINT_INPUT_MAX_BYTES: usize = 8192;

const HTTP_PROTOCOLS: &[&str] = &["http", "https"];
const WEBSOCKET_PROTOCOLS: &[&str] = &["ws", "wss"];

/// Which endpoints a call site accepts; `protocols` are URL schemes without the colon.
#[derive(Debug, Clone, Copy)]
pub struct CanonicalEndpointRule {
    pub protocols: &'static [&'static str],
    pub allow_path: bool,
    pub allow_relative: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointKind {
    Api,
    WebApp,
    Gateway,
    Service,
}

impl EndpointKind {
    const fn rule(self) -> CanonicalEndpointRule {
        match self {
            Self::Api => CanonicalEndpointRule {
                protocols: HTTP_PROTOCOLS,
                allow_path: true,
                allow_relative: true,
            },
            Self::WebApp => CanonicalEndpointRule {
                protocols: HTTP_PROTOCOLS,
                allow_path: false,
                allow_relative: false,
            },
            Self::Gateway => CanonicalEndpointRule {
                protocols: WEBSOCKET_PROTOCOLS,
                allow_path: true,
                allow_relative: false,
            },
            Self::Service => CanonicalEndpointRule {
                protocols: HTTP_PROTOCOLS,
                allow_path: true,
                allow_relative: false,
            },
        }
    }
}

pub(super) fn strip_trailing_slashes(path: &str) -> &str {
    path.trim_end_matches('/')
}

/// `^[a-zA-Z][a-zA-Z0-9+.-]*://`
fn has_network_protocol_prefix(value: &str) -> bool {
    let Some(separator) = value.find("://") else {
        return false;
    };
    let mut scheme = value[..separator].chars();
    scheme
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic())
        && scheme.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
}

fn network_authority(candidate: &str) -> &str {
    let Some(separator) = candidate.find("://") else {
        return "";
    };
    let rest = &candidate[separator + 3..];
    &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())]
}

fn normalize_relative_network_endpoint(trimmed: &str) -> Option<String> {
    if trimmed.starts_with("//") || trimmed.contains(['?', '#']) {
        return None;
    }
    let path = strip_trailing_slashes(trimmed);
    (!path.is_empty()).then(|| path.to_owned())
}

pub fn normalize_canonical_network_endpoint(
    value: &str,
    rule: &CanonicalEndpointRule,
) -> Option<String> {
    if value.len() > NETWORK_ENDPOINT_INPUT_MAX_BYTES {
        return None;
    }
    let trimmed = value.trim_matches(is_js_whitespace);
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with('/') {
        return if rule.allow_relative {
            normalize_relative_network_endpoint(trimmed)
        } else {
            None
        };
    }
    let candidate: Cow<'_, str> = if has_network_protocol_prefix(trimmed) {
        Cow::Borrowed(trimmed)
    } else {
        Cow::Owned(format!("https://{trimmed}"))
    };
    if network_authority(&candidate).is_empty() {
        return None;
    }
    let url = Url::parse(&candidate).ok()?;
    if !rule.protocols.contains(&url.scheme()) {
        return None;
    }
    let host = url.host_str().filter(|host| !host.is_empty())?;
    if !url.username().is_empty() || url.password().is_some_and(|password| !password.is_empty()) {
        return None;
    }
    if url.port() == Some(0)
        || url.query().is_some_and(|query| !query.is_empty())
        || url.fragment().is_some_and(|fragment| !fragment.is_empty())
    {
        return None;
    }
    let path = strip_trailing_slashes(url.path());
    if !path.is_empty() && !rule.allow_path {
        return None;
    }
    let mut endpoint = format!("{}://{host}", url.scheme());
    if let Some(port) = url.port() {
        endpoint.push(':');
        endpoint.push_str(&port.to_string());
    }
    endpoint.push_str(path);
    Some(endpoint)
}

pub fn normalize_instance_endpoint(input: &str, kind: EndpointKind) -> Option<String> {
    normalize_canonical_network_endpoint(input, &kind.rule())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NETWORK_ORIGIN_ONLY_RULE: CanonicalEndpointRule = CanonicalEndpointRule {
        protocols: HTTP_PROTOCOLS,
        allow_path: false,
        allow_relative: false,
    };

    fn origin(value: &str) -> Option<String> {
        normalize_canonical_network_endpoint(value, &NETWORK_ORIGIN_ONLY_RULE)
    }

    fn assert_origin(value: &str, expected: Option<&str>) {
        assert_eq!(origin(value).as_deref(), expected, "{value}");
    }

    #[test]
    fn a_written_default_port_collapses_to_the_same_origin_as_the_bare_host() {
        assert_origin("fluxer.app:443", Some("https://fluxer.app"));
        assert_origin("https://fluxer.app:443", Some("https://fluxer.app"));
        assert_origin("http://fluxer.app:80", Some("http://fluxer.app"));
        assert_origin("https://[::1]:443", Some("https://[::1]"));
    }

    #[test]
    fn one_origin_has_one_canonical_form_the_one_url_origin_agrees_with() {
        for value in [
            "fluxer.app",
            "fluxer.app:443",
            "https://fluxer.app:443",
            "https://fluxer.app/",
        ] {
            assert_origin(value, Some("https://fluxer.app"));
        }
        for value in [
            "fluxer.app:443",
            "https://fluxer.app:8443",
            "http://fluxer.app:80",
            "http://[::1]:8080",
        ] {
            let normalized = origin(value).expect(value);
            let url_origin = Url::parse(&normalized)
                .unwrap()
                .origin()
                .ascii_serialization();
            assert_eq!(url_origin, normalized, "{value}");
        }
    }

    #[test]
    fn a_host_slices_off_the_canonical_origin_without_a_port_riding_along() {
        let normalized = origin("web.fluxer.app:443").unwrap();
        assert_eq!(&normalized["https://".len()..], "web.fluxer.app");
    }

    #[test]
    fn a_non_default_port_is_kept() {
        assert_origin("fluxer.app:8443", Some("https://fluxer.app:8443"));
        assert_origin("http://localhost:3000", Some("http://localhost:3000"));
        assert_origin("https://[::1]:8443", Some("https://[::1]:8443"));
    }

    #[test]
    fn a_leading_zero_port_is_normalised_the_way_url_normalises_it() {
        assert_origin("https://fluxer.app:0443", Some("https://fluxer.app"));
        assert_origin("https://fluxer.app:08443", Some("https://fluxer.app:8443"));
    }

    #[test]
    fn port_zero_credentials_a_path_a_query_and_a_fragment_are_all_refused() {
        for value in [
            "https://fluxer.app:0",
            "https://user:pass@fluxer.app",
            "https://fluxer.app/api",
            "https://fluxer.app?a=1",
            "https://fluxer.app#a",
            "//fluxer.app",
            "wss://fluxer.app",
        ] {
            assert_origin(value, None);
        }
    }

    #[test]
    fn a_websocket_endpoint_drops_its_written_default_port_too() {
        let gateway = |value| normalize_instance_endpoint(value, EndpointKind::Gateway);
        assert_eq!(
            gateway("wss://gateway.fluxer.app:443").as_deref(),
            Some("wss://gateway.fluxer.app")
        );
        assert_eq!(
            gateway("ws://localhost:80/ws").as_deref(),
            Some("ws://localhost/ws")
        );
        assert_eq!(
            gateway("wss://gateway.fluxer.app:8443").as_deref(),
            Some("wss://gateway.fluxer.app:8443")
        );
        assert_eq!(gateway("https://gateway.fluxer.app"), None);
        assert_eq!(gateway("/gateway"), None);
    }

    #[test]
    fn an_api_endpoint_keeps_its_path_and_still_collapses_the_default_port() {
        let api = |value| normalize_instance_endpoint(value, EndpointKind::Api);
        assert_eq!(
            api("https://fluxer.app:443/api/v1/").as_deref(),
            Some("https://fluxer.app/api/v1")
        );
        assert_eq!(api("/api").as_deref(), Some("/api"));
        assert_eq!(api("/api/").as_deref(), Some("/api"));
        for value in ["/", "//a.com", "/a?x", "/a#x"] {
            assert_eq!(api(value), None, "{value}");
        }
    }

    #[test]
    fn a_webapp_endpoint_is_an_origin_only() {
        let webapp = |value| normalize_instance_endpoint(value, EndpointKind::WebApp);
        assert_eq!(
            webapp("web.fluxer.app/").as_deref(),
            Some("https://web.fluxer.app")
        );
        assert_eq!(webapp("https://web.fluxer.app/app"), None);
    }

    #[test]
    fn service_endpoints_match_the_typescript_normaliser() {
        let service = |value| normalize_instance_endpoint(value, EndpointKind::Service);
        for (input, expected) in [
            ("https://EXAMPLE.com/Api/", Some("https://example.com/Api")),
            ("example.com", Some("https://example.com")),
            ("  example.com/api// ", Some("https://example.com/api")),
            (
                "\u{feff}\u{a0}example.com\u{2028}",
                Some("https://example.com"),
            ),
            ("https://a.com?", Some("https://a.com")),
            ("https://a.com/#", Some("https://a.com")),
            ("https://a.com/x#y", None),
            ("ftp://a.com", None),
            ("https://", None),
            ("https:///a", None),
            ("https://[::1]:443/x", Some("https://[::1]/x")),
            (
                "https://[2001:DB8::1]:8080",
                Some("https://[2001:db8::1]:8080"),
            ),
            (
                "https://bücher.de/pä",
                Some("https://xn--bcher-kva.de/p%C3%A4"),
            ),
            ("http://0x7f.1", Some("http://127.0.0.1")),
            ("https://a.com:0", None),
            ("https://a.com:65536", None),
            ("https://a@a.com", None),
            ("https://:@a.com", Some("https://a.com")),
            ("/api", None),
            ("a b.com", None),
            ("https://a.com/a%20b/", Some("https://a.com/a%20b")),
            ("HTTP://A.COM:80", Some("http://a.com")),
            ("wss://g.example.com/ws/", None),
            ("https://a.com//", Some("https://a.com")),
            ("mailto://x", None),
            ("https://a.com\\x", Some("https://a.com/x")),
            ("localhost:3000", Some("https://localhost:3000")),
            ("https://a.com:/p", Some("https://a.com/p")),
            ("https://exa mple.com", None),
            ("", None),
            ("   ", None),
        ] {
            assert_eq!(service(input).as_deref(), expected, "{input:?}");
        }
    }

    #[test]
    fn input_over_the_byte_cap_is_refused_before_trimming() {
        let at_cap = format!(
            "{}example.com",
            " ".repeat(NETWORK_ENDPOINT_INPUT_MAX_BYTES - 11)
        );
        assert_eq!(at_cap.len(), NETWORK_ENDPOINT_INPUT_MAX_BYTES);
        assert_eq!(
            normalize_instance_endpoint(&at_cap, EndpointKind::Service).as_deref(),
            Some("https://example.com")
        );
        let over_cap = format!(" {at_cap}");
        assert_eq!(
            normalize_instance_endpoint(&over_cap, EndpointKind::Service),
            None
        );
        // Cap counts UTF-8 bytes, not characters: 2731 three-byte characters exceed 8192.
        let multibyte = "\u{3000}".repeat(2731);
        assert_eq!(multibyte.len(), 8193);
        assert_eq!(
            normalize_instance_endpoint(&multibyte, EndpointKind::Service),
            None
        );
    }
}
