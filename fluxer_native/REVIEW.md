# Native client review queue

Keep this file current as work lands. Automated fixtures establish their stated
scope; real-account, device and subjective UI checks stay pending until reviewed.
Some checks were prepared while the user was away on 2026-11-12. Record later
feedback against its specific scenario without treating it as broader acceptance.

## Communication push ready for user review — 2026-11-15

All delivered agent work is integrated on `yolo/gpui`, including final status-modal
and Inbox-menu corrections through `eebd62de5`. The full gate passes **570 tests,
13 ignored**, formatting, strict Clippy and the inventory audit. The native window
shows its exact build version. The implementation remains partial; no new item is
user-accepted merely because automated or offline checks passed.

Owned GUI review checked named mentions/Fluxer emoji, scrolling member rows and
tight avatar/name profile targets, Friends presentation, status text typing/Enter
save/Cancel/Tab/Escape and shortcut isolation, separate notification settings,
Inbox/filter dismissal, switcher, image fit/zoom/pan/reset/close and generated
silent video controls. Captures and logs are in `target/qa/push-2026-11-15/`.
The source-style Inbox menu and final status Submit event path were specifically
recaptured after integration. No real account writes, call joins, personal mic
capture or actual OS notifications/sounds were used to validate these changes.

The remaining acceptance queue is below. Full details and scope limits are in
[PUSH_2026_11_15.md](PUSH_2026_11_15.md).

| ID | Review | Evidence / status |
|---|---|---|
| PUSH-A | Unplug/reconnect selected microphone/output, change defaults, fail/recover capture, repeat join/leave and reconnect; check named fallback, retained volume/DSP/mutes, no unintended capture and honest stopped sharing | Fake-owner/inventory/cancellation and preference/wire tests pass. **Physical devices, network/suspend and cross-client call behavior pending.** |
| PUSH-B | Open Members, scroll a large community, change permissions/channel, resize, inspect roles/presence/custom status, click avatar/name and adjacent gaps; change own status and custom text | 600-row scrolling, profile click boundaries and status editor typing/keyboard/save/close verified offline; live profile relationship reconciliation tested. **Real account, physical IME and subjective review pending.** |
| PUSH-C | Complete @user/@role/#channel/:emoji with keyboard/pointer, edit/delete/undo token, test IME/multiline/code, obey slowmode/exemptions and authoritative rejection | Named atomic projection, shared artwork, insertion/Undo and slowmode captured offline; marker/permission/IME-boundary tests pass. **Live and actual IME review pending.** |
| PUSH-D | Home/Friends Online/All/Pending/Blocked, Message/open-existing DM, accept/decline/cancel/remove/block/unblock, cancel confirmations, retry failures and switch accounts | Exact transport and async fencing tests pass; Friends presentation checked offline. **Real-account/cross-client and populated relationship states pending.** Full profile modal remains open. |
| PUSH-E | Ctrl-K/Ctrl-T switcher keyboard/wheel, Inbox Unreads/Mentions/filter/pagination/error/retry, read preservation, notification suppression/click target/sound and separate settings page | Private sink and source REST/policy tests pass; Inbox/error/filter menu, switcher and settings checked offline. **Real OS/account and broader keyboard/accessibility review pending.** |
| PUSH-F | Image click/fit/zoom/pan/resize/gallery/Escape/focus; start media before full transfer, slow/stall/seek/pause/mute/stop, retain separate audio/video volume and mute across restart/account boundaries | Generated codec/throttled HTTP/frame/cancellation/preferences and offline image/native-video controls checked. **Real CDN/network/output/hardware and sustained performance review pending.** |

Multi-stream acceptance remains deferred at the user's request. Existing review
items below remain applicable. Historical "complete download first" and "zoom/
preferences future work" statements are superseded by the implemented but not yet
accepted push scope above.

## Ready for review

Implementation is on `yolo/gpui`. Attachment work started from build
`bb3f36d37`; use the current native window title to record the build being reviewed.
The latest integration gate passes 570 tests, formatting, strict Clippy and the
feature inventory audit; source-reference fixtures and scope are recorded in
STATUS.md. The holistic UI audit and follow-up priorities are in [UI_UX.md](UI_UX.md).
Full feature parity remains in progress.

