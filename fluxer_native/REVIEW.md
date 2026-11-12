# Native client review queue

Keep this file current as work lands. Automated fixtures establish their stated
scope; real-account, device and subjective UI checks stay pending until reviewed.
The user is away from their PC as of 2026-11-12. Continue implementation and local
verification without treating their absence as approval of a manual test result.

## Ready for review

Baseline: `yolo/gpui`, build `bb3f36d37`. The native gate passes 270 tests,
formatting, strict Clippy and the feature inventory audit. The app was reopened
with this exact build title. Full feature parity remains in progress.

| ID | Review | Steps and expected result | Evidence / status |
|---|---|---|---|
| A1 | Real attachment delivery | In a suitable test conversation, attach a small image and a normal file. Send with text, as a reply, and with no text. View each from regular Fluxer. Filenames, bytes, reply target and spoiler visibility should match. | **Pending real-account check.** Real-byte HTTP contracts and public-controller fixtures pass. |
| A2 | Attachment input and layout | Add several files using the system picker, drag files into chat, and paste a screenshot. Toggle a spoiler and remove a file; switch channels and back. Cards should remain compact and belong to the original channel. | **Pending usability review.** Generated-file preparation, actual Wayland image paste, native picker opening and spoiler preview hiding checked offline. Physical drag/drop and picker selection need review. |
| A3 | Upload failure and cancellation | Cancel a large upload; retry after a temporary connection failure. Files, text and reply intent should remain available. Type the next draft and add another file while a send is pending; completion must preserve the new draft/file. | **Pending real-network check.** Loopback retry/cancellation, retained nonce, draft preservation and changed-file rejection pass. |
| A4 | Upload policy and call responsiveness | Compare upload limits/preupload behavior with the same account in regular Fluxer. Upload while in a call and switch channels/settings. Voice, gateway updates and controls should stay responsive. | **Pending interoperability check.** Source-derived limits/privacy, shared batch planning and nonblocking actor fixtures pass. |
| E1 | Reaction picker feel | Open reactions repeatedly, search a Unicode/custom emoji, change skin tone, and add/remove a reaction. Icons should be centered and opening should feel immediate. | **Pending subjective/live check.** Sprite, search and virtualization fixtures pass; earlier local cold-open measurement was 112 ms. |
| V1 | Shared audio with regular Fluxer | Have another client watch a stream with Include audio enabled. Check system/application selection, stereo, stream mute/gain, and microphone mute/deafen independence. | **Pending real-client audio check.** Generated selected-application audio → publication → decoded stereo passes plain/E2EE locally. |
| V2 | Watching and stopping streams | Watch/re-watch another person's Live stream, use fullscreen/chat controls, then Stop Watching while voice continues. Check video smoothness, immediate local stop and absence of lingering stream audio. | **Pending sustained real-call check.** Local plain/E2EE lifecycle, 1080p30 throughput, teardown and mixed audio fixtures pass. |

Successful microphone transmission with GoXLR Chat Mic was previously confirmed
by the user. Session restore, joining voice, receiving audio and watching a stream
were also confirmed during development; these confirmations do not establish
acceptance of later changes or every related edge case.

## In progress

Server-backed message search, pinned messages and navigation into old history.
Add exact build IDs, automated evidence and practical review steps here when this
work is merged. Do not mark this section ready merely because controls exist.

## Recording outcomes

Keep the IDs above when reporting findings. Record the build, scenario, observed
result and date; distinguish a confirmed pass from a pending check. New bugs should
remain linked to the review item until their fix and follow-up evidence are saved.
Broader release gates and platform work remain in [PLAN.md](PLAN.md) and
[STATUS.md](STATUS.md).
