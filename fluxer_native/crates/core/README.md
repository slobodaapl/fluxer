# Fluxer client core

Deterministic, UI-independent domain and state for the native client. Identifiers
and permission bitsets use decimal JSON strings without floating point conversion.
Known wire fields are modeled; extensible models preserve unknown fields.

Implemented: guild/channel/member/user/message caches, ordered history, partial
message/channel/member updates, reaction projection, read cursors and mention
counts, local drafts, navigation, relationships, presence, expiring typing,
multi-connection voice state, desired mute/deafen/camera/share controls, base
role/channel overwrite permissions, and offline demo data.

`voice_participants(channel)` projects all gateway connections in a channel,
including the current account and channels the local media SDK has not joined.
It retains simultaneous connections and provides exact LiveKit identities,
nickname precedence, account/member avatar hashes, avatar-unset flags, and raw
voice flags. Group rows by user ID for the sidebar; display individual connections
in the call pane. `is_current_user` does not identify the local device. Name order
is deterministic, case-insensitive and numeric; the existing web UI's
locale-specific ICU collation and per-mounted-list frozen-name/join-order
snapshots are not reproduced by this stateless helper. Missing users are exposed
with an ID label without inserting synthetic cache records.

`apply_gateway(event, data)` consumes one dispatch. The gateway transport must
filter duplicate/stale dispatch sequence numbers first. `READY` validates before
replacing state and rejects an account change; replace `ClientState` when switching
accounts. History loads do not create mention notifications. Local changes are
for offline interaction or confirmed REST operations, not an optimistic delivery
implementation. Local message IDs are exclusively for the offline demo.

Remaining parity work includes disk persistence and keychain integration,
optimistic outbox/reconciliation, cache eviction, all remaining gateway event
families, rich message/embed rendering, exhaustive user settings, thread write
lifecycle/membership checks, guest permissions and MFA elevation, timeout clock
handling, media transport, and server-authoritative moderation checks. Permission
helpers are UI affordances and do not replace API authorization.
