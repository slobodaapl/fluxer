// SPDX-License-Identifier: AGPL-3.0-or-later

//! The Windows notification icon cache of NotificationIcon.ts (`cacheNotificationIcon`): toasts
//! take an image by path, so each decoded icon is written as
//! `<app data>/notification-icons/<sha256 of the source>.png` and reused while it exists (its
//! modification time refreshed). A write goes to `<key>.<pid>.<ms>.tmp` and is renamed into
//! place. After a write the directory is trimmed to the 512 newest `.png` files, one trim at a
//! time.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::png_image::{self, RgbaImage};

/// `NOTIFICATION_ICON_CACHE_MAX_FILES`.
const MAX_FILES: usize = 512;
const DIRECTORY_NAME: &str = "notification-icons";

pub struct IconCache {
    directory: PathBuf,
    trimming: Arc<AtomicBool>,
}

impl IconCache {
    pub fn new(app_data_dir: &Path) -> Self {
        Self {
            directory: app_data_dir.join(DIRECTORY_NAME),
            trimming: Arc::new(AtomicBool::new(false)),
        }
    }

    /// The cached file for `source`, written from `image` unless it exists. Blocking.
    pub fn store(&self, source: &str, image: &RgbaImage) -> io::Result<PathBuf> {
        std::fs::create_dir_all(&self.directory)?;
        let key: String = Sha256::digest(source.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let path = self.directory.join(format!("{key}.png"));
        if std::fs::File::open(&path).is_ok() {
            touch(&path);
            return Ok(path);
        }
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let temporary = self
            .directory
            .join(format!("{key}.{}.{millis}.tmp", std::process::id()));
        std::fs::write(&temporary, png_image::encode(image))?;
        if let Err(error) = std::fs::rename(&temporary, &path) {
            let _ = std::fs::remove_file(&temporary);
            return Err(error);
        }
        self.schedule_trim();
        Ok(path)
    }

    /// `scheduleNotificationIconCacheTrim`: a trim in the background unless one is running.
    fn schedule_trim(&self) {
        if self.trimming.swap(true, Ordering::AcqRel) {
            return;
        }
        let directory = self.directory.clone();
        let trimming = self.trimming.clone();
        tauri::async_runtime::spawn_blocking(move || {
            if let Err(error) = trim(&directory) {
                tracing::warn!(%error, "Failed to trim notification icon cache");
            }
            trimming.store(false, Ordering::Release);
        });
    }
}

/// `touchFile`: failures are ignored.
fn touch(path: &Path) {
    let _ = std::fs::File::options()
        .write(true)
        .open(path)
        .and_then(|file| file.set_modified(SystemTime::now()));
}

/// `trimNotificationIconCache`: the oldest `.png` files beyond [`MAX_FILES`] are removed.
fn trim(directory: &Path) -> io::Result<()> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_file()
            && path.extension().is_some_and(|extension| extension == "png")
        {
            files.push((entry.metadata()?.modified()?, path));
        }
    }
    if files.len() <= MAX_FILES {
        return Ok(());
    }
    files.sort_by_key(|(modified, _)| *modified);
    let excess = files.len() - MAX_FILES;
    for (_, path) in files.into_iter().take(excess) {
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn image(shade: u8) -> RgbaImage {
        RgbaImage {
            width: 1,
            height: 1,
            pixels: vec![shade, shade, shade, 255],
        }
    }

    #[test]
    fn a_source_is_written_once_and_reused() {
        let root = tempfile::tempdir().unwrap();
        let cache = IconCache::new(root.path());
        let first = cache.store("https://cdn.test/a.png", &image(10)).unwrap();
        let again = cache.store("https://cdn.test/a.png", &image(200)).unwrap();
        assert_eq!(first, again);
        assert_eq!(
            png_image::decode(&std::fs::read(&first).unwrap()).unwrap(),
            image(10)
        );
        let other = cache.store("https://cdn.test/b.png", &image(200)).unwrap();
        assert_ne!(first, other);
        assert_eq!(
            first.parent(),
            Some(root.path().join("notification-icons").as_path())
        );
        let leftovers = std::fs::read_dir(first.parent().unwrap())
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "tmp")
            })
            .count();
        assert_eq!(leftovers, 0);
    }

    #[test]
    fn trimming_keeps_the_newest_icons() {
        let root = tempfile::tempdir().unwrap();
        let base = SystemTime::now() - Duration::from_secs(10_000);
        for index in 0..MAX_FILES + 2 {
            let path = root.path().join(format!("{index}.png"));
            std::fs::write(&path, b"").unwrap();
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(base + Duration::from_secs(index as u64))
                .unwrap();
        }
        std::fs::write(root.path().join("note.txt"), b"").unwrap();
        trim(root.path()).unwrap();
        assert!(!root.path().join("0.png").exists());
        assert!(!root.path().join("1.png").exists());
        assert!(root.path().join("2.png").exists());
        assert!(root.path().join(format!("{}.png", MAX_FILES + 1)).exists());
        assert!(root.path().join("note.txt").exists());
    }
}
