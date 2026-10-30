// SPDX-License-Identifier: AGPL-3.0-or-later

//! Desktop notifications, the port of fluxer_desktop/src/main/NotificationsIpc.ts:
//! `show-notification`, `close-notification`, `close-notifications`,
//! `notification-sound-allowed` and the `notification-click` event.
//!
//! A notification gets the renderer's id, or `notification-<n>` when it has none, and its click
//! target (account and URL) is remembered before the platform shows it. Linux sends it to the
//! freedesktop notification server, Windows shows a toast, macOS posts a user notification.
//! A click takes the target, shows and focuses the main window, and sends
//! `[id, url, accountKey]` to the main window; without a main window the target is still taken
//! and nothing is sent. Electron raised the window with `restore`/`show`/`focus` on the
//! BrowserWindow itself; here it goes through `showWindow` ([`window::show_main_window`]),
//! which also keeps the window on screen, recovers a lost Linux renderer and leaves a window
//! the reveal gate still holds to the reveal.
//!
//! Not ported: the Linux notification launch modes of LaunchOptions.ts
//! (`--fluxer-disable-native-notifications` and friends), so Linux notifications are always
//! native.

pub mod commands;
#[cfg(any(not(target_os = "macos"), test))]
mod icon;
#[cfg(any(windows, test))]
mod icon_cache;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
mod sound;
#[cfg(any(target_os = "macos", test))]
mod sound_macos;
mod targets;
#[cfg(any(windows, test))]
mod toast_xml;
#[cfg(windows)]
mod windows;

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

use serde::Deserialize;
use serde_json::Value;
use tauri::{AppHandle, Manager};

use self::sound::SoundPolicy;
use self::targets::{NotificationTarget, NotificationTargets, read_account_key, read_url};
use crate::error::BridgeError;
use crate::events::BridgeEvents;
use crate::window::{self, MAIN_WINDOW_LABEL};

const CLICK_EVENT: &str = "notification-click";

/// `[id, url, accountKey]`, the arguments Electron sent with `notification-click`.
pub type ClickPayload = (String, Option<String>, Option<String>);

/// The renderer's `NotificationOptions`. Fields Electron read loosely stay JSON values; the
/// title and body were handed to native code as strings. The subtitle is read on macOS only,
/// the icon everywhere else.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationOptions {
    #[serde(default)]
    id: Option<Value>,
    title: String,
    body: String,
    #[cfg(target_os = "macos")]
    #[serde(default)]
    subtitle: Option<Value>,
    #[cfg(not(target_os = "macos"))]
    #[serde(default)]
    icon: Option<Value>,
    #[serde(default)]
    url: Option<Value>,
    #[serde(default)]
    account_key: Option<Value>,
}

impl NotificationOptions {
    /// The icon source, when it is a non-empty string.
    #[cfg(not(target_os = "macos"))]
    fn icon(&self) -> Option<&str> {
        self.icon.as_ref()?.as_str().filter(|icon| !icon.is_empty())
    }

    /// The subtitle, shown on macOS only.
    #[cfg(target_os = "macos")]
    fn subtitle(&self) -> Option<&str> {
        self.subtitle
            .as_ref()?
            .as_str()
            .filter(|subtitle| !subtitle.is_empty())
    }
}

pub struct Notifications {
    targets: Mutex<NotificationTargets>,
    /// `notificationIdCounter`.
    next_id: AtomicU64,
    sound: SoundPolicy,
    #[cfg(target_os = "linux")]
    linux: linux::LinuxNotifications,
    #[cfg(windows)]
    toasts: windows::Toasts,
    #[cfg(windows)]
    icon_cache: icon_cache::IconCache,
    #[cfg(target_os = "macos")]
    mac: macos::MacNotifications,
}

impl Notifications {
    #[cfg_attr(not(windows), allow(unused_variables))]
    pub fn new(app_data_dir: &Path) -> Self {
        Self {
            targets: Mutex::default(),
            next_id: AtomicU64::new(0),
            sound: SoundPolicy::default(),
            #[cfg(target_os = "linux")]
            linux: linux::LinuxNotifications::default(),
            #[cfg(windows)]
            toasts: windows::Toasts::default(),
            #[cfg(windows)]
            icon_cache: icon_cache::IconCache::new(app_data_dir),
            #[cfg(target_os = "macos")]
            mac: macos::MacNotifications::default(),
        }
    }

