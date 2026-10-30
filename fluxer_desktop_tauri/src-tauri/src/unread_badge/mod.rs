// SPDX-License-Identifier: AGPL-3.0-or-later

//! The unread badge: `set-badge-count`/`set-unread-badge` of IpcHandlers.ts with UnreadBadge.ts
//! and WindowsBadge.ts. A badge is a count, or [`UNREAD_DOT`] for unread messages without one.
//! Windows shows it as a taskbar overlay icon, macOS on the dock, Linux on the launcher entry
//! (where the dot clears the count, as Electron's `setBadgeCount(0)` did), and the tray marks
//! it on every platform.
//!
//! The count in the tray tooltip is plain digits; Electron formatted it for the native locale.

#[cfg(any(windows, test))]
mod windows_overlay;

#[cfg(windows)]
pub use self::windows_overlay::Overlay as WindowsOverlay;

use serde_json::Value;
#[cfg(not(windows))]
use tauri::Manager;
use tauri::{AppHandle, State, Webview};

use crate::documents::{Caller, DocumentRegistry};
use crate::i18n::MainI18n;
use crate::tray;

/// `UNREAD_BADGE_DOT`.
pub const UNREAD_DOT: i64 = -1;
/// `Number.MAX_SAFE_INTEGER`.
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// `parseUnreadBadge`: a safe integer of at least [`UNREAD_DOT`].
fn parse(value: Option<&Value>) -> Option<i64> {
    let value = value?.as_f64()?;
    (value.fract() == 0.0 && value.abs() <= MAX_SAFE_INTEGER && value >= UNREAD_DOT as f64)
        .then_some(value as i64)
}

/// `describeUnreadBadge`.
pub fn describe(i18n: &MainI18n, badge: i64) -> Option<String> {
    match badge {
        UNREAD_DOT => Some(i18n.t("desktop.badge.unreadMessages", &[])),
        ..=0 => None,
        count => Some(i18n.t(
            "desktop.badge.unreadMessagesCount",
            &[("count", &count.to_string())],
        )),
    }
}

/// `applyUnreadBadge`.
fn apply(app: &AppHandle, badge: i64) {
    #[cfg(windows)]
    windows_overlay::set(app, badge);
    #[cfg(target_os = "macos")]
    if let Some(window) = app.get_webview_window(crate::window::MAIN_WINDOW_LABEL) {
        // `app.dock.setBadge('•')`, else `app.setBadgeCount`. Not verified on macOS.
        let applied = if badge == UNREAD_DOT {
            window.set_badge_label(Some("•".to_owned()))
        } else {
            window.set_badge_count((badge > 0).then_some(badge))
        };
        if let Err(error) = applied {
            tracing::warn!(%error, "Failed to set the dock badge");
        }
    }
    #[cfg(target_os = "linux")]
    app.state::<crate::launcher_entry::LauncherEntry>()
        .set_count(badge.max(0));
    tray::set_unread_badge(app, badge);
}

/// `refreshWindowsBadgeOverlay`: the last badge again, for the window behavior setting and the
/// main window's `show`.
#[cfg(windows)]
pub fn refresh_windows_overlay(app: &AppHandle) {
    windows_overlay::refresh(app);
}

/// Electron's `isPrivilegedRendererDocumentSender` gate: anything else is dropped silently.
fn from_privileged_document(
    webview: &Webview,
    documents: &DocumentRegistry,
    token: String,
    channel: &str,
) -> bool {
    documents
        .capture_for_read(&Caller::new(webview, token), channel)
        .is_ok()
}

/// `set-badge-count`: a count, never the dot.
#[tauri::command]
pub fn set_badge_count(
    app: AppHandle,
    webview: Webview,
    documents: State<'_, std::sync::Arc<DocumentRegistry>>,
    count: Option<Value>,
    document_token: String,
) {
    if let Some(badge) = parse(count.as_ref())
        && from_privileged_document(&webview, &documents, document_token, "set-badge-count")
    {
        apply(&app, badge.max(0));
    }
}

/// `set-unread-badge`.
#[tauri::command]
pub fn set_unread_badge(
    app: AppHandle,
    webview: Webview,
    documents: State<'_, std::sync::Arc<DocumentRegistry>>,
    badge: Option<Value>,
    document_token: String,
) {
    if let Some(badge) = parse(badge.as_ref())
        && from_privileged_document(&webview, &documents, document_token, "set-unread-badge")
    {
        apply(&app, badge);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_safe_integers_from_the_dot_up_are_badges() {
        assert_eq!(parse(Some(&json!(-1))), Some(UNREAD_DOT));
        assert_eq!(parse(Some(&json!(0))), Some(0));
        assert_eq!(parse(Some(&json!(42))), Some(42));
        assert_eq!(parse(Some(&json!(3.0))), Some(3));
        assert_eq!(parse(Some(&json!(9_007_199_254_740_991_i64))), Some(9_007_199_254_740_991));
        for rejected in [
            json!(-2),
            json!(1.5),
            json!(9_007_199_254_740_992_i64),
            json!("3"),
            json!(null),
            json!(true),
        ] {
            assert_eq!(parse(Some(&rejected)), None, "{rejected}");
        }
        assert_eq!(parse(None), None);
    }

    #[test]
    fn the_description_names_the_dot_and_counts_and_nothing_else() {
        let directory = tempfile::tempdir().unwrap();
        let i18n = MainI18n::load(directory.path());
        assert_eq!(describe(&i18n, UNREAD_DOT).as_deref(), Some("Unread messages"));
        assert_eq!(describe(&i18n, 0), None);
        assert_eq!(
            describe(&i18n, 12_345).as_deref(),
            Some("Unread messages: 12345")
        );
    }
}
