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
  `AudioProcessingModule` in exact 10 ms blocks. AEC/AGC and selected noise suppression run
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
  per-participant microphone gain/local mute, per-stream share-audio gain/local
  mute, and deafen affect speaker samples. Microphone and share controls are
  independent, including when both tracks belong to the same participant. Native
  direct speaker playout is disabled to prevent double playback. Linux uses a
  dedicated PipeWire output stream at 48 kHz stereo f32. The picker lists logical
  sinks such as GoXLR Chat/Game/Music rather than their shared ALSA card; explicit
  `output:pipewire:<node.name>` choices target that exact current sink, while
  `output:default` follows system routing policy. Other platforms use CPAL to
  resample and convert to f32/i16/u16; mono is downmixed and extra surround
  channels remain silent. The standalone microphone
  APM receives the post-gain/mute mixer as its echo-cancellation reference.
- Noise suppression is selectable: Off, WebRTC, noise gate, Speex, RNNoise,
  GTCRN, or DeepFilterNet3. The model backends embed the same WASM binaries as the
  existing client and run locally through Wasmtime on a dedicated worker, with independent
  state for each microphone channel. WebRTC suppression runs only when WebRTC
  is selected; AEC/AGC remain independent. GTCRN's 768-sample frames are buffered
  across 480-sample capture blocks with a fixed 16 ms adapter delay. Switching
  backends discards buffered audio and recurrent state without republishing.
  Model errors fall back to WebRTC and report the reason in voice settings.
  Existing saved suppression flags retain their WebRTC/off behavior; the chosen
  backend is saved per account. Asset provenance and licenses live in
  [assets/noise/NOTICE.md](assets/noise/NOTICE.md).
- DeepFilterNet3 uses the existing patched `df_bg.wasm` and the exact hash-pinned
  model archive. It matches the web client's 60 Hz high-pass, x10/x0.1 gain,
  30 dB attenuation limit, and eight silence warm-up frames. Its 480-sample model
  frames match capture directly. Initialization runs on the suppression worker
  while WebRTC continues publishing; settings show preparation/fallback status.
  Compiled DFN code is cached by Wasmtime across launches. Switching away during
  preparation applies immediately, without sending microphone samples to DFN.
  The build uses the existing model file when hydrated by Git LFS; otherwise it
  fetches and verifies an 8 MB copy into Cargo's build output for embedding.
  Offline packagers can provide `FLUXER_DEEP_FILTER_MODEL=/absolute/path/to/DeepFilterNet3_onnx.tar.gz`.
  No model download or network access is needed during calls.
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
headset/speaker echo behavior needs hardware testing. Streamr
speaker enrollment/isolation, other effects, and global OS push-to-talk hooks
are not ported. Streamr is deferred in [PLAN.md](../../PLAN.md) and needs its separate
service, model assets, and enrollment UI.
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
python3 fluxer_native/scripts/test_output_routing.py --screen-watch
python3 fluxer_native/scripts/test_shared_audio_capture.py
```

Unit tests cover actual SDK track PCM for input gain/PTT, arbitrary callback
chunking, stereo, 44.1-to-48 kHz conversion, microphone meter isolation, missing
callbacks, and the gain-adjusted echo reference. Mixer tests cover clipping,
voice-activation release, participant mute/deafen, and bounded latency. Local
LiveKit/virtual-microphone tests exercise physical capture through publication
and reception without sending personal microphone audio to an external server.
The output runner creates two isolated sinks and checks both monitor PCM and
actual graph links. Its media mode decodes synthetic LiveKit audio through the
real mixer/output, verifies voice gain/mute, mixed voice/share audio, independent
stream mute/gain and stop-watch silence, switches sinks during the call, and checks teardown silence. No default devices or other application
routes are changed; all temporary sinks are removed after the run.
The screen-watch mode exercises the actual MediaEngine with synthetic video,
an isolated output and a muted microphone. Both plain and encrypted calls verify
explicit watch, local renderer release with a full event queue, rejection of late
frames/end events across same-SID rewatch, fresh pixels after rewatch, republish and
watch intent before publication metadata. Local audio/video detach before SDK
unsubscribe work; watch generations fence renderers from prior watch sessions. Wayland/macOS source discovery returns
an inert system-picker descriptor; only the explicit picker button starts
capture/consent. X11/Windows retain enumerated native source selection.

Video delivery uses separate lifecycle events and a newest-frame mailbox per
track. Stop clears that mailbox before SDK signaling; stale watch generations
cannot refill it. Native libyuv converts decoded frames directly to BGRA, so GPUI
can take ownership of the bytes without a pixel swap. Screen publication explicitly
disables simulcast and selects MaintainFramerate, following the existing desktop's
screen-share policy; the SDK default would introduce a 3 FPS low layer. Focused
simulcast reception explicitly requests high quality. The full-HD screen fixture
uses the same publication helper, checks sustained 30 FPS transport, and verifies
freshness after a deliberately paused consumer in plain and encrypted calls.

Shared-audio capture follows the selected system/application route after current
screen capture has produced its first frame. Outgoing gain/mute is independent of
microphone mute/deafen, matching `AudioPublishOptions.ts` and the existing media
adapter. Application discovery waits for PipeWire core synchronization, node info
and client identity properties before returning its inventory. The video and audio
publications share the `screen_share` stream group.

The shared-audio runner starts a sibling `pacat` process with generated 440/880 Hz
stereo tones in isolated virtual sinks. Its real selected-application capture is
published through MediaEngine and decoded by a second client in plain and E2EE
calls. It verifies tone presence, gain, explicit mute/unmute, continued transmission
while the sender is microphone-muted/deafened, and completed unpublication/capture
node cleanup. Distinct left/right tones must remain separated after decoding;
matching a two-channel PCM format alone does not pass. The SDK compatibility patch
mirrors the existing client's forceStereo metadata and per-track SDP behavior.
It does not record the system mix or personal applications.
