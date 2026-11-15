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

## Live follow-up: slow opening was repeatedly cancelled

The live message `1571511402613448704` in channel `1565067077809614849`
still showed Loading on build 237e46a73-local. A GDB probe of that exact resource
recorded visible=true, active=true, and repeated Loading/Paused/Loading cycles
roughly every 400 ms. The native worker was opening the HTTP source after its
player had already been cancelled (pending_seek=true, wanted=false). The GPU
validation log and kernel Xid checks remained clear during this observation.

The stale-prepaint watchdog assumed every visible player produces continuous
redraws. A loading, buffering or paused player can legitimately remain unchanged
for more than 250 ms. It was therefore being retired and reopened before the
media request could finish. Infer stale visibility from missing redraws only
while actively playing. Actual clipped prepaints, context removal and shutdown
still retire players; focus restoration still waits for fresh prepaint.

The explicit offline fixture now accepts `FLUXER_MEDIA_DEMO_DELAY_MS=600` with
`FLUXER_MEDIA_DEMO_GIF_ONLY=1 --demo --demo-media` (delay capped at five seconds,
only applied by the loopback fixture). Before the fix, the owned GUI run opened
18 decoders and retired 17, and its capture showed Loading. Afterward, one decoder
and one video request played normally, with no retirement through three actual
focus-away/back cycles. Captures/logs: ignored
`target/qa/gif-gpu-lifetime-2026-11-15/slow-{before,after}*`.

244 desktop tests passed (one existing performance case ignored), and strict
desktop/all-targets Clippy passed after this fix. A new live debugger session is
recorded under ignored `target/qa/live-gpu-debug-20261115-115531/`. On build
3feeaa810-local the exact reported message produced 80 consecutive Playing
snapshots across a loop boundary, with decoded dimensions 624x640, duration
1.467 seconds and no media error. The renderer/validation and kernel GPU logs
were clear at that check. This verifies recovery of that loading case separately
from the GPU fault, which was not reproduced in the live session. GDB remains
attached with automatic crash/failed-GPU-wait stack capture for continued review.
