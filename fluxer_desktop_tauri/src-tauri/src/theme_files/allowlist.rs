// SPDX-License-Identifier: AGPL-3.0-or-later

//! Allowlist membership: `new Set(allowed.map(path.resolve)).has(path.resolve(candidate))`.
//! `path.resolve` makes a path absolute against the working directory and folds `.` and `..`
//! lexically, without touching the file system, so a symlinked spelling of an allowed file is
//! not the same entry. `std::path::absolute` keeps `..` on POSIX, hence the fold here.

use std::collections::HashSet;
use std::path::{Component, PathBuf};

/// `path.resolve(path)`.
fn resolve(path: &str) -> PathBuf {
    let absolute = if path.is_empty() {
        std::env::current_dir()
    } else {
        std::path::absolute(path)
    }
    .unwrap_or_else(|_| PathBuf::from(path));
    let mut resolved = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                resolved.push(component);
            }
            Component::CurDir => {}
            // Popping at the root is a no-op, as `/..` resolves to `/`.
            Component::ParentDir => {
                resolved.pop();
            }
        }
    }
    resolved
}

pub(super) struct AllowedFiles(HashSet<PathBuf>);

impl AllowedFiles {
    pub fn new(allowed: &[String]) -> Self {
        Self(allowed.iter().map(|entry| resolve(entry)).collect())
    }

    pub fn contains(&self, path: &str) -> bool {
        self.0.contains(&resolve(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn resolve_folds_dots_and_separators_like_node() {
        assert_eq!(resolve("/a/b/../c/./d//e/"), PathBuf::from("/a/c/d/e"));
        assert_eq!(resolve("/../../x"), PathBuf::from("/x"));
        assert_eq!(resolve("/"), PathBuf::from("/"));
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(resolve("theme.css"), cwd.join("theme.css"));
        assert_eq!(resolve(""), cwd);
        assert_eq!(resolve("x/../.."), cwd.parent().unwrap_or(&cwd));
    }

    #[cfg(windows)]
    #[test]
    fn resolve_folds_dots_and_separators_like_node() {
        assert_eq!(resolve(r"C:\a\b\..\c/./d"), PathBuf::from(r"C:\a\c\d"));
        assert_eq!(resolve(r"C:\..\x"), PathBuf::from(r"C:\x"));
    }

    #[cfg(unix)]
    #[test]
    fn membership_compares_resolved_spellings_without_following_links() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("theme.css");
        std::fs::write(&file, "a{}").unwrap();
        let link = directory.path().join("link.css");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        let allowed = AllowedFiles::new(&[file.to_string_lossy().into_owned()]);
        let detour = directory.path().join("sub/../theme.css");
        assert!(allowed.contains(&detour.to_string_lossy()));
        assert!(!allowed.contains(&link.to_string_lossy()));
        // Node compares case-sensitively on every platform.
        assert!(!allowed.contains(&directory.path().join("THEME.css").to_string_lossy()));
    }
}
