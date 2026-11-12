`ready_user_refs.json` is a synthetic, redacted compatibility fixture built from
these authoritative existing server/client projection paths, not a captured
account session:

- `fluxer_gateway/src/session/session_ready_collect.erl:198-226` replaces guild
  member users with `{id}` references; `session_ready_dispatch.erl:147-153`
  supplies the top-level full users and private self user.
- `session_ready_collect.erl:229-245` strips relationship.user and keeps its ID.
- `fluxer_gateway/src/guild/guild_data.erl:397-409` wraps guild properties and
  includes roles, channels, members, voice states and presences.
- `fluxer_gateway/src/session/session_ready.erl:70-78` emits unavailable guilds
  containing only ID and availability flags.
- `fluxer_app/src/features/gateway/events/GatewayReady.ts:151-152` loads private
  self and full users before constructing members; `GuildMember.ts:40-44` resolves
  member user references from this cache. `Users.ts:195-200` preserves private self.

Private-channel recipient and presence ID-only rows exercise the same cached
reference handling. The current READY server generally emits full DM recipients;
ID-only offline presence dispatches are explicitly emitted by
`presence_broadcast_subscriptions.erl:273-286`. These fixture rows do not assert
that every READY field is stripped by session_ready_collect.
