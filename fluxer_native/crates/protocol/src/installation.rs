// SPDX-License-Identifier: AGPL-3.0-or-later
//! Mirrors ClientInstallationId.ts: one cached, persisted 16-byte random ID,
//! encoded as 32 lowercase hexadecimal digits. This is not an account token.
use std::{fs, path::Path, sync::OnceLock};

type RandomResult<T> = std::result::Result<T, ()>;

pub(crate) fn get() -> crate::Result<&'static str> {
    static ID: OnceLock<RandomResult<String>> = OnceLock::new();
    ID.get_or_init(|| {
        let path =
            dirs::data_local_dir().map(|root| root.join("fluxer-native/ClientInstallationId"));
        path.as_deref().map(load).unwrap_or_else(generate)
    })
    .as_deref()
    .map_err(|_| crate::Error::InstallationIdUnavailable)
}

fn valid(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

fn generate() -> RandomResult<String> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| ())?;
    // ClientInstallationId.ts uses all 128 random bits, not a UUID's fixed
    // version/variant bits, then pads each byte to two lowercase hex digits.
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn load(path: &Path) -> RandomResult<String> {
    if let Ok(stored) = fs::read_to_string(path)
        && valid(&stored)
    {
        return Ok(stored);
    }
    let created = generate()?;
    if let Some(parent) = path.parent() {
        let temporary = parent.join(format!(".installation-{}", generate()?));
        let persisted = fs::create_dir_all(parent)
            .and_then(|()| fs::write(&temporary, &created))
            .and_then(|()| fs::rename(&temporary, path));
        if persisted.is_err() {
            // PrebootMirror.writeRawStorageItem also tolerates unavailable
            // persistent storage while retaining the in-memory installation ID.
            let _ = fs::remove_file(temporary);
        }
    }
    Ok(created)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn installation_id_survives_reload_and_replaces_invalid_values() {
        let root =
            std::env::temp_dir().join(format!("fluxer-installation-{}", generate().unwrap()));
        let path = root.join("ClientInstallationId");
        let first = load(&path).unwrap();
        assert!(valid(&first));
        assert_eq!(load(&path).unwrap(), first);
        fs::write(&path, "not-an-installation-id").unwrap();
        let replaced = load(&path).unwrap();
        assert!(valid(&replaced));
        assert_ne!(replaced, first);
        assert_eq!(load(&path).unwrap(), replaced);
        fs::remove_dir_all(root).unwrap();
    }
}