    /// Lock order: the targets before any platform state.
    fn targets(&self) -> MutexGuard<'_, NotificationTargets> {
        self.targets.lock().expect("notification targets lock")
    }

    /// `getNotificationId`.
    fn id_for(&self, requested: Option<&Value>) -> String {
        match requested.and_then(Value::as_str) {
            Some(id) if !id.is_empty() => id.to_owned(),
            _ => format!(
                "notification-{}",
                self.next_id.fetch_add(1, Ordering::Relaxed) + 1
            ),
        }
    }

    /// `notification-sound-allowed`.
    pub async fn allows_sound(&self) -> bool {
        self.sound.allows_sound().await
    }
}

/// The `show-notification` handler after its sender check: the id shown.
pub async fn show(app: &AppHandle, options: NotificationOptions) -> Result<String, BridgeError> {
    let notifications = app.state::<Notifications>();
    let id = notifications.id_for(options.id.as_ref());
    notifications.targets().remember(
        &id,
        NotificationTarget {
            account_key: read_account_key(options.account_key.as_ref()),
            url: read_url(options.url.as_ref()),
        },
    );
    #[cfg(target_os = "linux")]
    notifications.linux.show(app, &id, &options).await?;
    #[cfg(windows)]
    notifications.toasts.show(app, &id, &options).await;
    #[cfg(target_os = "macos")]
    notifications.mac.show(&id, &options);
    Ok(id)
}

/// `close-notification`: ids that are not showing are ignored.
pub fn close(app: &AppHandle, id: &str) {
    let notifications = app.state::<Notifications>();
    #[cfg(target_os = "linux")]
    notifications.linux.close(id, &mut notifications.targets());
    #[cfg(windows)]
    windows::close(&notifications, id);
    #[cfg(target_os = "macos")]
    notifications.mac.close(id, &mut notifications.targets());
}

fn click_payload(id: String, target: Option<NotificationTarget>) -> ClickPayload {
    let target = target.unwrap_or_default();
    (id, target.url, target.account_key)
}

/// `sendNotificationClick` once the target is taken. Runs on the main thread, as Electron's
/// notification events did.
fn deliver_click(app: &AppHandle, click: ClickPayload) {
    let handle = app.clone();
    let scheduled = app.run_on_main_thread(move || {
        if handle.get_webview_window(MAIN_WINDOW_LABEL).is_none() {
            return;
        }
        window::show_main_window(&handle);
        handle
            .state::<BridgeEvents>()
            .emit(MAIN_WINDOW_LABEL, CLICK_EVENT, click);
    });
    if let Err(error) = scheduled {
        tracing::warn!(%error, "Failed to deliver a notification click");
    }
}

/// `resolveNotificationIcon`'s image; failures are logged and leave the notification without
/// an icon.
#[cfg(not(target_os = "macos"))]
async fn resolve_icon(app: &AppHandle, source: &str) -> Option<crate::png_image::RgbaImage> {
    let outbound = app
        .state::<std::sync::Arc<crate::runtime::RuntimeConfig>>()
        .outbound();
    let local_app = app.state::<std::sync::Arc<crate::local_app::LocalApp>>();
    let resolved = icon::resolve(
        source,
        icon::IconSources {
            outbound: &outbound,
            files: local_app.files(),
        },
    )
    .await;
    resolved
        .inspect_err(|error| {
            tracing::warn!(%error, source = %icon::describe(source), "Failed to load notification icon");
        })
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ids_are_the_renderers_or_numbered() {
        let notifications = Notifications::new(Path::new("."));
        assert_eq!(notifications.id_for(Some(&json!("m1"))), "m1");
        assert_eq!(notifications.id_for(None), "notification-1");
        assert_eq!(notifications.id_for(Some(&json!(""))), "notification-2");
        assert_eq!(notifications.id_for(Some(&json!(12))), "notification-3");
    }

    #[test]
    fn a_click_carries_the_target_or_nulls() {
        let target = NotificationTarget {
            account_key: Some("a".to_owned()),
            url: Some("/channels/1".to_owned()),
        };
        assert_eq!(
            serde_json::to_value(click_payload("m1".to_owned(), Some(target))).unwrap(),
            json!(["m1", "/channels/1", "a"])
        );
        assert_eq!(
            serde_json::to_value(click_payload("m2".to_owned(), None)).unwrap(),
            json!(["m2", null, null])
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn options_accept_loose_optional_fields() {
        let options: NotificationOptions = serde_json::from_value(json!({
            "title": "t", "body": "b", "icon": 3, "accountKey": ["x"], "url": "/u"
        }))
        .unwrap();
        assert_eq!(options.icon(), None);
        assert_eq!(read_account_key(options.account_key.as_ref()), None);
        assert_eq!(read_url(options.url.as_ref()).as_deref(), Some("/u"));
        assert!(serde_json::from_value::<NotificationOptions>(json!({"title": "t"})).is_err());
    }
}
