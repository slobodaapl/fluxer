// SPDX-License-Identifier: AGPL-3.0-or-later

//! The write-then-rename the TS used for its small state files: `<target>.tmp` is created
//! owner-only, filled, and renamed over the target.

use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

pub(crate) fn write(target: &Path, contents: &str) -> std::io::Result<()> {
    let pending = pending_path(target);
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(&pending)?.write_all(contents.as_bytes())?;
    std::fs::rename(&pending, target)
}

fn pending_path(target: &Path) -> PathBuf {
    let mut pending = OsString::from(target.as_os_str());
    pending.push(".tmp");
    PathBuf::from(pending)
}
