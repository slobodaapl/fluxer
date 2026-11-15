# StreamDeck and local call control

Linux native Fluxer exposes a session-bus API while the real application is
running. A StreamDeck host that can run a command can invoke it without focusing
Fluxer or injecting a shortcut into a game. Demo windows do not claim this name.

Configure a button to run:

```sh
/home/alyx/Development/tmp/fluxer/fluxer_native/scripts/fluxer-call toggle-mute
```

For a dedicated mute button use `mute`; it always requests muted rather than
toggling. Other commands are `unmute`, `toggle-deafen`, `deafen`, `undeafen`,
`disconnect`, `stop-sharing`, `status` and `watch`. Methods return an error if
there is no current call or the call changed before the action executed. The
script requires `gdbus` (GLib's D-Bus command-line client).

Equivalent direct call:

```sh
gdbus call --session --dest app.fluxer.Native \
  --object-path /app/fluxer/Native/Call \
  --method app.fluxer.Native.Call1.ToggleMute
```

## Interface

Well-known name: `app.fluxer.Native`. Object: `/app/fluxer/Native/Call`.
Interface: `app.fluxer.Native.Call1`. `ApiVersion` is 1.

| Method | Arguments | Result |
|---|---|---|
| ToggleMute / ToggleDeafen | none | Worker-acknowledged action or D-Bus error |
| SetMuted / SetDeafened | boolean | Idempotent requested local state |
| Disconnect / StopSharing | none | Stop existing call/share; never start one |

Read-only properties: `InCall`, `Status` (disconnected/connecting/connected/
reconnecting), `CallGeneration`, `Muted`, `Deafened`, `EffectiveMuted`,
`EffectiveDeafened`, and `ScreenSharing`. Use effective properties for a button's
indicator: a server mute or deafen remains effective even after a local unmute.
Changes are broadcast using the standard `PropertiesChanged` interface, polled
from the authoritative actor state at 100 ms; a subscriber can update a button
image without repeated requests. `status` reads all properties; `watch` prints
signals. Clients with their own property caches should consume those signals.

The worker serializes toggles and verifies the observed account/call generation,
deadline and live reply receiver. A timed-out/abandoned request cannot sit in a
queue and later unmute a different call. Frontend waits are capped at three
seconds, with at most sixteen concurrent actions. An existing bus owner cannot
be replaced by a second native process. This is a per-login-session local API,
not a network listener. It exposes no tokens/messages and has no join, microphone
test, screen-picker or start-capture method.

This is the native external-control contract; it dispatches the same existing
Fluxer voice commands as the UI. It does not add another client/server protocol.
The running app must already be authenticated and in a call. No D-Bus activation
file is installed, so invoking a button does not start Fluxer or join a call.

## Platforms and references

The [D-Bus API guidelines](https://dbus.freedesktop.org/doc/dbus-api-design.html)
recommend standard properties/change signals; the implementation uses existing
[zbus 5.19](https://docs.rs/zbus/5.19.0/zbus/connection/struct.Builder.html).

For Windows, a current-user named-pipe adapter is a practical first transport;
COM can be added for hosts needing automation objects. Both can reuse the same
actor/action/state contract. Microsoft's documentation covers
[named pipes](https://learn.microsoft.com/windows/win32/ipc/named-pipes) and
[desktop IPC including out-of-process COM](https://learn.microsoft.com/windows/apps/develop/communication/interprocess-communication).

For macOS, XPC is the native process-communication mechanism; Apple Events can
provide a scriptable front door for launchers/automation. Registration, sandbox
entitlements and host integration need platform-specific implementation and
testing. References: [XPC](https://developer.apple.com/documentation/xpc) and
[Apple's interapplication communication documentation](https://developer.apple.com/library/archive/documentation/mac/pdf/Interapplication_Communication/Intro_to_IAC.pdf).
Windows/macOS adapters and a dedicated StreamDeck plugin are future work; the
Linux command interface and state feedback are implemented now.

## Validation

The desktop test suite runs a real private D-Bus daemon and exercises RPCs,
property reads/change signals, duplicate-owner refusal, no-call errors and
service teardown without touching the user's session bus or real microphone.
Additional actor tests cover stale account/generation, expiry, dropped replies
and action ordering. A delayed backend test checks frontend timeout/cancellation
and bounded concurrency. Actual StreamDeck hardware and a real live call remain
manual acceptance checks.
