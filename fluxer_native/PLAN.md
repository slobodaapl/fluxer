# Rust / GPUI client migration

Target branch: `yolo/gpui`. The target is complete functional coverage of the existing
Fluxer client. The first release gate is community text channels, voice calls,
screen sharing, and audio management. A working shell or demo does not satisfy
that gate.

Current coordinated expansion: [2026-11-15 communication parity push](PUSH_2026_11_15.md).

## Client/server compatibility rule

Client/server interactions must match the existing desktop client. Implement
each flow from its current request builder, transport, gateway handler, and
server schema; preserve endpoint selection, headers, payload omissions, request
ordering, subscriptions, retries, and lifecycle behavior. Do not infer protocol
behavior from endpoint names, example responses, or what another chat service
does. Reuse existing Rust codecs and platform engines where possible.

Every migrated flow needs source references and fixtures derived from those
sources. Fixtures must reject unexpected requests and exercise actual response
shapes, including ID-only references and error envelopes. An API accepting a
request is insufficient evidence of parity. Record unsupported behavior as open
work; do not replace it with an invented interaction. See
`crates/protocol/CONTRACT.md` for the current transport evidence and gaps.

## Architecture and integration

- `crates/core`: platform-independent wire models, permissions, client state,
  gateway reduction, read state, drafts, preferences, and call state.
- `crates/protocol`: instance discovery, authenticated REST, errors/rate limits,
  gateway heartbeat, reconnect/resume, and Fluxer voice signaling.
- `crates/media`: native LiveKit/WebRTC room, microphone, speaker, DSP, screen
  capture/publication, and incoming media. No browser or Electron renderer.
- `crates/desktop`: GPUI windows, text input, lists, navigation, settings, call
  controls, and asynchronous orchestration of the other crates.
- `crates/client`: background account/session orchestration shared by GPUI views;
  authenticates, projects gateway/REST state, and manages call/media lifecycle.
- `crates/session`: encrypted OS credential storage, pinned saved-account codec,
  invalid-session retention and injectable persistence for restart tests.
- `crates/system-audio`: Node-free access to the existing Rust PipeWire routing
  engine, including application isolation and capture feedback prevention.

The native client has an independent Cargo workspace and lockfile. Existing
backend services, HTTP schemas, gateway protocols, web/mobile clients, and
deployments remain usable during migration. Server replacement is not needed to
replace the desktop renderer. Rust services already present in the repository
remain the reference implementation for their respective server behavior.

Implementation branches/worktrees:

| Owner | Branch | Worktree | Merge responsibility |
|---|---|---|---|
| State agent | `gpui/core` | `/tmp/fluxer-gpui-core` | Main agent |
| Protocol agent | `gpui/protocol` | `/tmp/fluxer-gpui-protocol` | Main agent |
| GPUI agent | `gpui/desktop` | `/tmp/fluxer-gpui-desktop` | Main agent |
| Client/backend agent | `gpui/backend` | `/tmp/fluxer-gpui-backend` | Main agent |
| Audio agent | `gpui/audio` | `/tmp/fluxer-gpui-audio` | Main agent |
| Main agent | `yolo/gpui` | Repository root | Integration, media, plans, checks |

Agents commit only their owned crate. Main merges reviewed commits, resolves
cross-crate contracts, owns the shared lockfile, and runs integration checks.
Further work follows the same isolated-worktree process.

## Release gates

### M0 — Native foundations

Build a GPUI executable on Linux, macOS, and Windows. Match Fluxer's navigation,
keyboard behavior, readable typography, selectable/copyable text, scrolling,
IME, clipboard, resizing, themes, and accessible focus behavior. Keep explicit
demo mode separate from authenticated operation. Never display demo messages as
server messages. Tests must preserve snowflake precision and wire compatibility.

### M1 — Community text chat (MVP)

Discover official and self-hosted instances; sign in through supported account
flows; load actual communities, categories, channels, members, roles, and
permissions. Load/paginate channel history; send/edit/delete/reply, display
delivery failures, preserve drafts, process real-time changes without duplicates,
ack reads, typing, mentions, reactions, pins, attachments, and permission changes.
Reconnect/resume must recover from missed events. Verify with two real clients,
including an account denied channel access. No optimistic success on failed REST.

### M2 — Voice and screen sharing (MVP)

Join/switch/leave community voice channels through Fluxer gateway opcode 4;
correlate `VOICE_SERVER_UPDATE` to the requested call; use server-provided
LiveKit endpoint, token, and connection ID. Publish microphone and play remote
audio. Reflect participants, speaking, reconnect, remote moderation, channel
deletion, and permission loss. Mute stops transmission; deafen stops playout and
transmission without losing the prior mute preference. Disconnect stops capture.

