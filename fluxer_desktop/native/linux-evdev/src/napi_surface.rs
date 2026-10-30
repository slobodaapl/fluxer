// SPDX-License-Identifier: AGPL-3.0-or-later

#[cfg(target_os = "linux")]
use std::{ops::ControlFlow, sync::Arc};

#[cfg(target_os = "linux")]
use napi::{
    Env, Status,
    bindgen_prelude::{Function, Object, Result, ToNapiValue},
    sys,
    threadsafe_function::{ThreadsafeFunctionCallMode, UnknownReturnValue},
};
use napi_derive::napi;

#[cfg(target_os = "linux")]
use crate::{EvdevError, KeyKind, MouseKind, NativeEvent};

#[cfg(target_os = "linux")]
impl From<EvdevError> for napi::Error {
    fn from(error: EvdevError) -> Self {
        Self::new(Status::GenericFailure, error.to_string())
    }
}

#[cfg(target_os = "linux")]
impl ToNapiValue for NativeEvent {
    unsafe fn to_napi_value(raw_env: sys::napi_env, event: Self) -> Result<sys::napi_value> {
        let env = Env::from_raw(raw_env);
        let mut object = Object::new(&env)?;
        match event {
            Self::Key {
                kind,
                keycode,
                key_name,
                ctrl,
                alt,
                shift,
                meta,
            } => {
                object.set(
                    "type",
                    match kind {
                        KeyKind::Down => "keydown",
                        KeyKind::Up => "keyup",
                    },
                )?;
                object.set("keycode", u32::from(keycode))?;
                object.set("keyName", key_name)?;
                object.set("ctrlKey", ctrl)?;
                object.set("altKey", alt)?;
                object.set("shiftKey", shift)?;
                object.set("metaKey", meta)?;
            }
            Self::Mouse {
                kind,
                button,
                ctrl,
                alt,
                shift,
                meta,
            } => {
                object.set(
                    "type",
                    match kind {
                        MouseKind::Down => "mousedown",
                        MouseKind::Up => "mouseup",
                    },
                )?;
                object.set("button", u32::from(button))?;
                object.set("ctrlKey", ctrl)?;
                object.set("altKey", alt)?;
                object.set("shiftKey", shift)?;
                object.set("metaKey", meta)?;
            }
        }
        unsafe { <Object<'_> as ToNapiValue>::to_napi_value(raw_env, object) }
    }
}

#[cfg(target_os = "linux")]
#[napi]
pub struct EvdevHook {
    inner: crate::EvdevHook,
}

#[cfg(target_os = "linux")]
#[napi]
impl EvdevHook {
    #[napi(constructor)]
    pub fn new(on_event: Function<NativeEvent, UnknownReturnValue>) -> Result<Self> {
        let callback = Arc::new(
            on_event
                .build_threadsafe_function::<NativeEvent>()
                .weak::<true>()
                .callee_handled::<false>()
                .build()
                .map_err(|err| {
                    napi::Error::new(
                        Status::GenericFailure,
                        format!("failed to build callback: {}", err.reason),
                    )
                })?,
        );
        Ok(Self {
            inner: crate::EvdevHook::new(move |event| {
                let status = callback.call(event, ThreadsafeFunctionCallMode::NonBlocking);
                if status == Status::Closing {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            }),
        })
    }

    #[napi]
    pub fn start(&self) -> Result<bool> {
        Ok(self.inner.start()?)
    }

    #[napi]
    pub fn stop(&self) -> Result<()> {
        Ok(self.inner.stop()?)
    }
}

#[napi(js_name = "isKeyboardReadable")]
pub fn is_keyboard_readable() -> bool {
    crate::is_keyboard_readable()
}
