// SPDX-License-Identifier: AGPL-3.0-or-later

//! The Linux path of NotificationsIpc.ts: `org.freedesktop.Notifications` through the native
//! client shared with the Electron shell (`fluxer_linux_notifications`), with no fallback. The
//! client is created on the first notification and kept; a failed creation is retried on the
//! next one. Its calls block, so they run on blocking threads; its events arrive on its own
//! signal thread.
//!
//! Each notification is sent with the `default` action, labelled `desktop.notifications.open`,
//! the `im` category, the desktop entry, `suppress-sound` (the renderer plays sounds), no
//! expiry, the icon as `image-data`, and `replaces_id` when the id is already showing. The
//! subtitle is not sent. Invoking any action is a click; a click or a close ends the
//! notification. The desktop entry is [`BuildChannel::desktop_id`], not Electron's
//! `app.fluxer.FluxerDesktop*`.

use std::collections::HashMap;
use std::ops::ControlFlow;
use std::sync::{Arc, Mutex, MutexGuard};

use fluxer_linux_notifications::{
    FreedesktopNotifications, NotifyAction, NotifyArgs, NotifyEvent, NotifyHints, NotifyImageData,
    Urgency,
};
use serde_json::json;
use tauri::{AppHandle, Manager};
use tokio::sync::OnceCell;

use super::targets::NotificationTargets;
use super::{ClickPayload, NotificationOptions, Notifications, click_payload};
use crate::build_channel::BuildChannel;
use crate::config::launch::{self, NativeNotificationsMode};
use crate::error::BridgeError;
use crate::i18n::MainI18n;
use crate::png_image::RgbaImage;

const DEFAULT_ACTION: &str = "default";
const OPEN_LABEL_KEY: &str = "desktop.notifications.open";
/// Electron threw plain `Error`s here.
const ERROR_NAME: &str = "Error";

#[derive(Default)]
pub struct LinuxNotifications {
    client: OnceCell<Arc<FreedesktopNotifications>>,
    ids: Mutex<NativeIds>,
}

/// `activeNotifications` (Linux handles) and `linuxNativeIdToString`.
#[derive(Default)]
struct NativeIds {
    active: HashMap<String, u32>,
    ids: HashMap<u32, String>,
}

impl NativeIds {
    fn shown(&mut self, id: &str, replaced: Option<u32>, native: u32) {
        if let Some(replaced) = replaced
            && replaced != native
        {
            self.ids.remove(&replaced);
        }
        self.ids.insert(native, id.to_owned());
        self.active.insert(id.to_owned(), native);
    }

    /// `cleanupLinuxNativeNotification`.
    fn cleanup(&mut self, id: &str, native: u32, targets: &mut NotificationTargets) {
        if self.active.get(id) == Some(&native) {
            self.active.remove(id);
            targets.forget(id);
        }
        self.ids.remove(&native);
    }

    /// The client's event callback: the click to deliver, if the event is one.
    fn event(
        &mut self,
        event: &NotifyEvent,
        targets: &mut NotificationTargets,
    ) -> Option<ClickPayload> {
        let (NotifyEvent::ActionInvoked { id: native, .. }
        | NotifyEvent::Closed { id: native, .. }) = *event;
        let id = self.ids.get(&native)?.clone();
        let click = matches!(event, NotifyEvent::ActionInvoked { .. })
            .then(|| click_payload(id.clone(), targets.take(&id)));
        self.cleanup(&id, native, targets);
        click
    }

    /// `closeNotificationHandle`: the native id to close, once forgotten here.
    fn close(&mut self, id: &str, targets: &mut NotificationTargets) -> Option<u32> {
        let native = *self.active.get(id)?;
        self.cleanup(id, native, targets);
        Some(native)
    }
}

