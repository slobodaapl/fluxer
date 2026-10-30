// SPDX-License-Identifier: AGPL-3.0-or-later

//! The renderer's native gateway WebSocket transport (`window.electron.nativeGatewayTransport`)
//! and the origin admission policy behind it, ported from the Electron main process over the
//! `fluxer_gateway_socket` crate.

pub mod commands;
mod origin_registry;
mod request;
mod transport;
mod validation;
pub(crate) mod websocket_origin;

use std::sync::Arc;
use std::time::Instant;

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use self::origin_registry::GatewayOriginRegistry;
pub use self::transport::GatewayTransport;
use self::validation::fetch_validated_gateway_endpoint;
use crate::config::launch::{native_gateway_disable_reason, process_args};
use crate::documents::DocumentRegistry;
use crate::error::BridgeError;
use crate::net::outbound::OutboundHttp;

/// `domain` of every known-instance record, the registry's additional anchors.
fn known_instance_domains(records: Result<Vec<Value>, BridgeError>) -> Vec<String> {
    match records {
        Ok(records) => records
            .iter()
            .filter_map(|record| record.get("domain").and_then(Value::as_str))
            .map(str::to_owned)
            .collect(),
        Err(error) => {
            tracing::debug!(%error, "[NativeGateway] Known instances are unreadable");
            Vec::new()
        }
    }
}

/// `createDesktopNativeGatewayTransport` with the shared origin registry: warms the native
/// runtime, which also decides availability. `known_instances` reads the app store's records.
pub fn start(
    documents: Arc<DocumentRegistry>,
    outbound: Arc<OutboundHttp>,
    known_instances: impl Fn() -> BoxFuture<'static, Result<Vec<Value>, BridgeError>>
    + Send
    + Sync
    + 'static,
) -> GatewayTransport {
    let disable_reason = native_gateway_disable_reason(&process_args());
    if let Some(reason) = disable_reason {
        tracing::info!(
            reason,
            "[NativeGateway] The native gateway transport is disabled, using the browser transport"
        );
    }
    let available = match fluxer_gateway_socket::warmup() {
        Ok(()) => disable_reason.is_none(),
        Err(error) => {
            tracing::warn!(
                %error,
                "[NativeGateway] The native gateway transport is unavailable, using the browser transport"
            );
            false
        }
    };
    let resolver = outbound.clone();
    let registry = GatewayOriginRegistry::new(
        Box::new(move |anchor| {
            let outbound = resolver.clone();
            async move {
                fetch_validated_gateway_endpoint(&outbound, &anchor, &CancellationToken::new())
                    .await
            }
            .boxed()
        }),
        Box::new(move || known_instances().map(known_instance_domains).boxed()),
        Box::new(Instant::now),
    );
    GatewayTransport::new(documents, registry, outbound, available)
}
