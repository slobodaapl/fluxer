# Fluxer native client

Rust and GPUI desktop client under development on `yolo/gpui`.
The MVP target is community text chat, voice calls, screen sharing, and audio
management. Complete feature parity remains the migration goal. See
[PLAN.md](PLAN.md), the [coverage ledger](parity.json), and [STATUS.md](STATUS.md)
for acceptance criteria and current gaps.

## Build and run

Run from a full checkout of this repository. The native client is an independent
Cargo workspace; its lockfile does not change the server workspace's resolution.

```sh
cargo run --manifest-path fluxer_native/Cargo.toml -p fluxer-desktop
cargo run --manifest-path fluxer_native/Cargo.toml -p fluxer-desktop -- --demo
# Offline landscape/portrait screen viewer, synthetic participants, no capture:
cargo run --manifest-path fluxer_native/Cargo.toml -p fluxer-desktop -- --demo-stream
```

Normal launch restores the saved account on Linux using the OS Secret Service.
New sign-ins save the token, pinned instance, locale and last community/channel
as one encrypted credential record. Closing the app keeps the session; Sign out
removes it. A locked or unavailable vault is reported explicitly. Demo mode uses
local fixtures and does not restore or overwrite a saved account. macOS/Windows
credential backends and the account switcher remain migration work.

Keep Cargo build output on disk rather than a RAM-backed `/tmp`. Cargo's default
`fluxer_native/target` location already does this; custom `CARGO_TARGET_DIR`
settings should also point to a disk-backed directory.

Linux needs a working X11 or Wayland session, Vulkan-capable GPU/driver,
fontconfig/freetype, xkbcommon (including X11 support), ALSA, PipeWire, GLib,
OpenSSL, and development headers. Build tooling includes Rust, pkg-config,
CMake, and Clang 21 or later for the current LiveKit WebRTC artifact. The first
build downloads GPUI/Rust dependencies and LiveKit's native WebRTC library.
The client uses the OS portal for Wayland screen selection. Application/system
audio capture currently uses the existing Linux PipeWire engine.

Chat shows discovered-instance avatars, inline image attachments, embed cards,
and forwarded snapshots. Audio/video attachments open externally. Message bodies
support mouse and keyboard selection, Unicode word selection, and Ctrl/Cmd+C.
The emoji picker searches the existing Fluxer catalog, including skin tones and
community emoji; arrow keys select a result, Enter chooses it, and Escape closes
the picker. Replies have a cancellable preview and author-mention control;
forwarding has a searchable destination picker. Reply intent and unsent drafts
survive channel switching, and editing preserves the unsent composer draft.

Full markdown and inline custom-emoji rendering, attachment uploads, native
audio/video playback, selection across messages, and selection autoscroll remain
open. Embed text is currently plain and is not selectable. These features do not
establish complete messaging parity with the existing client.

macOS and Windows require their native Rust toolchain and GPUI/LiveKit platform
prerequisites; their builds and capture behavior have not yet been validated by
this migration. Full cross-platform parity remains a release gate.

## Validation

```sh
fluxer_native/scripts/verify.sh
```

This audits source feature inventory, formatting, strict Clippy, and workspace
tests. The HTTP/WebSocket tests bind ephemeral localhost ports. The media tests
include synthetic PCM processing without opening microphones or screen capture.

To test actual encoded media transport, start a disposable local LiveKit dev
server bound to loopback, then run:

```sh
python3 fluxer_native/scripts/test_media_loopback.py
```

The script creates short-lived tokens for a unique test room and exchanges
synthetic audio/video between two native clients. It never captures personal
media. The test server command is documented in that script. This validates
transport separately from OS permissions and hardware; it does not replace
two-account Fluxer call and screen-sharing acceptance tests.

With that local server running, Linux can also test the complete physical-input
API through DSP/controls, publication and remote PCM decoding:

```sh
python3 fluxer_native/scripts/test_microphone_capture.py
```

This creates an isolated virtual microphone fed by a generated tone, checks the
meter plus decoded gain/PTT/mute behavior, then removes its temporary audio
modules. It leaves default devices and existing application routes unchanged.
The Rust test refuses default/hardware input IDs and non-loopback servers.

## Video performance checks

The offline animated viewer exercises the actual GPUI image upload/render path
with synthetic 1920×1080 BGRA frames at 30 FPS:

```sh
FLUXER_VIDEO_PERF=1 FLUXER_VIDEO_PERF_STOP_MS=20000 cargo run --manifest-path fluxer_native/Cargo.toml -p fluxer-desktop -- --demo-stream-animated
```

Focus the fixture window. It prints frame acceptance/render rates, maximum poll
and view-build times, tracked image counts, and scheduled Stop action/redraw
latency. `rendered_fps` counts scenes that reached GPUI presentation, not monitor
scanout. `stop_render_received_ms` measures when Render observes cleared pixels;
`stop_next_frame_callback_ms` includes the next platform frame's scheduling delay.
This fixture stays offline and does not restore an account or open capture.
`FLUXER_VIDEO_PERF=1` can also report metadata-only counters during normal use.
Development builds optimize the frame/rendering crates while retaining debug
symbols and assertions.

With the disposable local server running, validate actual encoded transport:

```sh
python3 fluxer_native/scripts/test_output_routing.py --screen-watch
```

Both plain and encrypted full-HD calls use the production publication policy,
measure sustained decoded frame rate, pause the consumer to check newest-frame
freshness, and exercise stop/rewatch without hardware capture or personal media.

## Compatibility notes

The client retains existing Fluxer HTTP, gateway, and LiveKit services. Native
system audio shares source with `fluxer_desktop/native/linux-audio-capture`,
avoiding a separate, divergent feedback-routing implementation. The GPUI
0.2.2 dependency graph includes an obsolete xattr dependency; `compat/xattr`
forwards that API to maintained xattr to support current Linux libc.
The pinned LiveKit Rust dependency also has a workspace-local
[compatibility patch](compat/livekit/FLUXER_PATCHES.md) for RTC receiver reuse,
normal decryption-hook restoration and ordered subscription signaling.
