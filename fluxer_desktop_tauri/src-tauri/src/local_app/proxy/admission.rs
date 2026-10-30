// SPDX-License-Identifier: AGPL-3.0-or-later

//! Which requests may reach the proxy routes.
//!
//! Electron admitted a request when it carried the per-frame secret header its `webRequest` hook
//! added (LocalAppProtocolAuthorization.ts). Tauri has no request hook, so the bridge adds the
//! header itself: it wraps the main frame's `fetch` and `XMLHttpRequest` and sends the current
//! document's ownership token (bridge/protocol.ts). Every `/api` request and every non-`GET`/
//! `HEAD` `/proxy` request must carry the token of the requesting webview's current document.
//! `/proxy` reads stay open, as Electron's asset reads were: media elements and download links
//! load them and cannot carry a header, and they hold no credentials, forge no `Origin`, and
//! reach only the plan's own origins.
//!
//! WebKitGTK passes no `Origin` or `Sec-Fetch-*` to scheme handlers and `Referer` rarely, so the
//! metadata checks below only bite on webviews that send it. The document-destination refusal
//! of LocalAppRequestHandler.ts follows, from headers only, since a Tauri request has no
//! `destination`.
//!
//! Electron also authorised each popout window's webContents (`attachChildWindowGuards` in
//! Window.ts). Those windows hold documents the main renderer writes into, whose media loads
//! reach `/proxy` from the popout's own webview; [`AuthorizedWebviews`] admits their labels on the
//! same rules, the document-token rule included, which a popout only passes with a token of its
//! own document.
//!
//! Differs from Electron: script, style, worker, XSLT and manifest destinations are refused too.
//! The routes answer on the local app origin, so a proxied body loaded as one of those would run
//! with the renderer's privileges; the renderer reaches the routes through `fetch` and media
//! elements only.

use std::collections::HashSet;
use std::sync::Mutex;

use tauri::http::{HeaderMap, Method, header};

use crate::documents::DocumentRegistry;
use crate::local_app::url::{is_renderer_document_url, origin};
use crate::window::MAIN_WINDOW_LABEL;

/// The header Electron's `webRequest` hook injected; the bridge sends the document token in it.
pub const PROTOCOL_AUTHORIZATION_HEADER: &str = "x-fluxer-desktop-protocol-authorization";

