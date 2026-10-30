// SPDX-License-Identifier: AGPL-3.0-or-later

//! Theme files on disk, the port of fluxer_desktop/src/main/ThemeLocalFiles.ts and
//! ThemeLinkedFileWatch.ts: local assets a theme references (read back as `data:` URLs), CSS
//! imported from a folder, and CSS files linked to a theme and watched for edits. Every path
//! the user picks joins the `theme_allowed_local_files` setting, and only paths on that list
//! are read or watched later.
//!
//! Electron quirks kept as they were: the allowlist only grows until it is cleared; clearing it
//! does not stop active watches; a folder import fails as a whole when a subfolder cannot be
//! listed.

mod allowlist;
pub mod commands;
mod css_file;
mod local;
pub mod watch;

use serde::Serialize;

/// `ThemeLinkedFileError` of DesktopBridgeTypes.ts; `readThemeLocalFiles` reports a subset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeFileError {
    NotAllowed,
    Missing,
    NotFile,
    TooLarge,
    TooMany,
    ReadFailed,
}

/// `ThemeLinkedFileChange`: a file's CSS, or why it has none.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ThemeLinkedFileChange {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub css: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ThemeFileError>,
}

impl ThemeLinkedFileChange {
    fn failed(path: &str, error: ThemeFileError) -> Self {
        Self {
            path: path.to_owned(),
            css: None,
            error: Some(error),
        }
    }
}

/// `ThemeDirectoryCssFile`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeDirectoryCssFile {
    pub file_name: String,
    pub path: String,
    pub css: String,
}
