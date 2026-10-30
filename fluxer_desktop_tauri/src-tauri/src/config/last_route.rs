// SPDX-License-Identifier: AGPL-3.0-or-later

//! The route the main window reopens, the port of fluxer_desktop/src/main/DesktopLastRoute.ts
//! with the path rules of packages/desktop_ipc/src/LastRouteContract.ts. The legacy-storage
//! seeding has no counterpart: there is no Electron local storage to seed from.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::js::utf16_length;
use crate::local_app;
use crate::storage::private_file;

const LAST_ROUTE_FILE_NAME: &str = "last-route.json";
const MAX_LAST_ROUTE_LENGTH: usize = 512;
const LANDING_PATH: &str = "channels/@me";

fn last_route_file(directory: &Path) -> PathBuf {
    directory.join(LAST_ROUTE_FILE_NAME)
}

/// `isRestorableDesktopRoutePath`.
pub fn restorable_route_path(value: &Value) -> Option<&str> {
    let path = value.as_str()?;
    let length = utf16_length(path);
    let restorable = length > 0
        && length <= MAX_LAST_ROUTE_LENGTH
        && path.starts_with('/')
        && !path.starts_with("//")
        && !path.contains('\\')
        && !path.contains("://")
        && !path
            .chars()
            .any(|character| matches!(character as u32, 0..=0x1f | 0x7f))
        && path.split('/').all(|segment| segment != "..");
    restorable.then_some(path)
}

/// `recordDesktopLastRoute`. Electron wrote the file in place; this goes through the
/// owner-only write-then-rename the other state files use.
pub fn record(directory: &Path, route_path: &Value) {
    let Some(path) = restorable_route_path(route_path) else {
        return;
    };
    if let Err(error) = private_file::write(
        &last_route_file(directory),
        &json!({ "path": path }).to_string(),
    ) {
        tracing::warn!(%error, "Failed to persist the last route");
    }
}

fn read_stored_route_path(directory: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(last_route_file(directory)).ok()?;
    let parsed: Value = serde_json::from_str(&raw).ok()?;
    restorable_route_path(parsed.get("path")?).map(str::to_owned)
}

/// `resolveDesktopLandingUrl`: the stored route under the local app, or `/channels/@me`.
pub fn resolve_landing_url(directory: &Path) -> String {
    let base = format!("{}/", local_app::url::origin());
    let landing = format!("{base}{LANDING_PATH}");
    let Some(stored) = read_stored_route_path(directory) else {
        return landing;
    };
    let resolved = url::Url::parse(&base)
        .and_then(|base| base.join(stored.trim_start_matches('/')))
        .map(String::from);
    match resolved {
        Ok(resolved) if resolved.starts_with(&base) => resolved,
        _ => landing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origin() -> &'static str {
        local_app::url::origin()
    }

    #[test]
    fn route_paths_follow_the_contract() {
        for valid in [
            "/",
            "/channels/@me",
            "/channels/1/2?x=1#y",
            "/a/./b",
            "/a..b",
        ] {
            assert_eq!(restorable_route_path(&json!(valid)), Some(valid), "{valid}");
        }
        let too_long = format!("/{}", "a".repeat(512));
        for invalid in [
            "",
            "channels",
            "//evil.example",
            "/a\\b",
            "/x?u=https://evil.example",
            "/a\nb",
            "/a\u{7f}b",
            "/a/../b",
            "/..",
            too_long.as_str(),
        ] {
            assert_eq!(restorable_route_path(&json!(invalid)), None, "{invalid:?}");
        }
        assert_eq!(restorable_route_path(&json!(1)), None);
    }

    #[test]
    fn landing_url_falls_back_without_a_valid_record() {
        let directory = tempfile::tempdir().unwrap();
        let landing = format!("{}/channels/@me", origin());
        assert_eq!(resolve_landing_url(directory.path()), landing);
        std::fs::write(last_route_file(directory.path()), "not json").unwrap();
        assert_eq!(resolve_landing_url(directory.path()), landing);
        std::fs::write(last_route_file(directory.path()), r#"{"path":"//evil"}"#).unwrap();
        assert_eq!(resolve_landing_url(directory.path()), landing);
        std::fs::write(last_route_file(directory.path()), r#"["/channels/1"]"#).unwrap();
        assert_eq!(resolve_landing_url(directory.path()), landing);
        // A scheme-looking first segment resolves outside the app and is refused.
        std::fs::write(
            last_route_file(directory.path()),
            r#"{"path":"/javascript:alert(1)"}"#,
        )
        .unwrap();
        assert_eq!(resolve_landing_url(directory.path()), landing);
    }

    #[test]
    fn recorded_route_resolves_under_the_local_app() {
        let directory = tempfile::tempdir().unwrap();
        record(directory.path(), &json!("/channels/1/2?x=1#y"));
        assert_eq!(
            std::fs::read_to_string(last_route_file(directory.path())).unwrap(),
            r#"{"path":"/channels/1/2?x=1#y"}"#
        );
        assert_eq!(
            resolve_landing_url(directory.path()),
            format!("{}/channels/1/2?x=1#y", origin())
        );
        record(directory.path(), &json!("//evil.example"));
        assert_eq!(
            resolve_landing_url(directory.path()),
            format!("{}/channels/1/2?x=1#y", origin())
        );
    }
}
