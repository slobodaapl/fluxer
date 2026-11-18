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
# Offline animated GIFs (append --demo-gifs-overflow for scrolling checks):
cargo run --manifest-path fluxer_native/Cargo.toml -p fluxer-desktop -- --demo-gifs
# Offline image attachments, embeds and forwarded images:
cargo run --manifest-path fluxer_native/Cargo.toml -p fluxer-desktop -- --demo-images
# Offline reaction picker (synthetic messages, no saved account):
FLUXER_EMOJI_PERF=1 cargo run --manifest-path fluxer_native/Cargo.toml -p fluxer-desktop -- --demo-emoji
# Offline landscape/portrait screen viewer, synthetic participants, no capture:
cargo run --manifest-path fluxer_native/Cargo.toml -p fluxer-desktop -- --demo-stream
# Four simultaneous synthetic streams:
cargo run --manifest-path fluxer_native/Cargo.toml -p fluxer-desktop -- --demo-stream --demo-stream-many
# Same offline streams with one selected on the main stage:
cargo run --manifest-path fluxer_native/Cargo.toml -p fluxer-desktop -- --demo-stream --demo-stream-main
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
OpenSSL, and development headers. Build tooling includes Rust 1.97 or later (for
the embedded Wasmtime runtime), pkg-config,
CMake, and Clang 21 or later for the current LiveKit WebRTC artifact. The first
build downloads GPUI/Rust dependencies and LiveKit's native WebRTC library.
The client uses the OS portal for Wayland screen selection. Application/system
audio capture currently uses the existing Linux PipeWire engine.

Voice & audio settings offer Off, WebRTC, noise gate, Speex, RNNoise, GTCRN, and
DeepFilterNet3 noise suppression. The models are bundled and run locally; selection applies to
the microphone test and calls and is saved per account. DFN initializes in the
background while WebRTC keeps the microphone running. The first build fetches a
hash-verified 8 MB DFN model when its Git LFS archive is not hydrated; offline
builders can set `FLUXER_DEEP_FILTER_MODEL` to that archive. Streamr speaker
isolation is deferred in [PLAN.md](PLAN.md).

Chat shows discovered-instance avatars, inline image attachments, embed cards,
and forwarded snapshots. Audio/video attachments have native playback controls and an in-app full view. Message bodies
support mouse and keyboard selection, Unicode word selection, and Ctrl/Cmd+C.
The emoji picker searches the existing Fluxer catalog, including skin tones and
community emoji in a compact popover using Fluxer’s bundled emoji artwork. Only visible rows are built; results and
search metadata are cached, and a tone selector replaces the expanded skin-tone
list. Search spans categories, while clearing it restores the selected category.
Emoji/images and category/tone controls use centered square cells. Arrow keys
select a result, Enter chooses it, and Escape or an outside click closes the
picker. `FLUXER_EMOJI_PERF=1` logs first paint/preparation times and visible-cell
counts without message content. Replies have a cancellable preview and author-mention control;
forwarding has a searchable destination picker. Reply intent and unsent drafts
survive channel switching, and editing preserves the unsent composer draft.

Full media-viewer parity, block-formatting parity, selection across messages,
and selection autoscroll remain open. Attachment upload interoperability
still needs real-client acceptance. Embed text is currently plain and is not selectable. These features do not
establish complete messaging parity with the existing client.

macOS and Windows require their native Rust toolchain and GPUI/LiveKit platform
prerequisites; their builds and capture behavior have not yet been validated by
this migration. Full cross-platform parity remains a release gate.

Visible chat GIFs and animated-image embeds play automatically, with clipped
images paused. The account GIF autoplay setting is respected; disabled autoplay
allows pointer-hover playback.

Spoiler/sensitive image previews preload in the background and remain blurred
until clicked. The first click reveals inline; another opens the viewer.

Chat images open in an in-app viewer. Use Left/Right to browse the message's
images, and Escape or the backdrop to close. **Open original** is available
explicitly and follows the normal external-link warning policy.

## Linux desktop launcher and tray

Install Native's launcher and taskbar icon for the current user after building:

```sh
python3 fluxer_native/scripts/install_desktop_entry.py
```

The installer points at `target/debug/fluxer-desktop` by default; `--binary` selects
another installed build. It installs `app.fluxer.Native` alongside classic Fluxer.
Desktop settings control the system tray icon, close-to-tray, startup hiding and
colored artwork. Close keeps the existing session/call alive only with a working
tray host; tray Quit or Ctrl+Q exits. The hidden window restores if the host is
lost. Reopening the launcher raises the existing native instance. Tray support is
currently Linux-only; minimize-to-tray is not exposed on Wayland. Details and
manual acceptance are recorded in [TRAY_2026_11_18.md](TRAY_2026_11_18.md).

## Screen-share audio

