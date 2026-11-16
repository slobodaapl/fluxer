// SPDX-License-Identifier: AGPL-3.0-or-later
//! Native, non-credential audio preferences, isolated by instance origin and user.
use fluxer_media::{AudioMixSettings, AudioSettings};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::{Path, PathBuf},
};

// Media player useMediaVolume persists audio settings; VideoVolume persists
// video settings. Native keeps both scoped to the authenticated account.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilePlaybackKind {
    Audio,
    Video,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FilePlaybackPreference {
    pub volume: f32,
    pub muted: bool,
}
impl Default for FilePlaybackPreference {
    fn default() -> Self {
        Self {
            volume: 1.0,
            muted: false,
        }
    }
}
impl FilePlaybackPreference {
    pub fn sanitized(self) -> Self {
        Self {
            volume: if self.volume.is_finite() {
                self.volume.clamp(0.0, 1.0)
            } else {
                1.0
            },
            muted: self.muted,
        }
    }
}
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
    pub audio_playback: FilePlaybackPreference,
    pub video_playback: FilePlaybackPreference,
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
            audio_playback: FilePlaybackPreference::default(),
            video_playback: FilePlaybackPreference::default(),
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
const MAX_PREFERENCES_BYTES: u64 = 4 * 1024 * 1024;
fn read(path: &Path) -> Result<BTreeMap<String, AudioPreferences>, String> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(_) => return Err("Audio preferences could not be read".into()),
    };
    let mut bytes = Vec::new();
    file.take(MAX_PREFERENCES_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Audio preferences could not be read")?;
    if bytes.len() as u64 > MAX_PREFERENCES_BYTES {
        return Err("Audio preferences file is too large".into());
    }
    serde_json::from_slice(&bytes).map_err(|_| "Audio preferences could not be read".into())
}
pub(crate) fn load(key: &str) -> Result<AudioPreferences, String> {
    let Some(path) = path() else {
        return Err("Audio preferences location unavailable".into());
    };
    let mut prefs = read(&path)?.remove(key).unwrap_or_default();
    prefs.audio_playback = prefs.audio_playback.sanitized();
    prefs.video_playback = prefs.video_playback.sanitized();
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
    file.as_file()
        .sync_all()
        .map_err(|_| "Audio preferences could not be saved")?;
    file.persist(path)
        .map_err(|_| "Audio preferences could not be saved")?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use fluxer_media::NoiseSuppressionBackend;
    #[test]
    fn noise_backend_migrates_old_preferences_and_round_trips_per_account() {
        let enabled: AudioPreferences =
            serde_json::from_str(r#"{"dsp":{"noise_suppression":true}}"#).unwrap();
        assert_eq!(
            enabled.dsp.effective_noise_suppression_backend(),
            NoiseSuppressionBackend::Standard
        );
        let disabled: AudioPreferences =
            serde_json::from_str(r#"{"dsp":{"noise_suppression":false}}"#).unwrap();
        assert_eq!(
            disabled.dsp.effective_noise_suppression_backend(),
            NoiseSuppressionBackend::None
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("noise.json");
        for backend in NoiseSuppressionBackend::ALL {
            let mut prefs = AudioPreferences::default();
            prefs.dsp.noise_suppression_backend = backend;
            prefs.dsp.noise_suppression = backend != NoiseSuppressionBackend::None;
            save_at(&path, backend.id(), &prefs).unwrap();
        }
        let saved = read(&path).unwrap();
        for backend in NoiseSuppressionBackend::ALL {
            assert_eq!(
                saved[backend.id()]
                    .dsp
                    .effective_noise_suppression_backend(),
                backend
            );
        }
    }
    #[test]
    fn playback_preferences_are_sanitized_and_old_records_get_defaults() {
        let old: AudioPreferences = serde_json::from_str("{}").unwrap();
        assert_eq!(old.audio_playback, FilePlaybackPreference::default());
        assert_eq!(old.video_playback, FilePlaybackPreference::default());
        assert_eq!(
            FilePlaybackPreference {
                volume: f32::NAN,
                muted: true
            }
            .sanitized(),
            FilePlaybackPreference {
                volume: 1.0,
                muted: true
            }
        );
        assert_eq!(
            FilePlaybackPreference {
                volume: -3.0,
                muted: false
            }
            .sanitized()
            .volume,
            0.0
        );
        assert_eq!(
            FilePlaybackPreference {
                volume: 4.0,
                muted: false
            }
            .sanitized()
            .volume,
            1.0
        );
    }
    #[test]
    fn oversized_preferences_are_bounded_and_preserved() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("large.json");
        std::fs::File::create(&path)
            .unwrap()
            .set_len(MAX_PREFERENCES_BYTES + 1)
            .unwrap();
        assert!(read(&path).is_err());
        assert!(save_at(&path, "fixture::1", &AudioPreferences::default()).is_err());
        assert_eq!(
            std::fs::metadata(path).unwrap().len(),
            MAX_PREFERENCES_BYTES + 1
        );
    }
    #[test]
    fn accounts_are_isolated_and_invalid_file_is_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audio.json");
        let mut first = AudioPreferences {
            input: "input:usb".into(),
            ..Default::default()
        };
        first.audio_playback = FilePlaybackPreference {
            volume: 0.25,
            muted: true,
        };
        first.video_playback = FilePlaybackPreference {
            volume: 0.8,
            muted: false,
        };
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
        assert_eq!(
            stored["https://one.example::1"].audio_playback,
            first.audio_playback
        );
        assert_eq!(
            stored["https://one.example::1"].video_playback,
            first.video_playback
        );
        assert_eq!(
            stored["https://one.example::2"].audio_playback,
            FilePlaybackPreference::default()
        );
        assert_eq!(
            stored["https://one.example::2"].video_playback,
            FilePlaybackPreference::default()
        );
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