| ID | Review | Steps and expected result | Evidence / status |
|---|---|---|---|
| P1 | Community and voice profile images | Check community icons, the in-voice sidebar, voice participant cards and the stream viewer filmstrip against regular Fluxer. Try a community-specific avatar, a global avatar and no avatar; speaking rings and mute/live badges should remain visible. | **Pending real-account review.** Source URL/member precedence/default asset tests and local image fixture checks are recorded below. |
| P2 | Image fallback and updates | Check a community without an icon, a missing image, an avatar cleared in community settings, and an avatar changed while connected. Switch communities while remaining in a call; call images should retain the joined community's avatar context. | **Pending live/hot-update review.** Shared credential-free image loading, source defaults, member AVATAR_UNSET, safe identity resolution and image-error fallbacks implemented. Animated image behavior remains future work. |
| P3 | Profile popup and click targets | Click only the actual avatar or name in chat, search/pins, voice rows/cards, call filmstrip and your account dock. Clicking adjacent gutters, gaps or empty row space should not open it. Check mentions, community nickname/avatar/banner flags, roles, bio, limited/unavailable data, Retry, Escape/outside close and keyboard focus restoration. Try short windows and long profiles. | **Pending real-account and subjective review.** Local profile GET/lifecycle fixtures and owned popup captures pass; click boundaries are checked separately below. Full profile modal/actions and rich biography interactions remain open. |
| M1 | Grouping and conversation density | Compare repeated authors, long conversations, replies and midnight boundaries with Fluxer. Same-author continuations should share a header; a new group starts after seven minutes from its first message. Check hover timestamps and old-message jumps. | **Pending live/subjective review.** Eight source-rule tests, ten source-executed cases and offline date/group rendering checked. Cozy layout implemented; compact/density preference migration remains open. |
| M2 | Selectable formatting and spoilers | Select across bold/italic/underline/strike, links, code, quotes, Unicode and wrapped lines. Ctrl/Cmd+C should copy rendered text and omit concealed spoilers; click a spoiler to reveal it. Explicit Copy Message should retain Markdown source. | **Pending sustained usability review.** Existing composer/input regressions plus rich-run clipping, Unicode range, hidden-target/copy and soft-wrap emoji tests pass. Source fonts and actual inline backgrounds checked offline. |
| M3 | Mentions, emoji and links | Compare known user/role/channel labels and custom emoji with Fluxer. Click a channel link and an old message link; inspect an untrusted external URL and cancel. Check registered emoji renames, failures and spoiler concealment. | **Pending real-account review.** Generated loopback emoji images and resolved labels checked; no external destination opened. Trust/host/scope fixtures pass. User/role popovers, animated/jumbo sizing, timestamps and full preference parity remain incomplete. |
| M4 | Reply and attachment presentation | Follow a reply to its original message; check missing references, forwards, images, spoilers/sensitive attachments and narrow-window wrapping. Images should retain aspect ratio without an extra file card underneath. | **Pending live/layout review.** Reference lookup uses existing cached/wire data and jump commands; media URL/privacy and bounded dimensions regressions pass. |
| M5 | In-app image viewer | Click attachment, embed and forwarded images in chat and search/pins. Check fit at small/large window sizes, arrow-key browsing and wrapping, Escape/backdrop close, focus restoration and Tab confinement. Open original should use the existing link warning policy; cancel should return to the image. | **Pending real-account/subjective review.** Source-backed gallery/URL/privacy regressions pass; offline modal checks are recorded below. Native video/audio is now covered by M9; pan/zoom and broader viewer parity remain future work; media menus are covered by M10. |
| M6 | Visible GIF playback | Check uploaded GIFs, animated-image embeds and forwarded GIFs while scrolling. Open one in the image modal; close it and resume chat. Try GIF autoplay disabled and hover. Concealed/sensitive media should stay hidden. | **Pending real-account/subjective review.** Source animated-proxy URLs, stable GPUI frame IDs, actual clip gating and generated GIF codec/policy fixtures implemented. MP4-backed GIFV playback is implemented under M9; reduced-motion/save-data policy and full preference UI remain migration work. |
| M7 | Preloaded concealed media | Scroll spoiler/sensitive pictures and GIFs into view. Expect a loaded blurred preview; first click reveals inline, second opens the viewer. Check concealment in embeds/forwards, keyboard activation, and gallery navigation before/after reveal. | **Pending real-account/subjective review.** Current policy preloads visible concealed previews and caches their decoded media; only blurred frame zero is painted before reveal. Expired/suppressed media stays unavailable. This supersedes earlier no-fetch behavior. |
| M9 | Native audio/video and GIFV | Play MP4/WebM and audio attachments in chat, forwards and search/pins. Check pause/resume, seek, mute/volume, output device, Open original and full view/Escape. Try rotated phone video, GIFV with autoplay off/on, scroll-out/return, concealed/expired media, failures and channel changes. Test while voice/shared audio is active. | **Pending real-account, physical-device and subjective review.** Real codecs, HTTP contracts, private PipeWire PCM routing and generated app presentation/actions are checked below. Complete-file caching precedes playback; range startup, preference persistence, rate/PiP/subtitles/HDR and broader platform/codec coverage remain open. |
| M10 | Media context menus and export | Right-click or Shift+F10 on attachments, image/video embeds, forwards, search/pins and both viewers. Check Save as, Copy image, Copy media link, Open original and message actions. Concealed/expired/download-hidden media must not expose download actions. Close a menu with Escape before closing its viewer. | **Pending real-account and subjective review.** Source policy, atomic export, image conversion and stale-target tests pass. External Wayland PNG copy is verified. Native Save dialog opens; end-to-end picker completion remains unverified. See evidence below. |
| U11 | Text editing context menus | Select text in the composer and use right-click/Shift+F10 to Undo/Redo, Cut/Copy/Paste, Delete and Select all. Check emoji/Unicode, selection preservation, read-only external URL fields and secret fields. | **Pending sustained usability review.** Unicode/history, selection and restricted-action tests pass; owned offline keyboard checks are recorded below. |
| U12 | Read and notification controls | Use channel/community/category Mark as read, then message Mark unread; it should remain unread while that channel stays open. Leave/return or explicitly mark read. Try each mute duration, unmute, default/explicit notification levels and a muted parent category/community. Confirm persistence and read markers in regular Fluxer after reopening. Check DM scope and permission changes. | **Pending real-account, cross-client and subjective review.** Exact local POST/PATCH contracts, real response-shaped settings decoding, source defaults/mention rules and generated menu actions are verified below. Thread notification writes are intentionally unavailable pending membership transport. |
| U1 | Conversation density and contextual actions | Compare a busy channel with regular Fluxer. Hover a message and use right-click/Shift+F10; React/Reply/More should appear without moving content. Copy selected text, edit/reply/forward, and cancel Delete/Pin confirmations. | **Pending subjective/live review.** Offline SVG, hover/right-click, keyboard menu, confirmation opening-release guard and Tab-wrap/Enter-on-Cancel checked; no real messages or pins changed. |
| U7 | Message toolbar legibility | Hover short and wrapped messages, code and media; check React/Reply/More against the text beneath it. Select nearby text and open reactions. | **Pending subjective review.** Toolbar backing is solid with a stronger border/shadow, and paints after message text/media so content cannot paint through it. Hidden toolbars retain their existing visibility behavior. |
| U2 | Audio settings and account dock | Check the compact name/mic/deafen/settings dock, connected/reconnecting/share/disconnect states, real device dropdowns, volume sliders and DSP switches. Use mouse drag, arrows and Home/End. | **Pending device/live review.** Offline layout, slider drag/fine adjustment/range limits checked without starting microphone capture. Native gain curve still uses its existing linear 0–2 scale. |
| U3 | Search, pins and attachments | Browse results and pins; use corner actions, select text without jumping, and open a result. Add image/file attachments and check overlay controls, spoiler/retry/cancel and uncluttered empty composer. | **Pending subjective/live review.** Offline compact search layout, generated-file cards/spoiler concealment and confirmation cancellation checked. Existing upload/search/controller contracts pass. |
| U4 | Keyboard and small windows | Use Tab/Shift+Tab, timeline Up/Down, menus and confirmation dialogs at 880×600. Try long channel/user/file names and increased text size. Controls should remain reachable, focus visible and dialogs on screen. | **Pending complete accessibility review.** Local keyboard/slider/menu fixtures pass; screen-reader semantics, preference migration and broader focus consistency remain incomplete. |
| U6 | Window moves and scaling | Move a window between monitors while watching video and using chat; try different scale factors and window sizes. | **Fix implemented; pending user review.** Delayed-callback starvation now has an independent, bounded repaint/recovery path, configure acknowledgement and current-output fixes. Before/after protocol-delay evidence is recorded below. Retest normal startup, monitor moves, scaling and hide/restore; other compositor/GPU issues are not ruled out. |
| U5 | Stream controls | Watch and share with regular Fluxer; check the quieter toolbar, chat/fullscreen/volume and Stop Watching/Stop sharing controls. | **Pending real-call review.** Synthetic viewer toolbar/volume layout checked; UI commands and immediate local stop paths retained. Prior synthetic media evidence remains scoped as V1/V2 below. |
| U8 | Chat scrolling and community selection | Drag the chat scrollbar, click its track and scroll with the pointer over it; check a short conversation and a narrow call chat. Select communities and compare the thin selection border. | **Pending user review.** Thumb/drag geometry regressions pass; the purple selection fill is removed. |
| M8 | Immediate text and attachment sending | Press Enter repeatedly, including identical messages and attachment-only sends. The composer clears and a local message appears immediately; upload progress belongs to that row. Force a failure, retry/remove it, and keep typing/staging other files. | **Pending live/feel review.** Per-attempt nonce, ownership, progress, retry/cancel, server/gateway ordering and newer-draft regressions pass. |
| E2 | Consistent emoji artwork | Compare the same Unicode/tone/ZWJ emoji in picker, composer, messages, reactions and reply/search previews. Selection/copy must preserve Unicode. | **Pending visual review.** Shared bundled Fluxer artwork and source-byte/UTF-16 mapping regressions pass. Catalog variants without sprite tiles use the discovered static CDN; OS glyphs are used only outside the catalog. |
| U9 | Composer undo and redo | Type, delete, paste and insert emoji; use Ctrl/Cmd+Z, Ctrl/Cmd+Shift+Z and Ctrl+Y. Check restored selection and channel/edit/send boundaries. | **Pending hands-on review.** Grouped edits, atomic paste/IME/emoji insertion, redo and reversed selection regressions pass. |
| U10 | Voice, stream and navigation context menus | Right-click streams, voice sidebar rows/cards/filmstrip, the connected account dock, channels/headers and community icons. Check mute/0–200% volume, persistent sliders/toggles, independent user/connection/stream audio, other-device safety, watch/stop and copy/navigation actions. Try Shift+F10, Tab/arrows/Home/End, Escape, viewport edges and a participant leaving while its menu is open. | **Pending live audio, pointer and user acceptance.** Offline keyboard/menu presentation and multi-device model checks are recorded below. Remaining menu parity is prioritized in UI_UX.md; multi-stream layout review stays deferred. |
| N5 | Pin system notices and message links | Read a pin notice, follow “a message”, then “all pinned messages”; try an old/deleted target and a search result containing a notice. | **Pending user review.** Type 6 uses the system sentence and existing channel-specific navigation; missing-target centering guards and source reference/permission cases have tests. |
| A1 | Real attachment delivery | In a suitable test conversation, attach a small image and a normal file. Send with text, as a reply, and with no text. View each from regular Fluxer. Filenames, bytes, reply target and spoiler visibility should match. | **Pending real-account check.** Real-byte HTTP contracts and public-controller fixtures pass. |
| A2 | Attachment input and layout | Add several files using the system picker, drag files into chat, and paste a screenshot. Toggle a spoiler and remove a file; switch channels and back. Cards should remain compact and belong to the original channel. | **Pending usability review.** Generated-file preparation, actual Wayland image paste, native picker opening and spoiler preview hiding checked offline. Physical drag/drop and picker selection need review. |
| A3 | Upload failure and cancellation | Cancel a large upload; retry after a temporary connection failure. Files, text and reply intent should remain available. Type the next draft and add another file while a send is pending; completion must preserve the new draft/file. | **Pending real-network check.** Loopback retry/cancellation, retained nonce, draft preservation and changed-file rejection pass. |
| A4 | Upload policy and call responsiveness | Compare upload limits/preupload behavior with the same account in regular Fluxer. Upload while in a call and switch channels/settings. Voice, gateway updates and controls should stay responsive. | **Pending interoperability check.** Source-derived limits/privacy, shared batch planning and nonblocking actor fixtures pass. |
| N1 | Server search and filters | Search for an older message that was never loaded in GPUI. Compare scopes, author/channel/media filters, exact phrases, dates, sorting and pages with regular Fluxer. Open a result in another channel, then keep paging/sorting the retained search. | **Pending real-account check.** Exact HTTP contracts, detached controller flow and source-executed query/date/locale reference cases pass. |
| N2 | Old-history navigation | Jump to an old result or pin. The target should be centered/highlighted with surrounding history. Load older/newer messages and Return to latest; the newest message must be visible. Drafts, replies and staged files must survive. | **Pending sustained usability check.** Contiguous-window controller tests pass; offline five-page search, centered target highlight, preview wrapping, return to the newest message, pin pagination and cancel/confirm unpin checked. |
| N3 | Pins and permissions | In a suitable test conversation, pin/unpin a message and verify the result in regular Fluxer. Check confirmation, timestamp ordering, multiple pages, read-only channels with PIN_MESSAGES, and an account denied pin permission. | **Pending real-account check.** Mutation failures, source timestamp cursors, guarded pin acknowledgement and gateway invalidation fixtures pass. |
| N4 | Unread state across clients | Keep GPUI unfocused while messages arrive. Browse old history and use a search preview without marking unseen latest messages read. Mark a channel unread in regular Fluxer while GPUI is open; it should respect that manual state. Return to the live tail and verify read/mention counts synchronize. | **Pending two-client check.** Versioned server acknowledgements, focus/tail/modal guards and manual-unread fencing have local regression coverage. |
| E1 | Reaction picker feel | Open reactions repeatedly, search a Unicode/custom emoji, change skin tone, and add/remove a reaction. Icons should be centered and opening should feel immediate. | **Pending subjective/live check.** Sprite, search and virtualization fixtures pass; earlier local cold-open measurement was 112 ms. |
| V1 | Shared audio with regular Fluxer | Have another client watch a stream with Include audio enabled. Check system/application selection, stereo, stream mute/gain, and microphone mute/deafen independence. | **Pending real-client audio check.** Generated selected-application audio → publication → decoded stereo passes plain/E2EE locally. |
| V2 | Watching and stopping streams | Watch/re-watch another person's Live stream, use fullscreen/chat controls, then Stop Watching while voice continues. Check video smoothness, immediate local stop and absence of lingering stream audio. | **Pending sustained real-call check.** Local plain/E2EE lifecycle, 1080p30 throughput, teardown and mixed audio fixtures pass. |
| V3 | Multiple streams and layouts | Watch two or more streams. Switch Tile all/Main stage, select different streams, resize the window and open chat. Check landscape/portrait layouts, per-stream audio and stopping one stream while the others continue. | **Deferred by the user on 2026-11-13.** Multiple watch subscriptions already exist; this pass adds an explicit local layout choice and tiling from actual stage bounds. Offline evidence is recorded below. |