Enumerate/select/hot-switch microphones and speakers; follow the system default;
refresh devices after hotplug; input/output gain, per-user volume/local mute,
voice activity and push-to-talk, sensitivity, echo cancellation, noise suppression,
automatic gain control, microphone test/meter, and clear device errors. Do not
enable microphone capture just by opening the app or viewing settings.

Share a user-selected display/window via OS consent; publish and receive screen
video with aspect-correct GPUI presentation; stop on request or source removal;
pick resolution, frame rate, codec/quality within instance/account limits. Include
system/application audio with independent gain/mute and avoid feedback loops.
Support Wayland portals/PipeWire and X11 on Linux, ScreenCaptureKit on macOS, and
Windows capture APIs. Honor E2EE policy and never silently join an encrypted call
without encryption. Test two clients exchanging microphone, display, and shared
audio; test mute/deafen and device changes during sharing.

**MVP is not complete until both M1 and M2 pass end to end.** Platform-specific
gaps, credential requirements, or hardware validation remain recorded as open
work, not silently waived release gates.

### M3 — Complete communication parity

DMs/group DMs, private calls/ringing/consent/P2P where supported; friends,
requests, block/unblock, user profiles, presence/custom status; rich message
rendering (Markdown, code highlighting, spoilers, links, math, embeds), emoji,
GIFs/stickers/memes/favorites, files, image/audio/video viewers, downloads, voice
messages/TTS, search/filter/jump, inbox/mentions/unreads, slowmode, scheduled or
other existing message actions. Match forums, tags, post creation/search/sort,
thread creation/membership/archive/lock/subscriptions/unreads and split panels.

### M4 — Community and safety parity

Create/join/leave/delete/transfer communities, invite/vanity links, discovery and
visitor behavior, channel/category CRUD/order, notification overrides, member
lists/search, roles/hierarchy and permission overwrites, moderation/report flows,
timeouts/kicks/bans, audit log, emoji/sticker management, webhooks/integrations,
content/age restrictions, verification, trusted domains and privacy controls.
Use backend enforcement as the authority, with permission-aware client controls.

### M5 — Account and settings parity

Registration, unclaimed accounts, email verification/revert, password reset and
recovery kits, MFA/sudo/passkeys/WebAuthn, SSO/OAuth callbacks, IP authorization,
account restrictions, multi-account storage/switching, device/session management,
profile editing and connections/authorized applications. Cover every settings
tab and section from `SettingsSectionRegistry` and community/channel registries.
Include privacy/safety, linked accounts, blocked users, language/time, look and
feel/themes/custom theme import/export/live files, accessibility, notification
sounds, messages/media, voice/video, keybinds, desktop/advanced/developer controls.
Persist credentials in the OS secret service and preserve account isolation.

### M6 — Commerce and native platform parity

Existing premium/Plutonium entitlement display, checkout/return/cancel/billing,
gifts/codes/inventory and Operator Pass. Retain existing backend checkout flows;
this migration does not provision a new payment service.

Tray/dock/jump-list, unread badges, autostart, global shortcuts/PTT, deep links,
notifications with click navigation, spellcheck/dictionaries, download/open/save,
window geometry/popouts/fullscreen, OS appearance, streamer mode and process
detection, hardware encoders, native capture permissions, diagnostics, crash
recovery, proxy/certificate settings, distribution and signed updates. Preserve
branch-specific existing settings/update behavior until deliberately changed.

### M7 — Cutover

Run source coverage audit, gateway/REST contract fixtures, reducer and permission
tests, real two-client text/voice/share tests, reconnect/failure tests, platform
build/smoke matrix, accessibility/keyboard review, localization, performance and
resource-leak soak tests. Migrate local account/preferences safely. Package signed
platform builds. Retire the Electron client only after the parity ledger and all
release gates pass. Full parity includes existing edge behavior, not just visible
navigation entries.

## Coverage ledger

`parity.json` maps every current feature directory to a milestone and explicit
acceptance goals. It also snapshots the native addons and settings registries.
`python3 scripts/check_parity.py` fails on newly untracked feature directories,
native addons, or user settings tabs. This checks inventory coverage; it does not
claim implementation completeness. Source references are relative to repository
root. `STATUS.md` records implementation evidence and remaining release blockers.

Framework references: [GPUI](https://gpui.rs/),
[GPUI 0.2.2 API](https://docs.rs/gpui/0.2.2/gpui/),
[LiveKit Rust SDK](https://github.com/livekit/rust-sdks).
