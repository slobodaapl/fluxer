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

* `room/options.rs`: add the explicit `force_stereo` publishing option, false
  by default. Native shared audio sets it true; microphone publishing sets it
  only when the user actually requests eligible stereo capture.
* `room/participant/local_participant.rs`: send both the legacy AddTrackRequest
  stereo flag and TF_STEREO, plus TF_NO_DTX for non-DTX audio, matching the
  existing client's typed wire request. Mono tracks retain false/no stereo flag.
* `rtc_engine/opus_stereo.rs`: per-audio-section Opus SDP helper. Only selected
  sender CID/MIDs or explicitly offered remote stereo MIDs gain `stereo=1` and
  `sprop-stereo=1`; exact-key replacement is idempotent, preserves line endings,
  and leaves mono microphone and video sections alone.
* `rtc_engine/peer_transport.rs`: apply that policy to dual-PC publisher offers
  and remote answers and to subscriber answers. Initial offers locate tracks
  by msid CID before transceiver MIDs exist; the selected offered MIDs are kept
  for the answer. Subscriber stereo follows the remote offer's sprop-stereo MIDs.
* `rtc_engine/rtc_session.rs`: register/remove each sender's explicit stereo
  intent with its transport, so unpublishing or changing tracks clears stale
  MID intent without globally forcing mono microphones into stereo.
* `rtc_engine/mod.rs`: declare the Opus negotiation helper module.

Reference: the existing desktop's bundled livekit-client
`src/room/track/RemoteTrackPublication.ts:46-87` retains its track when changing
subscription demand. The upstream Rust eager set_track(None) loses the receiver
and deletes the E2EE decryptor, while a reused receiver need not signal onTrack.
The private cache preserves the Rust SDK's TrackSubscribed/TrackUnsubscribed
contract and restores its normal E2EE lifecycle rather than bypassing it.

Validation: native media unit tests, existing synthetic SDK audio/video loopback,
and actual MediaEngine synthetic screen watch/unwatch/rewatch/republish fixtures
(including a shared E2EE passphrase and changed source pixels after rewatch).

Stereo references in the bundled `fluxer_app/pkgs/livekit-client` source:
`LocalParticipant.ts:856,1007-1026,1149` derives forceStereo and sends the stereo
wire hints. `PCTransport.ts:228-253,376-413,786-806,935-969` scopes stereo by
CID/MID, configures both Opus parameters, and mirrors offered stereo into the
subscriber answer. Merely declaring opus/48000/2 does not request stereo.

Stereo validation: four isolated SDK unit checks passed for typed AddTrack
features, track-scoped publisher offer/answer negotiation, remote offered MIDs,
and fmtp creation/line endings. Actual application-capture tests publish distinct
left-440/right-880 Hz signals through the production PipeWire capture engine and
MediaEngine, including E2EE. They require >4x correct-channel separation and also
check gain, explicit mute/unmute, outgoing audio while locally deafened, final
unpublication, and cleanup of test-owned capture nodes. This rejects mono upmix.
