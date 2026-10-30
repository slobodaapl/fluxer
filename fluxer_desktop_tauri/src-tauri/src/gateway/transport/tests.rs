// SPDX-License-Identifier: AGPL-3.0-or-later

use std::sync::Arc;
use std::time::Duration;

use futures_util::{FutureExt, SinkExt, StreamExt};
use serde_json::{Value, json};
use tauri::ipc::{Channel, InvokeResponseBody};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use super::*;
use crate::documents::DocumentRegistry;
use crate::gateway::origin_registry::GatewayOriginRegistry;
use crate::runtime::test_support::{
    TOKEN_A, TOKEN_B, caller, loaded_documents, navigate, outbound,
};

const EVENT_TIMEOUT: Duration = Duration::from_secs(10);

fn connection_id(index: u16) -> String {
    format!(
        "gateway-renderer-0f8fad5b-d9cb-469f-a165-70867728950e:7c9e6679-7425-40de-944b-e07fc1f9{index:04x}"
    )
}

/// A loopback WebSocket server echoing every text and binary frame and every close.
async fn echo_server() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let Ok(mut socket) = tokio_tungstenite::accept_async(stream).await else {
                    return;
                };
                // tungstenite answers a close frame on its own while reading on.
                while let Some(Ok(message)) = socket.next().await {
                    if matches!(message, Message::Text(_) | Message::Binary(_))
                        && socket.send(message).await.is_err()
                    {
                        return;
                    }
                }
            });
        }
    });
    port
}

struct Harness {
    documents: Arc<DocumentRegistry>,
    transport: GatewayTransport,
    port: u16,
}

impl Harness {
    /// A transport whose registry already trusts the echo server's gateway, as a runtime plan
    /// for it would have recorded.
    async fn start(available: bool) -> Self {
        let port = echo_server().await;
        let documents = loaded_documents();
        let registry = GatewayOriginRegistry::new(
            Box::new(|_| async { Err("unreachable".to_owned()) }.boxed()),
            Box::new(|| async { Vec::new() }.boxed()),
            Box::new(std::time::Instant::now),
        );
        registry.record_anchor_gateway(
            &format!("http://127.0.0.1:{port}/api"),
            &format!("ws://127.0.0.1:{port}/gateway"),
        );
        let transport = GatewayTransport::new(documents.clone(), registry, outbound(), available);
        Self {
            documents,
            transport,
            port,
        }
    }

    fn url(&self) -> String {
        format!("ws://127.0.0.1:{}/gateway?v=1", self.port)
    }

    async fn create(
        &self,
        token: &str,
        id: &str,
        url: &str,
    ) -> Result<(CreateResult, Events), BridgeError> {
        let (sender, receiver) = mpsc::unbounded_channel();
        let channel = Channel::new(move |body| {
            let _ = sender.send(body);
            Ok(())
        });
        let result = self
            .transport
            .create(
                &caller(token),
                &json!({"connectionId": id, "url": url, "mode": "gateway"}),
                channel,
            )
            .await?;
        Ok((result, Events(receiver)))
    }

    async fn open(&self, id: &str) -> Events {
        let (_, mut events) = self.create(TOKEN_A, id, &self.url()).await.unwrap();
        assert_eq!(events.json().await["kind"], "open");
        events
    }
}

struct Events(mpsc::UnboundedReceiver<InvokeResponseBody>);

impl Events {
    async fn next(&mut self) -> Option<InvokeResponseBody> {
        tokio::time::timeout(EVENT_TIMEOUT, self.0.recv())
            .await
            .expect("an event or the channel's end arrives")
    }

    async fn json(&mut self) -> Value {
        match self.next().await {
            Some(InvokeResponseBody::Json(json)) => serde_json::from_str(&json).unwrap(),
            other => panic!("expected a JSON event, got {other:?}"),
        }
    }
}

