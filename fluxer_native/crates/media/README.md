# Native media and audio management

The media engine owns LiveKit/WebRTC capture and transport, native screen capture,
application/system share audio, a processed microphone pipeline, and a CPAL
speaker mixer. All device handles stay off the GPUI rendering thread. Device
capture starts on an explicit call join or microphone test and stops on leave,
failed join, mute/deafen, test stop, or drop.

Audio controls process actual samples:

- PlatformAudio supplies device routing and WebRTC AEC/noise suppression/AGC.
  A local, unpublished RTC track taps this processed microphone into a bounded
  NativeAudioStream. Application input gain (0–200%), open microphone, RMS voice
  activation with release, and push-to-talk gate are applied before forwarding
  into the published NativeAudioSource. The local meter reads post-gain peak.
- Remote RTC streams feed a bounded 48 kHz stereo mixer. Master output gain,
  per-participant gain and local mute, and deafen affect the speaker samples.
  Native direct speaker playout is explicitly disabled through LiveKit's public
  runtime factory, preventing double playback. CPAL performs device output;
  the mixer linearly resamples to its default hardware rate and converts to
  f32/i16/u16. It supports mono/stereo output; extra surround channels are silent.
- Device enumeration can be refreshed. Input selection uses WebRTC's native
  device switch; output selection atomically replaces the CPAL stream after the
  replacement opens successfully. A stale device identifier fails rather than
  silently choosing another device. Output stream failures produce media errors.
- Microphone test reports levels for local capture only and never publishes,
  records to disk, or plays audio back. It cannot run concurrently with a call.

`AudioMixSettings` is separate from `AudioSettings` (WebRTC DSP). All gain and
threshold values are validated. `set_push_to_talk_pressed(false)` should also
be called when the application loses focus, because key-up may not arrive.

Known limits: the WebRTC echo-cancellation reverse reference is its original
remote mix, whereas actual speaker audio is resampled and may have different
application gains/local mutes. Acoustic echo behavior therefore requires real
headset/speaker testing, especially with boosted gains. Noise suppression and
AGC are WebRTC controls; advanced existing Fluxer speaker isolation/effects and
global OS push-to-talk hooks are not ported. Hot-unplug errors require refreshing
and selecting another device; automatic preferred-device fallback is pending.
The CPAL device identifiers include enumeration index and name, so they should
not be treated as stable persistence keys across hotplug/restarts. This engine
owns process-wide WebRTC audio routing and expects one active media engine.

Validation:

```sh
cargo test --manifest-path fluxer_native/Cargo.toml -p fluxer-media --lib
cargo run --manifest-path fluxer_native/Cargo.toml -p fluxer-media --example audio_probe
# Explicitly capture five seconds of local mic level readings:
cargo run --manifest-path fluxer_native/Cargo.toml -p fluxer-media --example audio_probe -- --meter
```

Unit tests cover PCM gain, saturation, PTT, voice-activation release, participant
mixing, mute/deafen, bounded queue latency and resampling. An RTC pipeline test
forwards synthetic microphone frames through actual NativeAudioSource/Stream
objects and asserts changed gain and closed/open PTT samples at the final sink.
