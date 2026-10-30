// SPDX-License-Identifier: AGPL-3.0-or-later

//! `readThemeCssFile` of ThemeLinkedFileWatch.ts: a regular file of at most 1 MiB, decoded as
//! UTF-8 (invalid bytes become U+FFFD) without a leading byte order mark. The last path
//! component must not be a symlink: `lstat` refuses one, and `O_NOFOLLOW` refuses one swapped
//! in after that check. Windows has no `O_NOFOLLOW`, so there only `lstat` guards, as in Node.

use std::fs::OpenOptions;
use std::io::Read;

use super::{ThemeFileError, ThemeLinkedFileChange};

const THEME_CSS_MAX_BYTES: u64 = 1024 * 1024;

fn error_for(error: &std::io::Error) -> ThemeFileError {
    if error.kind() == std::io::ErrorKind::NotFound {
        return ThemeFileError::Missing;
    }
    #[cfg(unix)]
    if error.raw_os_error() == Some(libc::ELOOP) {
        return ThemeFileError::NotFile;
    }
    ThemeFileError::ReadFailed
}

pub(super) fn read_theme_css_file(path: &str) -> ThemeLinkedFileChange {
    match read(path) {
        Ok(Ok(css)) => ThemeLinkedFileChange {
            path: path.to_owned(),
            css: Some(css),
            error: None,
        },
        Ok(Err(error)) => ThemeLinkedFileChange::failed(path, error),
        Err(error) => ThemeLinkedFileChange::failed(path, error_for(&error)),
    }
}

fn read(path: &str) -> std::io::Result<Result<String, ThemeFileError>> {
    if !std::fs::symlink_metadata(path)?.is_file() {
        return Ok(Err(ThemeFileError::NotFile));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(&mut options, libc::O_NOFOLLOW);
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Ok(Err(ThemeFileError::NotFile));
    }
    if metadata.len() > THEME_CSS_MAX_BYTES {
        return Ok(Err(ThemeFileError::TooLarge));
    }
    // One byte past the cap catches a file that grew after `fstat`.
    let mut buffer = Vec::new();
    file.take(THEME_CSS_MAX_BYTES + 1)
        .read_to_end(&mut buffer)?;
    if buffer.len() as u64 > THEME_CSS_MAX_BYTES {
        return Ok(Err(ThemeFileError::TooLarge));
    }
    let css = String::from_utf8_lossy(&buffer);
    Ok(Ok(css.strip_prefix('\u{feff}').unwrap_or(&css).to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_path(path: &std::path::Path) -> ThemeLinkedFileChange {
        read_theme_css_file(&path.to_string_lossy())
    }

    #[test]
    fn decodes_lossily_and_drops_one_byte_order_mark() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("theme.css");
        std::fs::write(&file, b"\xef\xbb\xbf\xef\xbb\xbfa{}\xff").unwrap();
        assert_eq!(read_path(&file).css.as_deref(), Some("\u{feff}a{}\u{fffd}"));
    }

    #[test]
    fn the_size_cap_is_inclusive() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("theme.css");
        std::fs::write(&file, vec![b'a'; THEME_CSS_MAX_BYTES as usize]).unwrap();
        assert_eq!(
            read_path(&file).css.map(|css| css.len() as u64),
            Some(THEME_CSS_MAX_BYTES)
        );
        std::fs::write(&file, vec![b'a'; THEME_CSS_MAX_BYTES as usize + 1]).unwrap();
        assert_eq!(read_path(&file).error, Some(ThemeFileError::TooLarge));
    }

    #[test]
    fn missing_files_and_directories_are_classified() {
        let directory = tempfile::tempdir().unwrap();
        assert_eq!(
            read_path(&directory.path().join("absent.css")).error,
            Some(ThemeFileError::Missing)
        );
        assert_eq!(
            read_path(directory.path()).error,
            Some(ThemeFileError::NotFile)
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_as_the_last_component_is_refused_but_a_symlinked_directory_is_followed() {
        let directory = tempfile::tempdir().unwrap();
        let real = directory.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::write(real.join("theme.css"), "a{}").unwrap();
        std::os::unix::fs::symlink(real.join("theme.css"), directory.path().join("link.css"))
            .unwrap();
        std::os::unix::fs::symlink(&real, directory.path().join("alias")).unwrap();
        assert_eq!(
            read_path(&directory.path().join("link.css")).error,
            Some(ThemeFileError::NotFile)
        );
        assert_eq!(
            read_path(&directory.path().join("alias/theme.css"))
                .css
                .as_deref(),
            Some("a{}")
        );
    }

    #[cfg(unix)]
    #[test]
    fn o_nofollow_refuses_a_symlink_with_eloop() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("theme.css"), "a{}").unwrap();
        let link = directory.path().join("link.css");
        std::os::unix::fs::symlink(directory.path().join("theme.css"), &link).unwrap();
        let mut options = OpenOptions::new();
        options.read(true);
        std::os::unix::fs::OpenOptionsExt::custom_flags(&mut options, libc::O_NOFOLLOW);
        let error = options.open(&link).unwrap_err();
        assert_eq!(error_for(&error), ThemeFileError::NotFile);
    }
}
