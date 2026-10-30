# Fluxer patch to tauri 2.12.5

Source: crates.io `tauri` 2.12.5, unmodified except for two hunks.

## `src/webview/mod.rs`, `Webview::on_message`

Upstream skips the ACL check for `plugin:__TAURI_CHANNEL__|fetch` for every origin. On Windows
wry adds Tauri's initialization scripts to subframes as well, so a remote iframe (a YouTube
embed, or an ad frame inside it) has `__TAURI_INTERNALS__` with the invoke key and could fetch,
and so remove, queued channel data meant for the app document. The patch keeps the exemption
for local origins only; remote origins go through the ACL like every other command, and the
shell grants them none.

## `src/ipc/protocol.rs`, `handle_ipc_message`

On Linux the postMessage IPC path is refused. WebKitGTK exposes wry's `ipc` script message
handler to every frame, and wry builds the request URL from the top-level page URI, so a message
posted by a remote iframe is attributed to the app document and treated as a local origin. The
invoke key does not stop this: Tauri injects it into every top-level document, and a remote page
loaded top-level in any of the app's windows can read it back through its own `fetch`. The
custom protocol path, which Tauri uses unless a request to it fails, carries each frame's own
`Origin` and stays the only way in.

Drop this vendored copy once upstream ships equivalent checks.