impl LinuxNotifications {
    fn ids(&self) -> MutexGuard<'_, NativeIds> {
        self.ids.lock().expect("linux notification ids lock")
    }

    /// `ensureLinuxNativeClient`.
    async fn client(&self, app: &AppHandle) -> Result<Arc<FreedesktopNotifications>, BridgeError> {
        // `loadLinuxNotificationsModule` read the launch mode on every load attempt.
        let mode = launch::native_notifications_mode(&launch::process_args(), |name| {
            std::env::var(name).ok()
        })
        .map_err(|error| BridgeError::new(ERROR_NAME, error))?;
        if matches!(
            mode,
            NativeNotificationsMode::Off | NativeNotificationsMode::Electron
        ) {
            return Err(BridgeError::new(
                ERROR_NAME,
                format!(
                    "Linux native notifications are disabled by launch mode ({}); refusing \
                     Electron Notification fallback.",
                    mode.as_str()
                ),
            ));
        }
        self.client
            .get_or_try_init(|| async {
                let app = app.clone();
                tauri::async_runtime::spawn_blocking(move || {
                    FreedesktopNotifications::new(move |event| {
                        on_event(&app, &event);
                        ControlFlow::Continue(())
                    })
                })
                .await
                .map_err(|error| error.to_string())
                .and_then(|created| created.map_err(|error| error.to_string()))
                .map(Arc::new)
                .map_err(|error| {
                    BridgeError::new(
                        ERROR_NAME,
                        format!(
                            "Failed to construct FreedesktopNotifications client; refusing Electron \
                             Notification fallback. Original error: {error}"
                        ),
                    )
                })
            })
            .await
            .cloned()
    }

    /// `showLinuxNativeNotification`.
    pub async fn show(
        &self,
        app: &AppHandle,
        id: &str,
        options: &NotificationOptions,
    ) -> Result<(), BridgeError> {
        let client = self.client(app).await?;
        let replaced = self.ids().active.get(id).copied();
        let image = match options.icon() {
            Some(source) => super::resolve_icon(app, source).await.and_then(image_data),
            None => None,
        };
        let open_label = app.state::<MainI18n>().t(OPEN_LABEL_KEY, &[]);
        let args = notify_args(&options.title, &options.body, replaced, open_label, image);
        let summary = json!({
            "appName": args.app_name,
            "summary": args.summary,
            "hasBody": !args.body.is_empty(),
            "hasImageData": args.hints.image_data.is_some(),
            "desktopEntry": args.hints.desktop_entry,
        });
        let notified = tauri::async_runtime::spawn_blocking(move || client.notify(&args))
            .await
            .map_err(|error| error.to_string())
            .and_then(|notified| notified.map_err(|error| error.to_string()));
        match notified {
            Ok(native) => {
                self.ids().shown(id, replaced, native);
                Ok(())
            }
            Err(error) => Err(BridgeError::new(
                ERROR_NAME,
                format!(
                    "FreedesktopNotifications.notify failed; refusing Electron Notification \
                     fallback. Payload summary: {summary}. Original error: {error}"
                ),
            )),
        }
    }

    /// `closeNotificationHandle`: `CloseNotification` without waiting for it.
    pub fn close(&self, id: &str, targets: &mut NotificationTargets) {
        let Some(native) = self.ids().close(id, targets) else {
            return;
        };
        let Some(client) = self.client.get().cloned() else {
            return;
        };
        tauri::async_runtime::spawn_blocking(move || {
            if let Err(error) = client.close_notification(native) {
                tracing::warn!(%error, "FreedesktopNotifications.closeNotification failed");
            }
        });
    }
}

fn on_event(app: &AppHandle, event: &NotifyEvent) {
    let notifications = app.state::<Notifications>();
    // Lock order: targets, then ids.
    let mut targets = notifications.targets();
    let click = notifications.linux.ids().event(event, &mut targets);
    drop(targets);
    if let Some(click) = click {
        super::deliver_click(app, click);
    }
}

/// `imageToNotifyImageData`: tightly packed RGBA, not resized.
fn image_data(image: RgbaImage) -> Option<NotifyImageData> {
    let width = i32::try_from(image.width).ok()?;
    let height = i32::try_from(image.height).ok()?;
    let rowstride = width.checked_mul(4)?;
    NotifyImageData::new(width, height, rowstride, true, 8, 4, image.pixels)
        .inspect_err(|error| tracing::warn!(%error, "Notification icon is not valid image-data"))
        .ok()
}

