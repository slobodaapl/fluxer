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
| Attachment plan/bytes/complete/message | `features/messaging/upload/AttachmentUploadPlan.ts`, `ChunkedAttachmentUploader.ts`, `MessageQueue.tsx`; `MessageAttachmentUtils.ts`, `MessageRequestUtils.ts`; `packages/schema/src/domains/message/AttachmentUploadSchemas.ts` | `uploads.rs` verifies actual synthetic file bytes, isolated storage headers, multipart ranges/retries/concurrency, final metadata and fallback boundaries; `attachments.rs` covers cancellation, invisible content and retry hint policy |
| Read acknowledgement body/routes | `features/read_state/state/ReadStates.ts`; `ReadStateAckQueue.ts` | Authoritative `/read-states/ack` first; identical automatic entry shape; `/read-states/ack-bulk` fallback only for 404/405 |
| Gateway compression | `features/gateway/transport/PooledGatewayConnection.ts`, `GatewaySocket.ts`; `fluxer_app/rust/libfluxcore/src/zstd_stream.rs` | Reuses the exact Rust streaming codec; real bidirectional zstd sockets; fresh dictionary per connection; source compression fallback |
| Identify/resume | Same gateway sources; `ClientInfo.ts` | Flags 14, version 1, selected guild/presence/locale builders, resume token/session/sequence, same-socket rejected-resume Identify, restored presence, source heartbeat/backoff limits |
| READY/state/events | `fluxer_gateway/src/session/session_ready_collect.erl`; desktop gateway READY handler, user/member stores; `session_dispatch_voice.erl` | Core checked-in fixture resolves ID-only users, preserves own private fields; batched reactions/guild sync/channel and role updates |
| Search/pins/history | `features/search/utils/SearchUtils.ts`; `ChannelSearchPolling.ts`, `useChannelSearch.ts`; `ChannelPinsCommands.tsx`; `MessageCommands.tsx`; `RestTransport.ts` | `navigation.rs` and loopback `tests/navigation.rs` preserve search HTTP status/body, explicit pin routes/timestamp pages, history cursor omissions and source retry policy |
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
- REST coverage remains partial: rich messages, thread/forum
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
- The pinned SDK now mirrors the existing JS `forceStereo` publication option,
  AddTrack stereo/audio-feature metadata and track-specific Opus SDP negotiation.
  Shared audio explicitly requests stereo; microphone stereo remains opt-in.
  Real native capture tests verify separate left/right tones after remote decoding
  in plain and E2EE calls, rather than assuming stereo PCM implies stereo transport.
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

## Attachment upload contract

`UploadAttachment` holds a selected local path and metadata. The client owns
privacy and entitlement policy; the protocol only opens selected regular files.
`supports_presigned_attachment_uploads()` reads the discovered
`presigned_attachment_uploads` feature. `preupload_attachments()` only prepares
storage objects and returns server attachment DTOs; it never posts a message or
falls back to a multipart message. The controller may call it only when the
user's preupload preference and instance feature both permit it. Background
failure is retried through the full send path rather than silently posting.

The split staged-batch API mirrors `MessageQueue.tsx:487–576`:
`plan_attachments()` sends one shared planning POST and returns opaque
`PlannedAttachment` handles in the original file order, resolving response IDs
rather than response-array positions. The handles redact storage URLs from Debug
and are bound to the original instance, channel and file snapshot. Missing IDs
remain per-file errors, so a malformed sibling response cannot silently cancel
another valid transfer. `upload_planned_attachment()` independently performs
one file's PUTs and, when needed, its own singleton multipart completion. It
never posts a message or falls back. The controller may run these per-file
transfers concurrently. Each uses its own cancellation token; the shared
planning token is cancelled only when its last staged member is removed, as in
`TextareaAttachmentUploadCancellation.ts`. That membership bookkeeping belongs
to the controller. Textarea per-file progress reports retry resets directly;
the full-message outer progress retains the source's monotonic per-file maximum.

