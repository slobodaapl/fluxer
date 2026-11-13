# Implementation status

This is a migration in progress. Full feature parity and the MVP release gate
are not yet complete. The ledger's statuses apply to complete feature areas,
not individual types or controls.

Pending real-account, device and usability checks are tracked in
[REVIEW.md](REVIEW.md), separately from automated verification.

| Area | Evidence | Remaining validation/work |
|---|---|---|
| Inventory/plan | 47 feature areas, 25 native addons, 22 settings tabs audited | Detailed scenario coverage continues through release gates |
| Core state | 45 tests: wire IDs, permissions, READY projection, message deltas, reads, typing, forwarding destinations, voice controls and gateway participant rosters | Full gateway event catalog, permission edge cases, all feature state |
| HTTP/gateway | 71 tests: source-backed discovery/Origin/headers/IP/browser handoff, message references, bidirectional desktop zstd codec, heartbeat/reconnect/resume and voice signaling | Native passkey/SSO/CAPTCHA workflows, authenticated upload interoperability and broader REST coverage |
| Native media | Builds; synthetic two-client LiveKit audio/video exchange passed; capture lifecycle tests | Authenticated Fluxer calls, OS capture/consent, hardware changes, encryption interoperability |
| Linux audio | Existing shared-audio routing reused; exact PipeWire microphone sources and logical output sinks; 18 graph/identity/PCM regression tests pass | Actual application/system-audio sharing with a second Fluxer client |
| GPUI | Executable builds/links; Wayland demo visually verified; 102 input/emoji/attachment/search/media/viewer/message/avatar presentation tests pass; focused viewer visually checked at minimum size and with landscape/portrait sources | Interactive keyboard/accessibility review, full markdown/custom emoji rendering, authenticated upload interoperability, native audio/video playback, complete UI parity |
| Audio management | Physical input → WebRTC DSP → gain/PTT/VAD → native publication; participant mixer/mute, output gain, exact logical sink routing and meters; 42 media tests pass | Acoustic AEC validation, automatic hotplug fallback, global OS PTT, advanced voice effects |
| Session persistence | 7 codec/isolation tests plus real encrypted OS-vault synthetic CRUD passed; tokens/instance/locale/selection restore across cold startup | Linux Secret Service backend implemented; macOS/Windows backends, multi-account switching and full preference migration remain open |
| Client orchestration | 83 tests: real password/token/IP/browser sign-in, READY bootstrap, selected-guild subscription, gateway Live discovery, viewer keys, capture-state races, replies and forwarding | Actual two-account Fluxer login/chat/call/share validation |

`scripts/verify.sh` passes source audit, formatting, strict workspace Clippy and
368 tests. The complete opt-in LiveKit audio/video loopback now passes, including
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
sends, server-backed search and pins, historical navigation, password/TOTP, IP email authorization and browser
sign-in, voice join/leave/mute/deafen, gateway-backed participant rows beneath
voice channels and a participant pane above voice-channel chat with state badges
and exact-identity speaking indicators,
audio device/DSP/gain/PTT/VAD/participant controls, microphone test/meters, incoming
video with adaptive tiles or a selected main stage, stream/member filmstrip, optional chat, fullscreen,
independent stream audio controls and immediate Stop Watching, and display/window plus
system/application audio sharing. Screen selection
includes frame-rate and resolution choices. Live behavior remains subject to the
acceptance checks above; a button or transport test does not establish parity.

Chat now renders discovered-instance avatar images with initials on failure,
inline image attachments, file/audio/video cards, embeds, reply previews and
flat forwarded snapshots (text and media). The GPUI image loader uses a separate
credential-free HTTP client with four workers, pooled connections, bounded
response bytes and timeouts. Spoiler/sensitive images remain hidden; expired
attachments are unavailable. Native audio/video playback is still pending.

Message bodies wrap at the window width and support mouse drag, Unicode word
double-click, line triple-click, keyboard selection, and Ctrl/Cmd+C without
becoming editable. The full bundled emoji catalog includes aliases, categories,
skin variants and community emoji; the picker supports keyboard selection and
reaction chips toggle the exact emoji. Replies retain per-channel intent and
allow author mentions to be toggled. Forwarding searches writable destinations
and asks the server to construct the snapshot. Editing preserves the unsent
draft, while failed sends retain text and reply intent. Initial history opens
at the latest messages; incoming messages follow only while near the bottom.

