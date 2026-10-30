// SPDX-License-Identifier: AGPL-3.0-or-later

//! The theme file channels of ThemeLocalFiles.ts as Tauri commands. Electron gated only
//! `theme-linked-files-pick` and `theme-linked-files-watch`
//! (`requirePrivilegedRendererDocumentSender`, which ignores whether the page is loading); here
//! every channel takes that gate, as `capture_for_read`. Dialogs are modal to the calling
//! window and titled with the strings Electron used.

use std::sync::Arc;

use serde_json::Value;
use tauri::{AppHandle, Manager, State, Webview};

use super::allowlist::AllowedFiles;
use super::local::{
    ThemeLocalFileReadResult, ThemeLocalFileReference, import_directory, inspect_picked_files,
    read_local_files, read_picked_css_files,
};
use super::watch::ThemeLinkedFileWatches;
use super::{ThemeDirectoryCssFile, ThemeFileError, ThemeLinkedFileChange};
use crate::config::DesktopConfig;
use crate::dialog::{self, FileFilter, OpenDialogOptions, OpenTarget};
use crate::documents::{Caller, DocumentRegistry};
use crate::error::BridgeError;
use crate::events::BridgeEvents;
use crate::i18n::MainI18n;

const CHANGE_EVENT: &str = "theme-linked-file-changed";
const THEME_LINKED_FILES_MAX_WATCHED: usize = 500;
const CSS_EXTENSIONS: &[&str] = &["css"];

type Documents<'a> = State<'a, Arc<DocumentRegistry>>;
type Config<'a> = State<'a, Arc<DesktopConfig>>;
type Reply<T> = Result<T, BridgeError>;

fn gate(
    webview: &Webview,
    documents: &DocumentRegistry,
    token: String,
    channel: &str,
) -> Reply<()> {
    documents
        .capture_for_read(&Caller::new(webview, token), channel)
        .map(drop)
        .map_err(|_| {
            BridgeError::new(
                "UntrustedRendererDocumentSenderError",
                format!("{channel} is only reachable from a trusted top-level renderer document"),
            )
        })
}

