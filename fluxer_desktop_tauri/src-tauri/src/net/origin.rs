// SPDX-License-Identifier: AGPL-3.0-or-later

//! Canonical http(s) origins, ported from
//! `packages/instance_bootstrap/src/NetworkOrigin.ts` (`normalizeHTTPNetworkOrigin`).

use std::borrow::Cow;

use url::Url;

use crate::js::js_trim;

const NETWORK_ENDPOINT_INPUT_MAX_BYTES: usize = 8192;

/// `^[a-zA-Z][a-zA-Z0-9+.-]*://`
fn has_protocol_prefix(value: &str) -> bool {
    let bytes = value.as_bytes();
    if !bytes.first().is_some_and(u8::is_ascii_alphabetic) {
        return false;
    }
    let scheme_end = bytes
        .iter()
        .position(|&b| !(b.is_ascii_alphanumeric() || matches!(b, b'+' | b'.' | b'-')))
        .unwrap_or(bytes.len());
    bytes[scheme_end..].starts_with(b"://")
}

/// Text between the first `://` and the next `/`, `?` or `#`.
fn network_authority(candidate: &str) -> &str {
    let Some(separator) = candidate.find("://") else {
        return "";
    };
    let rest = &candidate[separator + 3..];
    rest.find(['/', '?', '#']).map_or(rest, |end| &rest[..end])
}

/// Normalizes a user- or server-supplied value to a bare `scheme://host[:port]`
/// http(s) origin. Scheme-less input defaults to `https://`. Inputs over 8192
/// UTF-8 bytes, relative paths, userinfo, port 0, a non-empty path, query or
/// fragment are rejected. Host serialization (IDNA, IPv4 forms, default-port
/// elision) follows the WHATWG URL parser.
pub fn normalize_http_network_origin(input: &str) -> Option<String> {
    if input.len() > NETWORK_ENDPOINT_INPUT_MAX_BYTES {
        return None;
    }
    let trimmed = js_trim(input);
    if trimmed.is_empty() || trimmed.starts_with('/') {
        return None;
    }
    let candidate = if has_protocol_prefix(trimmed) {
        Cow::Borrowed(trimmed)
    } else {
        Cow::Owned(format!("https://{trimmed}"))
    };
    if network_authority(&candidate).is_empty() {
        return None;
    }
    let url = Url::parse(&candidate).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let host = url.host_str().filter(|host| !host.is_empty())?;
    // WHATWG `username`/`password`/`search`/`hash` getters are empty strings
    // for both absent and empty components.
    if !url.username().is_empty() || url.password().is_some_and(|p| !p.is_empty()) {
        return None;
    }
    if url.port() == Some(0)
        || url.query().is_some_and(|q| !q.is_empty())
        || url.fragment().is_some_and(|f| !f.is_empty())
    {
        return None;
    }
    if !url.path().trim_end_matches('/').is_empty() {
        return None;
    }
    Some(match url.port() {
        Some(port) => format!("{}://{host}:{port}", url.scheme()),
        None => format!("{}://{host}", url.scheme()),
    })
}

/// WHATWG origin serialization for http(s) URLs; `None` for other schemes.
pub fn http_origin_of(url: &Url) -> Option<String> {
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return None;
    }
    Some(url.origin().ascii_serialization())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn normalized(input: &str) -> Option<String> {
        normalize_http_network_origin(input)
    }

    #[test]
    fn defaults_to_https_and_canonicalizes() {
        assert_eq!(
            normalized("example.com").as_deref(),
            Some("https://example.com")
        );
        assert_eq!(
            normalized("  HTTP://EXAMPLE.com:80/  ").as_deref(),
            Some("http://example.com")
        );
        assert_eq!(
            normalized("https://example.com:443//").as_deref(),
            Some("https://example.com")
        );
        assert_eq!(
            normalized("http://example.com:443").as_deref(),
            Some("http://example.com:443")
        );
        assert_eq!(
            normalized("localhost:8080").as_deref(),
            Some("https://localhost:8080")
        );
        assert_eq!(
            normalized("https://example.com.").as_deref(),
            Some("https://example.com.")
        );
    }

    #[test]
    fn serializes_hosts_like_whatwg() {
        assert_eq!(
            normalized("https://bücher.de").as_deref(),
            Some("https://xn--bcher-kva.de")
        );
        assert_eq!(
            normalized("http://[0:0::1]:8080").as_deref(),
            Some("http://[::1]:8080")
        );
        assert_eq!(
            normalized("http://0x7f.1").as_deref(),
            Some("http://127.0.0.1")
        );
    }

    #[test]
    fn empty_query_and_fragment_are_absent() {
        assert_eq!(
            normalized("https://example.com?").as_deref(),
            Some("https://example.com")
        );
        assert_eq!(
            normalized("https://example.com#").as_deref(),
            Some("https://example.com")
        );
    }

    #[test]
    fn rejects_non_origins() {
        for input in [
            "",
            "   ",
            "/relative",
            "//example.com",
            "https://",
            "https:///path",
            "ftp://example.com",
            "ws://example.com",
            "https://user@example.com",
            "https://user:pass@example.com",
            "https://example.com:0",
            "https://example.com/path",
            "https://example.com?query",
            "https://example.com#fragment",
            "https://exa mple.com",
            "http://:80",
        ] {
            assert_eq!(normalized(input), None, "{input:?}");
        }
    }

    #[test]
    fn enforces_input_byte_cap_before_trimming() {
        let host = "a".repeat(63);
        let fits = format!("{host}.com");
        let padded = format!("{fits}{}", " ".repeat(8192 - fits.len()));
        assert_eq!(padded.len(), 8192);
        assert_eq!(normalized(&padded), Some(format!("https://{fits}")));
        assert_eq!(normalized(&format!("{padded} ")), None);
    }

    #[test]
    fn origin_of_http_urls() {
        let url = Url::parse("https://Example.com:443/a?b#c").unwrap();
        assert_eq!(http_origin_of(&url).as_deref(), Some("https://example.com"));
        let url = Url::parse("http://[::1]:8080/").unwrap();
        assert_eq!(http_origin_of(&url).as_deref(), Some("http://[::1]:8080"));
        let url = Url::parse("file:///tmp/x").unwrap();
        assert_eq!(http_origin_of(&url), None);
    }
}
