# GIF focus recovery and message links

The short-loop regression generates a 50 ms one-frame MP4 and samples several
loop boundaries, pauses and resumes. It failed before the fix because a completed
iteration reset the initialized player to Loading. A loop now preserves its
readiness and last frame; initial loads, explicit seeks and real buffering retain
their existing loading states. The regression passes with synthetic output only.

GIF players also observe activation directly. Inactive windows do not expire
visibility based on missing frame callbacks. On return, the player keeps its
paused decoder until a fresh prepaint confirms the viewport, then reapplies
animation policy. Off-screen prepaints and retirement still release resources.
This avoids treating a compositor's restoration delay as a scroll out of view.

Explicit offline fixture:
`FLUXER_MEDIA_DEMO_GIF_ONLY=1 fluxer-desktop --demo --demo-media`.
Its generated silent GIFV logs decoder/focus/request events only in that explicit
mode. Cycle focus to an owned temporary dialog and back; expect one decoder and
one video request, no retirement/reload, and animated frames after return.
The actual KLIPY/Puppy Den message still needs live-account review. KLIPY's public
page was unavailable to the web fetch; no account token or provider key was used.
The local source KLIPY resolver confirms these links become GIFV video embeds.

“Copy link to message” uses the discovered webapp endpoint and the source
`/channels/{community-or-@me}/{channel}/{message}` route. It is available alongside
Copy text in message menus, including search/pins and media message actions.
Activation rechecks access; copying from a media viewer keeps that viewer open.
The route regression covers community/DM/voice-channel-shaped links, self-hosted
origins and internal-link parsing, and rejects zero IDs/credentialed origins.

Explicit offline message-menu fixture:
`FLUXER_MESSAGE_LINK_DEMO=1 fluxer-desktop --demo --demo-messages`.
It supplies a reserved example webapp origin and opens the actual message menu.
The UI check inspects the option and dismisses it without modifying the clipboard.

Logs/screenshots are retained under ignored
`target/qa/media-focus-links-2026-11-15/`. Strict desktop/media Clippy, desktop tests,
file playback tests, the scoped profile-cache test and delayed-profile GUI checks
are the relevant validation. Physical audio, the live KLIPY item and subjective
acceptance remain pending; existing unrelated audio changes are preserved.

Validation result: 240 desktop tests and 14 file-playback tests passed (one
intentional ignored performance case in each suite); one scoped profile-cache
transport test passed. Strict desktop/media Clippy passed. Three real focus cycles
recorded activation false/true, one decoder-open and one video HTTP request, with
no decoder retirement or reload. Returned screenshots show active frames without
the Loading or paused-play overlay. Profile cold/warm/error fixtures recorded one
request per image across reopen; the menu option was captured without copying.
