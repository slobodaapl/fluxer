// SPDX-License-Identifier: AGPL-3.0-or-later

//! Navigation gate for app webviews, after `preventUntrustedNavigation` in
//! fluxer_desktop/src/main/Window.ts. Electron told main-frame and subframe navigations apart.
//! wry's WebKitGTK and WKWebView handlers see both without saying which, so there the frame
//! targets the CSP allows in `frame-src` stay navigable and every other remote target is
//! refused. A main frame that still lands on one of them is sent back by
//! [`is_main_frame_document`], checked on page load, which reports the main frame only.
//! WebView2's `NavigationStarting` is main-frame only, so on Windows frame targets are refused.

use crate::local_app::url::{is_local_app_url, is_renderer_document_url};

const BLANK_DOCUMENT: &str = "about:blank";
#[cfg(not(windows))]
const FRAME_DOCUMENTS: [&str; 2] = [BLANK_DOCUMENT, "about:srcdoc"];
#[cfg(not(windows))]
const FRAME_SOURCE_PREFIXES: [&str; 2] = [
    "https://www.youtube.com/embed/",
    "https://www.youtube.com/s/player/",
];

pub fn allows_navigation(url: &str) -> bool {
    if is_local_app_url(url) {
        let allowed = is_renderer_document_url(url);
        if !allowed {
            tracing::warn!(url, "blocked a navigation to a reserved local app path");
        }
        return allowed;
    }
    if allows_frame_target(url) {
        return true;
    }
    tracing::warn!(url, "blocked a navigation away from the local app");
    false
}

#[cfg(not(windows))]
fn allows_frame_target(url: &str) -> bool {
    FRAME_DOCUMENTS.contains(&url)
        || FRAME_SOURCE_PREFIXES
            .iter()
            .any(|prefix| url.starts_with(prefix))
}

#[cfg(windows)]
fn allows_frame_target(_url: &str) -> bool {
    false
}

/// `preventUntrustedChildNavigation` of a popout window: its blank page stays navigable on every
/// platform, Windows included, where the main window refuses frame targets.
pub fn allows_child_navigation(url: &str) -> bool {
    url == BLANK_DOCUMENT || allows_navigation(url)
}

/// Whether the main frame may show `url`: a renderer document, or a blank page.
pub fn is_main_frame_document(url: &str) -> bool {
    url == BLANK_DOCUMENT || is_renderer_document_url(url)
}

#[cfg(test)]
mod tests {
    use super::{allows_child_navigation, allows_navigation, is_main_frame_document};
    use crate::local_app::url::origin;

    #[test]
    fn local_documents_navigate_but_reserved_paths_do_not() {
        assert!(allows_navigation(&format!("{}/channels/@me", origin())));
        assert!(allows_navigation(&format!("{}/", origin())));
        assert!(!allows_navigation(&format!("{}/api/x/v1/users", origin())));
        assert!(!allows_navigation(&format!("{}/proxy?url=x", origin())));
    }

    #[cfg(not(windows))]
    #[test]
    fn only_embeddable_remote_targets_navigate() {
        assert!(allows_navigation("https://www.youtube.com/embed/abc"));
        assert!(allows_navigation("about:blank"));
        assert!(!allows_navigation("https://www.youtube.com/watch?v=abc"));
        assert!(!allows_navigation("https://evil.test/"));
        assert!(!allows_navigation("file:///etc/passwd"));
    }

    #[cfg(windows)]
    #[test]
    fn main_frames_never_navigate_to_frame_targets() {
        assert!(!allows_navigation("https://www.youtube.com/embed/abc"));
        assert!(!allows_navigation("about:blank"));
        assert!(!allows_navigation("https://evil.test/"));
    }

    #[test]
    fn popouts_keep_their_blank_page_and_the_main_window_rules() {
        assert!(allows_child_navigation("about:blank"));
        assert!(allows_child_navigation(&format!(
            "{}/channels/@me",
            origin()
        )));
        assert!(!allows_child_navigation(&format!(
            "{}/api/x/v1/users",
            origin()
        )));
        assert!(!allows_child_navigation(&format!(
            "{}/proxy/x?url=y",
            origin()
        )));
        assert!(!allows_child_navigation("https://evil.test/"));
        assert!(!allows_child_navigation("about:blank#x"));
    }

    #[test]
    fn main_frames_show_renderer_documents_or_blank_pages_only() {
        assert!(is_main_frame_document(&format!(
            "{}/channels/@me",
            origin()
        )));
        assert!(is_main_frame_document("about:blank"));
        assert!(!is_main_frame_document("https://www.youtube.com/embed/abc"));
        assert!(!is_main_frame_document("about:srcdoc"));
        assert!(!is_main_frame_document(&format!(
            "{}/api/x/v1/users",
            origin()
        )));
        assert!(!is_main_frame_document(&format!(
            "{}/proxy/x?url=y",
            origin()
        )));
    }
}
