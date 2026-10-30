// SPDX-License-Identifier: AGPL-3.0-or-later

//! The `native-gateway-transport:*` channels of NativeGatewayTransport.ts as Tauri commands.
//! `create` binds the calling document (`capture`); the others only act on connections the
//! calling document owns. A missing request reads as `null` and is refused as malformed.
//!
//! `send_binary` takes its payload as the raw request body, with the connection id and
//! document token in headers, so frames skip JSON number arrays.

use serde_json::Value;
use tauri::ipc::{InvokeBody, Request};
use tauri::{State, Webview};

use super::transport::{CreateResult, EventChannel, GatewayTransport};
use crate::documents::Caller;
use crate::error::BridgeError;

const CONNECTION_ID_HEADER: &str = "x-fluxer-connection-id";
const DOCUMENT_TOKEN_HEADER: &str = "x-fluxer-document-token";

type Transport<'a> = State<'a, GatewayTransport>;
type Reply<T> = Result<T, BridgeError>;

fn arg(request: Option<Value>) -> Value {
    request.unwrap_or_default()
}

#[tauri::command]
pub fn native_gateway_transport_is_available(transport: Transport<'_>) -> bool {
    transport.is_available()
}

#[tauri::command]
pub async fn native_gateway_transport_create(
    webview: Webview,
    transport: Transport<'_>,
    document_token: String,
    request: Option<Value>,
    channel: EventChannel,
) -> Reply<CreateResult> {
    transport
        .create(
            &Caller::new(&webview, document_token),
            &arg(request),
            channel,
        )
        .await
}

#[tauri::command]
pub async fn native_gateway_transport_send_text(
    webview: Webview,
    transport: Transport<'_>,
    document_token: String,
    request: Option<Value>,
) -> Reply<()> {
    transport.send_text(&Caller::new(&webview, document_token), &arg(request))
}

#[tauri::command]
pub async fn native_gateway_transport_send_binary(
    webview: Webview,
    transport: Transport<'_>,
    request: Request<'_>,
) -> Reply<()> {
    let header = |name| {
        request
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
    };
    let caller = Caller::new(
        &webview,
        header(DOCUMENT_TOKEN_HEADER).unwrap_or_default().to_owned(),
    );
    let payload = match request.body() {
        InvokeBody::Raw(payload) => Some(payload.as_slice()),
        InvokeBody::Json(_) => None,
    };
    transport.send_binary(&caller, header(CONNECTION_ID_HEADER), payload)
}

#[tauri::command]
pub async fn native_gateway_transport_close(
    webview: Webview,
    transport: Transport<'_>,
    document_token: String,
    request: Option<Value>,
) -> Reply<()> {
    transport.close(&Caller::new(&webview, document_token), &arg(request))
}

#[tauri::command]
pub async fn native_gateway_transport_dispose(
    webview: Webview,
    transport: Transport<'_>,
    document_token: String,
    connection_id: Option<Value>,
) -> Reply<()> {
    transport.dispose(&Caller::new(&webview, document_token), &arg(connection_id))
}