Wayland offline checks exercised multiline Unicode copy, double-click copy,
emoji search and keyboard insertion, reply/draft channel switching, edit/cancel
draft recovery, and forwarding via the keyboard destination picker. A loopback
HTTP fixture verifies actual reply/forward request bodies and failed-reply draft
retention without capture. Another fixture serves and decodes a PNG through the
image worker. Live authenticated rich-chat interoperability remains unverified.
Full markdown/custom emoji rendering, authenticated upload interoperability, native audio/video playback,
cross-message selection, selection autoscroll and selectable embed text remain
open; the parity ledger stays partial.

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

The screen viewer follows the existing Fluxer focus layout: the selected stream
fills the available stage with aspect-preserving fit, secondary streams and members
sit in a compact filmstrip, and chat is optional. Expand hides navigation and
fullscreen uses the whole window; Escape exits fullscreen/expanded mode without
leaving voice. Stop Watching clears local pixels and intent immediately, separately
from Leave Voice. Per-stream gain/mute controls operate independently of the same
participant's microphone; master gain and deafen still apply to both.

Watch generations fence queued frames/end events across stop/rewatch, including
reuse of the same track SID. Lifecycle events outrank coalesced video, and local
video/audio detach before SDK signaling. Voice reconnection preserves viewer intent
and focus while releasing old pixels. Actual plain/E2EE fixtures filled the event
queue, verified immediate local stop (under 1 ms in these local runs), rejected late
frames, and resumed fresh pixels on rewatch. This measures local teardown, not a
remote network latency guarantee. Decoded LiveKit-to-PipeWire tests additionally
verified mixed voice/share audio, independent mute/gain, stop-watch silence and sink
switching. The offline `--demo-stream` fixture exercises the viewer without account
access, calls, capture or network media. Authenticated real-client acceptance is
still required.

A subsequent real-call report exposed performance gaps in the earlier stop-only
fixture. Under continuous 1080p video, the presentation poll could repeatedly
consume newly arriving frames before yielding to input, and its debug RGBA-to-BGRA
pixel loop measured about 68 ms per frame on this desktop. GPUI also requires
explicit sprite-atlas image eviction; dropping the Rust image alone does not release
its texture. Sustained presentation and texture lifetime checks now complement the
local teardown tests. An animated 1080p30 GPUI run rendered about 30 FPS, kept
one or two image IDs in flight, and dropped them all on stop. Its poll handler
stayed below 0.1 ms in these local runs. These counters measure GPUI rendering,
not physical display scanout. Targeted development optimization preserves debug symbols and
assertions while optimizing the video/rendering crates.

Publishing must also specify the existing desktop's screen-share layering policy.
The SDK's default simulcast policy adds a 3 FPS screen layer for larger sources;
`ScreenShareOptions.ts` explicitly disables simulcast. Tiny synthetic sources do not
exercise that branch, so validation must include realistic source dimensions and
measure decoded frame rate using the same publication policy as production.
The full-HD plain/encrypted fixtures sustained 29.2–29.3 decoded FPS at selected
30 FPS. After a 350 ms consumer pause the next frame was current or one source
frame behind; storage stayed bounded to one frame. Presentation takes one atomic
frame snapshot per tick, so continuous video cannot extend a UI poll indefinitely.
Control events travel independently of video, and BGRA conversion happens in the
native decoder path rather than a per-pixel loop on the UI thread.

Screen-share start now carries an explicit optional audio selection. The Linux
sharing pane defaults Include audio on, places the source choice before the
system picker, and exposes active stream-audio mute/status in the voice panel.
Audio waits for the current capture's first frame; cancelled/stopped starts revoke
pending audio. Requested-audio or gateway startup failures roll back both sources,
while old lifecycle events cannot stop a live replacement. Outgoing stream audio
is independent of microphone mute/deafen. Application discovery now synchronizes
PipeWire registry, node and client properties before returning sources.
Actual selected-application capture through publication and remote PCM decoding
passes in plain/E2EE local calls, including gain, mute/unmute, sender deafen and
capture-node/unpublication cleanup. The SDK's missing forceStereo metadata and
track-specific Opus negotiation are now implemented from the bundled JS source;
strict decoded left/right tone separation passes in both plain and encrypted calls.
Four focused SDK signaling/SDP checks pass, including mono microphone scoping and
unmodified video sections. The runner uses only generated tones and its own virtual
sinks; it does not capture personal applications or the system mix.