On Linux, the sharing pane enables **Include audio** by default and lets you
choose system audio or one playback application before opening the system
screen/window picker. Video and the selected audio start together after screen
capture succeeds. Turning Include audio off starts an explicit video-only share.
The voice panel reports whether audio is shared and offers a stream-audio mute
button. Microphone mute and deafen do not mute the outgoing stream; use its own
mute control. Stop sharing releases both video and audio.

If requested audio cannot start, sharing stops with an error instead of silently
falling back to video only. Application refresh uses a synchronized PipeWire
inventory and preserves the selected source. Audio sharing on other platforms
remains migration work.

The incoming stream viewer supports watching several streams at once. **Tile
all** fits the watched streams to the available stage, preserving each video's
aspect ratio. **Main stage** enlarges the selected stream and keeps the others
in the filmstrip. Click a tile to select its audio/stop controls, or double-click
to put it on the main stage. Switching layouts keeps existing subscriptions.

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

To verify actual shared-audio capture, with the disposable server running:

```sh
python3 fluxer_native/scripts/test_shared_audio_capture.py
```

This uses generated tones in runner-owned virtual sinks and a selected synthetic
application. It validates native capture through remote PCM decoding and removes
its capture nodes, playback process and sinks afterwards.

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
GPUI also has a local [Wayland repaint and clipboard patch](compat/gpui/FLUXER_PATCHES.md)
so a delayed compositor callback cannot indefinitely block changed content.


Messages and attachments appear locally as soon as Enter is pressed, freeing the
composer for the next draft. Upload/sending progress and failures stay with that
message; failed sends can be retried or removed. Chat has a narrow draggable
scrollbar, and community selection uses a thin border. Ctrl/Cmd+Z undoes composer
edits; Ctrl/Cmd+Shift+Z or Ctrl+Y redoes them. Picker, composer, messages and
reactions share Fluxer's emoji artwork. Pin announcements link to their original
message and the channel's pins instead of displaying a reply preview.


`cargo run --manifest-path fluxer_native/Cargo.toml -p fluxer-desktop -- --demo-outbox`
opens persistent local-only sending, uploading and failed-message examples for
layout review. These rows do not perform uploads or send account messages.


Native file playback on Linux uses FFmpeg libraries (libavformat, libavcodec,
libavutil, libswscale and libswresample) discovered through pkg-config at build
time, plus the existing PipeWire output engine. Install their development headers
to enable the backend. The regular application does not launch ffmpeg processes.
Other platforms and missing-library builds show an unavailable-player error and
retain Open original. Audio and video start on Play; MP4-backed GIFV loops muted
when visible and allowed by GIF autoplay. Player volume/mute, seeking and full
view are independent of the voice call. Closing/unmounting its message releases
the player; normal playback continues when the message merely scrolls out of view.

Media loads through credential-free HTTP into private temporary disk-cache files,
with progress and cancellation. This implementation caches a complete file before
opening it, up to 2 GiB per resource; progressive HTTP range playback remains
future work. It does not pass remote URLs or playlists to the decoder. Source
proxy URLs and signed fields are retained. Concealed media preloads paused and
shows a blurred video frame until reveal; expired media is unavailable.

`--demo-media` serves generated silent MP4/GIFV, a click-to-play Opus tone,
concealed video and an expired attachment over loopback. It uses no account or
capture. For automated local presentation checks only,
`FLUXER_MEDIA_DEMO_AUTO_PLAY=1` starts the silent MP4 and
`FLUXER_MEDIA_DEMO_ACTIONS=1` exercises full view/pause/seek/resume/close.
The audio tone never autoplays. Pending acceptance is M9 in REVIEW.md.


Right-click or Shift+F10 on media opens Save as, Copy media link, Open original,
still-image Copy image and message actions. These menus also work inside media
viewers and honor concealed/expired/channel-hidden media policy. Save uses the
system file dialog and background atomic export; clipboard images are exported
as PNG. Standard text inputs expose selection-aware editing menus, including
Undo/Redo. Remaining manual acceptance is tracked in REVIEW.md (M10/U11).


Channel/community/category context menus provide explicit Mark as read and
persisted notification preferences. Message menus offer Mark unread, which holds
automatic acknowledgement while the channel stays open. Notification menus have
source mute durations, unmute, All messages/Only mentions/Nothing and inherited
channel defaults. These settings are independent of voice playback mute. Thread
membership notification controls and full notification delivery remain migration
work. Cross-client acceptance is tracked in REVIEW.md (U12).


Click a user's actual avatar/name (or a user mention) to open an in-app profile
popup. Chat/search/pins and voice identity surfaces share the same profile reads,
with community-specific data where supplied. Cards support Escape/outside close,
keyboard activation, bounded placement and Retry; surrounding row/gutter space is
not a profile trigger. Full profile actions and rich biography interaction remain
migration work. Review checks are P3 in REVIEW.md.
