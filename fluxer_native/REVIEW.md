# Native client review queue

Keep this file current as work lands. Automated fixtures establish their stated
scope; real-account, device and subjective UI checks stay pending until reviewed.
The user is away from their PC as of 2026-11-12. Continue implementation and local
verification without treating their absence as approval of a manual test result.

## Ready for review

Implementation is on `yolo/gpui`. Attachment work started from build
`bb3f36d37`; use the current native window title to record the build being reviewed.
The UI refinement gate passes 360 tests, formatting, strict Clippy and the
feature inventory audit; source-reference fixtures and scope are recorded in
STATUS.md. The holistic UI audit and follow-up priorities are in [UI_UX.md](UI_UX.md).
Full feature parity remains in progress.

| ID | Review | Steps and expected result | Evidence / status |
|---|---|---|---|
| P1 | Community and voice profile images | Check community icons, the in-voice sidebar, voice participant cards and the stream viewer filmstrip against regular Fluxer. Try a community-specific avatar, a global avatar and no avatar; speaking rings and mute/live badges should remain visible. | **Pending real-account review.** Source URL/member precedence/default asset tests and local image fixture checks are recorded below. |
| P2 | Image fallback and updates | Check a community without an icon, a missing image, an avatar cleared in community settings, and an avatar changed while connected. Switch communities while remaining in a call; call images should retain the joined community's avatar context. | **Pending live/hot-update review.** Shared credential-free image loading, source defaults, member AVATAR_UNSET, safe identity resolution and image-error fallbacks implemented. Animated image behavior remains future work. |
| M1 | Grouping and conversation density | Compare repeated authors, long conversations, replies and midnight boundaries with Fluxer. Same-author continuations should share a header; a new group starts after seven minutes from its first message. Check hover timestamps and old-message jumps. | **Pending live/subjective review.** Eight source-rule tests, ten source-executed cases and offline date/group rendering checked. Cozy layout implemented; compact/density preference migration remains open. |
| M2 | Selectable formatting and spoilers | Select across bold/italic/underline/strike, links, code, quotes, Unicode and wrapped lines. Ctrl/Cmd+C should copy rendered text and omit concealed spoilers; click a spoiler to reveal it. Explicit Copy Message should retain Markdown source. | **Pending sustained usability review.** Existing composer/input regressions plus rich-run clipping, Unicode range, hidden-target/copy and soft-wrap emoji tests pass. Source fonts and actual inline backgrounds checked offline. |
| M3 | Mentions, emoji and links | Compare known user/role/channel labels and custom emoji with Fluxer. Click a channel link and an old message link; inspect an untrusted external URL and cancel. Check registered emoji renames, failures and spoiler concealment. | **Pending real-account review.** Generated loopback emoji images and resolved labels checked; no external destination opened. Trust/host/scope fixtures pass. User/role popovers, animated/jumbo sizing, timestamps and full preference parity remain incomplete. |
| M4 | Reply and attachment presentation | Follow a reply to its original message; check missing references, forwards, images, spoilers/sensitive attachments and narrow-window wrapping. Images should retain aspect ratio without an extra file card underneath. | **Pending live/layout review.** Reference lookup uses existing cached/wire data and jump commands; media URL/privacy and bounded dimensions regressions pass. |
| U1 | Conversation density and contextual actions | Compare a busy channel with regular Fluxer. Hover a message and use right-click/Shift+F10; React/Reply/More should appear without moving content. Copy selected text, edit/reply/forward, and cancel Delete/Pin confirmations. | **Pending subjective/live review.** Offline SVG, hover/right-click, keyboard menu, confirmation opening-release guard and Tab-wrap/Enter-on-Cancel checked; no real messages or pins changed. |
| U2 | Audio settings and account dock | Check the compact name/mic/deafen/settings dock, connected/reconnecting/share/disconnect states, real device dropdowns, volume sliders and DSP switches. Use mouse drag, arrows and Home/End. | **Pending device/live review.** Offline layout, slider drag/fine adjustment/range limits checked without starting microphone capture. Native gain curve still uses its existing linear 0–2 scale. |
| U3 | Search, pins and attachments | Browse results and pins; use corner actions, select text without jumping, and open a result. Add image/file attachments and check overlay controls, spoiler/retry/cancel and uncluttered empty composer. | **Pending subjective/live review.** Offline compact search layout, generated-file cards/spoiler concealment and confirmation cancellation checked. Existing upload/search/controller contracts pass. |
| U4 | Keyboard and small windows | Use Tab/Shift+Tab, timeline Up/Down, menus and confirmation dialogs at 880×600. Try long channel/user/file names and increased text size. Controls should remain reachable, focus visible and dialogs on screen. | **Pending complete accessibility review.** Local keyboard/slider/menu fixtures pass; screen-reader semantics, preference migration and broader focus consistency remain incomplete. |
| U6 | Window moves and scaling | Move a window between monitors while watching video and using chat; try different scale factors and window sizes. | **Needs investigation/review.** Moving the offline preview between outputs produced blank content until a navigation redraw. No live-call failure established; retain this as an unresolved display check. |
| U5 | Stream controls | Watch and share with regular Fluxer; check the quieter toolbar, chat/fullscreen/volume and Stop Watching/Stop sharing controls. | **Pending real-call review.** Synthetic viewer toolbar/volume layout checked; UI commands and immediate local stop paths retained. Prior synthetic media evidence remains scoped as V1/V2 below. |
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
