# Fluxer native protocol

`fluxer-protocol` implements native HTTP and WebSocket transport against the
existing Fluxer services. It does not replace those services.

```rust,no_run
use fluxer_protocol::{Client, Instance, LoginIdentifier, LoginOutcome};

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let instance = Instance::discover("https://your-instance.example").await?;
let client = Client::new(instance)?;
match client.login(LoginIdentifier::Email("you@example.com".into()), "password").await? {
    LoginOutcome::Authenticated { token, .. } => {
        let client = client.with_token(token);
        // Start Gateway with this token. READY supplies communities and channels.
        let _ = client;
    }
    LoginOutcome::Mfa { ticket, .. } => { /* prompt for TOTP, then login_mfa_totp */ }
    LoginOutcome::IpAuthorization { .. } => { /* display email challenge and poll_ip_authorization */ }
}
# Ok(())
# }
```

The REST API keeps the advertised API path and code version, uses Fluxer's raw
session token in `Authorization`, and never follows redirects. JSON success
responses are `serde_json::Value`; HTTP failures and rate limits are typed.
Mutations are never automatically retried. A generic `Client::request` is
available for routes whose UI has not yet been migrated.

Instance discovery tries `/.well-known/fluxer`, the supplied path's discovery
document, and `/api/.well-known/fluxer`. The response is capped at 1 MiB and must
declare its own API origin. Unknown instance codenames are rejected.

`Gateway::run` owns hello/identify/resume, heartbeat acknowledgements, bounded
socket writes, reconnect backoff, invalid-session reset, and terminal close
handling. It uses the existing desktop zstd streaming codec in both directions,
with the desktop compression-failure fallback. Drain its event receiver promptly.
Send user commands after `GatewayEvent::Connected`; stale commands issued while
disconnected are discarded. Dropping the command sender or event receiver stops
the task; `GatewayCommand::Close` disconnects explicitly. Debug output redacts
session tokens and raw gateway dispatch data.

Voice negotiation uses `GatewayCommand::VoiceState(VoiceStateUpdate)` (opcode 4).
Join with the selected guild/channel and no `connection_id`. Deserialize the
resulting `VOICE_SERVER_UPDATE` into `VoiceServerUpdate`, match its channel and
connection attempt, then connect the native LiveKit media engine to `endpoint`
using `token`. Reuse the server's `connection_id` for mute/deafen/video/screen
updates and disconnection. Media capture, playback, codec processing, screen
selection, and E2EE are the media engine's responsibilities. The gateway reports
`e2ee_capable: false` by default.

Authentication currently supports password login, TOTP/backup code challenges,
IP authorization with email resend/poll continuation, and the desktop browser
handoff CODE path. Passkey ceremonies, browser SSO,
registration, CAPTCHA resolution, advanced uploads,
and OS credential persistence need dedicated client workflows.

Run `cargo test -p fluxer-protocol` from `fluxer_native`. Integration tests use
loopback HTTP/WebSocket servers and require permission to bind local sockets.
They verify real request headers/body routing, rate limits, redirect refusal,
gateway authentication, heartbeat exchange, voice signaling, and session resume.

See [CONTRACT.md](CONTRACT.md) for source references, tested desktop interactions,
and remaining compatibility gaps.
