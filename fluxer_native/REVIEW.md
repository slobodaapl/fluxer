# Native client review queue

Keep this file current as work lands. Automated fixtures establish their stated
scope; real-account, device and subjective UI checks stay pending until reviewed.
The user is away from their PC as of 2026-11-12. Continue implementation and local
verification without treating their absence as approval of a manual test result.

## Ready for review

Implementation is on `yolo/gpui`. Attachment work started from build
`bb3f36d37`; use the current native window title to record the build being reviewed.
The final navigation gate passes 320 tests, formatting, strict Clippy and the
feature inventory audit; source-reference fixtures and scope are recorded in
STATUS.md. Full feature parity remains in progress.

| ID | Review | Steps and expected result | Evidence / status |
|---|---|---|---|
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
