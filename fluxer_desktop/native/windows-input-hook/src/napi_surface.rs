// SPDX-License-Identifier: AGPL-3.0-or-later

use napi::{
    Env, Status,
    bindgen_prelude::{Function, Object, Result, ToNapiValue},
    sys,
    threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode, UnknownReturnValue},
};
use napi_derive::napi;

use crate::{EventKind, InputHookError, QueuedEvent};

type EventThreadsafeFunction =
    ThreadsafeFunction<QueuedEvent, UnknownReturnValue, QueuedEvent, napi::Status, false, true>;

impl From<InputHookError> for napi::Error {
    fn from(error: InputHookError) -> Self {
        Self::new(Status::GenericFailure, error.to_string())
    }
}

impl ToNapiValue for QueuedEvent {
    unsafe fn to_napi_value(raw_env: sys::napi_env, event: Self) -> Result<sys::napi_value> {
        let env = Env::from_raw(raw_env);
        let mut object = Object::new(&env)?;
        object.set("type", event.kind.as_str())?;
        object.set("ctrlKey", event.ctrl)?;
        object.set("altKey", event.alt)?;
        object.set("shiftKey", event.shift)?;
        object.set("metaKey", event.meta)?;
        match event.kind {
            EventKind::KeyDown | EventKind::KeyUp => {
                object.set("keycode", event.keycode)?;
                object.set("keyName", event.key_name())?;
                object.set("scanCode", event.scan_code)?;
                object.set("extended", event.extended)?;
            }
            EventKind::MouseDown | EventKind::MouseUp => {
                object.set("button", u32::from(event.button))?;
                if event.has_xy {
                    object.set("x", event.x)?;
                    object.set("y", event.y)?;
                }
            }
        }
        unsafe { <Object<'_> as ToNapiValue>::to_napi_value(raw_env, object) }
    }
}

#[napi]
pub struct InputHook {
    inner: crate::InputHook,
}

#[napi]
impl InputHook {
    #[napi(constructor)]
    pub fn new(callback: Function<QueuedEvent, UnknownReturnValue>) -> Result<Self> {
        let tsfn: EventThreadsafeFunction = callback
            .build_threadsafe_function::<QueuedEvent>()
            .weak::<true>()
            .callee_handled::<false>()
            .build()
            .map_err(|err| {
                napi::Error::new(
                    napi::Status::GenericFailure,
                    format!("failed to create InputHook callback: {}", err.reason),
                )
            })?;
        Ok(Self {
            inner: crate::InputHook::new(move |event| {
                let _ = tsfn.call(event, ThreadsafeFunctionCallMode::NonBlocking);
            }),
        })
    }

    #[napi]
    pub fn start(&self) -> Result<()> {
        Ok(self.inner.start()?)
    }

    #[napi]
    pub fn stop(&self) {
        self.inner.stop();
    }
}

#[napi(js_name = "isAvailable")]
pub fn is_available() -> bool {
    crate::is_available()
}
