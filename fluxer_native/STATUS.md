# Implementation status

This is a migration in progress. Full feature parity and the MVP release gate
are not yet complete. The ledger's statuses apply to complete feature areas,
not individual types or controls.

Pending real-account, device and usability checks are tracked in
[REVIEW.md](REVIEW.md), separately from automated verification.

## Chat rendering push integrated — 2026-11-16

The Terra/Sol chat worktrees and root fixes are merged into `yolo/gpui`: measured
headings/lists, real tables, Arborium highlighting and code Copy, scoped mention
cards and keyboard reveal, jumbo/animated emoji and localized timestamps. READY
reconnects preserve drafts/UI; real account/instance boundaries reset private
state and restore authoritative drafts. The full local workspace gate passes
**674 tests, zero failures, 14 ignored**, audit, formatting and strict Clippy.

The installed live app is **375faae7b-local**, with session restoration and unchanged
audio preferences checked under GDB/Vulkan validation. See
[CHAT_PARITY_2026_11_16.md](CHAT_PARITY_2026_11_16.md) for evidence, limitations and
[REVIEW.md](REVIEW.md) for pending acceptance. The local gate/build includes the
preexisting uncommitted DSP work, which remains separate from the chat commits.
Full migration parity, manual acceptance, the deferred aggregate media budget and
multi-stream review remain open.

## Communication push integrated — 2026-11-15

All delivered work is on `yolo/gpui`, including the final status-input/modal and
Inbox menu corrections through `eebd62de5`. `scripts/verify.sh` passes the audit,
formatting, strict Clippy and **570 tests (13 ignored)**. A version-tagged native
executable is prepared from the final integration; its window title identifies
its build. Full feature parity and physical/live acceptance remain incomplete.

The push adds preferred/effective audio-device recovery and account-scoped
settings; virtual community members/presence and own status; source completion
and slowmode; DMs/Friends/relationship actions; notifications, quick switching and
remote mentions inbox; image zoom/pan; progressive range playback with buffering
and immediate controls; and remembered audio/video volume/mute choices.

Owned GUI checks cover the corrected composer, 600-member list and tight profile
hit targets, Friends, status editor typing/save/focus/cancellation and shortcut
isolation, settings navigation, Inbox filter menu/dismissal, quick-switcher
presentation, image transforms and generated native video controls. Unit and
private transport/codec fixtures cover the stated server, lifecycle and HTTP
contracts. They do not replace real Fluxer account, OS-device, IME, accessibility
or subjective acceptance. See [REVIEW.md](REVIEW.md) and the complete delivery
record in [PUSH_2026_11_15.md](PUSH_2026_11_15.md).

Remaining major scope includes full call recovery and capture republish, timed/
emoji/idle presence, full profile modal and broader profile interactions, complete
regional mature gating, additional codecs/platforms and rate/PiP/subtitle/HDR
media controls. Multi-stream review remains deferred by the user.

## Earlier baseline and historical evidence

| Area | Evidence | Remaining validation/work |
|---|---|---|
| Inventory/plan | 47 feature areas, 25 native addons, 22 settings tabs audited | Detailed scenario coverage continues through release gates |
| Core state | 48 tests: wire IDs, permissions, READY projection, message deltas, reads, typing, forwarding destinations, voice controls and gateway participant rosters | Full gateway event catalog, permission edge cases, all feature state |
| HTTP/gateway | 71 tests: source-backed discovery/Origin/headers/IP/browser handoff, message references, bidirectional desktop zstd codec, heartbeat/reconnect/resume and voice signaling | Native passkey/SSO/CAPTCHA workflows, authenticated upload interoperability and broader REST coverage |
| Native media | Builds; synthetic two-client LiveKit audio/video exchange passed; capture lifecycle tests | Authenticated Fluxer calls, OS capture/consent, hardware changes, encryption interoperability |
| Linux audio | Existing shared-audio routing reused; exact PipeWire microphone sources and logical output sinks; 18 graph/identity/PCM regression tests pass | Actual application/system-audio sharing with a second Fluxer client |
| GPUI | Executable builds/links; Wayland demo visually verified; 212 input/emoji/attachment/search/media/viewer/message/avatar/Wayland/context-menu tests pass; focused viewer visually checked at minimum size and with landscape/portrait sources | Interactive keyboard/accessibility review, full markdown/custom emoji rendering, authenticated upload interoperability, complete media-viewer and UI parity |
| Audio management | Physical input → WebRTC DSP → gain/PTT/VAD → native publication; participant mixer/mute, output gain, exact logical sink routing and meters; 42 media tests pass | Acoustic AEC validation, automatic hotplug fallback, global OS PTT, advanced voice effects |
| Session persistence | 7 codec/isolation tests plus real encrypted OS-vault synthetic CRUD passed; tokens/instance/locale/selection restore across cold startup | Linux Secret Service backend implemented; macOS/Windows backends, multi-account switching and full preference migration remain open |
| Client orchestration | 83 tests: real password/token/IP/browser sign-in, READY bootstrap, selected-guild subscription, gateway Live discovery, viewer keys, capture-state races, replies and forwarding | Actual two-account Fluxer login/chat/call/share validation |