Successful microphone transmission with GoXLR Chat Mic was previously confirmed
by the user. Session restore, joining voice, receiving audio and watching a stream
were also confirmed during development; these confirmations do not establish
acceptance of later changes or every related edge case.

## Remaining scope notes

Search, pins and historical navigation are ready for the checks above once the
latest build is installed. They do not complete the broader quick switcher,
search autocomplete/history, mature-content consent UI or full inbox features.
A small set of V8 legacy human-date spellings remains explicitly listed in
crates/protocol/CONTRACT.md; the verified reference cases are tracked separately.
No production messages, pins, uploads or read-state mutations were issued by the
new local test fixtures.

## Recording outcomes

Keep the IDs above when reporting findings. Record the build, scenario, observed
result and date; distinguish a confirmed pass from a pending check. New bugs should
remain linked to the review item until their fix and follow-up evidence are saved.
Broader release gates and platform work remain in [PLAN.md](PLAN.md) and
[STATUS.md](STATUS.md).

## Local UI evidence (2026-11-12)

The ignored `target/qa/ui-refinement/` directory contains offline screenshots and
the verification logs. Captures show SVG rendering, message hover/context menu,
Delete/Pin dialogs, compact search results, audio sliders and staged/spoiler
attachment cards. Slider pointer drag, arrow fine adjustment and Home-to-zero
were checked; two-choice dialogs remained open after the initiating key release
and canceled after Tab wrapping back to Cancel. The minimum supported width
(880 px) was exercised; a complete 880×600, increased-text and screen-reader
review remains U4. No real account messages/pins/uploads or microphone capture
were used for this UI verification.

