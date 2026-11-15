# Profile loading and cached media

Profile data retains the classic client's 60-second user/community cache.
The popup preloads its effective banner and avatar through the same GPUI
`ImgResourceLoader` URL keys used by normal images. It shows one source-style
skeleton/spinner until both metadata and remote artwork settle, then reveals the
card. Image errors count as settled and use the accent/default-avatar fallback.
A separate clipped banner layer prevents cover-sized art painting over the bio.
Escape/outside dismissal remains available; hidden profile actions cannot fire
while the skeleton is shown. No extra network client, credential forwarding or
additional unbounded image cache is introduced.

Explicit offline fixture: `FLUXER_PROFILE_DEMO=loading fluxer-desktop --demo`;
`loading-error` returns a delayed missing banner.
Both serve avatar/banner PNGs over private loopback with 300/1200 ms delays and
reopen after three seconds using identical resource URLs. Logs must show exactly
one request per image, a completed reopen and no request count=2. Inspect pending,
ready, reopened and Escape-closed captures. The failure case should reveal the
normal accent with its loaded avatar, not spin indefinitely.

Evidence is retained under ignored `target/qa/profile-loading-2026-11-15/`.
Source: `UserProfileLoadingSkeleton.tsx`, `UserProfile.ts` (60-second expiry),
`UserProfileCommands.tsx` (scoped cache/in-flight reuse), native profile transport
and GPUI's cached asset loader. Real-account, animated-banner and subjective
acceptance remain pending. The change does not modify profile request contracts.