`scripts/verify.sh` passes source audit, formatting, strict workspace Clippy and
502 tests. The complete opt-in LiveKit audio/video loopback now passes, including
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
attachments are unavailable. Native audio/video attachments now use the in-app player described below.

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
Full markdown/custom emoji rendering, authenticated upload interoperability, full media-viewer parity,
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


## In-app image viewer (2026-11-12)

Clicking an attachment image, embedded image or flat forwarded image now opens
an in-app modal from chat and search/pin previews. Attachment galleries retain
the clicked image's position, browse visible images from that message and wrap
with Left/Right like MediaViewerModal. Images fit the viewport without cropping
or enlarging small originals. Escape, the close control and the backdrop dismiss
the viewer and restore prior focus; Tab remains within its controls. The viewer
blocks background send/read acknowledgements and search/settings shortcuts.

Open original is explicit and uses the existing trusted-domain warning policy;
its warning appears above the viewer. The image proxy remains credential-free
and bounded. Full-view proxy URLs remove only the source transform parameters;
original links retain signatures. Embedded pictures target their media URL,
while article titles keep their article link. Concealed previews now preload blurred and stay outside clear-image galleries
until revealed; expired media and suppressed embeds remain unavailable. Media spoiler reveal/preferences, video/audio viewing, pan/zoom,
download/copy/favorite controls and full media-viewer parity remain incomplete.

The full gate passes 376 tests, including eight new gallery/privacy regressions.
`--demo-images` uses loopback-generated images and exposes optional
`--demo-images-large` and `--demo-image-error` cases. The review queue keeps
real-account and subjective acceptance separate from local fixture checks.


## Visible GIF playback (2026-11-12)

Chat attachment/embedded GIFs and flagged animated images now receive stable
GPUI image identities, enabling its existing GIF/animated-WebP decoder clock.
Full-view image modal entries use the same animated source policy. Requests
follow MediaProxyUtils: remove static encoding/quality choices, preserve proxy
fit dimensions and signed fields, and set animated=true. The source animated
thumbnail rule for IMAGE embeds is retained; original browser links stay intact.

A custom image element defers image loading and frame layout until its actual
allocated bounds intersect both the viewport and ancestor scroll mask. Fully
clipped images schedule no animation; chat images pause behind the image modal.
The account's gif_auto_play setting is read from existing authoritative settings;
disabled autoplay falls back to pointer hover. Concealed images and spoiler-linked embeds follow the blurred preload policy
below; expired media and suppressed embeds remain unavailable.

Generated GIFs validate real frame differences and proxy flag requirements.
Visibility geometry, autoplay, actual codec timing and gallery URL regressions
bring the gate to 386 tests. Actual GIFV video decoding, reduced-motion/save-data
and keyboard-focus preference behavior remain separate parity work; a GIF image
fallback inside a GIFV embed does not establish MP4 playback support. Live
acceptance is tracked as M6 in REVIEW.md.


## Concealed previews preload blurred (2026-11-12)

Visible spoiler/sensitive image and GIF previews now load automatically. A
custom asset shares the normal GPUI decoded-image cache and generates a separate,
strongly blurred first frame on a background executor. The concealed renderer
never receives the clear image as a fallback and never runs the GIF clock.
First click reveals inline; subsequent activation opens the ordinary viewer.
Unrevealed entries are excluded from galleries, while expired/suppressed media
keeps its unavailable state. Spoiler-linked embeds hide their information until
reveal, alongside the blurred selected media. Message/snapshot/media keys isolate
reveals and channel/account transitions clear them.

