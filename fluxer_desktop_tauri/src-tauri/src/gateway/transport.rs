// SPDX-License-Identifier: AGPL-3.0-or-later

//! Renderer-owned native gateway sockets, ported from
//! `fluxer_desktop/src/main/NativeGatewayTransport.ts`.
//!
//! Each connection belongs to the renderer document that created it and delivers its events on
//! the Tauri channel that document passed to `create`. Binary frames travel as raw channel
//! bodies (an `ArrayBuffer` in the webview), every other event as JSON without `connectionId`.
//!
//! Differences from Electron:
//! - Routing is always DIRECT (outbound HTTP has no proxy support yet), so the proxied-route
//!   refusal never triggers.
//! - A document's watcher stays installed until the document goes away, not only while it has
//!   connections: origin registry state is keyed by document here, so the watcher is what
//!   releases it.
//! - The crate returns an event's queue reservation when it is read (`release`), which happens
//!   just before the channel send hands it to the webview IPC, not when JS runs the listener.

use std::collections::HashMap;
use std::mem;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use fluxer_gateway_socket::{
    EventDelivery, GatewayConnection, GatewayEvent, GatewaySocketError, QueuedGatewayEvent,
};
use serde::Serialize;
use serde_json::Value;
use tauri::ipc::{Channel, InvokeResponseBody};

use super::origin_registry::GatewayOriginRegistry;
use super::request::{
    CLOSE_REASON_MAX_BYTES, CloseRequest, CreateRequest, ERROR_MAX_BYTES, FRAME_MAX_BYTES,
    SendTextRequest, binary_payload_missing, require_binary_payload, require_close_request,
    require_connection_id, require_create_request, require_send_text_request,
};
use super::websocket_origin::websocket_http_origin;
use crate::documents::{Caller, DocumentOwner, DocumentRegistry, InvalidationReason, WatchGuard};
use crate::error::BridgeError;
use crate::net::outbound::OutboundHttp;
use crate::runtime::errors::outbound_error;

const RENDERER_CONTEXT: &str = "Native gateway transport";
const GATEWAY_MODE: &str = "gateway";
const MAX_CONNECTIONS_PER_DOCUMENT: usize = 8;
const ABNORMAL_CLOSURE: u16 = 1006;
const EVENT_KIND_CLOSE: &str = "close";
const EVENT_KIND_ERROR: &str = "error";

pub type EventChannel = Channel<InvokeResponseBody>;

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CreateResult {
    pub connection_id: String,
}

/// `NativeGatewayTransportEvent` without `connectionId`; the bridge adds it from the channel.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WireEvent<'a> {
    kind: &'a str,
    data: Option<&'a str>,
    binary: Option<()>,
    code: Option<u16>,
    reason: Option<&'a str>,
    was_clean: Option<bool>,
    message: Option<&'a str>,
}

fn event_body(event: GatewayEvent) -> InvokeResponseBody {
    if let Some(binary) = event.binary {
        return InvokeResponseBody::Raw(binary);
    }
    let wire = WireEvent {
        kind: event.kind,
        data: event.data.as_deref(),
        binary: None,
        code: event.code,
        reason: event.reason.as_deref(),
        was_clean: event.was_clean,
        message: event.message.as_deref(),
    };
    InvokeResponseBody::Json(serde_json::to_string(&wire).expect("gateway events serialise"))
}

/// The size limits `parseNativeGatewayTransportEvent` enforces on the native addon's events.
fn within_contract(event: &GatewayEvent) -> bool {
    let fits = |value: Option<usize>, max: usize| value.is_none_or(|length| length <= max);
    fits(event.data.as_ref().map(String::len), FRAME_MAX_BYTES)
        && fits(event.binary.as_ref().map(Vec::len), FRAME_MAX_BYTES)
        && fits(
            event.reason.as_ref().map(String::len),
            CLOSE_REASON_MAX_BYTES,
        )
        && fits(event.message.as_ref().map(String::len), ERROR_MAX_BYTES)
}

fn unavailable() -> BridgeError {
    BridgeError::new(
        "NativeGatewayTransportUnavailableError",
        "The native gateway transport addon is unavailable",
    )
}

fn shutting_down() -> BridgeError {
    BridgeError::new(
        "NativeGatewayTransportShutdownError",
        "The native gateway transport is shutting down",
    )
}

fn not_found(connection_id: &str) -> BridgeError {
    BridgeError::new(
        "NativeGatewayTransportConnectionNotFoundError",
        format!("Native gateway transport connection {connection_id} does not exist"),
    )
}