## Conversation presentation evidence (2026-11-12)

`target/qa/message-presentation/` contains the source-shaped `--demo-messages`
fixture captures and regression logs. The fixture covers seven-minute grouping
boundaries, two local dates, formatted Unicode, code/quote/list content, concealed
spoilers, resolved user/channel/role labels, a reply and a custom emoji served as
generated PNG bytes over loopback. Bundled source fonts corrected missing bold,
italic and monospace styles; final captures include actual inline backgrounds.

The local full gate passes 353 tests; the final desktop checks cover the later
font/background refinements. No real messages, pins or uploads were sent, and no
microphone was captured. Clipboard, pointer/keyboard activation, real-client emoji
behavior and source-account preference acceptance still need hands-on review;
synthetic input did not consistently target the offline fixture during this pass.

## Avatar imagery evidence (2026-11-12)

`--demo-avatars` uses a synthetic gateway roster and a loopback-only image server.
The captured voice channel shows a community image, a failed-community-icon
acronym, a community-specific avatar with a speaking ring, global profile images
and a deliberately cleared avatar using the source default. Combining it with
`--demo-stream` checks the call filmstrip. Captures and the 360-test verification
log are in `target/qa/avatar-images/`. No real voice connection, capture or account
mutation was used. Real image changes, animated assets and two-client acceptance
remain separate from these offline checks.


