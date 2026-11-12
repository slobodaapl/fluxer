# Implementation status

This is a migration in progress. Full feature parity and the MVP release gate
are not yet complete. The ledger's statuses apply to complete feature areas,
not individual types or controls.

| Area | Evidence | Remaining validation/work |
|---|---|---|
| Inventory/plan | 47 feature areas, 25 native addons, 22 settings tabs audited | Detailed scenario coverage continues through release gates |
| Core state | 23 tests: wire IDs, permissions, READY projection, message deltas, reads, typing and voice controls | Full gateway event catalog, permission edge cases, all feature state |
| HTTP/gateway | 38 tests: source-backed discovery/Origin/headers/IP/browser handoff, bidirectional desktop zstd codec, heartbeat/reconnect/resume and voice signaling | Native passkey/SSO/CAPTCHA workflows, uploads and broader REST coverage |
| Native media | Builds; synthetic two-client LiveKit audio/video exchange passed; capture lifecycle tests | Authenticated Fluxer calls, OS capture/consent, hardware changes, encryption interoperability |
| Linux shared audio | Existing PipeWire routing reused; 11 graph/identity regression tests pass | Actual application/system-audio sharing with a second Fluxer client |
| GPUI | Executable builds/links; Wayland demo visually verified; 7 multiline/Unicode/IME tests pass | Interactive keyboard/accessibility review, rich messages, attachments, complete UI parity |
| Audio management | Real input/output gain, participant mixer/mute, PTT/VAD, DSP, device routing and local meters; 20 media tests pass; hardware enumerated | Acoustic AEC validation, automatic hotplug fallback, global OS PTT, advanced voice effects |
| Client orchestration | 23 tests: real password/token/IP/browser sign-in, READY bootstrap, selected-guild subscription, stream viewer keys and capture-state races | Actual two-account Fluxer login/chat/call/share validation |

`scripts/verify.sh` passes source audit, formatting, strict workspace Clippy and
122 tests. The opt-in LiveKit test verifies explicit microphone/screen
subscription and screen unwatch while fresh microphone audio continues. The
current run times out restoring the microphone immediately after deafen: the
SDK's local unsubscribe event precedes wire completion, and its asynchronous
subscription requests are not ordered. This is an open SDK compatibility gap;
the complete synthetic test is not currently passing. The native lockfile is
committed separately from the server workspace lockfile.

An explicit desktop-device regression reproduced `stop_recording failed` during
repeated teardown after a muted call. All stop paths now check LiveKit's recording
initialized state, matching its device-switch implementation. Both idle setup
and actual muted local join/leave/rejoin pass on this desktop with the fix. Run
`scripts/test_media_loopback.py --audio-lifecycle` against the disposable local
server to repeat these two hardware checks; they are opt-in in the normal gate.

The shared build cache was moved from RAM-backed `/tmp` into the ignored
`fluxer_native/target/gpui-shared-cache` directory. The old `/tmp` path is only a
symlink for existing Cargo cache references; new build output consumes disk.

The implemented native UI includes community/channel navigation, paginated text
history, multiline compose/edit/delete/reactions/copy, draft retention on failed
sends, loaded-message search, password/TOTP, IP email authorization and browser
sign-in, voice join/leave/mute/deafen,
audio device/DSP/gain/PTT/VAD/participant controls, microphone test/meters, incoming
video with explicit Watch/Stop controls, and display/window plus
system/application audio sharing. Screen selection
includes frame-rate and resolution choices. Live behavior remains subject to the
acceptance checks above; a button or transport test does not establish parity.

On this Umbriel Wayland session, a newly created window can remain in a hidden
tab/workspace and retain its first frame until focused. Focusing the explicitly
identified `app.fluxer.Native` window delivered redraws and displayed the populated
demo. This was checked separately from the existing Electron client.

No complete feature area is marked verified solely because it has a model,
button, demo, or passing unit test. Remaining broader parity work is in PLAN.md.

Client/server interactions follow the existing desktop source. See
[the transport contract audit](crates/protocol/CONTRACT.md) for exact source
references and remaining gaps. Public production discovery declares webapp
`https://fluxer.com`; a credential-free empty login probe with that Origin passed
the Origin gate and returned `CAPTCHA_REQUIRED` rather than `INVALID_API_ORIGIN`.
This verifies the Origin policy, not an authenticated account session. The native
browser sign-in button uses the existing desktop CODE handoff so browser-based
verification can complete through the normal web client. No real credentials
were submitted during that probe.
