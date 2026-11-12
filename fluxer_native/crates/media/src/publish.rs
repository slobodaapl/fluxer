// SPDX-License-Identifier: AGPL-3.0-or-later
//! Source-derived publish policy. Keep in step with GuildConstants.ts and
//! features/voice/utils/AudioPublishOptions.ts in the existing desktop client.
use livekit::{
    e2ee::key_provider::KeyProviderOptions,
    options::{AudioEncoding, TrackPublishOptions},
    prelude::TrackSource,
};

/// Mirrors GuildConstants.resolveVoiceChannelBitrate, including guild feature
/// caps. Feed the result to configure_microphone_publish before joining a room.
pub fn resolve_voice_channel_bitrate(raw: Option<f64>, guild_features: &[String]) -> u64 {
    let Some(raw) = raw.filter(|value| value.is_finite() && *value > 0.) else {
        return 64_000;
    };
    let maximum = guild_features.iter().fold(96_000, |max, feature| {
        max.max(match feature.as_str() {
            "AUDIO_BITRATE_128_KBPS" => 128_000,
            "AUDIO_BITRATE_256_KBPS" => 256_000,
            "AUDIO_BITRATE_384_KBPS" => 384_000,
            _ => 96_000,
        })
    });
    raw.round().clamp(8_000., maximum as f64) as u64
}

// AudioPublishOptions.normaliseAudioBitrateBps accepts both historical kbps
// values and bps. Channel policy above runs first in the existing client.
fn normalize_audio_bitrate(raw: Option<f64>) -> Option<u64> {
    let raw = raw.filter(|value| value.is_finite() && *value > 0.)?;
    let rounded = raw.round();
    let bps = if (8. ..=512.).contains(&rounded) {
        rounded * 1000.
    } else {
        rounded
    };
    Some(bps.clamp(8_000., 510_000.) as u64)
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct MicrophonePublishSettings {
    bitrate: u64,
    stereo_capture: bool,
}
impl Default for MicrophonePublishSettings {
    fn default() -> Self {
        Self {
            bitrate: 64_000,
            stereo_capture: false,
        }
    }
}
impl MicrophonePublishSettings {
    pub fn new(resolved_bitrate: Option<f64>, stereo_capture: bool) -> Self {
        Self {
            bitrate: normalize_audio_bitrate(resolved_bitrate).unwrap_or(64_000),
            stereo_capture,
        }
    }
    pub fn channels(self) -> u32 {
        if self.stereo_capture && self.bitrate >= 64_000 {
            2
        } else {
            1
        }
    }
    pub fn options(self) -> TrackPublishOptions {
        TrackPublishOptions {
            source: TrackSource::Microphone,
            force_stereo: self.channels() == 2,
            audio_encoding: Some(AudioEncoding {
                max_bitrate: self.bitrate,
            }),
            dtx: false,
            red: true,
            ..Default::default()
        }
    }
}

pub(crate) fn screen_audio_options() -> TrackPublishOptions {
    TrackPublishOptions {
        source: TrackSource::ScreenshareAudio,
        force_stereo: true,
        audio_encoding: Some(AudioEncoding {
            max_bitrate: 128_000,
        }),
        dtx: false,
        red: true,
        stream: "screen_share".into(),
        ..Default::default()
    }
}

pub(crate) fn e2ee_key_provider_options() -> KeyProviderOptions {
    // ExternalE2EEKeyProvider constructor overrides the shared provider's
    // ratchetWindowSize to zero and failureTolerance to -1. The Rust SDK shares
    // the salt, PBKDF2 algorithm, failure tolerance and keyring size already.
    KeyProviderOptions {
        ratchet_window_size: 0,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn guild_bitrate_caps_match_existing_client_fixtures() {
        assert_eq!(resolve_voice_channel_bitrate(None, &[]), 64_000);
        assert_eq!(resolve_voice_channel_bitrate(Some(f64::NAN), &[]), 64_000);
        assert_eq!(resolve_voice_channel_bitrate(Some(0.), &[]), 64_000);
        assert_eq!(resolve_voice_channel_bitrate(Some(1_000.), &[]), 8_000);
        assert_eq!(resolve_voice_channel_bitrate(Some(384_000.), &[]), 96_000);
        assert_eq!(
            resolve_voice_channel_bitrate(Some(384_000.), &["AUDIO_BITRATE_256_KBPS".into()]),
            256_000
        );
        assert_eq!(
            resolve_voice_channel_bitrate(
                Some(384_000.),
                &[
                    "AUDIO_BITRATE_128_KBPS".into(),
                    "AUDIO_BITRATE_384_KBPS".into()
                ]
            ),
            384_000
        );
    }
    #[test]
    fn microphone_publication_policy_and_capture_channels_match_source() {
        let mono = MicrophonePublishSettings::new(Some(48.), true);
        assert_eq!(mono.channels(), 1);
        assert_eq!(mono.options().audio_encoding.unwrap().max_bitrate, 48_000);
        let stereo = MicrophonePublishSettings::new(Some(64_000.), true);
        assert_eq!(stereo.channels(), 2);
        assert_eq!(
            MicrophonePublishSettings::new(Some(96_000.), false).channels(),
            1
        );
        let options = stereo.options();
        assert!(options.force_stereo);
        assert!(
            !MicrophonePublishSettings::new(Some(64_000.), false)
                .options()
                .force_stereo
        );
        assert!(!options.dtx);
        assert!(options.red);
        assert_eq!(options.source, TrackSource::Microphone);
        assert_eq!(normalize_audio_bitrate(Some(512.)), Some(510_000));
    }
    #[test]
    fn shared_audio_publication_matches_screen_share_options() {
        let options = screen_audio_options();
        assert!(options.force_stereo);
        assert_eq!(options.audio_encoding.unwrap().max_bitrate, 128_000);
        assert!(!options.dtx);
        assert!(options.red);
        assert_eq!(options.source, TrackSource::ScreenshareAudio);
        assert_eq!(options.stream, "screen_share");
    }
    #[test]
    fn external_shared_e2ee_provider_matches_electron_overrides() {
        let options = e2ee_key_provider_options();
        assert_eq!(options.ratchet_window_size, 0);
        assert_eq!(options.failure_tolerance, -1);
        assert_eq!(options.key_ring_size, 16);
        assert_eq!(options.ratchet_salt, b"LKFrameEncryptionKey");
        assert!(matches!(
            options.key_derivation_algorithm,
            livekit::e2ee::key_provider::KeyDerivationAlgorithm::PBKDF2
        ));
    }
}