## Stream layout and toolbar evidence (2026-11-12)

Offline fixtures cover two and four simultaneous synthetic streams with mixed
landscape/portrait shapes, plus the explicit main-stage mode. The hover toolbar
was captured over a generated code message with its solid backing visible.
Pointer checks in the offline fixture switched Tile all → Main stage, selected
the other stream, switched back, and stopped only that tile while the remaining
stream stayed visible. These validate local controls with synthetic frames.
Captures, logs and the full verification result are saved under
`target/qa/stream-layouts/`. No account session, call or capture was opened by
these fixtures. Live per-stream audio/stop behavior, layout switching during a
real call, and subjective legibility remain pending as U7/V3.

The animated single-stream fixture exercises the new grid with generated
1080p30 frames: steady acceptance/presentation was approximately 30 FPS. Cleared
Render followed Stop handling in 6.4 ms, with a 45.3 ms scheduled-action delay and
a 241.3 ms next-frame callback; the latter includes platform frame scheduling.


## Image modal evidence (2026-11-12)

Offline Wayland checks opened attachment, embed and forwarded image previews
inside Fluxer. Right navigated to the second image; Tab/Enter activated Previous,
and Shift+Tab/Enter wrapped to Close and returned to the conversation. Escape
canceled the external-image warning while retaining the viewer, then dismissed
the viewer itself. Backdrop dismissal left the underlying channel unchanged.
The 1800×1000 fixture fit without cropping, small originals retained their size,
and an immediate 404 displayed the in-app unavailable state. Active thumbnail
tooltips are suppressed while the viewer is open.

Captures, fixture logs and the full gate are saved in `target/qa/image-modal/`.
Neither fixture logged a concealed/expired-image fetch. Only generated images
were used; no messages or uploads were sent, no capture was started, and the
untrusted original was canceled before any browser navigation. Minimum-window
modal usability, sustained real-account behavior and subjective acceptance
remain pending as M5.


## Current concealed-media policy

