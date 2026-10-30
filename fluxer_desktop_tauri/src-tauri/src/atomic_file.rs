// SPDX-License-Identifier: AGPL-3.0-or-later

//! `writeFileAtomically` of fluxer_desktop/src/main/AtomicFile.ts: the contents go to a fresh
//! owner-only `<target>.<16 hex>.tmp` beside the target, are synced, and the temporary file is
//! renamed over the target, after which the directory is synced. Windows renames are retried
//! while another process (an indexer or a virus scanner) holds the file.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::Duration;

use crate::error::BridgeError;

const IO_ERROR: &str = "ModuleStoreIOError";
/// `WINDOWS_TRANSIENT_FS_RETRY_DELAYS_MS`.
const WINDOWS_TRANSIENT_RETRY_DELAYS: [Duration; 6] = [
    Duration::from_millis(25),
    Duration::from_millis(50),
    Duration::from_millis(100),
    Duration::from_millis(200),
    Duration::from_millis(400),
    Duration::from_millis(800),
];

/// `isWindowsTransientFsError`: what libuv reports as EPERM, EACCES or EBUSY on Windows.
fn is_windows_transient(error: &std::io::Error) -> bool {
    // ERROR_SHARING_VIOLATION, ERROR_LOCK_VIOLATION, ERROR_BUSY.
    cfg!(windows)
        && (error.kind() == std::io::ErrorKind::PermissionDenied
            || matches!(error.raw_os_error(), Some(32 | 33 | 170)))
}

/// `renameWithRetry`.
pub(crate) fn rename_with_retry(source: &Path, target: &Path) -> std::io::Result<()> {
    let mut delays = WINDOWS_TRANSIENT_RETRY_DELAYS.iter();
    loop {
        match std::fs::rename(source, target) {
            Ok(()) => return Ok(()),
            Err(error) => match delays.next() {
                Some(delay) if is_windows_transient(&error) => std::thread::sleep(*delay),
                _ => return Err(error),
            },
        }
    }
}

/// `fsyncDirectory`; Windows cannot open a directory for syncing.
fn fsync_directory(directory: &Path) -> Result<(), BridgeError> {
    if cfg!(windows) {
        return Ok(());
    }
    let handle = File::open(directory).map_err(|_| {
        BridgeError::new(
            IO_ERROR,
            format!("failed to open {} for fsync", directory.display()),
        )
    })?;
    handle
        .sync_all()
        .map_err(|_| BridgeError::new(IO_ERROR, format!("failed to fsync {}", directory.display())))
}

fn temporary_suffix() -> String {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).expect("operating system randomness is unavailable");
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// `writeFileAtomically(target, contents, 0o600)`.
pub(crate) fn write(target: &Path, contents: &[u8]) -> Result<(), BridgeError> {
    let directory = target.parent().unwrap_or(Path::new("."));
    let mut name = target.file_name().unwrap_or_default().to_owned();
    name.push(format!(".{}.tmp", temporary_suffix()));
    let temporary = directory.join(name);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut handle = options.open(&temporary).map_err(|_| {
        BridgeError::new(
            IO_ERROR,
            format!("failed to create {}", temporary.display()),
        )
    })?;
    if handle
        .write_all(contents)
        .and_then(|()| handle.sync_all())
        .is_err()
    {
        drop(handle);
        let _ = std::fs::remove_file(&temporary);
        return Err(BridgeError::new(
            IO_ERROR,
            format!("failed to write {}", temporary.display()),
        ));
    }
    drop(handle);
    if rename_with_retry(&temporary, target).is_err() {
        let _ = std::fs::remove_file(&temporary);
        return Err(BridgeError::new(
            IO_ERROR,
            format!("failed to publish {}", target.display()),
        ));
    }
    fsync_directory(directory)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_the_target_owner_only_and_leaves_no_temporary_file() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("media.png");
        std::fs::write(&target, b"old").unwrap();
        write(&target, b"new contents").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new contents");
        let names: Vec<_> = std::fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["media.png"]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&target).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn a_missing_directory_fails_with_the_create_message() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("absent").join("media.png");
        let error = write(&target, b"x").unwrap_err();
        assert_eq!(error.name(), IO_ERROR);
        assert!(error.message().starts_with("failed to create "));
        assert!(error.message().ends_with(".tmp"));
    }
}
