# Call recovery expansion

Reference behavior: `fluxer_app/src/features/voice/engine/VoiceChannelConnector.tsx` defines connect, disconnect, and sync voice-state payloads. `VoiceEngineV2AppConnectionHostAdapter.ts` and `VoiceEngineV2AppReconnectPolicy.ts` define the desktop reconnect lifecycle; `VoiceEngineV2AppScreenShareLiveKitFlows.ts` checks whether the previous capture remains usable. Native gateway drops voice commands before authentication (`crates/protocol/src/gateway.rs`), so the worker replays offline local voice changes and pending leave on authentication, in leave-before-current-state order. Account reset clears pending replay.

The native call actor exposes serialized, deadline- and account/generation-fenced remote controls. A caller that drops its reply before execution cancels the request. Local and remote controls use the same command handlers. Tests exercise stale requests, cancellation, repeated controls, leave ordering, and gateway reconnect replay without media hardware or account traffic.

Existing native device recovery retains requested input/output identifiers and rejects replacement when a usable selection cannot be found. Existing replacement stops a screen share and reports stopped rather than opening a new picker. Full media transport loss recovery, suspend/resume, a user-visible reshare-required notice, and cancellation during the currently awaited media connection remain unfinished. LiveKit reconnecting/reconnected events are surfaced but a terminal media disconnect currently leaves the call. No physical network, device, microphone, screen capture, or desktop acceptance was performed in this worktree.

Manual acceptance when a test account and hardware are available:

- Join, mute/deafen, change preferred input/output and watch a stream; interrupt and restore network, then check state and sound.
- During gateway reconnect change mute, then leave; confirm the old server connection leaves and no call silently returns.
- Start a share, replace or interrupt the transport, and confirm the share is visibly stopped and no picker or capture starts without an explicit user action.
- Exercise suspend/resume, missing preferred devices and device restoration; verify the selected identifiers and mixer/DSP preferences and record failures.
- Verify remote controls while changing accounts and leaving/rejoining the same channel, including delayed and abandoned requests.
