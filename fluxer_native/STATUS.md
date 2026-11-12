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
| Linux audio | Existing shared-audio routing reused; exact PipeWire microphone sources and logical output sinks; 18 graph/identity/PCM regression tests pass | Actual application/system-audio sharing with a second Fluxer client |
| GPUI | Executable builds/links; Wayland demo visually verified; 7 multiline/Unicode/IME tests pass | Interactive keyboard/accessibility review, rich messages, attachments, complete UI parity |
| Audio management | Physical input → WebRTC DSP → gain/PTT/VAD → native publication; participant mixer/mute, output gain, exact logical sink routing and meters; 33 media tests pass | Acoustic AEC validation, automatic hotplug fallback, global OS PTT, advanced voice effects |
| Session persistence | 7 codec/isolation tests plus real encrypted OS-vault synthetic CRUD passed; tokens/instance/locale/selection restore across cold startup | Linux Secret Service backend implemented; macOS/Windows backends, multi-account switching and full preference migration remain open |
| Client orchestration | 33 tests: real password/token/IP/browser sign-in, READY bootstrap, selected-guild subscription, gateway Live discovery, viewer keys and capture-state races | Actual two-account Fluxer login/chat/call/share validation |

`scripts/verify.sh` passes source audit, formatting, strict workspace Clippy and
169 tests. The complete opt-in LiveKit audio/video loopback now passes, including
screen unwatch while fresh microphone audio continues and microphone restoration
after deafen. A pinned workspace-local SDK patch retains the RTC receiver through
explicit unsubscribe, restores it through normal renderer/decryption hooks and
orders SFU subscription requests. See `compat/livekit/FLUXER_PATCHES.md` for the
source-backed delta. The native lockfile is committed separately from the server
workspace lockfile.

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
The user subsequently confirmed successful microphone transmission in a real
Fluxer call. Full screen-sharing and advanced audio interoperability gates
remain open.

Linux output enumeration/playback now uses logical PipeWire sinks instead of
ALSA backends/cards. The final inventory was checked against the supplied real
Fluxer screenshot: Linux default, HDMI/monitor, GoXLR Chat/Game/Music/Sample/System
and JDS DAC, in the same order. Internal Fluxer capture sinks and raw internal
hardware nodes are excluded. Each explicit choice routes to its exact node;
selecting one never changes the OS default or another application's route.
Both direct and decoded LiveKit playback tests pass with two isolated virtual
sinks: monitor PCM and graph links verify destination isolation, gain, participant
mute, live switching and completed teardown. Refresh errors are reported rather
than silently replacing the device list with an empty inventory.

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

Input/output devices, input mode, share frame rate, resolution and shared-audio
source now use one compact dropdown component. Wayland/macOS screen sharing has
a direct button opening the system display/window picker, after quality choices;
viewing or refreshing the sharing pane does not construct a capturer or open a
permission dialog. Platforms with enumerated native sources retain that picker.
Popups are bounded, scrollable and fitted to the window; arrows, Home/End,
type-ahead, Enter, Escape and Tab support keyboard selection. Device refresh
preserves exact IDs, including unavailable selections, without switching inputs.
Sharing starts through an explicit action after choosing its source. Offline
Wayland preview verified layout, Enter selection/dismissal, Escape, Tab and
Home/End navigation; strict checks cover the compiled UI. Ctrl/Cmd+, opens audio
settings. Complete accessibility and platform UI parity remain pending.

Live watch availability follows the joined channel's gateway `self_stream`
roster before SFU publication metadata arrives, matching the existing client.
Participant cards expose Watch/Stop and gateway display names; viewer keys are
sent for explicit watch intent. Temporary SDK metadata loss does not cancel a
gateway-live watch; authoritative stop/leave/move does. Another connection on
the current account cannot replace the local connection binding. Subscription
failures surface visibly, and stopping a local renderer releases its displayed
frame without waiting for the SDK unsubscribe event.
Actual MediaEngine screen-viewing fixtures pass both unencrypted and shared-key
encrypted calls: no automatic video delivery, explicit watch, stop, same-track
rewatch with changed source pixels, replacement publication and pending watch
before metadata. These fixtures use synthetic video and an isolated output,
without microphone or screen capture. Production interoperability remains an
acceptance gate.

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