Existing source uses stable `CloudAttachment.id` values during staging and
zero-based decimal index IDs for a final message. When reusing completed staged
uploads, the controller remaps IDs and merges only `upload_filename`,
`file_size` and `content_type` into current final attachment metadata, as in
`MessageQueue.tsx:653–706`. `build_message_body()` preserves reply, mention and
nonce fields; it omits content that `MessageValidators.ts` classifies as entirely
invisible. `send_message_body()` sends a prepared JSON message without uploading
again. `send_message_with_attachments()` owns the full fallback decision.

For presigned uploads, the API plan is an authenticated POST containing
`{attachments:[{id,filename,file_size,content_type}]}`. An empty selected MIME type
becomes `application/octet-stream`. File order is preserved while response plans
are mapped by ID. Each file is sent sequentially; multipart parts use four
concurrent requests, byte offsets from array index times `part_size`, and at
most four attempts. Only network failures and HTTP 408/425/429/5xx retry, using
the existing 500–8000 ms exponential jitter and a retry hint capped at 30 s.
The source's outer per-file progress is monotonic even when an internal part
resets on retry (`AttachmentUploadPlan.ts:181–188`). Native byte progress counts
chunks supplied to the HTTP body, with bounded transport read-ahead; a file is
marked complete only after its PUT succeeds. Completion is a separate API POST
with `{uploads:[{upload_filename,upload_id}]}`; no ETags are invented.

Storage requests use a separate reqwest client with no account, Origin,
installation/capability, cookie, User-Agent or compression headers, and redirects
are disabled. Only the plan's Content-Type and actual Content-Length are added.
Storage capability URLs are excluded from transport errors. API requests retain
the exact first-party auth, discovered webapp Origin and metadata headers.
Neither scoped upload client adds an overall timeout, matching RestTransport's
zero default timeout. Cancellation interrupts requests, error-body reads and
retry delays. At most 64 KiB is read per file-body chunk; a multipart part opens
and seeks its own descriptor, so even large selected files are never read into
one in-memory buffer. File size/modified-time checks reject stale selections.

Fallback follows `MessageQueue.tsx:725–845`:

- With the feature disabled, send one multipart message directly.
- Plan failure normally falls back; endpoint-unreachable/network/5xx failures
  cannot fall back when the sum of file bytes exceeds 25 MiB.
- Missing plan IDs are fatal. PUT failure can fall back only at or below 25 MiB.
- Multipart completion failure and cancellation never fall back.
- The final message request is not automatically retried after uncertain receipt.

A multipart message has a `payload_json` text field and `files[index]` streamed
file parts. Its attachment metadata omits uploaded-object fields. Presigned final
JSON includes those fields along with filename, title and optional
caption/flags/duration/waveform. There is deliberately no abort HTTP endpoint:
existing `abortRemainingMultipartUploads()` leaves unfinished storage uploads
for server garbage collection. Local cancellation follows that same behavior.

Fixtures use only synthetic temporary files and separate loopback API/storage
servers. They do not send local files to an external instance. Remaining upload
work includes desktop/native interoperability, complete media preprocessing and
privacy/preupload UI parity, and broader rich-message composition; this transport
contract alone does not close those areas.

## Search, pins and history transport

`search_messages(params)` posts the already-normalized API JSON to
`/search/messages`. The caller owns query parsing and merges the source defaults
(`hits_per_page:25`, `page:1`), context, scope, sort and mature-content policy.
The protocol neither remaps camelCase names nor adds missing filter fields.
`SearchUtils.ts:195–235,474–520` sends the context channel/guild when present;
a nonempty channel filter is retained as `channel_id` and also sent as
`channel_ids`. Undefined fields are omitted while false and empty arrays remain.

