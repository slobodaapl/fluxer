# GPUI 0.2.2 Wayland repaint recovery

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