fn error_name<T>(result: Result<T, BridgeError>) -> &'static str {
    match result {
        Ok(_) => panic!("the request was accepted"),
        Err(error) => error.name(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn relays_frames_both_ways_and_releases_the_connection_on_close() {
    let harness = Harness::start(true).await;
    let id = connection_id(1);
    let mut events = harness.open(&id).await;
    let transport = &harness.transport;

    transport
        .send_text(
            &caller(TOKEN_A),
            &json!({"connectionId": id, "payload": "hello"}),
        )
        .unwrap();
    assert_eq!(
        events.json().await,
        json!({"kind": "message", "data": "hello", "binary": null, "code": null,
               "reason": null, "wasClean": null, "message": null})
    );

    transport
        .send_binary(&caller(TOKEN_A), Some(&id), Some(&[1, 2, 3]))
        .unwrap();
    match events.next().await {
        Some(InvokeResponseBody::Raw(bytes)) => assert_eq!(bytes, vec![1, 2, 3]),
        other => panic!("expected a raw binary event, got {other:?}"),
    }

    transport
        .close(
            &caller(TOKEN_A),
            &json!({"connectionId": id, "code": 4000, "reason": "bye"}),
        )
        .unwrap();
    assert_eq!(
        events.json().await,
        json!({"kind": "close", "data": null, "binary": null, "code": 4000,
               "reason": "bye", "wasClean": true, "message": null})
    );
    // The record and its channel are gone once the close is delivered.
    assert!(events.next().await.is_none());
    assert_eq!(
        error_name(transport.send_text(
            &caller(TOKEN_A),
            &json!({"connectionId": id, "payload": "x"})
        )),
        "NativeGatewayTransportConnectionNotFoundError"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn only_the_owning_document_can_use_or_dispose_a_connection() {
    let harness = Harness::start(true).await;
    let id = connection_id(1);
    let mut events = harness.open(&id).await;
    let transport = &harness.transport;
    let stranger = caller(TOKEN_B);

    assert_eq!(
        error_name(transport.send_text(&stranger, &json!({"connectionId": id, "payload": "x"}))),
        "NativeGatewayTransportConnectionOwnerMismatchError"
    );
    assert_eq!(
        error_name(transport.dispose(&stranger, &json!(id))),
        "NativeGatewayTransportConnectionOwnerMismatchError"
    );
    assert_eq!(
        error_name(harness.create(TOKEN_A, &id, &harness.url()).await),
        "DuplicateNativeGatewayTransportConnectionError"
    );

    transport.dispose(&caller(TOKEN_A), &json!(id)).unwrap();
    assert!(events.next().await.is_none());
    // Disposing an unknown connection is a no-op.
    transport.dispose(&caller(TOKEN_A), &json!(id)).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn navigation_disposes_the_documents_connections_and_registry_state() {
    let harness = Harness::start(true).await;
    let mut first = harness.open(&connection_id(1)).await;
    let mut second = harness.open(&connection_id(2)).await;
    let owner = harness.documents.capture(&caller(TOKEN_A), "Test").unwrap();
    assert!(harness.transport.registry().tracks_document(&owner));

    navigate(&harness.documents, TOKEN_B);

    assert!(first.next().await.is_none());
    assert!(second.next().await.is_none());
    {
        let state = harness.transport.inner.state();
        assert!(state.connections.is_empty());
        assert!(state.owner_watchers.is_empty());
    }
    assert!(!harness.transport.registry().tracks_document(&owner));
    // The new document starts with its own budget and connections.
    let (_, mut events) = harness
        .create(TOKEN_B, &connection_id(3), &harness.url())
        .await
        .unwrap();
    assert_eq!(events.json().await["kind"], "open");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_document_holds_at_most_eight_connections() {
    let harness = Harness::start(true).await;
    let mut open = Vec::new();
    for index in 0..8 {
        open.push(harness.open(&connection_id(index)).await);
    }
    assert_eq!(
        error_name(
            harness
                .create(TOKEN_A, &connection_id(8), &harness.url())
                .await
        ),
        "NativeGatewayTransportConnectionLimitError"
    );
    harness
        .transport
        .dispose(&caller(TOKEN_A), &json!(connection_id(0)))
        .unwrap();
    open.push(harness.open(&connection_id(8)).await);
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_unadmitted_origins_and_releases_the_reservation() {
    let harness = Harness::start(true).await;
    let id = connection_id(1);
    assert_eq!(
        error_name(
            harness
                .create(TOKEN_A, &id, "wss://elsewhere.invalid/")
                .await
        ),
        "GatewayOriginNotAdmittedError"
    );
    {
        let state = harness.transport.inner.state();
        assert!(state.connections.is_empty() && state.reserved.is_empty());
    }
    // The id is free again.
    harness.open(&id).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_requests_from_non_current_documents_and_malformed_requests() {
    let harness = Harness::start(true).await;
    assert_eq!(
        error_name(
            harness
                .create(TOKEN_B, &connection_id(1), &harness.url())
                .await
        ),
        "RendererDocumentCaptureError"
    );
    assert_eq!(
        error_name(harness.create(TOKEN_A, "connection", &harness.url()).await),
        "InvalidNativeGatewayTransportRequestError"
    );
    assert_eq!(
        error_name(
            harness
                .transport
                .send_binary(&caller(TOKEN_A), Some(&connection_id(1)), None)
        ),
        "InvalidNativeGatewayTransportRequestError"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn cleanup_disposes_everything_and_refuses_later_requests() {
    let harness = Harness::start(true).await;
    let mut events = harness.open(&connection_id(1)).await;
    assert!(harness.transport.is_available());

    harness.transport.cleanup();

    assert!(events.next().await.is_none());
    assert!(!harness.transport.is_available());
    assert_eq!(
        error_name(
            harness
                .create(TOKEN_A, &connection_id(2), &harness.url())
                .await
        ),
        "NativeGatewayTransportShutdownError"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unavailable_transport_refuses_to_create() {
    let harness = Harness::start(false).await;
    assert!(!harness.transport.is_available());
    assert_eq!(
        error_name(
            harness
                .create(TOKEN_A, &connection_id(1), &harness.url())
                .await
        ),
        "NativeGatewayTransportUnavailableError"
    );
}

#[test]
fn oversized_native_events_break_the_contract() {
    let event = |reason: &str, message: &str| GatewayEvent {
        kind: EVENT_KIND_CLOSE,
        data: None,
        binary: None,
        code: Some(1000),
        reason: Some(reason.to_owned()),
        was_clean: Some(true),
        message: Some(message.to_owned()),
    };
    assert!(within_contract(&event(
        &"r".repeat(123),
        &"m".repeat(16 * 1024)
    )));
    assert!(!within_contract(&event(&"r".repeat(124), "")));
    assert!(!within_contract(&event("", &"m".repeat(16 * 1024 + 1))));
}
