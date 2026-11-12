# LiveKit Rust 0.10.1 compatibility patch

Source is the unmodified crates.io livekit 0.10.1 package except the changes
listed below. Upstream source headers and Apache-2.0 license are retained.
This directory is a workspace-local Cargo patch; registry caches are untouched.

* `room/publication/remote.rs`: retain the RTC handle privately during explicit
  unsubscribe. Resubscribe restores it through ordinary subscribed hooks so
  native renderers and E2EE decryptors are recreated even when WebRTC reuses a
  receiver/MID without another onTrack. Actual replacement and final publication
  removal invalidate the retained handle. Repeated false/true stays idempotent.
* `room/participant/remote_participant.rs`: send subscription updates through one
  FIFO worker per publication. Re-registering the publication reuses that queue;
  final removal drops it. The exact UpdateSubscription payload is unchanged.

Reference: the existing desktop's bundled livekit-client
`src/room/track/RemoteTrackPublication.ts:46-87` retains its track when changing
subscription demand. The upstream Rust eager set_track(None) loses the receiver
and deletes the E2EE decryptor, while a reused receiver need not signal onTrack.
The private cache preserves the Rust SDK's TrackSubscribed/TrackUnsubscribed
contract and restores its normal E2EE lifecycle rather than bypassing it.

Validation: native media unit tests, existing synthetic SDK audio/video loopback,
and actual MediaEngine synthetic screen watch/unwatch/rewatch/republish fixtures
(including a shared E2EE passphrase and changed source pixels after rewatch).
