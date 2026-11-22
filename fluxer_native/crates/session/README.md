# Native session persistence

`SessionStore` is a Send + Sync interface with boxed Send futures for load, save,
and clear. `SavedSession` contains the pinned instance, account token and profile,
locale, selected guild/channel, and validity flag. Its Debug output omits the
token, profile, instance document, and account identity. Errors contain static
messages without backend details or record contents.

On Linux, `OsSessionStore` uses oo7 0.5 with `async-std` and `native_crypto`.
It explicitly requests encrypted Secret Service negotiation; it does not use
oo7's automatic plaintext D-Bus negotiation fallback. The vault provider manages
storage at rest. The application writes no plaintext session file and does not
fall back to memory when OS storage fails. Startup looks for the saved session
in the default collection and lets Secret Service prompt to unlock the matching
item if required. Dismissing the prompt preserves the record for a later retry.
On macOS, the store uses a local, non-synchronizing Keychain generic password
with service `app.fluxer.Native` and account `active-session`. macOS controls
access prompts and storage encryption. Other platforms return `Unsupported`.

The production item is scoped by **application = app.fluxer.Native** and
**kind = active-session**. Only matching items are searched, replaced, or
deleted. There is no broad credential enumeration, migration of another app's
records, or automatic broken-item cleanup. Multiple matching items are reported
as invalid instead of choosing an arbitrary account.

One versioned secret blob contains all session fields. The codec checks the
qualified account key (`api URL without trailing slashes::user ID`), profile ID
when present, token/header bounds, URL schemes and credentials, versions,
selections, and size limits. A full discovery document must project to the exact
stored API client endpoint, gateway, web app, code version, and feature values.
The codec performs no discovery or referral and never sends a token anywhere.
Explicit local-development instances with a null discovery document remain
pinned to their supplied typed endpoints. Temporary serialized buffers and token
strings are zeroized when dropped.

Callers decide restoration policy. A `valid = false` record is retained for
account display and must not trigger automatic authentication. A cold restore
must start from an empty state and identify through the gateway, rather than
resume a nonexistent message cache. Network discovery, authentication policy,
operation timeouts, and logout behavior belong to the client controller.

`MemorySessionStore` clones share an encoded in-memory record so tests can
recreate workers without touching the OS vault. Independent instances remain
isolated. This is an injectable test/ephemeral store, not an error fallback.

Normal tests exercise codec validation, instance isolation, invalid-account
retention, and worker-like memory recreation. The ignored vault test creates
unique synthetic application scopes, uses fake credentials, and cleans up only
those scopes. It never searches production session attributes or another app's
data:

```sh
FLUXER_SESSION_VAULT_TEST=1 cargo test --manifest-path fluxer_native/Cargo.toml \
  -p fluxer-session synthetic_vault_roundtrip_is_scoped_and_cleans_up -- --ignored
```
