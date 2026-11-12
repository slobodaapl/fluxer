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
```

Normal launch signs in to a selected instance. Demo mode uses clearly labeled
local fixtures; it does not connect calls or transmit messages. The current
client keeps session credentials in memory.

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

## Compatibility notes

The client retains existing Fluxer HTTP, gateway, and LiveKit services. Native
system audio shares source with `fluxer_desktop/native/linux-audio-capture`,
avoiding a separate, divergent feedback-routing implementation. The GPUI
0.2.2 dependency graph includes an obsolete xattr dependency; `compat/xattr`
forwards that API to maintained xattr to support current Linux libc.
