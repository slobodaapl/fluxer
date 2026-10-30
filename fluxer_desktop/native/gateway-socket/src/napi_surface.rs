// SPDX-License-Identifier: AGPL-3.0-or-later

use std::sync::Arc;

use napi::{
    Error, Status,
    bindgen_prelude::{Buffer, Function, Result},
    threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode, UnknownReturnValue},
};
use napi_derive::napi;

use crate::{
    EventDelivery, GatewayConnection, GatewayEvent, GatewaySocketError, QueuedGatewayEvent,
};

impl From<GatewaySocketError> for Error {
    fn from(error: GatewaySocketError) -> Self {
        match error {
            GatewaySocketError::InvalidArg(message) => Self::new(Status::InvalidArg, message),
            GatewaySocketError::Generic(message) => Self::new(Status::GenericFailure, message),
        }
    }
}

#[napi(object, use_nullable = true)]
pub struct NativeGatewayEvent {
    pub kind: String,
    pub data: Option<String>,
    pub binary: Option<Buffer>,
    pub code: Option<u16>,
    pub reason: Option<String>,
    #[napi(js_name = "wasClean")]
    pub was_clean: Option<bool>,
    pub message: Option<String>,
}

impl From<GatewayEvent> for NativeGatewayEvent {
    fn from(event: GatewayEvent) -> Self {
        Self {
            kind: event.kind.to_owned(),
            data: event.data,
            binary: event.binary.map(Buffer::from),
            code: event.code,
            reason: event.reason,
            was_clean: event.was_clean,
            message: event.message,
        }
    }
}

#[napi(object, use_nullable = true)]
pub struct NativeGatewayConnectOptions {
    pub url: String,
    pub address: Option<String>,
    pub mode: String,
}

type EventThreadsafeFunction = ThreadsafeFunction<
    QueuedGatewayEvent,
    UnknownReturnValue,
    NativeGatewayEvent,
    Status,
    false,
    true,
    0,
>;

type TerminalEventThreadsafeFunction = ThreadsafeFunction<
    GatewayEvent,
    UnknownReturnValue,
    NativeGatewayEvent,
    Status,
    false,
    true,
    0,
>;

struct NapiEventDelivery {
    regular: EventThreadsafeFunction,
    terminal: TerminalEventThreadsafeFunction,
}

impl EventDelivery for NapiEventDelivery {
    fn deliver(&self, event: QueuedGatewayEvent) -> bool {
        self.regular
            .call(event, ThreadsafeFunctionCallMode::NonBlocking)
            == Status::Ok
    }

    fn deliver_terminal(&self, event: GatewayEvent) {
        let _ = self
            .terminal
            .call(event, ThreadsafeFunctionCallMode::NonBlocking);
    }
}

#[napi(js_name = "NativeGatewayConnection")]
pub struct NativeGatewayConnection {
    inner: GatewayConnection,
}

#[napi]
impl NativeGatewayConnection {
    #[napi(js_name = "sendText")]
    pub fn send_text(&self, text: String) -> Result<()> {
        Ok(self.inner.send_text(text)?)
    }

    #[napi(js_name = "sendBinary")]
    pub fn send_binary(&self, payload: Buffer) -> Result<()> {
        Ok(self.inner.send_binary(payload.to_vec())?)
    }

    #[napi]
    pub fn close(&self, code: u32, reason: String) -> Result<()> {
        Ok(self.inner.close(code, reason)?)
    }

    #[napi]
    pub fn dispose(&self) -> Result<()> {
        Ok(self.inner.dispose()?)
    }
}

#[napi(js_name = "windowsIntermediateStoreRoots")]
pub fn windows_intermediate_store_roots() -> Vec<Buffer> {
    crate::windows_intermediate_store_roots()
        .into_iter()
        .map(Buffer::from)
        .collect()
}

#[napi]
pub fn warmup() -> Result<()> {
    Ok(crate::warmup()?)
}

#[napi]
pub fn connect(
    options: NativeGatewayConnectOptions,
    on_event: Function<NativeGatewayEvent, UnknownReturnValue>,
    on_terminal_event: Function<NativeGatewayEvent, UnknownReturnValue>,
) -> Result<NativeGatewayConnection> {
    let (target, mode) =
        crate::prepare_connect(&options.url, options.address.as_deref(), &options.mode)?;
    let regular = on_event
        .build_threadsafe_function::<QueuedGatewayEvent>()
        .weak::<true>()
        .callee_handled::<false>()
        .max_queue_size::<0>()
        .build_callback(|context| {
            let event = context.value.release();
            Ok(NativeGatewayEvent::from(event))
        })
        .map_err(|error| {
            Error::new(
                Status::GenericFailure,
                format!("failed to create gateway socket callback: {}", error.reason),
            )
        })?;
    let terminal = on_terminal_event
        .build_threadsafe_function::<GatewayEvent>()
        .weak::<true>()
        .callee_handled::<false>()
        .max_queue_size::<0>()
        .build_callback(|context| Ok(NativeGatewayEvent::from(context.value)))
        .map_err(|error| {
            Error::new(
                Status::GenericFailure,
                format!(
                    "failed to create gateway socket terminal callback: {}",
                    error.reason
                ),
            )
        })?;
    Ok(NativeGatewayConnection {
        inner: GatewayConnection::start(
            target,
            mode,
            Arc::new(NapiEventDelivery { regular, terminal }),
        )?,
    })
}
