// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{ffi::c_void, ops::ControlFlow, ptr, sync::Arc};

use napi::{
    Env, JsValue, Status, ValueType,
    bindgen_prelude::{Array, AsyncTask, Function, Object, Result, Task, ToNapiValue, Unknown},
    sys,
    threadsafe_function::{ThreadsafeFunctionCallMode, UnknownReturnValue},
};
use napi_derive::napi;

use crate::{NotificationsError, NotifyAction, NotifyArgs, NotifyHints, NotifyImageData, Urgency};

const EVENT_QUEUE_LIMIT: usize = 1024;

impl From<NotificationsError> for napi::Error {
    fn from(error: NotificationsError) -> Self {
        match error {
            NotificationsError::InvalidArg(message) => Self::new(Status::InvalidArg, message),
            NotificationsError::Generic(message) => Self::new(Status::GenericFailure, message),
        }
    }
}

pub enum NotifyEvent {
    ActionInvoked { id: u32, action_key: String },
    Closed { id: u32, reason: u32 },
}

impl From<crate::NotifyEvent> for NotifyEvent {
    fn from(event: crate::NotifyEvent) -> Self {
        match event {
            crate::NotifyEvent::ActionInvoked { id, action_key } => {
                Self::ActionInvoked { id, action_key }
            }
            crate::NotifyEvent::Closed { id, reason } => Self::Closed { id, reason },
        }
    }
}