This supersedes the earlier no-fetch treatment and is the intended preview
policy. No new API routes, authenticated media headers or preferences writes were
added. The source client uses ThumbHash for placeholders; the native path uses a
bounded first-frame downsample and Gaussian blur of the loaded image instead.
Four blur regressions and three reveal-scope regressions bring the full gate to
393 tests. Runtime evidence and remaining review are tracked under M6/M7.


Runtime fixtures prove visible GIF frame changes, complete clip/idle behavior and
preloaded blurred GIF cache reuse. The selected concealed GIF was decoded before
a programmed reveal and generated no additional HTTP request when revealed.
Only generated media was captured. Artifact paths and the remaining real-account,
hover/keyboard and subjective checks are recorded in REVIEW.md.


## Messaging interaction fixes (2026-11-13)

Text and attachment sends now use immediate GPUI local echoes and a worker-owned
outbox keyed by an individual submission ID/nonce. Enter clears the captured
composer/reply/files immediately, permits subsequent sends, and moves upload
progress to the local row. Failed rows retain text and immutable file ownership
for explicit retry/removal; retries reuse their nonce and stale asynchronous
callbacks cannot mutate replacement attempts. Gateway and REST acceptance are
reconciled without duplicate echoes or clearing a later draft. No server routes
or upload contracts were invented for this UI change.

The timeline has a subtle draggable scrollbar with track paging and wheel support.
Community selection uses a thin border without a purple tile fill. Unicode emoji
across the picker, composer, messages, reactions and previews share Fluxer art;
editing and clipboard/IME offsets remain the original Unicode bytes. Composer
undo/redo groups adjacent edits and preserves selection, with explicit history
boundaries for programmatic draft resets. Pin system messages render source type 6
as an author/action sentence with links to the original and channel pins; search
results share that presentation. Failed/deleted-target jumps release centering
state, and denied history navigation is rejected before entering it.

The full 412-test verification gate passes, including actual localhost send/upload
fixtures. User-facing checks remain U8/M8/E2/U9/N5 in REVIEW.md. Multi-stream
review is deferred at the user's request.


## Native file audio/video playback (2026-11-13)

Playable attachments and declared audio/video/GIFV embed resources now render
native players in chat, flat forwards and search/pins. Audio and ordinary video
start explicitly; GIFV follows viewport, focus and existing GIF autoplay gates,
loops and stays muted. Normal audio/video continue while scrolled away; unmount,
channel/account changes and retirement stop the owned player. Separate player
instances isolate chat and browser-surface visibility and controls. Full view
shares the current position/decoder with its inline instance and restores focus
on close. Open original uses the existing link-warning policy. Search cards retain
space for playback controls instead of clipping them to an image-preview height.

A worker owns FFmpeg demux/decoders, bounded PCM/video queues, seek generations and
PipeWire output. Video is converted directly to BGRA, including quarter-turn
phone-video rotation and sample-aspect correction; the GPUI mailbox retains the
latest frame, retires old atlas images and avoids redundant copies. Controls and
teardown do not join/block the UI. Decoder allocation and thread limits apply
before opening codecs. The PipeWire forwarding library is initialized before
opening file output, as required when linked with LiveKit. Native FFmpeg receives
only a local cached file with an ordinary-container/protocol whitelist.

Four HTTP workers spool complete resources to private disk-cache files, bounded
to 2 GiB each, with progress and cancellation. Signed query tokens survive source
transforms, and account credentials are not copied to media requests. Concealed
video preloads paused and paints a separately blurred frame; audio remains silent
until explicit play/reveal. Superseded seeks are skipped before native I/O. Delayed
audio, EOF display duration, paused/end seeks, retry and output lifecycle have
regressions. The full gate passes 440 tests (13 opt-in tests ignored); desktop 162
and media 51 ordinary tests pass. The opt-in 1080p30 decoder/mailbox test measured
30.28 FPS landscape and 30.29 rotated, with clock ratios 0.999/0.998; this is not
GUI scanout. An isolated PipeWire monitor verified actual decoded PCM, volume,
mute, pause, seek, sink switching and stop using generated audio only.