`SearchResponse` preserves HTTP status, unmodified JSON and the response retry
hint. It retains `channels`, `threads` and `members` for controller ingestion.
Indexing is exactly `body.indexing === true`, independent of status: both 200 and
202 are accepted. A 202 response without the body flag is not automatically
classified as indexing. Missing/nonobject bodies fail. No request automatically
polls, switches endpoints or falls back to a local search. The source
`useChannelSearch.ts` polling path repeats page 1 and
`ChannelSearchPolling.ts` uses 1500 ms times 1.25 to the poll count, capped at
5000 ms; `channel_search_poll_interval()` exposes that policy without scheduling
network work. A new query, reset or account change must cancel the controller's
old task and discard stale results. The source indexing path does not change its
poll interval based on response retry headers.

`pins_page(channel,before)` GETs `/channels/:channel/messages/pins?limit=25`.
The optional cursor is the oldest `pinned_at` timestamp; it is not the pinned
message ID. The raw response is `{items:[{message,pinned_at}],has_more}`. An empty
body becomes an empty page, matching `ChannelPinsCommands.tsx:33–64`.
`pin_message` uses bodyless PUT `/channels/:channel/pins/:message` and
`unpin_message` uses bodyless DELETE of that same route. Neither operation
optimistically changes the wire response or invents a follow-up route.
`acknowledge_pins` is an explicit bodyless POST `/channels/:channel/pins/ack`.
Fetching a page never sends it. The existing `ChannelPinsContent.tsx:94–96`
mount effect calls `ReadStates.ackPins`, whose
`applyPinAck` (`ReadStates.ts:914–923`) suppresses the request when the locally
acknowledged timestamp already equals the latest pin timestamp. The controller
owns that state and UI visibility policy.

The checked-in server uses `MessagePinService.ts:180–194`'s shared
`loadPinnableMessage` for both pin and unpin: a guild requires `PIN_MESSAGES`.
The message context action (`MessageActionUtils.tsx:239–241`) matches it. The
existing pins-list UI's `MANAGE_MESSAGES` unpin visibility predicate differs;
that UI predicate does not authorize the server operation. Protocol calls retain
normal server permission errors rather than treating the UI predicate as success.

`messages_page(channel,before,after,around,limit)` GETs the existing messages
route with each supplied cursor and the exact requested limit. Absent cursor
values are omitted; limits are not silently clamped. The server schema accepts
1–100 and defaults to 50; the source native navigation caller should normally
request 50 (`MAX_MESSAGES_PER_CHANNEL`). An empty successful body is an empty
array. This is a transport page, not a declaration that history is contiguous:
the controller owns jump/return context, edge detection and page merging.

`MessageCommands.tsx:269–282` explicitly asks for two retries. This means up to
three requests, with the exact `RestTransport.ts:47,1019–1025` retryable status
set: 502, 504, 507, 522, 523, 524, 598, 599, plus network failures. Delays start at
1000 then 2000 ms, with 25 percent jitter. Generic 500/503 and strict 429 do not
retry. Search, pin fetches and mutations retain the default zero retries.
429 hints follow the existing transport's unit convention:
`recordPacing` (`RestTransport.ts:640–655`) treats positive
`X-RateLimit-Reset-After` as milliseconds and prioritizes it over `Retry-After`
seconds or HTTP date. Hints are exposed/surfaced, not an implicit indexing delay.
The complete shared REST pacing cache and auto-retry modes remain separate work.
The browser transport also waits for `navigator.onLine` when retries are enabled;
native has no equivalent connectivity gate yet and performs the finite HTTP
attempts directly. It does not invent an OS-online heuristic.

These requests preserve existing first-party headers and discovered webapp
Origin rules (GET omits Origin; mutations/search include it). Scoped clients
retain the source's zero overall timeout; responses are bounded by the native
16 MiB reader, and dropping the task cancels request/body reads and history
retry delays. Fixtures verify actual loopback requests, finite retry exhaustion,
status/body preservation and cancellation of a stalled search response. Tests
use synthetic messages and never mutate a production pin.
