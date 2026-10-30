// SPDX-License-Identifier: AGPL-3.0-or-later

//! DesktopAccountStoreTelemetry.ts. Events are tracing records instead of the counts file the
//! Electron shell persisted; the permission probe is unchanged.

use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DesktopAccountStoreEvent {
    Migrated,
    Quarantined,
    FallbackToWeb,
    PermissionHardeningFailed,
}

impl DesktopAccountStoreEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Migrated => "desktop.account_store.migrated",
            Self::Quarantined => "desktop.account_store.quarantined",
            Self::FallbackToWeb => "desktop.account_store.fallback_to_web",
            Self::PermissionHardeningFailed => "desktop.account_store.permission_hardening_failed",
        }
    }
}

pub(super) fn record_desktop_account_store_event(event: DesktopAccountStoreEvent) {
    tracing::info!(event = event.as_str(), "[AppStore] Account store event");
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DesktopStorePermissionState {
    #[cfg_attr(not(unix), allow(dead_code))]
    Hardened,
    Widened,
    #[cfg_attr(unix, allow(dead_code))]
    Unsupported,
    #[cfg_attr(not(unix), allow(dead_code))]
    Unknown,
}

#[cfg(unix)]
pub(super) fn probe_desktop_store_permissions(store_file: &Path) -> DesktopStorePermissionState {
    use std::os::unix::fs::PermissionsExt;

    const GROUP_AND_WORLD_BITS: u32 = 0o077;
    let directory = store_file.parent().unwrap_or(store_file);
    let modes = std::fs::metadata(store_file).and_then(|file| {
        Ok((
            file.permissions().mode(),
            std::fs::metadata(directory)?.permissions().mode(),
        ))
    });
    match modes {
        Ok((file, directory)) => {
            if (file & GROUP_AND_WORLD_BITS) != 0 || (directory & GROUP_AND_WORLD_BITS) != 0 {
                DesktopStorePermissionState::Widened
            } else {
                DesktopStorePermissionState::Hardened
            }
        }
        Err(error) => {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(store_file = %store_file.display(), %error, "Failed to inspect account store permissions");
            }
            DesktopStorePermissionState::Unknown
        }
    }
}

#[cfg(not(unix))]
pub(super) fn probe_desktop_store_permissions(_store_file: &Path) -> DesktopStorePermissionState {
    DesktopStorePermissionState::Unsupported
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn chmod(path: &Path, mode: u32) {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn group_or_world_bits_on_the_file_or_directory_widen_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("desktop-app-store.sqlite3");
        assert_eq!(
            probe_desktop_store_permissions(&file),
            DesktopStorePermissionState::Unknown
        );
        std::fs::write(&file, "").unwrap();
        chmod(dir.path(), 0o700);
        chmod(&file, 0o600);
        assert_eq!(
            probe_desktop_store_permissions(&file),
            DesktopStorePermissionState::Hardened
        );
        chmod(&file, 0o640);
        assert_eq!(
            probe_desktop_store_permissions(&file),
            DesktopStorePermissionState::Widened
        );
        chmod(&file, 0o600);
        chmod(dir.path(), 0o755);
        assert_eq!(
            probe_desktop_store_permissions(&file),
            DesktopStorePermissionState::Widened
        );
    }
}
