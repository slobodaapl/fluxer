// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::HashMap,
    ops::ControlFlow,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use futures_lite::{FutureExt, StreamExt, future};
use zbus::{
    MatchRule, MessageStream,
    blocking::{Connection as BlockingConnection, Proxy},
    message::Type as MessageType,
    zvariant::Value,
};

#[cfg(feature = "napi")]
mod napi_surface;

const NOTIFY_DEST: &str = "org.freedesktop.Notifications";
const NOTIFY_PATH: &str = "/org/freedesktop/Notifications";
const NOTIFY_IFACE: &str = "org.freedesktop.Notifications";
const CALL_TIMEOUT: Duration = Duration::from_millis(5_000);
const SIGNAL_POLL_INTERVAL: Duration = Duration::from_millis(200);
const SIGNAL_THREAD_START_TIMEOUT: Duration = Duration::from_secs(5);

const DBUS_SESSION_UNREACHABLE: &str = "DBus session bus unreachable: no $DBUS_SESSION_BUS_ADDRESS and no \
     $XDG_RUNTIME_DIR/bus socket — desktop notifications require a \
     running session bus (systemd-logind, elogind, or dbus-launch).";

fn has_dbus_session() -> bool {
    has_dbus_session_from(
        std::env::var("DBUS_SESSION_BUS_ADDRESS").ok().as_deref(),
        std::env::var("XDG_RUNTIME_DIR").ok().as_deref(),
        |path| std::path::Path::new(path).exists(),
    )
}

fn has_dbus_session_from(
    bus_address: Option<&str>,
    xdg_runtime_dir: Option<&str>,
    path_exists: impl Fn(&str) -> bool,
) -> bool {
    if bus_address.is_some_and(|v| !v.is_empty()) {
        return true;
    }
    if let Some(dir) = xdg_runtime_dir
        && !dir.is_empty()
    {
        let candidate = format!("{}/bus", dir.trim_end_matches('/'));
        if path_exists(&candidate) {
            return true;
        }
    }
    false
}

/// Why a call failed. The napi surface maps `InvalidArg` to `Status::InvalidArg` and `Generic` to
/// `Status::GenericFailure`, with the message unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationsError {
    InvalidArg(String),
    Generic(String),
}

