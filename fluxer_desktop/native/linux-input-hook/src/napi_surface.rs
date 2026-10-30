// SPDX-License-Identifier: AGPL-3.0-or-later

#[cfg(target_os = "linux")]
use std::{ops::ControlFlow, sync::Arc};

#[cfg(target_os = "linux")]
use napi::{
    Env, Status, Task,
    bindgen_prelude::{AsyncTask, Function, Object, Result, ToNapiValue},
    sys,
    threadsafe_function::{ThreadsafeFunctionCallMode, UnknownReturnValue},
};
use napi_derive::napi;

#[cfg(target_os = "linux")]
use crate::{DecodedEvent, EventKind, InputHookError, StartTicket};

#[cfg(target_os = "linux")]
impl From<InputHookError> for napi::Error {
    fn from(error: InputHookError) -> Self {
        Self::new(Status::GenericFailure, error.to_string())
    }
}

#[cfg(target_os = "linux")]
impl ToNapiValue for DecodedEvent {
    unsafe fn to_napi_value(raw_env: sys::napi_env, event: Self) -> Result<sys::napi_value> {
        let env = Env::from_raw(raw_env);
        let mut object = Object::new(&env)?;
        object.set("type", event.kind.as_str())?;
        object.set("ctrlKey", event.mods.ctrl)?;
        object.set("altKey", event.mods.alt)?;
        object.set("shiftKey", event.mods.shift)?;
        object.set("metaKey", event.mods.meta)?;
        match event.kind {
            EventKind::KeyDown | EventKind::KeyUp => {
                object.set("keycode", event.keycode)?;
                object.set("x11Keycode", u32::from(event.x11_keycode))?;
                object.set("keyName", event.key_name.as_str())?;
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

#[cfg(target_os = "linux")]
pub struct StartTask {
    inner: Arc<crate::InputHook>,
    ticket: StartTicket,
}

#[cfg(target_os = "linux")]
impl Task for StartTask {
    type Output = ();
    type JsValue = ();

    fn compute(&mut self) -> Result<Self::Output> {
        Ok(self.inner.start_with_ticket(self.ticket)?)
    }

    fn resolve(&mut self, _env: Env, _output: Self::Output) -> Result<Self::JsValue> {
        Ok(())
    }
}

#[cfg(target_os = "linux")]
#[napi]
pub struct InputHook {
    inner: Arc<crate::InputHook>,
}

#[cfg(target_os = "linux")]
#[napi]
impl InputHook {
    #[napi(constructor)]
    pub fn new(callback: Function<DecodedEvent, UnknownReturnValue>) -> Result<Self> {
        let tsfn = callback
            .build_threadsafe_function::<DecodedEvent>()
            .weak::<true>()
            .callee_handled::<false>()
            .build()
            .map_err(|err| {
                napi::Error::new(
                    Status::GenericFailure,
                    format!("failed to create TSFN: {}", err.reason),
                )
            })?;
        Ok(Self {
            inner: Arc::new(crate::InputHook::new(move |event| {
                let status = tsfn.call(event, ThreadsafeFunctionCallMode::NonBlocking);
                if status == Status::Closing {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            })),
        })
    }

    #[napi]
    pub fn start(&self) -> AsyncTask<StartTask> {
        AsyncTask::new(StartTask {
            inner: self.inner.clone(),
            ticket: self.inner.start_ticket(),
        })
    }

    #[napi]
    pub fn stop(&self) -> Result<()> {
        Ok(self.inner.stop()?)
    }
}

#[cfg(target_os = "linux")]
impl Drop for InputHook {
    fn drop(&mut self) {
        let _ = self.inner.stop();
    }
}

#[napi(js_name = "isAvailable")]
pub fn is_available() -> bool {
    crate::is_available()
}
