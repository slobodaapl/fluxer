# Desktop transport compatibility

The existing desktop client and checked-in server are the protocol authority.
Source references below are relative to the repository root. These are scoped
contract checks, not a claim that the whole native client has feature parity.

| Flow | Existing implementation | Native evidence |
|---|---|---|
| Discovery | `fluxer_desktop/src/main/DesktopRuntimeDiscovery.ts`; `packages/instance_bootstrap/src/ClientRuntimeDiscovery.ts`; `OfficialInstance.ts` in the same package | Required endpoint projection, official aliases, declared API validation and discovery follow are tested in `instance.rs` |
| API Origin | `fluxer_desktop/src/main/LocalAppAPIProxyRequestHandler.ts`, `LocalAppProxyHTTPPolicy.ts` | `transport.rs` checks discovered **webapp** origin, distinct API/webapp authorities, bodyless mutations, GET/HEAD omission, and production-style `INVALID_API_ORIGIN` rejection |
| REST headers | `fluxer_app/src/features/platform/transport/RestTransport.ts`; desktop `LocalAppProxyClient.ts`; app `ClientInstallationId.ts` | Feature/capability headers, stable 32-hex installation ID, raw authorization, gzip/br, locale and token omission on auth requests |
| Password/TOTP/session startup | `features/auth/commands/AuthenticationCommands.ts`, `AccountSignInIdentifier.ts`, `AuthSessionLifecycle.ts` | Client fixtures select identifier by instance account-identity feature, retain locale, validate explicit token login, avoid extra bootstrap REST calls, bind authenticated identity to READY |
| Saved-session startup/logout | `features/platform/state/auth_session/AuthSessionRestorer.ts`, `features/platform/state/auth_session/AuthSessionLifecycle.ts`; `features/app/state/RuntimeConfig.ts`; `AccountStorageKey.ts` | Encrypted OS-vault persistence, exact instance-qualified identity, discovery refresh with pinned offline fallback, cold IDENTIFY/READY, close-4004 invalidation, durable local logout even when server notification fails |
| IP authorization | `features/platform/utils/ResponseInspection.ts`; `features/auth/flow/IpAuthorizationScreen.tsx`; `features/app/constants/Endpoints.ts` | Modern 403/context parsing, integer validation/defaults, exact resend/poll requests, form-encoded ticket and auth locale; native controller/UI continue the challenge |
| Browser handoff (CODE return path) | `fluxer_desktop/src/main/BrowserHandoff.ts`; `features/auth/flow/browser_handoff/useBrowserLoginHandoff.ts` | Main-process headers, omitted return URI, private poll proof, 404/405-only persistent GET fallback, expiry/instance binding and public browser URL; native UI opens the browser on user action |
| Read acknowledgement body/routes | `features/read_state/state/ReadStates.ts`; `ReadStateAckQueue.ts` | Authoritative `/read-states/ack` first; identical automatic entry shape; `/read-states/ack-bulk` fallback only for 404/405 |
| Gateway compression | `features/gateway/transport/PooledGatewayConnection.ts`, `GatewaySocket.ts`; `fluxer_app/rust/libfluxcore/src/zstd_stream.rs` | Reuses the exact Rust streaming codec; real bidirectional zstd sockets; fresh dictionary per connection; source compression fallback |
| Identify/resume | Same gateway sources; `ClientInfo.ts` | Flags 14, version 1, selected guild/presence/locale builders, resume token/session/sequence, same-socket rejected-resume Identify, restored presence, source heartbeat/backoff limits |
| READY/state/events | `fluxer_gateway/src/session/session_ready_collect.erl`; desktop gateway READY handler, user/member stores; `session_dispatch_voice.erl` | Core checked-in fixture resolves ID-only users, preserves own private fields; batched reactions/guild sync/channel and role updates |
| Selected guild subscription | `features/gateway/transport/GatewayGuildSubscriptionSynchronizer.ts`, `GatewaySubscriptionLedger.ts` and pooled connection | Client fixture requires opcode 14 with `{subscriptions:{id:{active:true,sync:true}}}`; READY drives initial state |

Gateway client metadata describes the Rust executable truthfully while retaining
the server's existing property schema. It does not impersonate an Electron
browser or invent an Electron version.

## Open compatibility work

- Read acknowledgements still need desktop focus/visibility/bottom/modal
  predicates and the 3-second automatic batching queue. Matching route/body
  does not establish matching timing.
- LocalPresence preferences, idle/away detection, custom status and broader account
  preference persistence are incomplete; the initial native session uses online.
- Remaining auth ceremonies include WebAuthn, SSO/browser handoff, registration,
  CAPTCHA inside GPUI, recovery, sudo, account restrictions and secure multi-account
  storage. Browser handoff can use the existing web sign-in ceremonies; native
  deep-link return registration is not implemented.
- REST coverage remains partial: attachments/uploads, rich messages, thread/forum
  workflows, moderation and other feature areas are tracked in `parity.json`.
- Gateway coverage remains partial: member lists, complete resync/error recovery,
  all dispatch types, member/thread/sidebar/visitor subscriptions, account
  lifecycle and background account sessions. Saved locale and guild restore
  before Identify; the saved channel and its history restore after READY. Full
  preference migration remains open.
- REST pacing buckets, per-call retry overrides and automatic retry mode remain
  incomplete. Current native calls use the desktop default of zero retries and
  surface 429; the default alone does not cover the full transport policy.
- Native voice now uses explicit microphone/watched-screen subscriptions,
  `viewer_stream_keys`, deafen policy and credential change detection. Deferred
  watch removal, full publication/region migration, mesh/P2P transport, reliable
  screen codec negotiation messages and camera demand remain open.
- Rust LiveKit's publish API lacks the existing JS client's `forceStereo` option;
  matching stereo PCM does not alone establish matching SDP signaling.
- Stream availability now follows gateway `self_stream` in the joined channel,
  matching `useVoiceCallTracksAndLayout.ts`, before SFU metadata arrives. Native
  watch intent immediately projects the existing `viewer_stream_keys`; temporary
  metadata loss cannot revoke an authoritative gateway-live watch.
- The pinned Rust SDK now has a local compatibility patch for receiver reuse and
  ordered subscription signaling. Explicit unsubscribe retains the RTC handle
  privately; resubscribe restores normal SDK renderer/E2EE hooks, and final
  unpublication clears the handle. This fixes the reproduced screen-rewatch and
  microphone-deafen restoration failures. The complete raw SDK loopback and
  actual MediaEngine plain/encrypted screen fixtures pass without retry sleeps.
  See `compat/livekit/FLUXER_PATCHES.md`; full region/publication migration remains
  a separate interoperability gate.
- Native capture/device routing, codec settings and E2EE need two-client
  interoperability checks against the existing desktop client. Synthetic LiveKit
  transport alone cannot verify Fluxer call parity.

Source-backed tests and an authenticated two-client comparison are both required
before closing these gaps. Avoid logging real tokens, passwords or call keys in
comparison traces.
