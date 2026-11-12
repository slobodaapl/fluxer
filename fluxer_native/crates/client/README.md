# Native client controller

`Controller::spawn()` owns an asynchronous worker and exposes `send(Command)`
and nonblocking `try_recv()` to GPUI. The worker is the sole writer of
`fluxer_core::ClientState`. Network requests, gateway dispatches, native audio,
and screen capture stay off the rendering thread.

The normal login flow discovers the instance, signs in with password or a
session token and starts the resumable gateway. READY supplies account/community
state; fresh password/MFA sessions do not issue a redundant current-user fetch.
Explicit token login validates the token before starting the gateway.
TOTP challenges are surfaced as `MfaRequired` for `LoginMfa`. IP authorization
continues with the desktop email polling/resend flow and can be cancelled.
Account transitions clear runtime state, drafts, voice credentials and active
media before starting another session. Completed logins save the token and
instance through `fluxer-session` using the OS credential vault. Quitting keeps
the saved account; explicit logout clears it before best-effort server logout.
`Demo` stays offline and preserves the saved real account.

Text operations load channel history, fetch older pages, send/edit/delete,
toggle reactions through the server, acknowledge reads, and send typing events.
Successful sends and edits have explicit confirmation events so a UI retains
its composer on failure. REST message responses merge with gateway confirmations
by message ID. Live reaction state comes from gateway deltas. Selecting a channel
marks its local read state after history loads; an acknowledgement failure is
shown as an error and the server read state may remain behind.

Voice joins require a real authenticated gateway. The controller correlates
voice credentials to the requested channel and server connection ID; credentials
that arrive before the own voice-state event are buffered until they match.
It connects `fluxer-media` with the server-issued token and E2EE key. Join timeouts,
failed media initialization, disconnects, removed channels, and known permission
loss stop capture and release voice state. Server mute/deafen remains enforced
when the user changes local controls. Mixer gains, device selection, DSP, PTT,
VAD, individual participant controls, microphone meters, screen sharing, and
system/application audio route to the media engine.

Video frames are coalesced per track before delivery to the UI. Stopping a watch
evicts cached pixels and acknowledges local intent before SDK/network work.
Lifecycle events are drained before coalesced frames. UI consumers use
`try_recv_control` with a fixed budget, then `drain_frames` exactly once per
presentation tick. The atomic snapshot cannot refill while frames are rendered;
a sustained producer therefore cannot keep the UI in a 128-frame conversion loop. Watch generations reject
frames and end events queued before a stop/rewatch, even for a reused track SID.
Transport replacement preserves watch intent while releasing old pixels.
`SetStreamAudio` changes share-audio gain/mute independently of participant voice. Dropping the
controller cancels its worker even during a network request, causing native
media handles to stop capture. Other presentation events are ordered through a
standard channel and should be drained on each UI polling tick.

Tests include a real loopback HTTP/WebSocket text workflow and deterministic
voice-correlation checks. They never open a microphone. Run `cargo test -p
fluxer-client` with local socket permissions. Native passkey/SSO ceremonies, multi-account switching, rich composer features
and platform packaging remain separate client migration work.

BrowserLogin uses the existing desktop CODE handoff, with private proof storage,
expiry, delayed polling and cancellation. The UI receives only the public code
and browser URL. Stream reception requires explicit WatchStream intent; the
controller synchronizes the corresponding viewer_stream_keys on all voice state
updates and clears subscriptions on leave. Mesh calls fail explicitly before
capture because the native mesh backend is still unimplemented.

Watch controls are advertised from the joined channel's gateway `self_stream`
roster, including streams whose SFU publication metadata has not arrived yet.
Transient metadata loss cannot revoke a gateway-live watch; an authoritative
stream stop, move or leave removes availability and viewer keys. The local
connection is excluded while the same account's other devices remain watchable.

Cold restore follows AuthSessionRestorer: refresh the saved instance authority,
fall back to its pinned snapshot when discovery is unreachable, and IDENTIFY for
full READY. It does not GET /users/@me or RESUME without a hydrated gateway cache.
The saved identity must match READY. Close 4004 marks the stored record invalid;
transient network failures preserve it. Restore I/O is cancellable; it never joins
a call or opens capture. `Controller::spawn_with_store` injects isolated test
persistence without accessing the real OS vault.