The generated app fixture proves native frame presentation, changing GIFV pixels,
blurred video preloading, and programmatically dispatched full-view/pause/seek/
resume/close actions through the real player handlers. No expired-media request
or ordinary audio autoplay occurred. Artifact paths and live acceptance are M9
in REVIEW.md. Remaining work includes progressive range/buffered startup, preference
persistence/motion migration, playback rate/PiP/subtitles/HDR, arbitrary-angle or
reflection transforms, more codec/platform coverage and native-media/shared-audio
interoperability. Full feature parity remains incomplete.

### Playback startup follow-up (requested 2026-11-13)

Evaluate progressive playback of remote audio/video against the current complete
download before playback. The goal is to start playing while bytes arrive and
support seeking without first downloading the entire file. This is HTTP media
buffering/range playback, separate from live voice and screen sharing.

Compare the existing Fluxer client's request and buffering behavior before
choosing an implementation. Cover MP4 metadata at the beginning/end of a file,
WebM/audio, servers with and without byte-range support, signed/expiring URLs,
slow or interrupted connections, cancellation and bounded disk/memory use.
Preserve credential isolation, spoiler concealment and immediate stop. Keep
complete-download fallback where progressive playback is unavailable. Record
time to first frame/audio, seek latency and buffering behavior under a throttled
connection before deciding whether it should become the default.

## Wayland redraw recovery (2026-11-13)

A workspace-local GPUI 0.2.2 compatibility patch connects dirty notifications
and refreshes to a coalesced recovery wake. Regular compositor callbacks preserve
normal pacing and cancel obsolete work. A delayed callback gets a single 100 ms
fallback that paints changed content and registers a replacement callback; object
identity fences late callbacks so they cannot start duplicate pacing chains.
Configure/output/scale changes wake immediately after the current dispatch, with
a full scene refresh even when the logical extent stays the same. Every configure
is acknowledged, and primary display selection excludes outputs already left.
Closed windows invalidate pending work before their native surface is destroyed.

An offline Wayland proxy reproduced starvation in the original build: holding one
surface callback froze changing GIFV frames until release. With the patch, frames
continued before release; the replacement callback arrived about 118 ms after
the held callback in the measured run. Holding all callbacks allowed only one
replacement attempt, with rendering resuming after release. Four physical-output
moves, two fullscreen transitions and close immediately after a transition passed
on the patched fixture. This is controlled offline evidence; U6 retains normal
startup, user workflow and broader compositor/GPU review in REVIEW.md.

The full source/format/strict workspace Clippy gate passes 448 tests (13 opt-in
tests ignored), including eight regressions against the production scheduling
policy. The private-socket proxy smoke checks descriptor transfer/cleanup, partial
writes, protocol fragmentation and late callback release. See
`compat/gpui/FLUXER_PATCHES.md`; no account traffic or personal media was used.

## Voice and navigation context menus (2026-11-13)

Compact native context menus now cover watched/available streams, voice cards,
sidebar participants, call filmstrip members, the connected account dock,
channels/headers and community icons. They expose existing local audio and viewer
commands plus mention/copy/navigation actions. Shift+F10 opens the same menus.
Toggle/slider adjustments remain open; regular actions close and restore focus.
Target and permission checks run again when an action activates, and deferred
callbacks are fenced when a menu is replaced or its target leaves.

User and remote-connection volume/mute are stored separately and composed for
the existing native mixer; stream audio is independent. Exact SDK identities
prevent cross-user prefix matches. New/repeated remote join events reapply the
stored scopes, and voice-channel/account transitions reset the appropriate
connection/account state. Only the explicitly local dock controls this device's
microphone/deafen/disconnect; an unresolved gateway row never proves ownership.
Channel links use discovery's webapp origin. No new server moderation,
notification settings or acknowledgement payloads are inferred.

The gate passes 457 tests, formatting, source audit and strict workspace Clippy.
Offline captures checked all menu types, actual Shift+F10 opening from a focused
community icon, keyboard mute and 0–200% slider behavior, shortcut blocking and
Escape. Synthetic user-volume changes affected both of Alex's devices while
retaining the phone's independent 30% gain; your other device stayed separate.
These checks used no call, capture, audio playback or account writes. Live audio,
pointer use and broader accessibility remain U10 in REVIEW.md. Further menu
priorities and classic-client/Discord references are in UI_UX.md.


