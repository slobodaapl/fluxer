# GPUI 0.2.2 Wayland repaint and clipboard fixes

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
