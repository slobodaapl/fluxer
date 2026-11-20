# Private Linux audio smoke test

The smoke test runs native media probes with synthetic PipeWire sources and
sinks. It starts a private D-Bus session, PipeWire, the WirePlumber policy
profile, and the PulseAudio compatibility daemon. It uses a temporary runtime
and separate XDG configuration, cache, and state directories. The preflight
connects to the private socket and requires an empty PulseAudio card list before
running any media probe.

From the repository root, run the preflight with Bash, `dbus-run-session`,
PipeWire, WirePlumber with its `policy` profile, `pipewire-pulse`, `pactl`, and
Python 3 installed.

```bash
fluxer_native/scripts/qa/private_audio_smoke.sh /tmp/fluxer-audio-preflight --preflight
```

The full run additionally needs Cargo, `curl`, GNU `timeout`, `pacat`, and
`pw-dump`. Start a separate LiveKit development server bound to
`127.0.0.1:7880` with its loopback node IP and default development credentials
(`devkey` and `secret`). Run it outside the private D-Bus session. The smoke
script checks the local listener and leaves the server lifecycle to its caller.

```bash
CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=/tmp/fluxer-audio-target \
    FLUXER_AUDIO_SMOKE_TIMEOUT=300s \
    fluxer_native/scripts/qa/private_audio_smoke.sh /tmp/fluxer-audio-smoke
```

Cargo inherits the caller's environment, including `CARGO_TARGET_DIR` and
`CARGO_NET_OFFLINE`. The per-scenario timeout defaults to 160 seconds. Allow
more time for an initial build. Use a fresh artifact directory for each run.

| Scenario | Existing probe |
| --- | --- |
| Loopback | Two synthetic participants exchange audio in a local LiveKit room. |
| Microphone | A synthetic input reaches the receiver. |
| Output | Playback follows the selected synthetic output. |
| Watch | The screen-share audio watch follows the selected synthetic output. |
| Share | A synthetic local application source reaches its subscriber. |

The artifact directory keeps daemon logs, PulseAudio socket information, the
card inventory, and a log for each scenario. The script reports each result,
returns a failure if any scenario fails, and shuts down the private audio
daemons and removes their runtime directory on exit.
