// SPDX-License-Identifier: AGPL-3.0-or-later

//! The file work behind the ThemeLocalFiles.ts channels, without the dialogs: inspecting picked
//! assets, reading allowed assets as `data:` URLs, and collecting CSS from a folder or a pick.
//! Picked assets and `readThemeLocalFiles` follow symlinks (`stat`); CSS reads do not.

use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::Serialize;

use super::allowlist::AllowedFiles;
use super::css_file::read_theme_css_file;
use super::{ThemeDirectoryCssFile, ThemeFileError};
use crate::error::BridgeError;

const THEME_LOCAL_FILE_MAX_BYTES: u64 = 50 * 1024 * 1024;
const THEME_DIRECTORY_MAX_CSS_FILES: usize = 200;

/// `ThemeLocalFileReference`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeLocalFileReference {
    id: String,
    name: String,
    pub path: String,
    mime_type: &'static str,
    size: u64,
}

/// `ThemeLocalFileReadResult`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeLocalFileReadResult {
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ThemeFileError>,
}

fn lossy(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn basename(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `getMimeType`: by the lowercased extension.
fn mime_type(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .map(|extension| extension.to_string_lossy().to_lowercase());
    match extension.as_deref() {
        Some("css") => "text/css",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        Some("otf") => "font/otf",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("avif") => "image/avif",
        Some("svg") => "image/svg+xml",
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mp3") => "audio/mpeg",
        Some("ogg") => "audio/ogg",
        _ => "application/octet-stream",
    }
}

/// `theme-local-files-pick` after the dialog: regular files up to 50 MiB, others skipped.
pub(super) fn inspect_picked_files(paths: &[PathBuf]) -> Vec<ThemeLocalFileReference> {
    let mut references = Vec::new();
    for path in paths {
        match std::fs::metadata(path) {
            Ok(metadata) if metadata.is_file() && metadata.len() <= THEME_LOCAL_FILE_MAX_BYTES => {
                let path_string = lossy(path);
                references.push(ThemeLocalFileReference {
                    id: path_string
                        .bytes()
                        .map(|byte| format!("{byte:02x}"))
                        .collect(),
                    name: basename(path),
                    mime_type: mime_type(path),
                    size: metadata.len(),
                    path: path_string,
                });
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(filePath = %path.display(), %error, "Failed to inspect selected theme file");
            }
        }
    }
    references
}

/// `theme-local-files-read` for the string entries of the request, in order.
pub(super) fn read_local_files(
    paths: &[String],
    allowed: &AllowedFiles,
) -> Vec<ThemeLocalFileReadResult> {
    paths
        .iter()
        .map(|path| {
            let outcome = if allowed.contains(path) {
                read_local_file(Path::new(path))
            } else {
                Err(ThemeFileError::NotAllowed)
            };
            let (data_url, error) = match outcome {
                Ok(data_url) => (Some(data_url), None),
                Err(error) => (None, Some(error)),
            };
            ThemeLocalFileReadResult {
                path: path.clone(),
                data_url,
                error,
            }
        })
        .collect()
}

fn read_local_file(path: &Path) -> Result<String, ThemeFileError> {
    let failed = |error: std::io::Error| {
        tracing::warn!(filePath = %path.display(), %error, "Failed to read theme local file");
        ThemeFileError::ReadFailed
    };
    let metadata = std::fs::metadata(path).map_err(failed)?;
    if !metadata.is_file() {
        return Err(ThemeFileError::NotFile);
    }
    if metadata.len() > THEME_LOCAL_FILE_MAX_BYTES {
        return Err(ThemeFileError::TooLarge);
    }
    let data = std::fs::read(path).map_err(failed)?;
    Ok(format!(
        "data:{};base64,{}",
        mime_type(path),
        STANDARD.encode(data)
    ))
}

/// `collectCssFilesFromDirectory`: depth first in listing order, entries that are neither a
/// directory nor a file (symlinks among them) skipped, at most 200. A folder that cannot be
/// listed fails the whole walk.
fn collect_css_files(directory: &Path) -> std::io::Result<Vec<PathBuf>> {
    fn visit(directory: &Path, files: &mut Vec<PathBuf>) -> std::io::Result<()> {
        if files.len() >= THEME_DIRECTORY_MAX_CSS_FILES {
            return Ok(());
        }
        let listing = std::fs::read_dir(directory).map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!("{error}, scandir '{}'", directory.display()),
            )
        })?;
        for entry in listing {
            if files.len() >= THEME_DIRECTORY_MAX_CSS_FILES {
                return Ok(());
            }
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                visit(&entry.path(), files)?;
            } else if kind.is_file()
                && entry
                    .file_name()
                    .to_string_lossy()
                    .to_lowercase()
                    .ends_with(".css")
            {
                files.push(entry.path());
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    visit(directory, &mut files)?;
    Ok(files)
}

/// `theme-directory-import` after the dialog. The folder is resolved once; the files under it
/// keep the names they were listed by.
pub(super) fn import_directory(selected: &Path) -> Result<Vec<ThemeDirectoryCssFile>, BridgeError> {
    let directory = match dunce::canonicalize(selected) {
        Ok(directory) => directory,
        Err(error) => {
            tracing::warn!(directoryPath = %selected.display(), %error, "Failed to resolve selected theme directory");
            return Ok(Vec::new());
        }
    };
    let files = collect_css_files(&directory)
        .map_err(|error| BridgeError::new("Error", error.to_string()))?;
    let mut themes = Vec::new();
    for file in files {
        let path = lossy(&file);
        let read = read_theme_css_file(&path);
        let Some(css) = read.css else {
            tracing::warn!(filePath = %path, error = ?read.error, "Failed to import CSS file from theme directory");
            continue;
        };
        let relative = file.strip_prefix(&directory).map(lossy).unwrap_or_default();
        themes.push(ThemeDirectoryCssFile {
            file_name: if relative.is_empty() {
                basename(&file)
            } else {
                relative
            },
            path,
            css,
        });
    }
    Ok(themes)
}

/// `theme-linked-files-pick` after the dialog: each pick resolved and read, failures skipped.
pub(super) fn read_picked_css_files(paths: &[PathBuf]) -> Vec<ThemeDirectoryCssFile> {
    let mut files = Vec::new();
    for selected in paths {
        let resolved = match dunce::canonicalize(selected) {
            Ok(resolved) => resolved,
            Err(error) => {
                tracing::warn!(filePath = %selected.display(), %error, "Failed to resolve selected theme CSS file");
                continue;
            }
        };
        let path = lossy(&resolved);
        let read = read_theme_css_file(&path);
        let Some(css) = read.css else {
            tracing::warn!(filePath = %path, error = ?read.error, "Failed to read selected theme CSS file");
            continue;
        };
        files.push(ThemeDirectoryCssFile {
            file_name: basename(&resolved),
            path,
            css,
        });
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    fn string(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn picked_files_skip_directories_missing_and_oversized_entries() {
        let directory = tempfile::tempdir().unwrap();
        let font = directory.path().join("Font.WOFF2");
        std::fs::write(&font, b"wOF2").unwrap();
        let large = directory.path().join("large.png");
        std::fs::File::create(&large)
            .unwrap()
            .set_len(THEME_LOCAL_FILE_MAX_BYTES + 1)
            .unwrap();
        let references = inspect_picked_files(&[
            font.clone(),
            large,
            directory.path().to_owned(),
            directory.path().join("absent"),
        ]);
        let path = string(&font);
        assert_eq!(
            references,
            [ThemeLocalFileReference {
                id: path.bytes().map(|byte| format!("{byte:02x}")).collect(),
                name: "Font.WOFF2".to_owned(),
                path,
                mime_type: "font/woff2",
                size: 4,
            }]
        );
    }

    #[test]
    fn reads_allowed_files_as_data_urls_and_reports_the_rest() {
        let directory = tempfile::tempdir().unwrap();
        let image = directory.path().join("bg.jpeg");
        std::fs::write(&image, b"\xff\xd8\xff").unwrap();
        let unlisted = directory.path().join("other.css");
        std::fs::write(&unlisted, "a{}").unwrap();
        let missing = directory.path().join("gone.png");
        let allowed =
            AllowedFiles::new(&[string(&image), string(directory.path()), string(&missing)]);
        let results = read_local_files(
            &[
                string(&image),
                string(&unlisted),
                string(directory.path()),
                string(&missing),
            ],
            &allowed,
        );
        let json = serde_json::to_value(&results).unwrap();
        assert_eq!(
            json,
            serde_json::json!([
                {"path": string(&image), "dataUrl": "data:image/jpeg;base64,/9j/"},
                {"path": string(&unlisted), "error": "not_allowed"},
                {"path": string(directory.path()), "error": "not_file"},
                // `readThemeLocalFiles` has no `missing`: a vanished file fails the read.
                {"path": string(&missing), "error": "read_failed"},
            ])
        );
    }

    #[cfg(unix)]
    #[test]
    fn directory_import_walks_css_files_skipping_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let theme = root.path().join("theme");
        std::fs::create_dir_all(theme.join("parts")).unwrap();
        std::fs::write(theme.join("main.CSS"), "\u{feff}main{}").unwrap();
        std::fs::write(theme.join("parts/extra.css"), "extra{}").unwrap();
        std::fs::write(theme.join("notes.txt"), "x").unwrap();
        std::os::unix::fs::symlink(theme.join("parts"), theme.join("loop")).unwrap();
        std::os::unix::fs::symlink(theme.join("main.CSS"), theme.join("alias.css")).unwrap();
        // The selection itself may be a symlink; it is resolved before the walk.
        std::os::unix::fs::symlink(&theme, root.path().join("picked")).unwrap();
        let mut imported = import_directory(&root.path().join("picked")).unwrap();
        imported.sort_by(|left, right| left.file_name.cmp(&right.file_name));
        let real = std::fs::canonicalize(&theme).unwrap();
        assert_eq!(
            imported,
            [
                ThemeDirectoryCssFile {
                    file_name: "main.CSS".to_owned(),
                    path: string(&real.join("main.CSS")),
                    css: "main{}".to_owned(),
                },
                ThemeDirectoryCssFile {
                    file_name: "parts/extra.css".to_owned(),
                    path: string(&real.join("parts/extra.css")),
                    css: "extra{}".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn directory_import_stops_at_two_hundred_files() {
        let root = tempfile::tempdir().unwrap();
        for index in 0..(THEME_DIRECTORY_MAX_CSS_FILES + 5) {
            std::fs::write(root.path().join(format!("{index}.css")), "a{}").unwrap();
        }
        assert_eq!(
            import_directory(root.path()).unwrap().len(),
            THEME_DIRECTORY_MAX_CSS_FILES
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_unlistable_subfolder_fails_the_whole_import() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("ok.css"), "a{}").unwrap();
        let locked = root.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let listable = std::fs::read_dir(&locked).is_ok();
        let result = import_directory(root.path());
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
        if listable {
            // Running with CAP_DAC_OVERRIDE: the folder cannot be locked.
            return;
        }
        let error = result.unwrap_err();
        assert!(error.message().contains("scandir"), "{}", error.message());
    }

    #[cfg(unix)]
    #[test]
    fn picked_css_files_are_resolved_and_unreadable_ones_skipped() {
        let root = tempfile::tempdir().unwrap();
        let real = root.path().join("real.css");
        std::fs::write(&real, "a{}").unwrap();
        let link = root.path().join("link.css");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let picked = read_picked_css_files(&[link, root.path().join("absent.css")]);
        let resolved = std::fs::canonicalize(&real).unwrap();
        assert_eq!(
            picked,
            [ThemeDirectoryCssFile {
                file_name: "real.css".to_owned(),
                path: string(&resolved),
                css: "a{}".to_owned(),
            }]
        );
    }
}
