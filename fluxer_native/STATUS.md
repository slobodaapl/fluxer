# Implementation status

This is a migration in progress. Full feature parity and the MVP release gate
are not yet complete. The ledger's statuses apply to complete feature areas,
not individual types or controls.

| Area | Evidence | Remaining validation/work |
|---|---|---|
| Inventory/plan | 47 feature areas, 25 native addons, 22 settings tabs audited | Detailed scenario coverage continues through release gates |
| Core state | 28 tests: wire IDs, permissions, READY projection, message deltas, reads, typing, voice controls and gateway participant rosters | Full gateway event catalog, permission edge cases, all feature state |
| HTTP/gateway | 43 tests: source-backed discovery/Origin/headers/IP/browser handoff, bidirectional desktop zstd codec, heartbeat/reconnect/resume and voice signaling | Native passkey/SSO/CAPTCHA workflows, uploads and broader REST coverage |
| Native media | Builds; synthetic two-client LiveKit audio/video exchange passed; capture lifecycle tests | Authenticated Fluxer calls, OS capture/consent, hardware changes, encryption interoperability |
| Linux audio | Existing shared-audio routing reused; exact PipeWire microphone sources; 13 graph/identity/PCM regression tests pass | Actual application/system-audio sharing with a second Fluxer client |
| GPUI | Executable builds/links; Wayland demo visually verified; 7 multiline/Unicode/IME tests pass | Interactive keyboard/accessibility review, rich messages, attachments, complete UI parity |
| Audio management | Physical input → WebRTC DSP → gain/PTT/VAD → native publication; participant mixer/mute, output gain, device routing and meters; 27 media tests pass | Acoustic AEC validation, automatic hotplug fallback, global OS PTT, advanced voice effects |
| Session persistence | 7 codec/isolation tests plus real encrypted OS-vault synthetic CRUD passed; tokens/instance/locale/selection restore across cold startup | Linux Secret Service backend implemented; macOS/Windows backends, multi-account switching and full preference migration remain open |
| Client orchestration | 30 tests: real password/token/IP/browser sign-in, READY bootstrap, selected-guild subscription, stream viewer keys and capture-state races | Actual two-account Fluxer login/chat/call/share validation |

`scripts/verify.sh` passes source audit, formatting, strict workspace Clippy and
155 tests. The opt-in LiveKit test verifies explicit microphone/screen
subscription and screen unwatch while fresh microphone audio continues. The
current run times out restoring the microphone immediately after deafen: the
SDK's local unsubscribe event precedes wire completion, and its asynchronous
subscription requests are not ordered. This is an open SDK compatibility gap;
the complete synthetic test is not currently passing. The native lockfile is
committed separately from the server workspace lockfile.

The microphone path now captures physical PCM directly. The previous WebRTC
Device-track tap never delivered PCM because its source implements `AddSink` as
a no-op. Linux uses exact PipeWire source names, preserving GoXLR Chat Mic versus
Stream Mix/Sampler. Standalone WebRTC resampling/DSP precedes application controls
and the published native track. Explicit test stop and disconnect wait for device
release; mute/deafen release input capture. See the media README for hardware
validation commands and remaining DSP limits.

The pinned WebRTC archive also exports weak PipeWire forwarding symbols. Native
media initializes its documented library loader before calling pipewire-rs;
otherwise the linker resolves `pw_init` to an uninitialized forwarding pointer.
A regression exercises those symbols in a LiveKit-linked binary without opening
devices. Idle setup and real muted local join/leave/rejoin pass on this desktop.
The exact GoXLR Chat Mic source now produces real nonzero meter levels. The
isolated virtual-microphone test also passes capture → publication → a second
client's decoded PCM, including proportional gain, PTT, mute/unmute and meter
teardown. Its generated tone uses a local disposable server and temporary audio
modules; no personal microphone audio is sent by this integration test. These
checks do not replace authenticated interoperability with the existing client.

The shared build cache was moved from RAM-backed `/tmp` into the ignored
`fluxer_native/target/gpui-shared-cache` directory. The old `/tmp` path is only a
symlink for existing Cargo cache references; new build output consumes disk.

The implemented native UI includes community/channel navigation, paginated text
history, multiline compose/edit/delete/reactions/copy, draft retention on failed
sends, loaded-message search, password/TOTP, IP email authorization and browser
sign-in, voice join/leave/mute/deafen, gateway-backed participant rows beneath
voice channels and a participant pane above voice-channel chat with state badges
and exact-identity speaking indicators,
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

The earlier memory-only session behavior has been replaced by secure Linux
session persistence. Password/TOTP, email authorization and browser handoff
successes all save the account; cold startup refreshes its pinned authority and
requests full READY. The restart fixture forbids redundant current-user/list
requests and proves restoration without microphone initialization. Logout clears
the vault before best-effort server notification; transient failures retain the
session, and definitive invalidation disables automatic restoration. An older
memory-only build cannot contribute a saved token, so one final sign-in is needed
after this update.