On 2026-11-13, the user reviewed the concealed-first offline image mock in build
`b3c3864d3` and confirmed its appearance looks good. This accepts the mocked
spoiler-image presentation; real-account, GIF, embed/forward, keyboard and gallery
scenarios in M7 remain pending.

Concealed images and GIFs now preload when their preview is visible and display a
strongly blurred first frame with a reveal layer. The clear decoded media shares
the normal image cache, so revealing does not start a second preview download.
Concealed GIFs stay paused until revealed, and galleries omit unrevealed entries.
Reveal state is scoped to the message/snapshot/media entry and clears on channel
change and sign-out. This intentional preload policy supersedes earlier
no-fetch expectations in the historical M4/M5 and fixture evidence above.
Expired URLs and suppressed embeds remain excluded. ThumbHash is the source
client's placeholder format; this implementation blurs the loaded image itself.


## GIF and concealed-preview runtime evidence (2026-11-12)

Generated GIF window captures contain changing source palette pixels, proving
actual attachment/embed animation. The automatic overflow fixture moved GIFs out
of view: 20,628 source palette pixels became zero, all GIF elements transitioned
to clipped mode, and the settled image-render trace stopped changing. A separate
concealed-first fixture logged both blurred previews ready before its programmed
reveal. The selected GIF had one HTTP request before reveal and still one after;
its animation began after reveal. This checks cache reuse with a fixture-driven
state change; it is not a claim that the real-account click/keyboard review ran.

Artifacts are in `target/qa/gif-animation/` and `target/qa/concealed-media/`.
Source proxy requests were logged as animated=true, and no MP4 metadata URL was
fetched. The GIFV fixture uses an actual GIF image fallback; native MP4/GIFV
playback remains separate. Autoplay-disabled and pointer-hover policy has
regression coverage; desktop input targeting was not stable enough to claim a
complete interactive hover run. M6/M7 retain the hands-on checks.


## Interaction fixes (2026-11-13)

The fixes for scrolling, community selection, optimistic text/attachment sends,
emoji consistency, composer undo/redo and pin system notices are integrated on
`yolo/gpui`. The full gate passes 412 tests (11 opt-in tests remain ignored),
formatting, strict workspace Clippy and the feature inventory audit. Local HTTP
fixtures exercise actual send/upload contracts without production account writes.
Failure retention now belongs to an outbox row rather than the composer; the next
draft and newly staged files remain independent. Retry preserves the original
nonce, while separate identical submissions receive separate nonces.

Multi-stream acceptance is explicitly deferred. The previous spoiler mock approval
is scoped to its appearance and does not close the remaining M7 scenarios.


Offline Wayland checks of the merged build exercised actual keyboard typing,
Ctrl+Z, Ctrl+Shift+Z and Enter in a guarded demo window. Captures show composer
emoji artwork, empty text after undo, restored text after redo, and the sent
message with the composer cleared. The same fixture shows a visible scrollbar,
thin active-community border and compact pin sentence. These captures and the
412-test gate log are in `target/qa/interaction-fixes/`. A duplicate GPUI hover
style found on the first demo launch was removed before installation. The
subsequent pin-click/scrollbar-drag check stopped when the focus guard detected
a different window; no successful interactive navigation/drag result is claimed.
The fixture generated no production messages/uploads or microphone capture.


## Pending status layout correction (2026-11-13)

The user reported vertically stacked sending text in build `489049ac5`. The
persistent `--demo-outbox` fixture reproduced delivery/error labels constrained
to roughly one glyph wide. The status row now fills the message body width, its
text takes the remaining flex space, and action icons keep their width. Pending
body/attachment containers also have explicit width constraints; row spacing and
line height follow normal chat. Before/after mock captures show horizontal
sending/upload/error labels in `target/qa/pending-layout/`. The corrected desktop
passes all 143 tests (one opt-in ignored), strict Clippy and formatting. Real-account
send feel remains M8; this check establishes the persistent transient-state layout.


## Native playback evidence (2026-11-13)

The full gate passes 448 tests, strict Clippy, formatting and the feature inventory
audit (13 opt-in tests remain ignored). Source-backed MIME/URL/proxy/concealment
contracts, actual HTTP signed GET/privacy/redirect/truncation/cancellation/size/
timeout tests, and generated MP4/H.264/AAC, WebM/VP8/Opus and WAV decoding tests
are included. Timing regressions cover delayed audio, final-frame duration,
one-frame loops, paused/end seeking, rapid seek supersession, rotation and SAR.

Private PipeWire tests monitor only their own two synthetic sinks. Reported RMS
was 0.212111 initially, 0.053028 after volume change, zero while muted/paused,
0.084846 after seek and 0.084836 after switching to sink B, with sink A silent.
Stop silenced both. The opt-in 1080p decoder/mailbox test measured 91 frames in
3.005 s landscape and91 in 3.004 s rotated, clock ratios 0.999 and0.998. These are
engine/mailbox results with simulated speaker timing, not display scanout or a
physical GoXLR listening check. Logs are in `target/qa/native-playback/`.

