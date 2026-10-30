// SPDX-License-Identifier: AGPL-3.0-or-later

//! The vendored tauri patch (vendor/tauri/FLUXER_PATCH.md): remote frames may not fetch queued
//! channel data, which upstream let any origin do.

use tauri::ipc::{CallbackFn, InvokeBody};
use tauri::test::{INVOKE_KEY, get_ipc_response, mock_builder, mock_context, noop_assets};
use tauri::webview::InvokeRequest;
use tauri::{WebviewUrl, WebviewWindowBuilder};

const FETCH_CHANNEL_DATA_COMMAND: &str = "plugin:__TAURI_CHANNEL__|fetch";
const CHANNEL_ID_HEADER: &str = "Tauri-Channel-Id";

fn fetch_channel_data(url: &str) -> Result<tauri::ipc::InvokeResponseBody, serde_json::Value> {
    let app = mock_builder()
        .build(mock_context(noop_assets()))
        .expect("the mock app builds");
    let webview = WebviewWindowBuilder::new(&app, "main", WebviewUrl::default())
        .build()
        .expect("the mock webview builds");
    let mut headers = tauri::http::HeaderMap::new();
    headers.insert(CHANNEL_ID_HEADER, "0".parse().unwrap());
    get_ipc_response(
        &webview,
        InvokeRequest {
            cmd: FETCH_CHANNEL_DATA_COMMAND.to_owned(),
            callback: CallbackFn(0),
            error: CallbackFn(1),
            url: url.parse().unwrap(),
            body: InvokeBody::default(),
            headers,
            invoke_key: INVOKE_KEY.to_owned(),
        },
    )
}

fn refused_by_acl(result: &Result<tauri::ipc::InvokeResponseBody, serde_json::Value>) -> bool {
    result
        .as_ref()
        .is_err_and(|error| error.to_string().contains("not allowed"))
}

#[test]
fn remote_frames_cannot_fetch_channel_data() {
    let remote = fetch_channel_data("https://www.youtube.com/embed/abc");
    let local = fetch_channel_data("tauri://localhost");
    assert!(refused_by_acl(&remote), "a remote frame fetched channel data: {remote:?}");
    // The local request passes the ACL and reaches the channel store, which has nothing queued.
    assert!(!refused_by_acl(&local), "a local fetch was refused: {local:?}");
}
