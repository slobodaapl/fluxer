// SPDX-License-Identifier: AGPL-3.0-or-later
//! Mirrors ClientInstallationId.ts: one cached, persisted 16-byte random ID,
//! encoded as 32 lowercase hexadecimal digits. This is not an account token.
use std::{fs, path::Path, sync::OnceLock};

pub(crate) fn get() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| {
        let path =
            dirs::data_local_dir().map(|root| root.join("fluxer-native/ClientInstallationId"));
        path.as_deref().map(load).unwrap_or_else(generate)
    })
}

fn valid(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

fn generate() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

fn load(path: &Path) -> String {
    if let Ok(stored) = fs::read_to_string(path)
        && valid(&stored)
    {
        return stored;
    }
    let created = generate();
    if let Some(parent) = path.parent() {
        let temporary = parent.join(format!(".installation-{}", generate()));
        let persisted = fs::create_dir_all(parent)
            .and_then(|()| fs::write(&temporary, &created))
            .and_then(|()| fs::rename(&temporary, path));
        if persisted.is_err() {
            // PrebootMirror.writeRawStorageItem also tolerates unavailable
            // persistent storage while retaining the in-memory installation ID.
            let _ = fs::remove_file(temporary);
        }
    }
    created
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn installation_id_survives_reload_and_replaces_invalid_values() {
        let root = std::env::temp_dir().join(format!("fluxer-installation-{}", generate()));
        let path = root.join("ClientInstallationId");
        let first = load(&path);
        assert!(valid(&first));
        assert_eq!(load(&path), first);
        fs::write(&path, "not-an-installation-id").unwrap();
        let replaced = load(&path);
        assert!(valid(&replaced));
        assert_ne!(replaced, first);
        assert_eq!(load(&path), replaced);
        fs::remove_dir_all(root).unwrap();
    }
}