The loopback `--demo-media` app displays real decoded silent video, changing GIFV
pixels, a blurred paused spoiler-video preview and an idle audio card. Its optional
automation dispatches the actual player handlers for full view, pause, seek,
resume and close; captures/logs show each phase. This is a fixture-driven action
check, not a claim that pointer/keyboard input or live-account acceptance passed.
Neither an expired file nor the ordinary audio tone was fetched automatically.
The fixture uses no credentials, account writes, microphone or personal media.

M9 remains pending for real files/devices, sustained playback in calls, full-view
keyboard/focus usability and shared-audio integration. Large files currently wait
for their complete bounded disk download before decoding. This is explicitly not
progressive/range streaming. Per-player volume is local; full persisted preference
and accessibility/motion migration, media gallery mixing and broader media menus
remain part of parity work. Multi-stream review remains deferred by the user.


The exact-build smoke run (`62c9fea28`) also reproduced stale initial window
contents: automated frame captures timed out while the fixture nevertheless
initialized and logged all five player actions. A guarded search open/close forced
a redraw; subsequent captures showed current native frames and changing GIFV
pixels. `final-visible-check.json` records that successful visible check; the
initial `final-actions-check.json` capture timeout is retained as evidence rather
than relabeled a pass. This is additional U6 evidence, not a claim that the
monitor/compositor redraw problem is fixed. Native playback and lifecycle tests
remain green. The next investigation should isolate that presentation issue.

## U6 redraw recovery evidence (2026-11-13)

The original `redraw-before` fixture froze when a test-only Wayland proxy delayed
one surface frame callback. Three captures during the hold were identical and
no new frame callback arrived until release. This establishes callback starvation
as a reproducible failure mode; it does not establish that every earlier stale
window had the same cause. Unmodified repeated monitor moves did not reproduce
the intermittent failure in this session.

The patched `redraw-after` fixture kept showing changing native frames while the
old callback remained held. The measured replacement callback arrived 118 ms
after the hold began. Late release left one outstanding pacing callback rather
than creating a duplicate chain. A final-source run also checked the case where
all callbacks are withheld: the original and one replacement callback accumulated,
then no further timer-driven frames were requested. Releasing them resumed
changing GIFV pixels. Four monitor moves and two fullscreen transitions retained
changing frames without keyboard/navigation redraws. Closing immediately after
a configure-producing transition exited cleanly, with no frame request after
surface destruction. Only the owned offline fixture was moved/captured.

`target/qa/redraw/` contains before/after protocol logs, scoped screenshots and
`final-check.json`; `verify.log` records 448 passing tests, 13 opt-in ignored,
formatting, source audit and strict workspace Clippy. The committed proxy's smoke
uses only temporary sockets and synthetic protocol messages:
`python3 scripts/qa/test_wayland_redraw_proxy.py`. Eight normal desktop regressions
exercise the actual dependency's scheduling/output policy. No production account
writes, personal media, audio playback or microphone capture were used.

U6 remains pending for the user's ordinary startup, moving/resizing between their
monitors, scaling and hide/restore. The callback-starvation fix is implemented;
broader compositor/GPU acceptance is not inferred from these controlled checks.

## U10 context-menu evidence (2026-11-13)

`target/qa/context-menus/` contains offline stream/user/connection/self/channel/
community captures, generated model diagnostics and the 457-test verification
log (13 opt-in tests ignored). Stream keyboard checks toggled mute once with
Enter, then adjusted the focused slider to 200%, 0% and 1% while mute remained
checked and the menu stayed open. Ctrl+F stayed within the menu; Escape dismissed
it. Actual Tab navigation followed by Shift+F10 opened the focused community
icon's menu, using its own anchor and restoring focus on dismissal.

The explicit `--demo-stream` fixture can open a menu with
`FLUXER_CONTEXT_DEMO_TARGET=stream|user|connection|self|channel|community`.
That environment variable only affects the offline stream fixture, where it
adds a synthetic two-device Alex and your other device. With user mute enabled
and User volume at 200%, the model's desktop gain became 2.0 and phone gain 0.6,
preserving the phone's separate 30% connection volume. Changes to your other
device's Connection volume/mute affected only that identity. The explicit local
dock menu toggled this device's mute/deafen in the fixture. These were actual
menu handlers driven by guarded keyboard input, with synthetic state only.

Normal tests cover exact user/connection identity matching, independent audio
scopes, retained new-device defaults, unresolved own-device safety, stale/wrong
action types, finite gains, viewport bounds, menu keyboard selection and copied
channel links using a discovered frontend. No physical audio or pointer test is
claimed here; live calls, own-share behavior, target disappearance/focus under
real gateway events and subjective usability still need review. Full persisted
preferences and broader menu coverage remain in UI_UX.md. Multi-stream layout
acceptance stays deferred; using several generated stream tiles for menu checks
does not accept that separate feature.


## M10/U11 media and editing menu evidence (2026-11-13)