fn duplicate(connection_id: &str) -> BridgeError {
    BridgeError::new(
        "DuplicateNativeGatewayTransportConnectionError",
        format!("Native gateway transport connection {connection_id} already exists"),
    )
}

fn owner_mismatch() -> BridgeError {
    BridgeError::new(
        "NativeGatewayTransportConnectionOwnerMismatchError",
        "Native gateway transport connection is owned by another renderer document",
    )
}

fn connection_limit() -> BridgeError {
    BridgeError::new(
        "NativeGatewayTransportConnectionLimitError",
        "Native gateway transport connection limit reached for this renderer document",
    )
}

fn invalid_target() -> BridgeError {
    BridgeError::new(
        "NativeGatewayTransportInvalidTargetError",
        "Native gateway transport target has no HTTP origin",
    )
}

/// napi rejected with a plain `Error` carrying the crate's message.
fn native_error(error: GatewaySocketError) -> BridgeError {
    BridgeError::new("Error", error.to_string())
}

struct ConnectionRecord {
    owner: DocumentOwner,
    /// `None` until `connect` returns.
    native: Option<GatewayConnection>,
    channel: EventChannel,
}

struct State {
    accepting: bool,
    connections: HashMap<String, ConnectionRecord>,
    reserved: HashMap<String, DocumentOwner>,
    owner_watchers: HashMap<DocumentOwner, WatchGuard>,
}

impl State {
    fn connection_count_for(&self, owner: &DocumentOwner) -> usize {
        let open = self
            .connections
            .values()
            .filter(|record| record.owner == *owner)
            .count();
        let reserved = self
            .reserved
            .iter()
            .filter(|(id, reserved)| **reserved == *owner && !self.connections.contains_key(*id))
            .count();
        open + reserved
    }
}

struct Inner {
    documents: Arc<DocumentRegistry>,
    registry: GatewayOriginRegistry,
    outbound: Arc<OutboundHttp>,
    /// The native transport runtime started.
    available: bool,
    state: Mutex<State>,
}

#[derive(Clone)]
pub struct GatewayTransport {
    inner: Arc<Inner>,
}