/// The `Notify` payload of `showLinuxNativeNotification`.
fn notify_args(
    title: &str,
    body: &str,
    replaced: Option<u32>,
    open_label: String,
    image: Option<NotifyImageData>,
) -> NotifyArgs {
    let channel = BuildChannel::current();
    NotifyArgs {
        app_name: channel.app_name().to_owned(),
        replaces_id: replaced.unwrap_or(0),
        app_icon: String::new(),
        summary: title.to_owned(),
        body: body.to_owned(),
        actions: vec![NotifyAction {
            key: DEFAULT_ACTION.to_owned(),
            label: open_label,
        }],
        hints: NotifyHints {
            urgency: Some(Urgency::Normal),
            category: Some("im".to_owned()),
            desktop_entry: Some(channel.desktop_id().to_owned()),
            sound_file: None,
            suppress_sound: Some(true),
            transient: Some(false),
            action_icons: Some(false),
            image_data: image,
        },
        expire_timeout_ms: -1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notifications::targets::NotificationTarget;

    fn remembered(targets: &mut NotificationTargets, id: &str) {
        targets.remember(
            id,
            NotificationTarget {
                account_key: Some("account".to_owned()),
                url: Some(format!("/channels/{id}")),
            },
        );
    }

    fn invoked(native: u32) -> NotifyEvent {
        NotifyEvent::ActionInvoked {
            id: native,
            action_key: DEFAULT_ACTION.to_owned(),
        }
    }

    #[test]
    fn a_renderer_payload_becomes_a_silent_persistent_im_notification() {
        let options: NotificationOptions = serde_json::from_value(json!({
            "id": "m1", "title": "Ada", "subtitle": "ignored", "body": "hi", "icon": "data:x"
        }))
        .unwrap();
        let image = image_data(RgbaImage {
            width: 2,
            height: 1,
            pixels: vec![1, 2, 3, 4, 5, 6, 7, 8],
        });
        let args = notify_args(
            &options.title,
            &options.body,
            Some(7),
            "Open".to_owned(),
            image,
        );
        let channel = BuildChannel::current();
        assert_eq!(
            (
                args.app_name.as_str(),
                args.summary.as_str(),
                args.body.as_str()
            ),
            (channel.app_name(), "Ada", "hi")
        );
        assert_eq!(
            (
                args.replaces_id,
                args.app_icon.as_str(),
                args.expire_timeout_ms
            ),
            (7, "", -1)
        );
        assert_eq!(
            args.actions,
            [NotifyAction {
                key: "default".to_owned(),
                label: "Open".to_owned()
            }]
        );
        let hints = &args.hints;
        assert_eq!(hints.urgency, Some(Urgency::Normal));
        assert_eq!(hints.category.as_deref(), Some("im"));
        assert_eq!(hints.desktop_entry.as_deref(), Some(channel.desktop_id()));
        assert_eq!(
            (hints.suppress_sound, hints.transient, hints.action_icons),
            (Some(true), Some(false), Some(false))
        );
        assert_eq!(hints.sound_file, None);
        let image = hints.image_data.as_ref().unwrap();
        assert_eq!(
            (
                image.width,
                image.height,
                image.rowstride,
                image.has_alpha,
                image.bits_per_sample,
                image.channels
            ),
            (2, 1, 8, true, 8, 4)
        );
        assert_eq!(image.data, [1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(
            notify_args("t", "b", None, String::new(), None).replaces_id,
            0
        );
    }

    #[test]
    fn an_invoked_action_clicks_once_with_the_remembered_target() {
        let mut targets = NotificationTargets::default();
        let mut ids = NativeIds::default();
        remembered(&mut targets, "m1");
        ids.shown("m1", None, 41);
        let click = ids.event(&invoked(41), &mut targets).unwrap();
        assert_eq!(
            serde_json::to_value(&click).unwrap(),
            json!(["m1", "/channels/m1", "account"])
        );
        assert!(ids.event(&invoked(41), &mut targets).is_none());
        assert!(ids.close("m1", &mut targets).is_none());
    }

    #[test]
    fn a_closed_notification_forgets_its_target() {
        let mut targets = NotificationTargets::default();
        let mut ids = NativeIds::default();
        remembered(&mut targets, "m1");
        ids.shown("m1", None, 41);
        assert!(
            ids.event(&NotifyEvent::Closed { id: 41, reason: 2 }, &mut targets)
                .is_none()
        );
        assert_eq!(targets.take("m1"), None);
        assert!(ids.event(&invoked(41), &mut targets).is_none());
    }

    #[test]
    fn a_replacement_under_a_new_native_id_drops_the_old_one() {
        let mut targets = NotificationTargets::default();
        let mut ids = NativeIds::default();
        ids.shown("m1", None, 41);
        remembered(&mut targets, "m1");
        ids.shown("m1", Some(41), 42);
        assert!(ids.event(&invoked(41), &mut targets).is_none());
        let click = ids.event(&invoked(42), &mut targets).unwrap();
        assert_eq!(click.0, "m1");
    }

    #[test]
    fn closing_returns_the_native_id_and_forgets_the_target() {
        let mut targets = NotificationTargets::default();
        let mut ids = NativeIds::default();
        remembered(&mut targets, "m1");
        ids.shown("m1", None, 41);
        assert_eq!(ids.close("m1", &mut targets), Some(41));
        assert_eq!(targets.take("m1"), None);
        assert_eq!(ids.close("unknown", &mut targets), None);
    }
}
