# Implementation status

This is a migration in progress. Full feature parity and the MVP release gate
are not yet complete. The ledger's statuses apply to complete feature areas,
not individual types or controls.

| Area | Evidence | Remaining validation/work |
|---|---|---|
| Inventory/plan | 47 feature areas, 25 native addons, 22 settings tabs audited | Detailed scenario coverage continues through release gates |
| Core state | 16 tests: wire IDs, permissions, READY projection, message deltas, reads, typing and voice controls | Full gateway event catalog, permission edge cases, all feature state |
| HTTP/gateway | 13 tests, including real localhost auth/message/heartbeat/reconnect/resume and voice signaling | Auth beyond password/TOTP, uploads and broader REST workflows |
| Native media | Builds; synthetic two-client LiveKit audio/video exchange passed; capture lifecycle tests | Authenticated Fluxer calls, OS capture/consent, hardware changes, encryption interoperability |
| Linux shared audio | Existing PipeWire routing reused; 11 graph/identity regression tests pass | Actual application/system-audio sharing with a second Fluxer client |
| GPUI | Executable builds/links; Wayland demo visually verified; 7 multiline/Unicode/IME tests pass | Interactive keyboard/accessibility review, rich messages, attachments, complete UI parity |
| Audio management | Real input/output gain, participant mixer/mute, PTT/VAD, DSP, device routing and local meters; 10 media tests pass; hardware enumerated | Acoustic AEC validation, automatic hotplug fallback, global OS PTT, advanced voice effects |
| Client orchestration | Real auth/text/gateway/call/media integration; 7 tests, including public controller delivery, localhost text workflow and capture-state races | Actual two-account Fluxer login/chat/call/share validation |

`scripts/verify.sh` passes source audit, formatting, strict workspace Clippy and
64 tests. The opt-in LiveKit synthetic transport test also passes. The native
lockfile is committed separately from the server workspace lockfile.

The implemented native UI includes community/channel navigation, paginated text
history, multiline compose/edit/delete/reactions/copy, draft retention on failed
sends, loaded-message search, password/TOTP sign-in, voice join/leave/mute/deafen,
audio device/DSP/gain/PTT/VAD/participant controls, microphone test/meters, incoming
video, and display/window plus system/application audio sharing. Screen selection
includes frame-rate and resolution choices. Live behavior remains subject to the
acceptance checks above; a button or transport test does not establish parity.

On this Umbriel Wayland session, a newly created window can remain in a hidden
tab/workspace and retain its first frame until focused. Focusing the explicitly
identified `app.fluxer.Native` window delivered redraws and displayed the populated
demo. This was checked separately from the existing Electron client.

No complete feature area is marked verified solely because it has a model,
button, demo, or passing unit test. Remaining broader parity work is in PLAN.md.
