# GPUI 0.2.2 native client fixes

This directory contains the published crates.io GPUI 0.2.2 source with the
changes below. Upstream copyright headers and Apache-2.0 license are retained.
The workspace uses a local Cargo patch; registry caches remain untouched.

* `src/platform.rs`, `src/window.rs`: allow a platform to supply a nonblocking
  dirty-window wake. Entity notifications and full-window refreshes request it.
  Other platforms retain their existing display links through the default hook.
* `src/platform/linux/wayland/redraw.rs`: coalesce wakes and cancel obsolete
  work when a regular frame arrives. Dirty content gets one 100 ms recovery
  deadline if the compositor delays its frame callback. Configure, output and
  scale transitions request an immediate deferred repaint. A suspended surface
  gets at most one timer-driven recovery until a new callback arrives; this is
  not a permanent render/refresh timer. Closing cancels and fences queued work.
* `src/platform/linux/wayland/window.rs`: a recovery presents the dirty scene
  and registers one replacement frame callback. Remember its object identity so
  a delayed old callback cannot start another pacing chain. Refresh GPUI bounds
  on output changes and same-size configure changes. Acknowledge every configure
  instead of discarding a resize while waiting for a compositor callback. Select
  the primary display from current surface outputs, never a removed output.
* `src/platform/linux/wayland/client.rs`: pass frame callback identity through
  dispatch so obsolete callbacks can be ignored.
* `src/platform/linux/wayland.rs`: declare the scheduling policy module.

The existing client could remain dirty indefinitely: notification/refresh only
set dirty state, while Wayland rendering relied on the previous surface callback
to request the next frame. A delayed callback therefore stalled asynchronous UI
updates and could leave a resized drawable without a fresh frame.

The desktop test gate includes the actual dependency scheduling policy by path.
It covers coalescing, normal pacing, stale deadlines, bounded hidden recovery,
first configure, close cancellation and output selection. The test-only proxy in
`scripts/qa/wayland_redraw_proxy.py` can hold one old callback while forwarding
replacement callbacks, then release the old callback to check fencing. Its
private-socket smoke covers message fragmentation and descriptor forwarding.
Offline before/after fixture captures are recorded in `REVIEW.md`; these establish
callback-starvation recovery, not acceptance on every compositor or GPU.


## Image clipboard and bounded transfer

* `clipboard_mime.rs` and `clipboard.rs`: retain actual clipboard entry types,
  advertise their image MIME types and serve matching bytes. Image-only selections
  no longer advertise fabricated text; text and GPUI internal marker behavior is
  retained. Unknown MIME requests receive no payload.
* `serial.rs` and `client.rs`: use the most recently received keyboard or pointer
  input serial when setting the selection, including after asynchronous image
  preparation. Arrival order avoids numeric comparisons across serial wraparound.
* `clipboard_write.rs` and `clipboard.rs`: set clipboard pipes nonblocking while
  preserving existing descriptor flags, and write at most 64 KiB per dispatch.
  Slow readers therefore cannot block rendering. Interrupted writes, zero writes,
  closed readers and source registration failures are handled without panicking.

Six normal desktop tests include these production helper modules by path: MIME
selection/payload, input serial ordering/wraparound, and bounded nonblocking pipe
writes (including a stalled reader and exact delivery). An owned offline media
menu also supplied a generated PNG to an external Wayland clipboard reader; see
M10 in REVIEW.md. These tests supplement the eight repaint scheduling regressions.

## Atlas GPU lifetime and fatal frame waits

The 2026-11-15 live freeze dump records an NVIDIA Xid 13 GPU MMU fault followed
by the main thread waiting forever in Blade's GPU completion loop. Decoder
threads were idle. Immediate atlas destruction during image eviction is a
source-confirmed lifetime defect and a plausible cause; the dump alone does not
prove that this defect caused that particular GPU fault.

* `blade/atlas_lifetime.rs` and `blade/blade_atlas.rs`: eviction removes the key
  but preserves its texture/view and slot. Each texture tracks its own latest
  submission, including uploads/initializations and retained-scene redraws.
  At the next draw, collection requires no keys, no queued upload, no current
  scene reference, and successful nonblocking GPU completion. Zero-key textures
  cannot receive new allocations before collection. Destroy views before images.
* `blade/blade_renderer.rs`: after a failed 10-second frame wait, print the device
  information directly to stderr and exit unsuccessfully rather than retrying
  forever on the event loop. This is fatal handling, not device recovery. Exiting
  avoids unsafe reuse or destruction of resources with unconfirmed completion.
* `blade/blade_context.rs`: `FLUXER_GPU_VALIDATION=1` enables Blade's validation
  option for explicit diagnostic runs. The desktop installs a narrow renderer
  warning/error logger so validation and GPU failures reach launch stderr.

The normal desktop tests include the production lifetime policy by path. They
cover eviction during the latest submission, retained-scene resubmission,
eviction before initial upload, and shared textures with newly queued uploads.
The offline short-WebM scrolling fixture and validation results are recorded in
`../../qa/gif-gpu-lifetime.md`. No registry source files were changed.
