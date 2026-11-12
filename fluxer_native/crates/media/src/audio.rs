// SPDX-License-Identifier: AGPL-3.0-or-later
//! Sample-level audio gain, activation gates and bounded participant mixer.
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputMode {
    #[default]
    OpenMic,
    VoiceActivity,
    PushToTalk,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioMixSettings {
    pub input_gain: f32,
    pub output_gain: f32,
    pub input_mode: InputMode,
    pub voice_activation_threshold_db: f32,
    pub voice_activation_release_ms: u32,
}
impl Default for AudioMixSettings {
    fn default() -> Self {
        Self {
            input_gain: 1.0,
            output_gain: 1.0,
            input_mode: InputMode::OpenMic,
            voice_activation_threshold_db: -50.0,
            voice_activation_release_ms: 200,
        }
    }
}
impl AudioMixSettings {
    pub fn validate(&self) -> bool {
        [self.input_gain, self.output_gain]
            .iter()
            .all(|v| v.is_finite() && (0.0..=2.0).contains(v))
            && self.voice_activation_threshold_db.is_finite()
            && (-96.0..=0.0).contains(&self.voice_activation_threshold_db)
            && self.voice_activation_release_ms <= 2000
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct ParticipantAudioSettings {
    pub gain: f32,
    pub muted: bool,
}
impl Default for ParticipantAudioSettings {
    fn default() -> Self {
        Self {
            gain: 1.0,
            muted: false,
        }
    }
}
impl ParticipantAudioSettings {
    pub fn validate(self) -> bool {
        self.gain.is_finite() && (0.0..=2.0).contains(&self.gain)
    }
}

#[derive(Default)]
pub(crate) struct InputProcessor {
    release_samples: u64,
}
impl InputProcessor {
    /// Returns peak after gain and before activation, so a closed gate still has
    /// a useful input meter. Muted/deafened/PTT-closed frames are zeroed.
    pub fn process(
        &mut self,
        samples: &mut [i16],
        sample_rate: u32,
        channels: u32,
        settings: &AudioMixSettings,
        blocked: bool,
        ptt: bool,
    ) -> f32 {
        let gain = settings.input_gain;
        let mut peak = 0.0_f32;
        let mut sum = 0.0_f64;
        for sample in samples.iter_mut() {
            let value = (*sample as f32 / 32768.0 * gain).clamp(-1.0, 1.0);
            peak = peak.max(value.abs());
            sum += (value as f64).powi(2);
            *sample = (value * 32767.0).round() as i16;
        }
        let rms = (sum / samples.len().max(1) as f64).sqrt() as f32;
        let active = rms >= 10.0_f32.powf(settings.voice_activation_threshold_db / 20.0);
        if active {
            self.release_samples =
                sample_rate as u64 * settings.voice_activation_release_ms as u64 / 1000;
        }
        let gate_open = match settings.input_mode {
            InputMode::OpenMic => true,
            InputMode::PushToTalk => ptt,
            InputMode::VoiceActivity => active || self.release_samples > 0,
        };
        self.release_samples = self
            .release_samples
            .saturating_sub(samples.len() as u64 / channels.max(1) as u64);
        if blocked || !gate_open {
            samples.fill(0);
        }
        peak
    }
}

pub(crate) struct MixerTrack {
    pub identity: String,
    samples: VecDeque<[f32; 2]>,
}
#[derive(Default)]
pub(crate) struct PlaybackMixer {
    pub tracks: HashMap<String, MixerTrack>,
    pub participants: HashMap<String, ParticipantAudioSettings>,
    pub master_gain: f32,
    pub deafened: bool,
    pub peak: f32,
    pub phase: f64,
    pub previous: [f32; 2],
    pub next: [f32; 2],
    pub events: Option<tokio::sync::mpsc::Sender<crate::MediaEvent>>,
}
impl PlaybackMixer {
    pub fn new() -> Self {
        Self {
            master_gain: 1.0,
            ..Self::default()
        }
    }
    pub fn add_track(&mut self, id: String, identity: String) {
        self.tracks.insert(
            id,
            MixerTrack {
                identity,
                samples: VecDeque::new(),
            },
        );
    }
    pub fn push(&mut self, id: &str, samples: &[i16], channels: u32) {
        let Some(track) = self.tracks.get_mut(id) else {
            return;
        };
        let channels = channels as usize;
        if channels == 0 {
            return;
        }
        for frame in samples.chunks_exact(channels) {
            let left = frame[0] as f32 / 32768.0;
            let right = frame.get(1).map_or(left, |v| *v as f32 / 32768.0);
            track.samples.push_back([left, right]);
        }
        // Bound latency to 100 ms; favor recent audio after a stalled device.
        while track.samples.len() > 4800 {
            track.samples.pop_front();
        }
    }
    fn next_stereo(&mut self) -> [f32; 2] {
        let mut out = [0.0; 2];
        for track in self.tracks.values_mut() {
            let sample = track.samples.pop_front().unwrap_or([0.0; 2]);
            let settings = self
                .participants
                .get(&track.identity)
                .copied()
                .unwrap_or_default();
            if !self.deafened && !settings.muted {
                out[0] += sample[0] * settings.gain;
                out[1] += sample[1] * settings.gain;
            }
        }
        for sample in &mut out {
            *sample = (*sample * self.master_gain).clamp(-1.0, 1.0);
        }
        out
    }
    pub fn render_stereo(&mut self, rate: u32) -> [f32; 2] {
        let step = 48_000.0 / rate.max(1) as f64;
        while self.phase >= 1.0 {
            self.previous = self.next;
            self.next = self.next_stereo();
            self.phase -= 1.0;
        }
        let fraction = self.phase as f32;
        let mut out = [0.0; 2];
        for (i, sample) in out.iter_mut().enumerate() {
            *sample = self.previous[i] + (self.next[i] - self.previous[i]) * fraction;
            self.peak = self.peak.max(sample.abs());
        }
        self.phase += step;
        if self.deafened || self.master_gain == 0.0 {
            out.fill(0.0);
        }
        out
    }
    pub fn clear(&mut self) {
        self.tracks.clear();
        self.previous = [0.0; 2];
        self.next = [0.0; 2];
        self.phase = 0.0;
        self.peak = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn input_gain_and_gate_affect_actual_pcm() {
        let mut processor = InputProcessor::default();
        let mut settings = AudioMixSettings {
            input_gain: 2.0,
            ..AudioMixSettings::default()
        };
        let mut frame = [20_000_i16; 480];
        let peak = processor.process(&mut frame, 48_000, 1, &settings, false, false);
        assert_eq!(peak, 1.0);
        assert_eq!(frame[0], i16::MAX);
        settings.input_mode = InputMode::PushToTalk;
        processor.process(&mut frame, 48_000, 1, &settings, false, false);
        assert_eq!(frame, [0; 480]);
        frame.fill(10_000);
        processor.process(&mut frame, 48_000, 1, &settings, false, true);
        assert!(frame[0] > 0);
        processor.process(&mut frame, 48_000, 1, &settings, true, true);
        assert_eq!(frame, [0; 480]);
    }
    #[test]
    fn vad_hangover_and_release() {
        let mut processor = InputProcessor::default();
        let settings = AudioMixSettings {
            input_mode: InputMode::VoiceActivity,
            voice_activation_threshold_db: -20.0,
            voice_activation_release_ms: 20,
            ..AudioMixSettings::default()
        };
        let mut loud = [10_000_i16; 480];
        processor.process(&mut loud, 48_000, 1, &settings, false, false);
        assert!(loud[0] > 0);
        let mut quiet = [100_i16; 480];
        processor.process(&mut quiet, 48_000, 1, &settings, false, false);
        assert!(quiet[0] > 0);
        quiet.fill(100);
        processor.process(&mut quiet, 48_000, 1, &settings, false, false);
        assert_eq!(quiet[0], 0);
    }
    #[test]
    fn participant_and_master_gain_mute_and_deafen_mix_pcm() {
        let mut mixer = PlaybackMixer::new();
        mixer.add_track("a".into(), "alex".into());
        mixer.add_track("b".into(), "sam".into());
        mixer.push("a", &[8192; 20], 2);
        mixer.push("b", &[8192; 20], 2);
        mixer.participants.insert(
            "alex".into(),
            ParticipantAudioSettings {
                gain: 2.0,
                muted: false,
            },
        );
        mixer.master_gain = 0.5;
        assert_eq!(mixer.next_stereo(), [0.375; 2]);
        mixer.participants.get_mut("alex").unwrap().muted = true;
        assert_eq!(mixer.next_stereo(), [0.125; 2]);
        mixer.deafened = true;
        assert_eq!(mixer.next_stereo(), [0.0; 2]);
    }
    #[test]
    fn mixer_clips_and_bounds_latency() {
        let mut mixer = PlaybackMixer::new();
        mixer.add_track("a".into(), "alex".into());
        mixer.push("a", &vec![i16::MAX; 20_000], 2);
        assert_eq!(mixer.tracks["a"].samples.len(), 4800);
        mixer.master_gain = 2.0;
        assert_eq!(mixer.next_stereo(), [1.0; 2]);
        for _ in 0..5000 {
            let sample = mixer.render_stereo(44_100);
            assert!(sample.iter().all(|v| v.is_finite() && v.abs() <= 1.0));
        }
    }
    #[test]
    fn settings_reject_nonfinite_or_extreme_values() {
        assert!(
            !AudioMixSettings {
                input_gain: f32::NAN,
                ..AudioMixSettings::default()
            }
            .validate()
        );
        assert!(
            !ParticipantAudioSettings {
                gain: -1.0,
                muted: false
            }
            .validate()
        );
    }
}