/// The string entries of an array payload; anything else is no paths.
fn string_entries(payload: Option<Value>) -> Vec<String> {
    match payload {
        Some(Value::Array(items)) => items
            .into_iter()
            .filter_map(|item| match item {
                Value::String(path) => Some(path),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .expect("theme file work does not panic")
}

#[tauri::command]
pub async fn theme_local_files_pick(
    webview: Webview,
    documents: Documents<'_>,
    config: Config<'_>,
    i18n: State<'_, MainI18n>,
    document_token: String,
) -> Reply<Vec<ThemeLocalFileReference>> {
    gate(
        &webview,
        &documents,
        document_token,
        "theme-local-files-pick",
    )?;
    let picked = dialog::show_open_dialog(
        &webview.window(),
        OpenDialogOptions {
            title: i18n.t("desktop.themes.addLocalFiles", &[]),
            target: OpenTarget::Files { multiple: true },
            filter: None,
        },
    )
    .await;
    if picked.is_empty() {
        return Ok(Vec::new());
    }
    let references = blocking(move || inspect_picked_files(&picked)).await;
    let paths: Vec<String> = references
        .iter()
        .map(|reference| reference.path.clone())
        .collect();
    config.add_allowed_theme_local_files(&paths);
    Ok(references)
}

#[tauri::command]
pub async fn theme_local_files_read(
    webview: Webview,
    documents: Documents<'_>,
    config: Config<'_>,
    document_token: String,
    paths: Option<Value>,
) -> Reply<Vec<ThemeLocalFileReadResult>> {
    gate(
        &webview,
        &documents,
        document_token,
        "theme-local-files-read",
    )?;
    let paths = string_entries(paths);
    let allowed = AllowedFiles::new(&config.allowed_theme_local_files());
    Ok(blocking(move || read_local_files(&paths, &allowed)).await)
}

#[tauri::command]
pub async fn theme_local_files_clear(
    webview: Webview,
    documents: Documents<'_>,
    config: Config<'_>,
    document_token: String,
) -> Reply<()> {
    gate(
        &webview,
        &documents,
        document_token,
        "theme-local-files-clear",
    )?;
    config.clear_allowed_theme_local_files();
    Ok(())
}

#[tauri::command]
pub async fn theme_directory_import(
    webview: Webview,
    documents: Documents<'_>,
    config: Config<'_>,
    i18n: State<'_, MainI18n>,
    document_token: String,
) -> Reply<Vec<ThemeDirectoryCssFile>> {
    gate(
        &webview,
        &documents,
        document_token,
        "theme-directory-import",
    )?;
    let picked = dialog::show_open_dialog(
        &webview.window(),
        OpenDialogOptions {
            title: i18n.t("desktop.themes.importFolder", &[]),
            target: OpenTarget::Directory,
            filter: None,
        },
    )
    .await;
    let Some(directory) = picked.into_iter().next() else {
        return Ok(Vec::new());
    };
    let themes = blocking(move || import_directory(&directory)).await?;
    let paths: Vec<String> = themes.iter().map(|theme| theme.path.clone()).collect();
    config.add_allowed_theme_local_files(&paths);
    Ok(themes)
}

#[tauri::command]
pub async fn theme_linked_files_pick(
    webview: Webview,
    documents: Documents<'_>,
    config: Config<'_>,
    i18n: State<'_, MainI18n>,
    document_token: String,
    options: Option<Value>,
) -> Reply<Vec<ThemeDirectoryCssFile>> {
    gate(
        &webview,
        &documents,
        document_token,
        "theme-linked-files-pick",
    )?;
    let multiple = !options
        .as_ref()
        .and_then(Value::as_object)
        .is_some_and(|options| options.get("multiple") == Some(&Value::Bool(false)));
    let title = if multiple {
        "desktop.themes.importCss"
    } else {
        "desktop.themes.linkFile"
    };
    let picked = dialog::show_open_dialog(
        &webview.window(),
        OpenDialogOptions {
            title: i18n.t(title, &[]),
            target: OpenTarget::Files { multiple },
            filter: Some(FileFilter {
                name: i18n.t("desktop.themes.cssFilesFilter", &[]),
                extensions: CSS_EXTENSIONS,
            }),
        },
    )
    .await;
    if picked.is_empty() {
        return Ok(Vec::new());
    }
    let files = blocking(move || read_picked_css_files(&picked)).await;
    let paths: Vec<String> = files.iter().map(|file| file.path.clone()).collect();
    config.add_allowed_theme_local_files(&paths);
    Ok(files)
}

/// `theme-linked-files-watch`: the request replaces what this document watches. Paths off the
/// allowlist and past the first 500 are reported at once instead of watched.
#[tauri::command]
pub async fn theme_linked_files_watch(
    app: AppHandle,
    webview: Webview,
    documents: Documents<'_>,
    config: Config<'_>,
    watches: State<'_, Arc<ThemeLinkedFileWatches>>,
    document_token: String,
    paths: Option<Value>,
) -> Reply<()> {
    gate(
        &webview,
        &documents,
        document_token.clone(),
        "theme-linked-files-watch",
    )?;
    let mut requested = Vec::new();
    for path in string_entries(paths) {
        if !requested.contains(&path) {
            requested.push(path);
        }
    }
    let overflow = requested.split_off(requested.len().min(THEME_LINKED_FILES_MAX_WATCHED));
    let label = webview.label().to_owned();
    if requested.is_empty() {
        watches.dispose(&label);
        return Ok(());
    }
    let allowed_files = AllowedFiles::new(&config.allowed_theme_local_files());
    let (allowed, refused): (Vec<String>, Vec<String>) = requested
        .into_iter()
        .partition(|path| allowed_files.contains(path));
    let is_current = {
        let documents = Arc::clone(&documents);
        let label = label.clone();
        let token = document_token.clone();
        move || documents.is_document_token(&label, &token)
    };
    let emit = {
        let is_current = is_current.clone();
        let label = label.clone();
        Arc::new(move |change: ThemeLinkedFileChange| {
            if is_current() {
                app.state::<BridgeEvents>()
                    .emit(&label, CHANGE_EVENT, change);
            }
        })
    };
    let set = watches.subscription(&label, &document_token, emit, is_current);
    for path in &refused {
        set.emit(ThemeLinkedFileChange::failed(
            path,
            ThemeFileError::NotAllowed,
        ));
    }
    for path in &overflow {
        set.emit(ThemeLinkedFileChange::failed(path, ThemeFileError::TooMany));
    }
    if allowed.is_empty() {
        watches.dispose(&label);
        return Ok(());
    }
    set.replace(&allowed);
    Ok(())
}