The reaction/composer emoji picker now opens as a bounded popover with centered
square icon cells, a category rail, tone controls, search and an inspector. The old
eager grid built all 3,970 base/tone choices and took 5.7–6.6 seconds to appear on
this desktop. The virtualized grid built 56 visible cells in the measured run;
cold first paint took 91 ms and external click-to-visible took 112 ms. Cached
reopening painted in 23–29 ms (78 ms in an external capture check). These are
local fixture measurements, not a cross-platform latency guarantee.

Unicode tiles use the existing Fluxer sprite sheets, cropped lazily with a bounded
256-image cache and explicit GPU eviction. This avoids inconsistent native font
fallback; mixed-tone variants without bundled artwork preserve their exact Unicode
fallback and reaction values. Search spans categories and ranks exact aliases ahead
of keyword matches. Clearing search restores the selected category, and incoming
custom-emoji availability changes refresh the open catalog without invalidating it
for unrelated voice/state updates. Tests cover search/tone/custom wire values,
source sprite indices and dimensions, crop colors and bounded image ownership.

The attachment composer now stages files per channel through the native file
picker, external file drops and image clipboard paste. Compact cards show bounded
image previews, sizes, spoilers, upload progress, removal, cancellation and retry.
Preparation/decoding uses four background slots; streaming HTTP does not block
gateway/media processing. Attachment-only sends and replies share the existing
message request builder. Failed or cancelled requests retain files and drafts;
completion preserves text, reply changes and files added after Send. Account reset
fences pending results and releases private clipboard temporary files.

Upload requests follow AttachmentUploadPlan, ChunkedAttachmentUploader and
MessageQueue: one background plan per staging batch, independent file cancellation,
four concurrent multipart parts, source retry/fallback boundaries, completion and
credential-free storage PUTs. Instance limits, account traits and guild features
resolve the local size/count policy. Synced protobuf privacy preferences determine
whether preupload is permitted, including the original account-age default. Partial
gateway settings updates retain omitted fields. Changed disk files fail before
upload; retries retain the original nonce for an unchanged message.

Twelve loopback upload fixtures verify bytes, authenticated API headers, storage
credential isolation, batch plans, concurrency, retries, cancellation and fallback.
Three public-controller fixtures verify failed-send retention, channel responsiveness
during held requests, attachment-only sending, new-draft/file preservation and
changed-file rejection. Ten worker/preparation regressions cover privacy, account
reset, cancellation, limits, previews and temporary-file cleanup. Wayland offline
checks visually verified compact cards, native picker opening, generated-file
preparation, actual image paste and spoiler preview hiding. The --demo-attachments
fixture uses the real staging worker without uploading or restoring an account.
Authenticated two-client upload acceptance and remaining platform checks are open;
these fixtures do not claim full messaging parity.

Server message search now uses the existing POST /search/messages request shape,
including context, supported filter syntax, scopes, sorting, one-based pages and
source indexing polling. Queries keep exact snowflake strings. The native parser
is checked against 78 filter cases and 1,728 date cases executed from the existing
TypeScript parser with its lockfile-pinned Luxon dependency. Host locale week data
is checked against actual Intl behavior. Polling refreshes relative date bounds
without cloning message history. Three uncommon V8 legacy date spellings remain
explicitly documented in crates/protocol/CONTRACT.md; autocomplete, saved search
history, quick switcher and mature-content consent UI remain broader parity work.

Search/pin previews live outside the timeline. A result jump fetches its surrounding
messages and centers/highlights the target; older/newer paging maintains a contiguous
visible window capped at 200 IDs. Return to latest fetches a fresh window and scrolls
to its tail after layout, preserving drafts, reply intent and staged attachments.
The search pane remains usable for paging/sorting after a cross-channel jump.
Pins use source timestamp pagination, shared PIN_MESSAGES permission policy,
confirmation, gateway invalidation and guarded pin acknowledgement. Rejected
mutations retain their displayed item; live HTTP success does not duplicate the
refresh already driven by the gateway.

