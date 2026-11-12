# Native media and audio management

The media engine owns LiveKit transport, native screen capture, application/system
share audio, a processed microphone pipeline, and a native speaker mixer. Device
handles stay off the GPUI rendering thread. Physical microphone capture starts
only for an unmuted call or an explicit microphone test. Mute/deafen, leave,
failed join, test stop, and drop release capture.

Audio controls process actual samples:

- Linux microphone capture uses a dedicated PipeWire input stream. The source
  picker preserves individual logical inputs, including GoXLR Chat Mic, rather
  than reducing a device to its ALSA hardware name. System Default uses PipeWire
  policy; explicit choices target the exact source node. Other platforms use
  CPAL input capture. Input handles live on a dedicated owner thread.
- A bounded PCM queue feeds WebRTC's SDK `AudioResampler` and standalone
  `AudioProcessingModule` in exact 10 ms blocks. AEC/noise suppression/AGC run
  before application input gain (0–200%), RMS voice activation/release, and
  push-to-talk. Processed frames feed the published `NativeAudioSource`; the
  local meter reports post-gain peak before the activation gate.
- This deliberately avoids tapping a `RtcAudioSource::Device` track. In the
  pinned WebRTC SDK, `pc/local_audio_source.h` implements `AddSink` as a no-op;
  attaching `NativeAudioStream` to that source never delivers microphone PCM.
  Publishing an unrelated native track or manually starting the ADM cannot
  make that tap work. Native tests observe final track PCM, rather than treating
  successful publication/device initialization as evidence of capture.
- Remote streams feed a bounded 48 kHz stereo mixer. Master output gain,
  per-participant gain/local mute, and deafen affect speaker samples. Native
  direct speaker playout is disabled to prevent double playback. Linux uses a
  dedicated PipeWire output stream at 48 kHz stereo f32. The picker lists logical
  sinks such as GoXLR Chat/Game/Music rather than their shared ALSA card; explicit
  `output:pipewire:<node.name>` choices target that exact current sink, while
  `output:default` follows system routing policy. Other platforms use CPAL to
  resample and convert to f32/i16/u16; mono is downmixed and extra surround
  channels remain silent. The standalone microphone
  APM receives the post-gain/mute mixer as its echo-cancellation reference.
- Input selection releases old capture before opening its replacement. A stale
  explicit identifier fails instead of silently choosing another source. Output
  selection validates the current identifier, releases and joins the old output,
  then opens its replacement. Stale explicit Linux sink IDs fail without ALSA
  or default-device fallback. Stop acknowledges completed output teardown before
  reconnect starts another stream. Device errors
  surface as media errors; active input with no callbacks for two seconds emits
  an actionable stall error. Silence still produces frames and is not a stall.
- Microphone test meters local capture only and never publishes, records to
  disk, or plays microphone audio. It cannot run concurrently with a call.

`AudioMixSettings` is separate from `AudioSettings` (WebRTC DSP). Gain/threshold
values are validated. Call `set_push_to_talk_pressed(false)` on focus loss,
because key-up may not arrive.

Known limits: the AEC reference is the 48 kHz stereo mixer before final hardware
resampling/mono downmix; physical DAC/acoustic delay is not measured. Real
headset/speaker echo behavior needs hardware testing. Advanced existing Fluxer
speaker isolation/effects and global OS push-to-talk hooks are not ported.
Hot-unplug errors require refreshing/selecting another device; automatic
preferred-device fallback is pending. CPAL identifiers on other platforms contain
index/name and are not stable persistence keys. Linux PipeWire choices use exact
source/sink node names. `try_devices()` reports enumeration failures; `devices()`
retains the legacy best-effort interface for probes.

Validation:

```sh
cargo test --manifest-path fluxer_native/Cargo.toml -p fluxer-media --lib
cargo run --manifest-path fluxer_native/Cargo.toml -p fluxer-media --example audio_probe
# Explicit local microphone level readings (no network publication):
cargo run --manifest-path fluxer_native/Cargo.toml -p fluxer-media --example audio_probe -- --meter
python3 fluxer_native/scripts/test_media_loopback.py --audio-lifecycle
python3 fluxer_native/scripts/test_microphone_capture.py
python3 fluxer_native/scripts/test_output_routing.py
python3 fluxer_native/scripts/test_output_routing.py --media
```

Unit tests cover actual SDK track PCM for input gain/PTT, arbitrary callback
chunking, stereo, 44.1-to-48 kHz conversion, microphone meter isolation, missing
callbacks, and the gain-adjusted echo reference. Mixer tests cover clipping,
voice-activation release, participant mute/deafen, and bounded latency. Local
LiveKit/virtual-microphone tests exercise physical capture through publication
and reception without sending personal microphone audio to an external server.
The output runner creates two isolated sinks and checks both monitor PCM and
actual graph links. Its media mode decodes synthetic LiveKit audio through the
real mixer/output, verifies gain and participant mute, switches sinks during the
call, and checks teardown silence. No default devices or other application
routes are changed; all temporary sinks are removed after the run.
