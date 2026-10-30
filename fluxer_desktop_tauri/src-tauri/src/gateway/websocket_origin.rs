// SPDX-License-Identifier: AGPL-3.0-or-later

//! WebSocket origins, ported from `fluxer_desktop/src/main/WebSocketOrigin.ts`.

use url::Url;

/// `hostname` plus a non-default port, the way the TS builds it from `URL.hostname`/`URL.port`.
fn origin_with(url: &Url, scheme: &str) -> Option<String> {
    let host = url.host_str()?;
    Some(match url.port() {
        Some(port) => format!("{scheme}://{host}:{port}"),
        None => format!("{scheme}://{host}"),
    })
}

/// `websocketOrigin`: the ws(s) origin of a ws(s) or http(s) URL.
pub fn websocket_origin(value: &str) -> Option<String> {
    let url = Url::parse(value).ok()?;
    match url.scheme() {
        "ws" | "http" => origin_with(&url, "ws"),
        "wss" | "https" => origin_with(&url, "wss"),
        _ => None,
    }
}

/// `websocketHTTPOrigin`: the http(s) origin a ws(s) or http(s) URL connects through.
pub fn websocket_http_origin(value: &str) -> Option<String> {
    let url = Url::parse(value).ok()?;
    match url.scheme() {
        "ws" | "http" => origin_with(&url, "http"),
        "wss" | "https" => origin_with(&url, "https"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_between_http_and_websocket_schemes() {
        assert_eq!(
            websocket_origin("https://Chat.Example:443/x?y").as_deref(),
            Some("wss://chat.example")
        );
        assert_eq!(
            websocket_origin("ws://10.0.0.1:8080/gateway").as_deref(),
            Some("ws://10.0.0.1:8080")
        );
        assert_eq!(
            websocket_http_origin("wss://[::1]:9000/").as_deref(),
            Some("https://[::1]:9000")
        );
        // A port is kept unless it is the default of the URL's own scheme.
        assert_eq!(
            websocket_http_origin("ws://example.test:443").as_deref(),
            Some("http://example.test:443")
        );
        assert_eq!(websocket_origin("ftp://example.test"), None);
        assert_eq!(websocket_http_origin("not a url"), None);
    }
}