Search, indexing, pins, history and acknowledgements run outside gateway/media
processing with account/request/context fences. Merely fetching history or displaying
a preview does not mark messages read. Automatic read acknowledgement requires a
focused native window, the live timeline tail, ready history, no blocking modal and
no manual-unread fence. Server read-state versions and mention counts remain
authoritative; stale responses cannot undo newer or manual state. Offline checks
verified five-page search, narrow-pane wrapping, centered old-message jumps,
return-to-latest, pin pagination and cancel/confirm unpin. Real-account/two-client
acceptance remains pending in REVIEW.md; fixtures use isolated peers and generated
messages, never production mutations.

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

The 2026-11-12 UI refinement pass replaces persistent message/search action banks
with contextual toolbars and menus; compacts headers, composer and attachment
cards; consolidates the voice/account dock; and replaces audio adjustment button
banks with labeled sliders and switches. Artwork matches Fluxer's pinned
Phosphor icon set. Keyboard access includes editable fields/controls, roving
timeline focus, and two-choice confirmation traps with opening-key-release
protection. The API/client/media worker layers are unchanged by this pass.
See [UI_UX.md](UI_UX.md) for the source-backed audit and remaining UI priorities,
and [REVIEW.md](REVIEW.md) for U1–U5 and pending real-account acceptance.

The next conversation presentation pass reuses the existing Rust Markdown parser
(`packages/markdown_parser/rust`) directly. Source grouping rules include the
seven-minute group-start window, local date dividers and reply/type/webhook/flag
boundaries; cozy rows use 40px avatars, 22px lines and compact continuations.
Rich readonly text shares measurement, wrapping, hit testing and copy indices;
formatting, links, mentions, inline/code-block backgrounds, spoiler conceal/reveal
and bounded custom-emoji images retain selection. Hidden spoilers are omitted
from selection copy; the explicit Copy Message action retains raw source, as in
Fluxer. Source Fluxer Sans/Mono faces are bundled with provenance and OFL notices.
Replies use contextual previews and existing jump navigation; image attachments
use source size bounds without a repeated file card. Public discovery metadata
is forwarded to the UI for source link trust/routing without extra requests.

Full Markdown/UI parity remains incomplete: heading sizes, nested list/table
layout, syntax highlighting, timestamp widgets, jumbo/animated emoji treatment,
profile/role popovers, full density/locale/hour-cycle preference migration and
trust-preference editing still need work. See message_markup.rs scope notes and
REVIEW.md M1–M4; live account acceptance remains pending.

Community/voice imagery now uses a shared credential-free image renderer with
source-derived media URLs. The rail shows community icons; the voice channel
sidebar, participant pane and stream filmstrip show profile pictures and retain
speaking rings. Community avatars take precedence over global avatars; explicit
AVATAR_UNSET selects the existing client's bundled default avatar. Both colored
and neutral source defaults are embedded, and missing/failed community icons
use source-style acronyms. The account dock/chat share the global-avatar helper.
Call images use the joined voice community even while browsing another one.
Static images are implemented; animation and system-bot branding are not claimed.
See REVIEW.md P1/P2 for live checks. No REST/gateway interaction changed.


## Stream layouts and toolbar follow-up (2026-11-12)

Multiple incoming screen subscriptions now have an explicit Tile all/Main stage
choice. The custom GPUI grid measures the actual stage after the surrounding
layout, considers each video's aspect ratio and preserves uncropped Contain
rendering. Layout/focus changes retain watch intent and decoded frames; per-tile
mute/stop target their own stream. Reconnect preserves the chosen layout. Tiled
mode removes duplicate watched thumbnails and compacts the participant strip.

Offline two/four-stream and main-stage captures are in
`target/qa/stream-layouts/`, alongside a hover-toolbar capture and the verification
log. The toolbar uses solid contrasting backing, border and shadow and paints
after the message body, preventing later text/media from covering its surface.
The animated tiled renderer sustained approximately 30 accepted/rendered FPS
after startup with synthetic 1080p30 frames. Scheduled Stop reached cleared Render
in 6.4 ms after handling (45.3 ms action scheduling delay; next-frame callback
241.3 ms, which includes platform scheduling). These are local presentation
measurements, not network latency, physical scanout or real-call acceptance.
Review items U7/V3 retain the subjective and two-client checks.