impl ToNapiValue for NotifyEvent {
    unsafe fn to_napi_value(raw_env: sys::napi_env, event: Self) -> Result<sys::napi_value> {
        let env = Env::from_raw(raw_env);
        let mut object = Object::new(&env)?;
        match event {
            Self::ActionInvoked { id, action_key } => {
                object.set("kind", "actionInvoked")?;
                object.set("id", id)?;
                object.set("actionKey", action_key)?;
            }
            Self::Closed { id, reason } => {
                object.set("kind", "closed")?;
                object.set("id", id)?;
                object.set("reason", reason)?;
            }
        }
        unsafe { <Object<'_> as ToNapiValue>::to_napi_value(raw_env, object) }
    }
}

#[napi]
pub struct FreedesktopNotifications {
    inner: Arc<crate::FreedesktopNotifications>,
}

#[napi]
impl FreedesktopNotifications {
    #[napi(constructor)]
    pub fn new(on_event: Function<NotifyEvent, UnknownReturnValue>) -> Result<Self> {
        let event_callback = on_event
            .build_threadsafe_function::<NotifyEvent>()
            .weak::<true>()
            .callee_handled::<false>()
            .max_queue_size::<EVENT_QUEUE_LIMIT>()
            .build()
            .map_err(|err| {
                napi::Error::new(
                    Status::GenericFailure,
                    format!("failed to create notification callback: {}", err.reason),
                )
            })?;
        let inner = crate::FreedesktopNotifications::new(move |event| {
            let status = event_callback.call(event.into(), ThreadsafeFunctionCallMode::NonBlocking);
            if status == Status::Closing {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        })?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    #[napi]
    pub fn notify(&self, payload: Object) -> Result<AsyncTask<NotifyTask>> {
        let args = parse_notify_payload(&payload)?;
        Ok(AsyncTask::new(NotifyTask {
            inner: self.inner.clone(),
            args,
        }))
    }

    #[napi(js_name = "closeNotification")]
    pub fn close_notification(&self, id: u32) -> Result<AsyncTask<CloseNotificationTask>> {
        Ok(AsyncTask::new(CloseNotificationTask {
            inner: self.inner.clone(),
            id,
        }))
    }

    #[napi(js_name = "getServerCapabilities")]
    pub fn get_server_capabilities(&self) -> Result<AsyncTask<GetCapabilitiesTask>> {
        Ok(AsyncTask::new(GetCapabilitiesTask {
            inner: self.inner.clone(),
        }))
    }

    #[napi]
    pub fn close(&self) -> Result<AsyncTask<CloseTask>> {
        Ok(AsyncTask::new(CloseTask {
            inner: self.inner.clone(),
        }))
    }
}

impl Drop for FreedesktopNotifications {
    fn drop(&mut self) {
        self.inner.close();
    }
}

pub struct NotifyTask {
    inner: Arc<crate::FreedesktopNotifications>,
    args: NotifyArgs,
}

impl Task for NotifyTask {
    type Output = u32;
    type JsValue = u32;

    fn compute(&mut self) -> Result<Self::Output> {
        Ok(self.inner.notify(&self.args)?)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

pub struct CloseNotificationTask {
    inner: Arc<crate::FreedesktopNotifications>,
    id: u32,
}

impl Task for CloseNotificationTask {
    type Output = ();
    type JsValue = ();

    fn compute(&mut self) -> Result<Self::Output> {
        Ok(self.inner.close_notification(self.id)?)
    }

    fn resolve(&mut self, _env: Env, _output: Self::Output) -> Result<Self::JsValue> {
        Ok(())
    }
}

pub struct GetCapabilitiesTask {
    inner: Arc<crate::FreedesktopNotifications>,
}

impl Task for GetCapabilitiesTask {
    type Output = Vec<String>;
    type JsValue = Vec<String>;

    fn compute(&mut self) -> Result<Self::Output> {
        Ok(self.inner.get_server_capabilities()?)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

pub struct CloseTask {
    inner: Arc<crate::FreedesktopNotifications>,
}

impl Task for CloseTask {
    type Output = ();
    type JsValue = ();

    fn compute(&mut self) -> Result<Self::Output> {
        self.inner.close();
        Ok(())
    }

    fn resolve(&mut self, _env: Env, _output: Self::Output) -> Result<Self::JsValue> {
        Ok(())
    }
}

fn parse_notify_payload(payload: &Object) -> Result<NotifyArgs> {
    Ok(NotifyArgs {
        app_name: read_string_or_empty(payload, "appName"),
        replaces_id: read_optional_u32(payload, "replacesId").unwrap_or(0),
        app_icon: read_string_or_empty(payload, "appIcon"),
        summary: read_string_or_empty(payload, "summary"),
        body: read_string_or_empty(payload, "body"),
        actions: parse_actions(payload),
        hints: parse_hints(payload)?,
        expire_timeout_ms: read_optional_i32(payload, "expireTimeoutMs").unwrap_or(-1),
    })
}

fn parse_actions(payload: &Object) -> Vec<NotifyAction> {
    let Some(actions) = read_array(payload, "actions") else {
        return Vec::new();
    };

    let mut out = Vec::with_capacity(actions.len() as usize);
    for index in 0..actions.len() {
        let action = actions.get::<Object>(index).ok().flatten();
        match action {
            Some(action) => out.push(NotifyAction {
                key: read_string_or_empty(&action, "key"),
                label: read_string_or_empty(&action, "label"),
            }),
            None => out.push(NotifyAction {
                key: String::new(),
                label: String::new(),
            }),
        }
    }
    out
}

fn parse_hints(payload: &Object) -> Result<NotifyHints> {
    let Some(hints_object) = read_object(payload, "hints") else {
        return Ok(NotifyHints::empty());
    };

    let mut hints = NotifyHints::empty();
    if let Some(value) = read_optional_string(&hints_object, "urgency") {
        hints.urgency = Urgency::parse(&value);
    }
    hints.category = read_optional_string(&hints_object, "category");
    hints.desktop_entry = read_optional_string(&hints_object, "desktopEntry");
    hints.sound_file = read_optional_string(&hints_object, "soundFile");
    hints.suppress_sound = read_optional_bool(&hints_object, "suppressSound");
    hints.transient = read_optional_bool(&hints_object, "transient");
    hints.action_icons = read_optional_bool(&hints_object, "actionIcons");

    if let Some(image_object) = read_object(&hints_object, "imageData") {
        hints.image_data = Some(parse_image_data(&image_object)?);
    }

    Ok(hints)
}

fn parse_image_data(value: &Object) -> Result<NotifyImageData> {
    let width = read_optional_i32(value, "width").unwrap_or(0);
    let height = read_optional_i32(value, "height").unwrap_or(0);
    let channels = read_optional_i32(value, "channels").unwrap_or(4);
    let rowstride = read_optional_i32(value, "rowstride").unwrap_or(0);
    let bits_per_sample = read_optional_i32(value, "bitsPerSample").unwrap_or(8);
    let has_alpha = read_optional_bool(value, "hasAlpha").unwrap_or(true);
    let data_value = read_unknown(value, "data")
        .ok_or_else(|| invalid_arg("imageData.data must be a Buffer or Uint8Array"))?;
    let data = read_image_bytes(data_value)?;

    Ok(NotifyImageData::new(
        width,
        height,
        rowstride,
        has_alpha,
        bits_per_sample,
        channels,
        data,
    )?)
}

fn read_unknown<'a>(object: &Object<'a>, key: &str) -> Option<Unknown<'a>> {
    object.get::<Unknown>(key).ok().flatten()
}

fn read_string_or_empty(object: &Object, key: &str) -> String {
    read_optional_string(object, key).unwrap_or_default()
}

fn read_optional_string(object: &Object, key: &str) -> Option<String> {
    let value = read_unknown(object, key)?;
    if value.get_type().ok()? != ValueType::String {
        return None;
    }
    unsafe { value.cast::<String>().ok() }
}

fn read_optional_u32(object: &Object, key: &str) -> Option<u32> {
    let value = read_unknown(object, key)?;
    if value.get_type().ok()? != ValueType::Number {
        return None;
    }
    unsafe { value.cast::<u32>().ok() }
}

fn read_optional_i32(object: &Object, key: &str) -> Option<i32> {
    let value = read_unknown(object, key)?;
    if value.get_type().ok()? != ValueType::Number {
        return None;
    }
    unsafe { value.cast::<i32>().ok() }
}

fn read_optional_bool(object: &Object, key: &str) -> Option<bool> {
    let value = read_unknown(object, key)?;
    if value.get_type().ok()? != ValueType::Boolean {
        return None;
    }
    unsafe { value.cast::<bool>().ok() }
}

fn read_object<'a>(object: &Object<'a>, key: &str) -> Option<Object<'a>> {
    let value = read_unknown(object, key)?;
    if value.get_type().ok()? != ValueType::Object {
        return None;
    }
    unsafe { value.cast::<Object>().ok() }
}

fn read_array<'a>(object: &Object<'a>, key: &str) -> Option<Array<'a>> {
    let value = read_unknown(object, key)?;
    if value.get_type().ok()? != ValueType::Object {
        return None;
    }
    unsafe { value.cast::<Array>().ok() }
}

fn read_image_bytes(value: Unknown) -> Result<Vec<u8>> {
    let raw = value.raw();
    let raw_env = value.value().env;

    let mut is_typed_array = false;
    let typed_status = unsafe { sys::napi_is_typedarray(raw_env, raw, &mut is_typed_array) };
    if typed_status == sys::Status::napi_ok && is_typed_array {
        let mut array_type: sys::napi_typedarray_type = 0;
        let mut len = 0usize;
        let mut data_ptr: *mut c_void = ptr::null_mut();
        let mut arraybuffer = ptr::null_mut();
        let mut byte_offset = 0usize;
        let status = unsafe {
            sys::napi_get_typedarray_info(
                raw_env,
                raw,
                &mut array_type,
                &mut len,
                &mut data_ptr,
                &mut arraybuffer,
                &mut byte_offset,
            )
        };
        if status != sys::Status::napi_ok {
            return Err(invalid_arg("imageData.data must be a readable typed array"));
        }
        if !is_uint8_typed_array(array_type) {
            return Err(invalid_arg("imageData.data must be a Buffer or Uint8Array"));
        }
        return copy_non_empty_bytes(data_ptr.cast(), len);
    }

    let mut is_buffer = false;
    let buffer_status = unsafe { sys::napi_is_buffer(raw_env, raw, &mut is_buffer) };
    if buffer_status == sys::Status::napi_ok && is_buffer {
        let mut data_ptr: *mut c_void = ptr::null_mut();
        let mut len = 0usize;
        let status = unsafe { sys::napi_get_buffer_info(raw_env, raw, &mut data_ptr, &mut len) };
        if status != sys::Status::napi_ok {
            return Err(invalid_arg("imageData.data must be a readable Buffer"));
        }
        return copy_non_empty_bytes(data_ptr.cast(), len);
    }

    Err(invalid_arg("imageData.data must be a Buffer or Uint8Array"))
}

fn is_uint8_typed_array(array_type: sys::napi_typedarray_type) -> bool {
    array_type == sys::TypedarrayType::uint8_array
}

fn copy_non_empty_bytes(data_ptr: *const u8, len: usize) -> Result<Vec<u8>> {
    if data_ptr.is_null() || len == 0 {
        return Err(invalid_arg("imageData.data must not be empty"));
    }
    Ok(unsafe { std::slice::from_raw_parts(data_ptr, len).to_vec() })
}

fn invalid_arg(reason: impl Into<String>) -> napi::Error {
    napi::Error::new(Status::InvalidArg, reason.into())
}
