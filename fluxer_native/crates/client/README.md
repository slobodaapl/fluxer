# Native client controller

`Controller::spawn()` owns an asynchronous worker and exposes `send(Command)`
and nonblocking `try_recv()` to GPUI. The worker is the sole writer of
`fluxer_core::ClientState`. Network requests, gateway dispatches, native audio,
and screen capture stay off the rendering thread.

The normal login flow discovers the instance, signs in with password or a
session token, fetches account/community data, and starts the resumable gateway.
TOTP challenges are surfaced as `MfaRequired` for `LoginMfa`. Account switches and
logout clear account state, drafts, voice credentials, and active media before
starting another session. Tokens are memory-only. `Demo` is explicitly offline;
it never claims to transmit voice or creates an audio engine.

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

Video frames are coalesced per track before delivery to the UI. Dropping the
controller cancels its worker even during a network request, causing native
media handles to stop capture. Other presentation events are ordered through a
standard channel and should be drained on each UI polling tick.

Tests include a real loopback HTTP/WebSocket text workflow and deterministic
voice-correlation checks. They never open a microphone. Run `cargo test -p
fluxer-client` with local socket permissions. Auth ceremonies beyond password/
TOTP, persistence, rich composer features, and platform packaging remain separate
client migration work.
