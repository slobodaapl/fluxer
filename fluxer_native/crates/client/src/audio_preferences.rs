// SPDX-License-Identifier: AGPL-3.0-or-later
//! Native, non-credential audio preferences, isolated by instance origin and user.
use fluxer_media::{AudioMixSettings, AudioSettings};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct UserAudioPreference {
    pub muted: Option<bool>,
    pub gain: Option<f32>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct AudioPreferences {
    pub input: String,
    pub output: String,
    pub input_label: String,
    pub output_label: String,
    pub dsp: AudioSettings,
    pub mixer: AudioMixSettings,
    pub users: BTreeMap<String, UserAudioPreference>,
}
impl Default for AudioPreferences {
    fn default() -> Self {
        Self {
            input: "input:default".into(),
            output: "output:default".into(),
            input_label: "System Default".into(),
            output_label: "System Default".into(),
            dsp: AudioSettings::default(),
            mixer: AudioMixSettings::default(),
            users: BTreeMap::new(),
        }
    }
}
fn path() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(target_os = "macos")]
    let base =
        std::env::var_os("HOME").map(|p| PathBuf::from(p).join("Library/Application Support"));
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|p| Path::new(p).is_absolute())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")));
    base.map(|p| p.join("fluxer-native/audio-preferences.json"))
}
fn read(path: &Path) -> Result<BTreeMap<String, AudioPreferences>, String> {
    match std::fs::read(path) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).map_err(|_| "Audio preferences could not be read".into())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(_) => Err("Audio preferences could not be read".into()),
    }
}
pub(crate) fn load(key: &str) -> Result<AudioPreferences, String> {
    let Some(path) = path() else {
        return Err("Audio preferences location unavailable".into());
    };
    let prefs = read(&path)?.remove(key).unwrap_or_default();
    if !prefs.mixer.validate()
        || prefs.input.is_empty()
        || prefs.output.is_empty()
        || prefs.users.iter().any(|(id, p)| {
            id.parse::<fluxer_core::Snowflake>().is_err()
                || p.gain
                    .is_some_and(|gain| !gain.is_finite() || !(0.0..=2.0).contains(&gain))
        })
    {
        return Err("Saved audio preferences are invalid".into());
    }
    Ok(prefs)
}
pub(crate) fn save(key: &str, prefs: &AudioPreferences) -> Result<(), String> {
    let path = path().ok_or("Audio preferences location unavailable")?;
    save_at(&path, key, prefs)
}
fn save_at(path: &Path, key: &str, prefs: &AudioPreferences) -> Result<(), String> {
    let mut all = read(path)?;
    all.insert(key.into(), prefs.clone());
    let parent = path
        .parent()
        .ok_or("Audio preferences location unavailable")?;
    std::fs::create_dir_all(parent).map_err(|_| "Audio preferences could not be saved")?;
    let mut file = tempfile::NamedTempFile::new_in(parent)
        .map_err(|_| "Audio preferences could not be saved")?;
    serde_json::to_writer(&mut file, &all).map_err(|_| "Audio preferences could not be saved")?;
    file.flush()
        .map_err(|_| "Audio preferences could not be saved")?;
    file.persist(path)
        .map_err(|_| "Audio preferences could not be saved")?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accounts_are_isolated_and_invalid_file_is_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audio.json");
        let mut first = AudioPreferences::default();
        first.input = "input:usb".into();
        first.users.insert(
            "123".into(),
            UserAudioPreference {
                muted: Some(true),
                gain: Some(0.5),
            },
        );
        save_at(&path, "https://one.example::1", &first).unwrap();
        save_at(
            &path,
            "https://one.example::2",
            &AudioPreferences::default(),
        )
        .unwrap();
        let stored = read(&path).unwrap();
        assert_eq!(stored["https://one.example::1"].input, "input:usb");
        assert_eq!(
            stored["https://one.example::1"].users["123"].gain,
            Some(0.5)
        );
        assert!(stored["https://one.example::2"].users.is_empty());
        assert_eq!(stored["https://one.example::2"].input, "input:default");
        std::fs::write(&path, b"broken").unwrap();
        assert!(save_at(&path, "https://one.example::1", &first).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"broken");
    }
}
