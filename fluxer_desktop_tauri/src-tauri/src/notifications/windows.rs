// SPDX-License-Identifier: AGPL-3.0-or-later

//! The Windows path of NotificationsIpc.ts, where Electron used its `Notification`: WinRT toasts
//! shown under [`BuildChannel::app_user_model_id`]. Showing an id again hides the toast it had
//! first. The icon goes through the on-disk cache, since a toast takes an image by path.
//! `Activated` is a click and `Dismissed` (timed out, closed by the user or hidden) ends the
//! notification, each only while the toast is still the id's current one; closing hides the
//! toast.
//!
//! Gaps, both left to packaging (P8): a click in Action Center after the process has exited
//! needs a COM activator registered under the toast activator CLSID, and an unpackaged process
//! shows toasts only once a Start menu shortcut carries its AppUserModelID (G012); without one
//! `Show` may succeed with nothing on screen. Not verified on Windows.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

use tauri::{AppHandle, Manager};
use windows::Data::Xml::Dom::XmlDocument;
use windows::Foundation::TypedEventHandler;
use windows::UI::Notifications::{
    ToastDismissedEventArgs, ToastFailedEventArgs, ToastNotification, ToastNotificationManager,
    ToastNotifier,
};
use windows::core::{HSTRING, IInspectable, Ref};

use super::toast_xml::toast_xml;
use super::{NotificationOptions, Notifications, click_payload};
use crate::build_channel::BuildChannel;

struct Toast {
    /// Tells this toast's events from those of a toast shown earlier under the same id.
    serial: u64,
    notification: ToastNotification,
}

#[derive(Default)]
pub struct Toasts {
    notifier: Mutex<Option<ToastNotifier>>,
    active: Mutex<HashMap<String, Toast>>,
    next_serial: AtomicU64,
}

impl Toasts {
    fn active(&self) -> MutexGuard<'_, HashMap<String, Toast>> {
        self.active.lock().expect("toast lock")
    }

    fn notifier(&self) -> windows::core::Result<ToastNotifier> {
        let mut notifier = self.notifier.lock().expect("toast notifier lock");
        if let Some(notifier) = notifier.as_ref() {
            return Ok(notifier.clone());
        }
        let created = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(
            BuildChannel::current().app_user_model_id(),
        ))?;
        *notifier = Some(created.clone());
        Ok(created)
    }

    fn hide(&self, toast: &Toast) {
        if let Err(error) = self
            .notifier()
            .and_then(|notifier| notifier.Hide(&toast.notification))
        {
            tracing::warn!(%error, "Failed to hide a toast");
        }
    }

    /// Ends the toast if it is still the current one for `id`.
    fn end(&self, id: &str, serial: u64) -> bool {
        let mut active = self.active();
        if active.get(id).is_some_and(|toast| toast.serial == serial) {
            active.remove(id);
            return true;
        }
        false
    }

    /// The Windows `show-notification` path.
    pub async fn show(&self, app: &AppHandle, id: &str, options: &NotificationOptions) {
        let previous = self.active().remove(id);
        if let Some(previous) = previous {
            self.hide(&previous);
        }
        let icon = match options.icon() {
            Some(source) => cached_icon(app, source).await,
            None => None,
        };
        let serial = self.next_serial.fetch_add(1, Ordering::Relaxed);
        let xml = toast_xml(
            &options.title,
            &options.body,
            icon.as_ref()
                .and_then(|path| url::Url::from_file_path(path).ok())
                .as_ref()
                .map(url::Url::as_str),
        );
        if let Err(error) = self.present(app, id, serial, &xml) {
            tracing::warn!(%error, "Failed to show a toast");
            // Nothing shows, so the target goes too, unless a later show owns the id by now.
            let notifications = app.state::<Notifications>();
            let mut targets = notifications.targets();
            let mut active = self.active();
            if active.get(id).is_none_or(|toast| toast.serial == serial) {
                active.remove(id);
                targets.forget(id);
            }
        }
    }

    fn present(
        &self,
        app: &AppHandle,
        id: &str,
        serial: u64,
        xml: &str,
    ) -> windows::core::Result<()> {
        let document = XmlDocument::new()?;
        document.LoadXml(&HSTRING::from(xml))?;
        let notification = ToastNotification::CreateToastNotification(&document)?;
        let activated = (app.clone(), id.to_owned());
        notification.Activated(&TypedEventHandler::new(
            move |_: Ref<ToastNotification>, _: Ref<IInspectable>| {
                let (app, id) = &activated;
                on_activated(app, id, serial);
                Ok(())
            },
        ))?;
        let dismissed = (app.clone(), id.to_owned());
        notification.Dismissed(&TypedEventHandler::new(
            move |_: Ref<ToastNotification>, _: Ref<ToastDismissedEventArgs>| {
                let (app, id) = &dismissed;
                on_dismissed(app, id, serial);
                Ok(())
            },
        ))?;
        notification.Failed(&TypedEventHandler::new(
            |_: Ref<ToastNotification>, args: Ref<ToastFailedEventArgs>| {
                let code = args.ok().and_then(ToastFailedEventArgs::ErrorCode);
                tracing::warn!(?code, "A toast failed to show");
                Ok(())
            },
        ))?;
        self.active().insert(
            id.to_owned(),
            Toast {
                serial,
                notification: notification.clone(),
            },
        );
        self.notifier()?.Show(&notification)
    }
}

/// `closeNotificationHandle`. The toast is hidden with no lock held: hiding raises `Dismissed`.
pub fn close(notifications: &Notifications, id: &str) {
    let toast = {
        let mut targets = notifications.targets();
        let toast = notifications.toasts.active().remove(id);
        if toast.is_some() {
            targets.forget(id);
        }
        toast
    };
    if let Some(toast) = toast {
        notifications.toasts.hide(&toast);
    }
}

fn on_activated(app: &AppHandle, id: &str, serial: u64) {
    let notifications = app.state::<Notifications>();
    let mut targets = notifications.targets();
    if !notifications.toasts.end(id, serial) {
        return;
    }
    let click = click_payload(id.to_owned(), targets.take(id));
    drop(targets);
    super::deliver_click(app, click);
}

fn on_dismissed(app: &AppHandle, id: &str, serial: u64) {
    let notifications = app.state::<Notifications>();
    let mut targets = notifications.targets();
    if notifications.toasts.end(id, serial) {
        targets.forget(id);
    }
}

/// `resolveNotificationIcon` on Windows: the decoded icon's cached file.
async fn cached_icon(app: &AppHandle, source: &str) -> Option<PathBuf> {
    let image = super::resolve_icon(app, source).await?;
    let app = app.clone();
    let source = source.to_owned();
    let stored = tauri::async_runtime::spawn_blocking(move || {
        app.state::<Notifications>()
            .icon_cache
            .store(&source, &image)
    })
    .await
    .map_err(|error| error.to_string())
    .and_then(|stored| stored.map_err(|error| error.to_string()));
    stored
        .inspect_err(|error| tracing::warn!(%error, "Failed to cache the notification icon"))
        .ok()
}