const SEC_FETCH_SITE_HEADER: &str = "sec-fetch-site";
const SEC_FETCH_DEST_HEADER: &str = "sec-fetch-dest";
const NAVIGATION_PREFERENCE_HEADER: &str = "upgrade-insecure-requests";
const SAME_ORIGIN_FETCH_SITE: &str = "same-origin";
const BLOCKED_DOCUMENT_DESTINATIONS: &[&str] = &["document", "embed", "frame", "iframe", "object"];
const BLOCKED_ACTIVE_DESTINATIONS: &[&str] = &[
    "audioworklet",
    "manifest",
    "paintworklet",
    "script",
    "serviceworker",
    "sharedworker",
    "style",
    "worker",
    "xslt",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejection {
    ForeignWebview,
    Authorization,
    Origin,
    FetchSite,
    Referer,
    DocumentDestination,
    ActiveContentDestination,
}

/// The webviews the routes serve: the main window always, popout windows while they exist.
#[derive(Default)]
pub struct AuthorizedWebviews {
    popouts: Mutex<HashSet<String>>,
}

impl AuthorizedWebviews {
    /// `LocalAppProtocolAuthorization.authorize(webContents)` for a popout.
    pub fn authorize(&self, webview_label: &str) {
        self.lock().insert(webview_label.to_owned());
    }

    pub fn revoke(&self, webview_label: &str) {
        self.lock().remove(webview_label);
    }

    fn contains(&self, webview_label: &str) -> bool {
        webview_label == MAIN_WINDOW_LABEL || self.lock().contains(webview_label)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashSet<String>> {
        self.popouts.lock().expect("authorized webviews lock")
    }
}

/// Whether a request of `method` to this route needs the document token.
pub fn requires_authorization(is_remote_resource: bool, method: &Method) -> bool {
    !(is_remote_resource && (method == Method::GET || method == Method::HEAD))
}

pub fn admit(
    webview_label: &str,
    webviews: &AuthorizedWebviews,
    headers: &HeaderMap,
    authorization_required: bool,
    documents: &DocumentRegistry,
) -> Result<(), Rejection> {
    if !webviews.contains(webview_label) {
        return Err(Rejection::ForeignWebview);
    }
    if authorization_required && !is_authorized(webview_label, headers, documents) {
        return Err(Rejection::Authorization);
    }
    // WebKit sends `Origin: null` from opaque origins; it never equals the local app origin.
    if !every_value(headers, header::ORIGIN.as_str(), |value| value == origin()) {
        return Err(Rejection::Origin);
    }
    if !every_value(headers, SEC_FETCH_SITE_HEADER, |value| {
        value == SAME_ORIGIN_FETCH_SITE
    }) {
        return Err(Rejection::FetchSite);
    }
    if !every_value(headers, header::REFERER.as_str(), is_renderer_document_url) {
        return Err(Rejection::Referer);
    }
    if is_document_request(headers) {
        return Err(Rejection::DocumentDestination);
    }
    if has_destination(headers, BLOCKED_ACTIVE_DESTINATIONS) {
        return Err(Rejection::ActiveContentDestination);
    }
    Ok(())
}

/// Exactly one header value, equal to the current document's token.
fn is_authorized(webview_label: &str, headers: &HeaderMap, documents: &DocumentRegistry) -> bool {
    let mut values = headers.get_all(PROTOCOL_AUTHORIZATION_HEADER).iter();
    match (values.next(), values.next()) {
        (Some(value), None) => value
            .to_str()
            .is_ok_and(|token| documents.is_document_token(webview_label, token)),
        _ => false,
    }
}

/// Every value of an absent header passes; a value that is not visible ASCII fails.
fn every_value(headers: &HeaderMap, name: &str, accept: impl Fn(&str) -> bool) -> bool {
    headers
        .get_all(name)
        .iter()
        .all(|value| value.to_str().is_ok_and(&accept))
}

/// `isProxyDocumentRequest` over headers. Each `Sec-Fetch-Dest` value is checked on its own,
/// where `Headers.get` would have joined repeated values into one that matched nothing.
fn is_document_request(headers: &HeaderMap) -> bool {
    has_destination(headers, BLOCKED_DOCUMENT_DESTINATIONS)
        || headers.contains_key(NAVIGATION_PREFERENCE_HEADER)
}

fn has_destination(headers: &HeaderMap, destinations: &[&str]) -> bool {
    headers.get_all(SEC_FETCH_DEST_HEADER).iter().any(|value| {
        value
            .to_str()
            .is_ok_and(|value| destinations.contains(&value))
    })
}

#[cfg(test)]
mod tests {
    use tauri::http::HeaderValue;

    use super::*;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(*name, HeaderValue::from_str(value).unwrap());
        }
        map
    }

    /// Admission for a route that needs no document token.
    fn admit_open(label: &str, headers: &HeaderMap) -> Result<(), Rejection> {
        admit(
            label,
            &AuthorizedWebviews::default(),
            headers,
            false,
            &DocumentRegistry::default(),
        )
    }

    fn document_url() -> String {
        format!("{}/channels/@me", origin())
    }

    #[test]
    fn same_origin_fetches_from_the_main_webview_are_admitted() {
        let referer = document_url();
        let full = headers(&[
            ("origin", origin()),
            ("sec-fetch-site", "same-origin"),
            ("sec-fetch-dest", "empty"),
            ("sec-fetch-mode", "cors"),
            ("referer", &referer),
        ]);
        assert_eq!(admit_open(MAIN_WINDOW_LABEL, &full), Ok(()));
        // WebKit may omit every piece of metadata; the webview label still has to match.
        assert_eq!(admit_open(MAIN_WINDOW_LABEL, &HeaderMap::new()), Ok(()));
        let root_referer = format!("{}/", origin());
        assert_eq!(
            admit_open(MAIN_WINDOW_LABEL, &headers(&[("referer", &root_referer)])),
            Ok(())
        );
    }

    fn none() -> AuthorizedWebviews {
        AuthorizedWebviews::default()
    }

    const TOKEN: &str = "abababababababababababababababababababababababababababababababab";

    fn registry_with_document() -> DocumentRegistry {
        let registry = DocumentRegistry::default();
        registry.navigation_started(MAIN_WINDOW_LABEL);
        registry.register(MAIN_WINDOW_LABEL, TOKEN).unwrap();
        registry
    }

    #[test]
    fn authorized_routes_need_the_current_document_token() {
        let registry = registry_with_document();
        let with = |token: &str| headers(&[(PROTOCOL_AUTHORIZATION_HEADER, token)]);
        assert_eq!(
            admit(MAIN_WINDOW_LABEL, &none(), &with(TOKEN), true, &registry),
            Ok(())
        );
        assert_eq!(
            admit(
                MAIN_WINDOW_LABEL,
                &none(),
                &HeaderMap::new(),
                true,
                &registry
            ),
            Err(Rejection::Authorization)
        );
        let other = "cd".repeat(32);
        assert_eq!(
            admit(MAIN_WINDOW_LABEL, &none(), &with(&other), true, &registry),
            Err(Rejection::Authorization)
        );
        let doubled = headers(&[
            (PROTOCOL_AUTHORIZATION_HEADER, TOKEN),
            (PROTOCOL_AUTHORIZATION_HEADER, TOKEN),
        ]);
        assert_eq!(
            admit(MAIN_WINDOW_LABEL, &none(), &doubled, true, &registry),
            Err(Rejection::Authorization)
        );
        registry.navigation_started(MAIN_WINDOW_LABEL);
        assert_eq!(
            admit(MAIN_WINDOW_LABEL, &none(), &with(TOKEN), true, &registry),
            Err(Rejection::Authorization)
        );
    }

    #[test]
    fn only_remote_resource_reads_skip_authorization() {
        assert!(requires_authorization(false, &Method::GET));
        assert!(requires_authorization(false, &Method::POST));
        assert!(requires_authorization(true, &Method::PUT));
        assert!(!requires_authorization(true, &Method::GET));
        assert!(!requires_authorization(true, &Method::HEAD));
    }

    #[test]
    fn other_webviews_are_rejected() {
        assert_eq!(
            admit_open("popout", &HeaderMap::new()),
            Err(Rejection::ForeignWebview)
        );
        assert_eq!(
            admit_open("", &HeaderMap::new()),
            Err(Rejection::ForeignWebview)
        );
    }

    #[test]
    fn popouts_are_admitted_while_authorized_on_the_main_window_rules() {
        const POPOUT: &str = "popout-1";
        let webviews = AuthorizedWebviews::default();
        let registry = registry_with_document();
        let open = |webviews: &AuthorizedWebviews, headers: &HeaderMap| {
            admit(POPOUT, webviews, headers, false, &registry)
        };
        assert_eq!(
            open(&webviews, &HeaderMap::new()),
            Err(Rejection::ForeignWebview)
        );
        webviews.authorize(POPOUT);
        // A media element in the popout document reading `/proxy`.
        assert_eq!(
            open(&webviews, &headers(&[("sec-fetch-dest", "image")])),
            Ok(())
        );
        assert_eq!(
            open(&webviews, &headers(&[("origin", "null")])),
            Err(Rejection::Origin)
        );
        assert_eq!(
            open(&webviews, &headers(&[("sec-fetch-dest", "iframe")])),
            Err(Rejection::DocumentDestination)
        );
        // The main document's token is not the popout's.
        let main_token = headers(&[(PROTOCOL_AUTHORIZATION_HEADER, TOKEN)]);
        assert_eq!(
            admit(POPOUT, &webviews, &main_token, true, &registry),
            Err(Rejection::Authorization)
        );
        let popout_token = "ef".repeat(32);
        registry.register(POPOUT, &popout_token).unwrap();
        assert_eq!(
            admit(
                POPOUT,
                &webviews,
                &headers(&[(PROTOCOL_AUTHORIZATION_HEADER, &popout_token)]),
                true,
                &registry
            ),
            Ok(())
        );
        webviews.revoke(POPOUT);
        assert_eq!(
            open(&webviews, &HeaderMap::new()),
            Err(Rejection::ForeignWebview)
        );
        assert_eq!(
            admit_open("popout-2", &HeaderMap::new()),
            Err(Rejection::ForeignWebview)
        );
    }

    #[test]
    fn origins_other_than_the_local_app_are_rejected() {
        let local_with_slash = format!("{}/", origin());
        for value in [
            "null",
            "https://evil.test",
            "http://127.0.0.1:1234",
            local_with_slash.as_str(),
            "",
        ] {
            assert_eq!(
                admit_open(MAIN_WINDOW_LABEL, &headers(&[("origin", value)])),
                Err(Rejection::Origin),
                "{value}"
            );
        }
        let repeated = headers(&[("origin", origin()), ("origin", "null")]);
        assert_eq!(
            admit_open(MAIN_WINDOW_LABEL, &repeated),
            Err(Rejection::Origin)
        );
        let mut opaque = HeaderMap::new();
        opaque.insert("origin", HeaderValue::from_bytes(b"\xff").unwrap());
        assert_eq!(
            admit_open(MAIN_WINDOW_LABEL, &opaque),
            Err(Rejection::Origin)
        );
    }

    #[test]
    fn fetch_sites_other_than_same_origin_are_rejected() {
        for value in ["cross-site", "same-site", "none", "Same-Origin"] {
            assert_eq!(
                admit_open(MAIN_WINDOW_LABEL, &headers(&[("sec-fetch-site", value)])),
                Err(Rejection::FetchSite),
                "{value}"
            );
        }
    }

    #[test]
    fn referers_outside_renderer_documents_are_rejected() {
        let api = format!("{}/api/key/v1/users", origin());
        let proxy = format!("{}/proxy/key?url=x", origin());
        for value in [
            "https://evil.test/",
            api.as_str(),
            proxy.as_str(),
            "not a url",
        ] {
            assert_eq!(
                admit_open(MAIN_WINDOW_LABEL, &headers(&[("referer", value)])),
                Err(Rejection::Referer),
                "{value}"
            );
        }
    }

    #[test]
    fn document_destinations_and_navigations_are_rejected() {
        for destination in BLOCKED_DOCUMENT_DESTINATIONS {
            assert_eq!(
                admit_open(
                    MAIN_WINDOW_LABEL,
                    &headers(&[("sec-fetch-dest", destination)])
                ),
                Err(Rejection::DocumentDestination),
                "{destination}"
            );
        }
        for destination in ["empty", "image", "video", "Document"] {
            assert_eq!(
                admit_open(
                    MAIN_WINDOW_LABEL,
                    &headers(&[("sec-fetch-dest", destination)])
                ),
                Ok(()),
                "{destination}"
            );
        }
        let repeated = headers(&[("sec-fetch-dest", "empty"), ("sec-fetch-dest", "iframe")]);
        assert_eq!(
            admit_open(MAIN_WINDOW_LABEL, &repeated),
            Err(Rejection::DocumentDestination)
        );
        assert_eq!(
            admit_open(
                MAIN_WINDOW_LABEL,
                &headers(&[("upgrade-insecure-requests", "1")])
            ),
            Err(Rejection::DocumentDestination)
        );
    }

    #[test]
    fn active_content_destinations_are_rejected() {
        for destination in BLOCKED_ACTIVE_DESTINATIONS {
            assert_eq!(
                admit_open(
                    MAIN_WINDOW_LABEL,
                    &headers(&[("sec-fetch-dest", destination)])
                ),
                Err(Rejection::ActiveContentDestination),
                "{destination}"
            );
        }
        for destination in ["empty", "image", "audio", "track", "font", "json", "Script"] {
            assert_eq!(
                admit_open(
                    MAIN_WINDOW_LABEL,
                    &headers(&[("sec-fetch-dest", destination)])
                ),
                Ok(()),
                "{destination}"
            );
        }
        let repeated = headers(&[("sec-fetch-dest", "empty"), ("sec-fetch-dest", "script")]);
        assert_eq!(
            admit_open(MAIN_WINDOW_LABEL, &repeated),
            Err(Rejection::ActiveContentDestination)
        );
    }
}