impl GatewayTransport {
    pub fn new(
        documents: Arc<DocumentRegistry>,
        registry: GatewayOriginRegistry,
        outbound: Arc<OutboundHttp>,
        available: bool,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                documents,
                registry,
                outbound,
                available,
                state: Mutex::new(State {
                    accepting: true,
                    connections: HashMap::new(),
                    reserved: HashMap::new(),
                    owner_watchers: HashMap::new(),
                }),
            }),
        }
    }

    pub fn registry(&self) -> &GatewayOriginRegistry {
        &self.inner.registry
    }

    /// The `isAvailable` route.
    pub fn is_available(&self) -> bool {
        self.inner.available && self.inner.state().accepting
    }

    pub async fn create(
        &self,
        caller: &Caller,
        request: &Value,
        channel: EventChannel,
    ) -> Result<CreateResult, BridgeError> {
        self.require_accepting()?;
        if !self.inner.available {
            return Err(unavailable());
        }
        let owner = self.inner.documents.capture(caller, RENDERER_CONTEXT)?;
        let CreateRequest { connection_id, url } = require_create_request(request)?;
        self.reserve(&owner, &connection_id)?;
        let result = self.open(&owner, &connection_id, &url, channel).await;
        let removed = {
            let mut state = self.inner.state();
            state.reserved.remove(&connection_id);
            if result.is_err() {
                state.connections.remove(&connection_id)
            } else {
                None
            }
        };
        drop(removed);
        // The owner can go away while its admission runs, after its watcher released the
        // registry; drop whatever state that admission recreated.
        if !self.inner.documents.is_current(&owner) {
            self.inner.registry.release_document(&owner);
        }
        if let Err(error) = &result {
            tracing::warn!(%error, "[NativeGateway] Refused a native gateway connection");
        }
        result.map(|()| CreateResult { connection_id })
    }

    pub fn send_text(&self, caller: &Caller, request: &Value) -> Result<(), BridgeError> {
        self.require_accepting()?;
        let SendTextRequest {
            connection_id,
            payload,
        } = require_send_text_request(request)?;
        self.with_owned_native(caller, &connection_id, |native| native.send_text(payload))
    }

    /// `connection_id` and `payload` are the header and raw body the bridge sent.
    pub fn send_binary(
        &self,
        caller: &Caller,
        connection_id: Option<&str>,
        payload: Option<&[u8]>,
    ) -> Result<(), BridgeError> {
        self.require_accepting()?;
        let connection_id = require_connection_id(connection_id)?;
        let payload = payload.ok_or_else(binary_payload_missing)?;
        require_binary_payload(payload)?;
        let payload = payload.to_vec();
        self.with_owned_native(caller, &connection_id, |native| native.send_binary(payload))
    }

    pub fn close(&self, caller: &Caller, request: &Value) -> Result<(), BridgeError> {
        self.require_accepting()?;
        let CloseRequest {
            connection_id,
            code,
            reason,
        } = require_close_request(request)?;
        self.with_owned_native(caller, &connection_id, |native| native.close(code, reason))
    }

    pub fn dispose(&self, caller: &Caller, connection_id: &Value) -> Result<(), BridgeError> {
        self.require_accepting()?;
        let connection_id = require_connection_id(connection_id.as_str())?;
        let removed = {
            let mut state = self.inner.state();
            let Some(record) = state.connections.get(&connection_id) else {
                return Ok(());
            };
            if !self.inner.documents.matches(&record.owner, caller) {
                return Err(owner_mismatch());
            }
            state.connections.remove(&connection_id)
        };
        drop(removed);
        Ok(())
    }

    /// Will-quit cleanup: refuses further requests and disposes every connection.
    pub fn cleanup(&self) {
        let (connections, watchers) = {
            let mut state = self.inner.state();
            state.accepting = false;
            state.reserved.clear();
            (
                mem::take(&mut state.connections),
                mem::take(&mut state.owner_watchers),
            )
        };
        drop(connections);
        drop(watchers);
        self.inner.registry.cleanup();
    }

    fn require_accepting(&self) -> Result<(), BridgeError> {
        if self.inner.state().accepting {
            Ok(())
        } else {
            Err(shutting_down())
        }
    }

    /// Reserves `connection_id` for `owner` and makes sure the owner is watched.
    fn reserve(&self, owner: &DocumentOwner, connection_id: &str) -> Result<(), BridgeError> {
        let mut state = self.inner.state();
        if state.connections.contains_key(connection_id)
            || state.reserved.contains_key(connection_id)
        {
            return Err(duplicate(connection_id));
        }
        if state.connection_count_for(owner) >= MAX_CONNECTIONS_PER_DOCUMENT {
            return Err(connection_limit());
        }
        if !state.owner_watchers.contains_key(owner) {
            let transport = Arc::downgrade(&self.inner);
            let watched = owner.clone();
            let guard = self
                .inner
                .documents
                .watch(owner, RENDERER_CONTEXT, move |reason| {
                    if let Some(transport) = transport.upgrade() {
                        transport.release_owner(&watched, reason);
                    }
                })?;
            state.owner_watchers.insert(owner.clone(), guard);
        }
        state
            .reserved
            .insert(connection_id.to_owned(), owner.clone());
        Ok(())
    }

    async fn open(
        &self,
        owner: &DocumentOwner,
        connection_id: &str,
        url: &str,
        channel: EventChannel,
    ) -> Result<(), BridgeError> {
        self.inner.registry.admit(owner, url).await?;
        let address = self.pinned_address_for(url).await?;
        {
            // Checked under the state lock so neither shutdown nor the owner's release can
            // slip between the check and the insert.
            let mut state = self.inner.state();
            if !state.accepting {
                return Err(shutting_down());
            }
            self.inner
                .documents
                .require_current(owner, RENDERER_CONTEXT)?;
            state.connections.insert(
                connection_id.to_owned(),
                ConnectionRecord {
                    owner: owner.clone(),
                    native: None,
                    channel,
                },
            );
        }
        let delivery = Arc::new(ConnectionDelivery {
            transport: Arc::downgrade(&self.inner),
            connection_id: connection_id.to_owned(),
        });
        let native =
            fluxer_gateway_socket::connect(url, address.as_deref(), GATEWAY_MODE, delivery)
                .map_err(native_error)?;
        let mut state = self.inner.state();
        match state.connections.get_mut(connection_id) {
            Some(record) => record.native = Some(native),
            // Released or closed before `connect` returned.
            None => {
                drop(state);
                drop(native);
            }
        }
        Ok(())
    }

    /// `pinnedAddressFor`: a cleartext socket must use the outbound layer's non-public address.
    async fn pinned_address_for(&self, url: &str) -> Result<Option<String>, BridgeError> {
        if !url.starts_with("ws:") {
            return Ok(None);
        }
        let http_origin = websocket_http_origin(url).ok_or_else(invalid_target)?;
        let address = self
            .inner
            .outbound
            .require_cleartext_transport_address(&http_origin)
            .await
            .map_err(|error| outbound_error(&error))?;
        Ok(Some(address.to_string()))
    }

    fn with_owned_native(
        &self,
        caller: &Caller,
        connection_id: &str,
        action: impl FnOnce(&GatewayConnection) -> Result<(), GatewaySocketError>,
    ) -> Result<(), BridgeError> {
        let state = self.inner.state();
        let record = state
            .connections
            .get(connection_id)
            .ok_or_else(|| not_found(connection_id))?;
        if !self.inner.documents.matches(&record.owner, caller) {
            return Err(owner_mismatch());
        }
        let native = record
            .native
            .as_ref()
            .ok_or_else(|| not_found(connection_id))?;
        action(native).map_err(native_error)
    }
}