impl std::fmt::Display for NotificationsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidArg(message) | Self::Generic(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for NotificationsError {}

pub type Result<T> = std::result::Result<T, NotificationsError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Urgency {
    Low,
    Normal,
    Critical,
}

impl Urgency {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "low" => Some(Self::Low),
            "normal" => Some(Self::Normal),
            "critical" => Some(Self::Critical),
            _ => None,
        }
    }

    fn as_byte(self) -> u8 {
        match self {
            Self::Low => 0,
            Self::Normal => 1,
            Self::Critical => 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotifyImageData {
    pub width: i32,
    pub height: i32,
    pub rowstride: i32,
    pub has_alpha: bool,
    pub bits_per_sample: i32,
    pub channels: i32,
    pub data: Vec<u8>,
}

impl NotifyImageData {
    /// Validates an 8-bit RGB(A) image. A `rowstride` of zero or less means `width * channels`.
    pub fn new(
        width: i32,
        height: i32,
        mut rowstride: i32,
        has_alpha: bool,
        bits_per_sample: i32,
        channels: i32,
        data: Vec<u8>,
    ) -> Result<Self> {
        if width <= 0 || height <= 0 {
            return Err(invalid_arg("imageData width and height must be positive"));
        }
        if rowstride <= 0 {
            rowstride = width.saturating_mul(channels);
        }
        validate_image_data_shape(
            width,
            height,
            rowstride,
            has_alpha,
            bits_per_sample,
            channels,
            data.len(),
        )?;

        Ok(Self {
            width,
            height,
            rowstride,
            has_alpha,
            bits_per_sample,
            channels,
            data,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotifyHints {
    pub urgency: Option<Urgency>,
    pub category: Option<String>,
    pub desktop_entry: Option<String>,
    pub sound_file: Option<String>,
    pub suppress_sound: Option<bool>,
    pub transient: Option<bool>,
    pub action_icons: Option<bool>,
    pub image_data: Option<NotifyImageData>,
}

impl NotifyHints {
    pub fn empty() -> Self {
        Self {
            urgency: None,
            category: None,
            desktop_entry: None,
            sound_file: None,
            suppress_sound: None,
            transient: None,
            action_icons: None,
            image_data: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotifyAction {
    pub key: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotifyArgs {
    pub app_name: String,
    pub replaces_id: u32,
    pub app_icon: String,
    pub summary: String,
    pub body: String,
    pub actions: Vec<NotifyAction>,
    pub hints: NotifyHints,
    pub expire_timeout_ms: i32,
}

#[derive(Debug)]
pub enum NotifyEvent {
    ActionInvoked { id: u32, action_key: String },
    Closed { id: u32, reason: u32 },
}

/// Blocking client for `org.freedesktop.Notifications`. Calls block the calling thread for up to
/// five seconds; signals are delivered from a dedicated thread owned by the client.
pub struct FreedesktopNotifications {
    method_conn: Mutex<Option<BlockingConnection>>,
    stop_signal_thread: Arc<AtomicBool>,
    signal_thread: Mutex<Option<JoinHandle<()>>>,
}

impl FreedesktopNotifications {
    /// Connects to the session bus and starts the signal thread. `on_event` runs on that thread;
    /// returning `ControlFlow::Break` stops event delivery.
    pub fn new(on_event: impl Fn(NotifyEvent) -> ControlFlow<()> + Send + 'static) -> Result<Self> {
        if !has_dbus_session() {
            return Err(generic_error(DBUS_SESSION_UNREACHABLE));
        }
        let method_conn = open_blocking_connection()
            .map_err(|err| generic_error(format!("notifications client init failed: {err}")))?;
        let stop_signal_thread = Arc::new(AtomicBool::new(false));
        let signal_thread =
            spawn_signal_thread(on_event, method_conn.clone(), stop_signal_thread.clone())
                .map_err(|err| generic_error(format!("notifications client init failed: {err}")))?;

        Ok(Self {
            method_conn: Mutex::new(Some(method_conn)),
            stop_signal_thread,
            signal_thread: Mutex::new(Some(signal_thread)),
        })
    }

    /// Returns the server-assigned notification id.
    pub fn notify(&self, args: &NotifyArgs) -> Result<u32> {
        self.with_connection("Notify failed", |conn| notify(conn, args))
    }

    pub fn close_notification(&self, id: u32) -> Result<()> {
        self.with_connection("CloseNotification failed", |conn| {
            close_notification(conn, id)
        })
    }

    pub fn get_server_capabilities(&self) -> Result<Vec<String>> {
        self.with_connection("GetCapabilities failed", get_capabilities)
    }

    /// Stops the signal thread and drops the connection. Later calls fail with
    /// "notifications client closed". Also runs on drop.
    pub fn close(&self) {
        self.stop_signal_thread.store(true, Ordering::Release);

        if let Ok(mut signal_thread) = self.signal_thread.lock()
            && let Some(thread) = signal_thread.take()
        {
            let _ = thread.join();
        }

        if let Ok(mut method_conn) = self.method_conn.lock()
            && let Some(conn) = method_conn.take()
        {
            let _ = conn.close();
        }
    }

    fn with_connection<T>(
        &self,
        label: &'static str,
        f: impl FnOnce(&BlockingConnection) -> Result<T>,
    ) -> Result<T> {
        let guard = self
            .method_conn
            .lock()
            .map_err(|_| generic_error("notifications client lock poisoned"))?;
        let conn = guard
            .as_ref()
            .ok_or_else(|| generic_error("notifications client closed"))?;
        f(conn).map_err(|err| generic_error(format!("{label}: {err}")))
    }
}

impl Drop for FreedesktopNotifications {
    fn drop(&mut self) {
        self.close();
    }
}

fn open_blocking_connection() -> zbus::Result<BlockingConnection> {
    zbus::blocking::connection::Builder::session()?
        .method_timeout(CALL_TIMEOUT)
        .build()
}

fn notification_proxy(conn: &BlockingConnection) -> Result<Proxy<'_>> {
    Proxy::new(conn, NOTIFY_DEST, NOTIFY_PATH, NOTIFY_IFACE)
        .map_err(|err| generic_error(err.to_string()))
}

fn notify(conn: &BlockingConnection, args: &NotifyArgs) -> Result<u32> {
    let proxy = notification_proxy(conn)?;
    let actions = flatten_actions(&args.actions);
    let hints = build_hints(&args.hints);
    proxy
        .call(
            "Notify",
            &(
                args.app_name.as_str(),
                args.replaces_id,
                args.app_icon.as_str(),
                args.summary.as_str(),
                args.body.as_str(),
                actions,
                hints,
                args.expire_timeout_ms,
            ),
        )
        .map_err(|err| generic_error(err.to_string()))
}

fn close_notification(conn: &BlockingConnection, id: u32) -> Result<()> {
    notification_proxy(conn)?
        .call("CloseNotification", &(id,))
        .map_err(|err| generic_error(err.to_string()))
}

fn get_capabilities(conn: &BlockingConnection) -> Result<Vec<String>> {
    notification_proxy(conn)?
        .call("GetCapabilities", &())
        .map_err(|err| generic_error(err.to_string()))
}

fn flatten_actions(actions: &[NotifyAction]) -> Vec<&str> {
    let mut out = Vec::with_capacity(actions.len() * 2);
    for action in actions {
        out.push(action.key.as_str());
        out.push(action.label.as_str());
    }
    out
}

fn build_hints(hints: &NotifyHints) -> HashMap<&'static str, Value<'_>> {
    let mut out = HashMap::new();
    if let Some(urgency) = hints.urgency {
        out.insert("urgency", Value::new(urgency.as_byte()));
    }
    if let Some(category) = hints.category.as_deref() {
        out.insert("category", Value::new(category));
    }
    if let Some(desktop_entry) = hints.desktop_entry.as_deref() {
        out.insert("desktop-entry", Value::new(desktop_entry));
    }
    if let Some(sound_file) = hints.sound_file.as_deref() {
        out.insert("sound-file", Value::new(sound_file));
    }
    if let Some(suppress_sound) = hints.suppress_sound {
        out.insert("suppress-sound", Value::new(suppress_sound));
    }
    if let Some(transient) = hints.transient {
        out.insert("transient", Value::new(transient));
    }
    if let Some(action_icons) = hints.action_icons {
        out.insert("action-icons", Value::new(action_icons));
    }
    if let Some(image) = hints.image_data.as_ref() {
        out.insert(
            "image-data",
            Value::new((
                image.width,
                image.height,
                image.rowstride,
                image.has_alpha,
                image.bits_per_sample,
                image.channels,
                image.data.as_slice(),
            )),
        );
    }
    out
}

fn spawn_signal_thread(
    on_event: impl Fn(NotifyEvent) -> ControlFlow<()> + Send + 'static,
    method_conn: BlockingConnection,
    stop: Arc<AtomicBool>,
) -> std::result::Result<JoinHandle<()>, String> {
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let stop_for_thread = stop.clone();
    let thread = thread::Builder::new()
        .name("fluxer-linux-notifications-signals".to_string())
        .spawn(move || {
            let setup = future::block_on(async {
                let conn = method_conn.inner();
                let rule = MatchRule::builder()
                    .msg_type(MessageType::Signal)
                    .path(NOTIFY_PATH)?
                    .interface(NOTIFY_IFACE)?
                    .build();
                let stream = MessageStream::for_match_rule(rule, conn, Some(64)).await?;
                zbus::Result::Ok((conn, stream))
            });

            let (_conn, mut stream) = match setup {
                Ok(parts) => {
                    let _ = ready_tx.send(Ok(()));
                    parts
                }
                Err(err) => {
                    let _ = ready_tx.send(Err(err.to_string()));
                    return;
                }
            };

            while !stop_for_thread.load(Ordering::Acquire) {
                let timeout = async {
                    async_io::Timer::after(SIGNAL_POLL_INTERVAL).await;
                    None
                };
                let next_message = stream.next().or(timeout);
                match future::block_on(next_message) {
                    Some(Ok(message)) => {
                        if let Some(event) = event_from_signal(&message)
                            && on_event(event).is_break()
                        {
                            break;
                        }
                    }
                    Some(Err(_)) => break,
                    None => {}
                }
            }
        })
        .map_err(|err| err.to_string())?;

    match ready_rx.recv_timeout(SIGNAL_THREAD_START_TIMEOUT) {
        Ok(Ok(())) => Ok(thread),
        Ok(Err(err)) => {
            let _ = thread.join();
            Err(err)
        }
        Err(err) => {
            stop.store(true, Ordering::Release);
            if matches!(err, mpsc::RecvTimeoutError::Disconnected) {
                let _ = thread.join();
            }
            Err(err.to_string())
        }
    }
}

fn event_from_signal(message: &zbus::Message) -> Option<NotifyEvent> {
    let header = message.header();
    if header.interface().map(|iface| iface.as_str()) != Some(NOTIFY_IFACE) {
        return None;
    }

    match header.member()?.as_str() {
        "ActionInvoked" => {
            let (id, action_key): (u32, String) = message.body().deserialize().ok()?;
            Some(NotifyEvent::ActionInvoked { id, action_key })
        }
        "NotificationClosed" => {
            let (id, reason): (u32, u32) = message.body().deserialize().ok()?;
            Some(NotifyEvent::Closed { id, reason })
        }
        _ => None,
    }
}

fn validate_image_data_shape(
    width: i32,
    height: i32,
    rowstride: i32,
    has_alpha: bool,
    bits_per_sample: i32,
    channels: i32,
    data_len: usize,
) -> Result<()> {
    if bits_per_sample != 8 {
        return Err(invalid_arg("imageData.bitsPerSample must be 8"));
    }

    let expected_channels = if has_alpha { 4 } else { 3 };
    if channels != expected_channels {
        return Err(invalid_arg(format!(
            "imageData.channels must be {expected_channels} when hasAlpha is {has_alpha}"
        )));
    }

    let width = usize::try_from(width).map_err(|_| invalid_arg("imageData width is invalid"))?;
    let height = usize::try_from(height).map_err(|_| invalid_arg("imageData height is invalid"))?;
    let rowstride =
        usize::try_from(rowstride).map_err(|_| invalid_arg("imageData.rowstride is invalid"))?;
    let channels =
        usize::try_from(channels).map_err(|_| invalid_arg("imageData.channels is invalid"))?;

    let row_bytes = width
        .checked_mul(channels)
        .ok_or_else(|| invalid_arg("imageData row byte count overflows"))?;
    if rowstride < row_bytes {
        return Err(invalid_arg(
            "imageData.rowstride is smaller than one pixel row",
        ));
    }

    let min_data_len = if height == 1 {
        row_bytes
    } else {
        rowstride
            .checked_mul(height - 1)
            .and_then(|bytes| bytes.checked_add(row_bytes))
            .ok_or_else(|| invalid_arg("imageData byte count overflows"))?
    };
    if data_len < min_data_len {
        return Err(invalid_arg(
            "imageData.data is too short for the supplied dimensions",
        ));
    }

    Ok(())
}

fn generic_error(reason: impl Into<String>) -> NotificationsError {
    NotificationsError::Generic(reason.into())
}

fn invalid_arg(reason: impl Into<String>) -> NotificationsError {
    NotificationsError::InvalidArg(reason.into())
}