`target/qa/media-menus/` retains scoped offline menu captures and verification
results. The final gate passes 485 tests (13 opt-in ignored), source audit,
formatting and strict workspace Clippy. Policy tests cover exact attachment/embed
and forwarded-snapshot identity, video-card versus thumbnail targets, current
permissions, suppression/expiry, concealment and media-channel download hiding.
Root tests reject removed/replaced timeline/browser targets and hidden titles.
Export tests exercise bounded image conversion, orientation, exact pixels,
cancellation/temporary-file cleanup and atomic replacement of an existing file.

The actual Copy image menu supplied an `image/png` Wayland selection to a separate
`wl-paste` process. Its bytes decoded to the generated fixture's exact dimensions
and checked pixels. Only validated synthetic fixture data was saved. The GPUI
patch also advertises actual image MIME types, uses the latest keyboard/mouse
input serial, and writes pipes nonblockingly with a per-dispatch budget; a stalled
private-pipe regression checks complete delivery without blocking the UI thread.

The native GTK Save File dialog opened from the actual media menu. Automated
keyboard confirmation was inconclusive, so end-to-end picker destination and
completion are **pending manual review**. The owned dialog was closed after the
check. No production messages, personal media, account settings, physical audio
or microphone capture were used. Live calls and subjective menu usability remain
pending; multi-stream layout acceptance remains deferred.


Scoped captures verify attachment/embed/forward menus and the absence of download
or original-link actions on concealed, expired and channel-download-hidden media.
Inside both image and native-video viewers, Escape dismisses the menu first and
a second Escape closes the viewer. Keyboard-driven composer checks performed
Undo and Redo on a draft containing an emoji, then selected/copied the exact
synthetic Unicode text to an external reader. No message was submitted. The final
nonblocking clipboard build also passed the external generated-PNG pixel check.
Pointer targeting and full real-account search/pins interactions remain manual
review items; these fixture captures do not establish broader acceptance.


## U12 read/notification evidence (2026-11-13)

`target/qa/read-notifications/` contains owned offline fixture captures and the
493-test gate log (13 opt-in ignored), with source audit, formatting and strict
workspace Clippy. Keyboard checks open the duration and notification pages,
return through Back, choose a fifteen-minute mute and an explicit notification
level, and activate message Mark unread and channel Mark as read. The sidebar
muted icon and inherited Only mentions label were visually checked. The fixture
uses the real worker's offline read/settings handlers. These are local synthetic
state changes, with no production account mutations, physical audio or capture.

Private loopback HTTP tests assert actual manual/bulk acknowledgement and settings
PATCH paths and bodies, including manual flags, server-supplied cursor/count/version
and indefinite mute serialization without inner channel IDs. Core fixtures cover
record-keyed settings responses, preserved settings fields, duration milliseconds,
large-community defaults, expiry and mention filtering. The implemented legacy
ACK fallback and private settings endpoint still need cross-client review. A category test verifies
immediate children and excludes nested categories. Stale/manual ACK routing uses
the existing source-backed reducer and manual unread is fenced against viewport
ACKs; failures restore a worker snapshot.

Remaining manual checks: cross-client persistence/markers, keyboard and pointer
usability under real gateway updates, timed expiry in normal use, permission loss,
and ordinary restart with real settings. Separate thread membership gateway
projection and full forum unread/notification-delivery parity remain incomplete.
Community suppression/mobile-push toggles and unread-badge customization are not
part of these compact menus. Multi-stream layout review stays deferred.


## P3 profile-popup evidence (2026-11-13)

`target/qa/profiles/` retains scoped offline screenshots, keyboard/pointer results
and the 502-test gate log (13 opt-in ignored), source audit, formatting and strict
workspace Clippy. The popup uses an opaque source-sized panel, banner/overlapping
avatar and scrollable content. Generated fixtures cover basic/edge/long/error/
limited/webhook states. Keyboard checks activate a real identity trigger, Copy ID
to an external clipboard reader (synthetic ID only), Retry and Escape. Live calls,
physical audio and production account mutations were not used.

Private loopback tests assert the real profile GET query including optional guild
scope and both mutual flags, deduplication and partial-user handling. Lifecycle
tests cover force refresh, stale/out-of-order results, account reset/invalidation,
expiry/capacity, limited data and async failures. Popup tests cover bounds, empty
member overrides and source accent precedence, including black. Existing avatar
policy tests cover guild overrides and cleared-avatar flags.

Manual acceptance remains pending for real avatars/banners, nicknames and role
changes, profile permissions/limited accounts, mouse/keyboard behavior while in a
call, cross-community search results, moving between monitors and subjective
layout. Full modal, rich/selectable biography, badges/connected accounts,
relationship/DM actions and live refresh of an already-open completed card remain
migration work. Multi-stream review is still deferred.


The click-area refinement separates image/name targets from voice/sidebar/dock/
filmstrip row containers, removes the stretched chat-avatar gutter target and
keeps names at intrinsic text width. Existing whole-row right-click voice menus
are independent. The final pointer check records image/name versus adjacent-gap
activation in `hitbox-check.json`; broader real-account pointer acceptance remains
pending.