impl Inner {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `releaseOwner`: the document navigated away or its webview was destroyed.
    fn release_owner(&self, owner: &DocumentOwner, reason: InvalidationReason) {
        tracing::info!(
            ?reason,
            "[NativeGateway] Releasing renderer document connections"
        );
        let (released, watcher) = {
            let mut state = self.state();
            let watcher = state.owner_watchers.remove(owner);
            self.registry.release_document(owner);
            let ids: Vec<String> = state
                .connections
                .iter()
                .filter(|(_, record)| record.owner == *owner)
                .map(|(id, _)| id.clone())
                .collect();
            let released: Vec<ConnectionRecord> = ids
                .iter()
                .filter_map(|id| state.connections.remove(id))
                .collect();
            (released, watcher)
        };
        drop(released);
        drop(watcher);
    }

    /// `handleNativeEvent`. Returns whether the event reached the renderer.
    fn handle_event(&self, connection_id: &str, event: GatewayEvent) -> bool {
        let mut state = self.state();
        let Some(record) = state.connections.get(connection_id) else {
            return false;
        };
        if !within_contract(&event) {
            tracing::error!("[NativeGateway] Rejected a malformed native gateway event");
            let record = state
                .connections
                .remove(connection_id)
                .expect("the record was just found");
            drop(state);
            self.fail_connection(record);
            return false;
        }
        let terminal = event.kind == EVENT_KIND_CLOSE;
        let delivered = self.send_event(&record.owner, &record.channel, event_body(event));
        if !delivered || terminal {
            let removed = state.connections.remove(connection_id);
            drop(state);
            drop(removed);
        }
        delivered
    }

    /// `failConnection`: an error event, then an abnormal close, then disposal.
    fn fail_connection(&self, record: ConnectionRecord) {
        let failure = |kind, message, code, reason, was_clean| {
            event_body(GatewayEvent {
                kind,
                data: None,
                binary: None,
                code,
                reason,
                was_clean,
                message,
            })
        };
        let error = failure(
            EVENT_KIND_ERROR,
            Some("Native gateway transport emitted an invalid event".to_owned()),
            None,
            None,
            None,
        );
        if self.send_event(&record.owner, &record.channel, error) {
            let close = failure(
                EVENT_KIND_CLOSE,
                None,
                Some(ABNORMAL_CLOSURE),
                Some("Invalid native transport event".to_owned()),
                Some(false),
            );
            self.send_event(&record.owner, &record.channel, close);
        }
    }

    /// `sendConnectionEvent`.
    fn send_event(
        &self,
        owner: &DocumentOwner,
        channel: &EventChannel,
        body: InvokeResponseBody,
    ) -> bool {
        if !self.documents.is_current(owner) {
            return false;
        }
        match channel.send(body) {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(
                    %error,
                    "[NativeGateway] Failed to deliver a gateway event to the renderer"
                );
                false
            }
        }
    }
}

struct ConnectionDelivery {
    transport: Weak<Inner>,
    connection_id: String,
}

impl EventDelivery for ConnectionDelivery {
    fn deliver(&self, event: QueuedGatewayEvent) -> bool {
        let event = event.release();
        self.transport
            .upgrade()
            .is_some_and(|transport| transport.handle_event(&self.connection_id, event))
    }

    fn deliver_terminal(&self, event: GatewayEvent) {
        if let Some(transport) = self.transport.upgrade() {
            transport.handle_event(&self.connection_id, event);
        }
    }
}

#[cfg(test)]
mod tests;