Media now has a shared right-click/Shift+F10 menu across attachments, media
embeds, forwarded snapshots, search/pins and image/native-player viewers. It
combines Save as, Copy media link, Open original, still-image clipboard copy and
existing message actions. It resolves the clicked asset against current message,
permission, expiry, concealment and channel download policy; asynchronous results
are fenced across account changes and asset replacement. Transfers and image
conversion run off the UI thread, with cancellation and bounded concurrency.
Saving uses the system file dialog and an atomic sibling-file replacement; image
copy exports PNG bytes through the patched Wayland clipboard. Background completion
requires an explicit Copy action when the app lacks focus. The standard text
inputs also offer selection-aware Undo/Redo, Cut/Copy/Paste, Delete and Select all;
read-only and secret fields restrict their actions. See M10/U11 in REVIEW.md for
verified scope and remaining acceptance, including native Save dialog completion.


## Explicit reads and notification settings (2026-11-13)

Channel/community menus now offer Mark as read when applicable. Category headings
have context menus and acknowledge their immediate readable children. Message
menus offer Mark unread for cached messages; manual unread suppresses automatic
viewport acknowledgement until an explicit read or leaving the channel. Explicit
read commands use the classic batched acknowledgement endpoint and its legacy
fallback, rather than reusing the monotonic viewport-ack command. Permission and
source mention rules, server ACK versions, failures and pending ACK cancellation
are checked at the worker boundary.

Channel, category, community and private-conversation notification menus support
All messages, Only mentions and Nothing, with category/community inheritance on
channel overrides. Mute supports the classic duration presets and indefinite
mute; unmute clears the direct override, while inherited mute remains in effect.
Sidebar rows show a small muted-notifications icon. READY and gateway settings
updates preserve unrelated fields; PATCH sends the source override-map shape and
retains existing overrides/collapse state. Large-community defaults and expired
mute configurations follow source rules. These controls are server notification
preferences, separate from voice playback mute/volume.

The 493-test gate includes real private-loopback POST/PATCH contract fixtures,
record-keyed READY settings, mention filtering, category read, inherited settings
and offline menu checks. Thread notification writes remain deferred because they
use membership flags. Thread read filtering currently uses projected membership;
separate membership gateway changes, full forum unread behavior, notification
suppression/mobile settings UI and complete native notification delivery remain
migration work. Cross-client persistence and subjective acceptance are U12 in
REVIEW.md. Existing manual-review items remain pending.


## Profile popups (2026-11-13)

Click an author's avatar/name in chat, search/pins, a voice participant's image/name,
the call filmstrip identity or your account image/name to open an anchored profile
card. User mentions also open profiles in their originating channel's community
context. Only the image and name hit areas activate it; surrounding gutters, row
padding and gaps do not. Keyboard Enter/Space works on the same identity targets.
The opaque card follows source dimensions, flips/clamps inside the viewport and
scrolls in short windows. It shows avatar/banner, display name/username, pronouns,
plain biography, account/community join dates and role chips when available, plus
real Copy ID and Retry actions. Escape/outside click dismiss and restore focus.

Profiles use the source GET query and an asynchronous worker, deduplicate concurrent
requests, cache each user/community scope for 60 seconds and cap retained profiles
at 128. Expired entries are pruned on subsequent loads/insertion; account/READY and
membership updates/removals invalidate caches. Partial user/member updates preserve missing
fields while explicit nulls clear them. Limited profiles do not refill stripped
biography/pronouns; webhooks show supplied identity without fetching an account.
Global/community avatar and banner flags, accents and default colors follow the
source display rules. Large profile payloads are boxed to keep ordinary worker
events small. The 502-test gate includes six backend lifecycle/wire cases and three
popup/appearance policy tests. Review is P3 in REVIEW.md.

Full profile modal, rich/selectable biography interactions, connected-account and
badge presentation, relationship/DM/moderation actions, profile animation and live
refresh of an already-open completed profile remain migration work. Source data
is retained for later parity work; these cards do not claim full profile coverage.
