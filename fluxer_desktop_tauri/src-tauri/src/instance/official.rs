// SPDX-License-Identifier: AGPL-3.0-or-later

//! Official-instance client API aliasing, ported from `OfficialInstance.ts`.

use url::Url;

use super::endpoint::strip_trailing_slashes;
use crate::js::is_js_whitespace;

const STABLE_API_ENDPOINT: &str = "https://web.fluxer.app/api";
const CANARY_API_ENDPOINT: &str = "https://web.canary.fluxer.app/api";
const OFFICIAL_CLIENT_API_PATH: &str = "/api";

/// Hosts (as `URL.host`, port included) that alias an official client API endpoint.
const OFFICIAL_CLIENT_API_HOST_ENDPOINTS: [(&str, &str); 4] = [
    ("web.fluxer.app", STABLE_API_ENDPOINT),
    ("fluxer.com", STABLE_API_ENDPOINT),
    ("web.canary.fluxer.app", CANARY_API_ENDPOINT),
    ("canary.fluxer.com", CANARY_API_ENDPOINT),
];

/// Maps an alias of an official API endpoint to its canonical endpoint.
pub fn official_client_api_endpoint_for_alias(endpoint: &str) -> Option<&'static str> {
    let url = Url::parse(endpoint.trim_matches(is_js_whitespace)).ok()?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some_and(|password| !password.is_empty())
        || url.query().is_some_and(|query| !query.is_empty())
        || url.fragment().is_some_and(|fragment| !fragment.is_empty())
    {
        return None;
    }
    if strip_trailing_slashes(url.path()) != OFFICIAL_CLIENT_API_PATH {
        return None;
    }
    let mut host = url.host_str()?.to_owned();
    if let Some(port) = url.port() {
        host.push(':');
        host.push_str(&port.to_string());
    }
    OFFICIAL_CLIENT_API_HOST_ENDPOINTS
        .iter()
        .find(|(alias, _)| *alias == host)
        .map(|(_, canonical)| *canonical)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_hosts_alias_to_their_channel_endpoint() {
        for (input, expected) in [
            ("https://web.fluxer.app/api", STABLE_API_ENDPOINT),
            ("https://fluxer.com/api", STABLE_API_ENDPOINT),
            ("https://fluxer.com/api/", STABLE_API_ENDPOINT),
            ("  https://fluxer.com/api//  ", STABLE_API_ENDPOINT),
            ("HTTPS://FLUXER.COM:443/api", STABLE_API_ENDPOINT),
            ("https://web.canary.fluxer.app/api", CANARY_API_ENDPOINT),
            ("https://canary.fluxer.com/api", CANARY_API_ENDPOINT),
            ("https://canary.fluxer.com/api?", CANARY_API_ENDPOINT),
        ] {
            assert_eq!(
                official_client_api_endpoint_for_alias(input),
                Some(expected),
                "{input}"
            );
        }
    }

    #[test]
    fn anything_else_is_not_an_alias() {
        for input in [
            "http://fluxer.com/api",
            "https://fluxer.com",
            "https://fluxer.com/api/v1",
            "https://fluxer.com:8443/api",
            "https://user@fluxer.com/api",
            "https://fluxer.com/api?x=1",
            "https://fluxer.com/api#x",
            "https://api.fluxer.app/api",
            "https://example.com/api",
            "/api",
            "",
        ] {
            assert_eq!(
                official_client_api_endpoint_for_alias(input),
                None,
                "{input}"
            );
        }
    }
}
