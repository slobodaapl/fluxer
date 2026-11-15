# Recovery, settings, people and desktop integration expansion

Requested scope: roadmap items 1/3/4/5, plus external control of in-call actions.
The broader media resource/concurrency budget (#2) remains deferred for design.
Branch: `yolo/gpui`; root reviews and integrates isolated Terra/Sol worktrees.
Preserve the existing uncommitted audio/DSP work (snapshot under ignored
`target/qa/expansion2-2026-11-15/`); do not include it in unrelated feature commits.

## Delivery goals

| Lane | Functional and UX contract | Required evidence |
|---|---|---|
| Call recovery | Retain device/audio/mute/deafen/watch intent through supported gateway/media recovery; process leave/cancel/account changes during connection; report stopped sharing honestly, without silently reopening OS capture | Source voice lifecycle references; stale/retry/cancel/moderation/order tests; physical network/hotplug/suspend checklist |
| Settings | Functional Appearance, Accessibility, Messages & Media, Keybinds pages; source-derived synced versus local preferences, actual rendering/application and durable persistence; compact controls and save/error feedback | Wire-preserving settings transport/protobuf tests, preference/keybind/conflict tests, owned GUI checks |
| Profiles/presence | Full profile modal from existing popup, shared image readiness/cache, usable rich biography links; status emoji/expiry/timed behavior and truthful supported idle projection | Source payload/expiry policy, lifecycle/account timer tests, keyboard/focus and delayed-artwork GUI checks |
| Notifications/unreads | Reliable notification service lifecycle, click navigation/focus, sound/suppression and cleared/stale target handling; preserve manual unread/history behavior and cross-client state | Private session-bus notification service, click/close/restart/clear tests; source read-policy fixtures; real desktop/two-client acceptance remains distinct |
| External call control | Linux session-bus API for explicit mute/deafen, atomic toggles, disconnect and stop-sharing; readable live/effective state and change signals; no injected keys or new capture/join actions | Real private-bus RPC/property/signals tests; actor account/call-generation/deadline/cancel fencing; documented StreamDeck shell commands and future Windows/macOS adapters |

## Integration

Terra: `expand2/call-recovery`, `expand2/profiles-presence`; Sol:
`expand2/settings`. Worktrees are under ignored `target/worktrees/expand2-*`.
Root owns desktop notifications, D-Bus frontend, source/UX review, worktree merges,
combined validation, final build and review records. Build/test commands use
`/tmp/fluxer-push-cargo` with each worktree's explicit manifest so the shared
disk-backed target is serialized and source changes are not confused.

Local server interactions must match the existing Fluxer request builders,
schemas, ordering and omission rules. External IPC is a native extension that
dispatches existing call actions through the same worker actor. Source references
and implemented limits accompany each lane; buttons alone are not delivery.

## Initial external-control design

Linux: well-known session-bus name `app.fluxer.Native`, object
`/app/fluxer/Native/Call`, versioned interface `app.fluxer.Native.Call1`.
Methods: ToggleMute, SetMuted, ToggleDeafen, SetDeafened, Disconnect, StopSharing.
Read state includes call availability/status, local/effective mute/deaf and
screen-sharing state. Changes use standard PropertiesChanged signals. A second
process cannot replace the existing owner. No network listener, credentials,
message access, auto-join, or screen-capture-start interface is introduced.

The worker owns current account/call generation and serializes actions; queued
stale/expired/abandoned actions are rejected. Methods acknowledge the worker
result, not just queue insertion. Platform adapters can later use Windows named
pipes/COM or macOS XPC/Apple Events against that same action/state contract.
Actual OS integration choices and their limitations are recorded with references
in the external-control documentation.

## Acceptance record

All five delivery lanes are integrated on `yolo/gpui`. Implementation expands
these areas; it does not establish complete classic feature parity. The broader
media resource/concurrency budget (#2), including decoder/GPU/player memory
outside the existing compressed-media cache, remains deferred for design.

Delivered:

- Cancellable voice connection/recovery, gateway intent replay, bounded guild
  transport rejoin, Linux suspend detection, and honest stopped-share/reshare
  feedback. Pending-room cancellation closes joined participants even while
  track publication is pending. Credential-first SFU ordering matches the server.
- Account compact display, font size and GIF autoplay preferences with exact
  source wire/protobuf preservation; local reduced motion and configurable native
  shortcuts; separate functional settings sections and save/error handling.
- Full profile modal reusing profile/artwork resources, biography link navigation,
  status emoji and duration dropdowns, expiry retry/backoff, and supported Wayland
  input-idle integration. Biography link warnings retain the profile on cancel.
- Notification transport ownership/teardown, bounded targets and session/channel
  fencing, cross-client accepted ACK/delete dismissal, and corrected role/DM
  mention accounting without changing existing read-ack policy.
- Linux D-Bus call control and `scripts/fluxer-call` for command-capable StreamDeck
  hosts, with authoritative property/change feedback and serialized actor actions.
  Windows/macOS adapters and a dedicated StreamDeck plugin are future work.

The full gate passed 638 tests (14 ignored), inventory audit, formatting and
strict workspace/all-target Clippy, including final GUI corrections. The loopback LiveKit
cancellation test additionally passed against a disposable synthetic server;
D-Bus tests use private buses. No real call joins, account setting writes,
physical capture or user notifications/sounds were used for acceptance.

Owned GUI fixtures under `target/qa/expansion2-2026-11-15/` checked settings
navigation, font preview/apply at 8/16/48px, compact wrapping/inline-author clicks,
shortcut record/rebind/dispatch/cancel/reset and leaving the recording page,
notification settings, full profiles and biography warning/cancel/focus, custom
status text/emoji artwork, mouse and keyboard selection/clearing/save, Escape focus
restoration, and duration dropdowns. GUI review caught and fixed
profile-link reentrant dispatch, warning stacking, nested deferred dropdown
rendering, compact first-block layout, and GPUI mixed-font coalescing. Generated
screenshots are evidence of these specific offline paths, not live acceptance.

Manual review remains required for physical device loss/restoration and selected
GoXLR devices; network and suspend recovery; real Fluxer interoperability while
sharing/watching; timed presence across account/reconnect; OS notification clicks,
sounds and cross-client read state; actual StreamDeck button and live state
feedback. Record outcomes against the review IDs in `REVIEW.md` and per-lane QA
notes; do not infer hardware or full migration acceptance from fixtures.
