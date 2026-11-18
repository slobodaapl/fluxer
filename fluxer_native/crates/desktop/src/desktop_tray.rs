// SPDX-License-Identifier: AGPL-3.0-or-later
//! Linux StatusNotifierItem/dbusmenu. All actions return to the GPUI/worker actor.
//! Reference: DesktopTray.ts, TrayIconImage.ts and DesktopConfig.ts.
use fluxer_client::{CallAction, CallControlSnapshot};
use fluxer_core::Snowflake;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
const WINDOW_BUS: &str = "app.fluxer.Native.Desktop";
#[cfg(target_os = "linux")]
const WINDOW_PATH: &str = "/app/fluxer/Native/Desktop";
#[cfg(target_os = "linux")]
const WINDOW_INTERFACE: &str = "app.fluxer.Native.Desktop1";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Snapshot {
    pub enabled: bool,
    pub colored: bool,
    pub visible: bool,
    pub can_restart: bool,
    pub unread: bool,
    pub account: Option<Snowflake>,
    pub presence: usize,
    pub call: Option<CallControlSnapshot>,
}
#[derive(Clone, Debug)]
pub(super) enum Action {
    Activate {
        action: Box<Action>,
        token: String,
    },
    Toggle,
    ToggleActive,
    Show,
    Settings,
    Presence {
        account: Snowflake,
        index: usize,
    },
    Call {
        expected: CallControlSnapshot,
        action: CallAction,
    },
    CopyBuild,
    Restart,
    Quit,
}
struct Shared {
    snapshot: Mutex<Snapshot>,
    available: AtomicBool,
    running: AtomicBool,
    error: Mutex<Option<String>>,
}
pub(super) struct DesktopTray {
    shared: Arc<Shared>,
    actions: mpsc::Receiver<Action>,
    wake: mpsc::SyncSender<()>,
}
impl DesktopTray {
    pub fn new(enabled: bool, window_service: bool) -> Self {
        let shared = Arc::new(Shared {
            snapshot: Mutex::new(Snapshot::default()),
            available: AtomicBool::new(false),
            running: AtomicBool::new(enabled),
            error: Mutex::new(None),
        });
        let (sender, actions) = mpsc::sync_channel(32);
        let (wake, receiver) = mpsc::sync_channel(1);
        #[cfg(target_os = "linux")]
        if enabled {
            let state = shared.clone();
            if let Err(error) = std::thread::Builder::new()
                .name("fluxer-desktop-tray".into())
                .spawn(move || linux::run(state, sender, receiver, window_service))
            {
                *shared.error.lock().unwrap() = Some(error.to_string());
            }
        }
        #[cfg(not(target_os = "linux"))]
        let _ = (sender, receiver, window_service);
        Self {
            shared,
            actions,
            wake,
        }
    }
    pub fn update(&self, snapshot: Snapshot) {
        let mut previous = self.shared.snapshot.lock().unwrap();
        if *previous != snapshot {
            *previous = snapshot;
            let _ = self.wake.try_send(());
        }
    }
    pub fn available(&self) -> bool {
        self.shared.available.load(Ordering::Acquire)
    }
    pub fn next_action(&self) -> Option<Action> {
        self.actions.try_recv().ok()
    }
    pub fn status(&self) -> String {
        if !cfg!(target_os = "linux") {
            return "System tray support is currently available on Linux.".into();
        }
        if self.available() {
            return "System tray connected. Closing the window can keep Fluxer Native running."
                .into();
        }
        self.shared.error.lock().unwrap().clone().unwrap_or_else(|| "Waiting for a system tray host. The window stays available until an icon can be shown.".into())
    }
}
impl Drop for DesktopTray {
    fn drop(&mut self) {
        self.shared.running.store(false, Ordering::Release);
        let _ = self.wake.try_send(());
    }
}
/// Reopening the desktop launcher raises the existing native window, not another login.
/// No D-Bus auto-activation is installed; name probing cannot recursively launch us.
pub(super) fn activate_existing() -> bool {
    #[cfg(target_os = "linux")]
    {
        let result = (|| -> zbus::Result<()> {
            let connection = zbus::blocking::connection::Builder::session()?
                .method_timeout(Duration::from_millis(700))
                .build()?;
            let bus = zbus::blocking::fdo::DBusProxy::new(&connection)?;
            if !bus.name_has_owner(WINDOW_BUS.try_into().unwrap())? {
                return Err(zbus::Error::Failure("No native window owner".into()));
            }
            let proxy =
                zbus::blocking::Proxy::new(&connection, WINDOW_BUS, WINDOW_PATH, WINDOW_INTERFACE)?;
            let token = std::env::var("XDG_ACTIVATION_TOKEN").unwrap_or_default();
            proxy.call::<_, _, ()>("Activate", &(token,))?;
            Ok(())
        })();
        result.is_ok()
    }
    #[cfg(not(target_os = "linux"))]
    false
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use ksni::blocking::TrayMethods;
    struct WindowInterface(mpsc::SyncSender<Action>);
    #[zbus::interface(name = "app.fluxer.Native.Desktop1")]
    impl WindowInterface {
        fn activate(&self, token: String) -> zbus::fdo::Result<()> {
            let action = if token.is_empty() || token.len() > 4096 {
                Action::Show
            } else {
                Action::Activate {
                    action: Box::new(Action::Show),
                    token,
                }
            };
            self.0.try_send(action).map_err(|_| {
                zbus::fdo::Error::LimitsExceeded("Native window action queue is busy".into())
            })
        }
    }
    pub(super) fn run(
        shared: Arc<Shared>,
        sender: mpsc::SyncSender<Action>,
        wake: mpsc::Receiver<()>,
        own_window_service: bool,
    ) {
        let mut handle: Option<ksni::blocking::Handle<Item>> = None;
        let mut window_service: Option<zbus::blocking::Connection> = None;
        let mut applied = Snapshot::default();
        let mut retry = Instant::now();
        let mut was_enabled = false;
        while shared.running.load(Ordering::Acquire) {
            let snapshot = shared.snapshot.lock().unwrap().clone();
            if snapshot.enabled && !was_enabled {
                retry = Instant::now();
            }
            was_enabled = snapshot.enabled;
            if window_service.as_ref().is_some_and(|c| c.is_closed()) {
                window_service = None;
            }
            if handle.as_ref().is_some_and(|h| h.is_closed()) {
                handle = None;
                shared.available.store(false, Ordering::Release);
            }
            if !snapshot.enabled {
                shared.available.store(false, Ordering::Release);
                if let Some(old) = handle.take() {
                    old.shutdown();
                }
            }
            if Instant::now() >= retry {
                retry = Instant::now() + Duration::from_secs(3);
                if own_window_service && window_service.is_none() {
                    window_service = (|| -> zbus::Result<_> {
                        zbus::blocking::connection::Builder::session()?
                            .method_timeout(Duration::from_secs(2))
                            .name(WINDOW_BUS)?
                            .serve_at(WINDOW_PATH, WindowInterface(sender.clone()))?
                            .build()
                    })()
                    .ok();
                }
                if snapshot.enabled && handle.is_none() {
                    let item = Item {
                        snapshot: snapshot.clone(),
                        sender: sender.clone(),
                        shared: shared.clone(),
                        last_activation: None,
                    };
                    match item.spawn() {
                        Ok(next) => {
                            applied = snapshot.clone();
                            handle = Some(next);
                            shared.available.store(true, Ordering::Release);
                            *shared.error.lock().unwrap() = None;
                        }
                        Err(error) => {
                            shared.available.store(false, Ordering::Release);
                            *shared.error.lock().unwrap() = Some(format!(
                                "System tray unavailable: {error}. Closing the window will quit until a tray host is available."
                            ));
                        }
                    }
                }
            }
            if snapshot != applied
                && let Some(handle) = handle.as_ref()
            {
                handle.update(|item| item.snapshot = snapshot.clone());
                applied = snapshot;
            }
            let _ = wake.recv_timeout(Duration::from_millis(250));
        }
        shared.available.store(false, Ordering::Release);
        if let Some(handle) = handle {
            handle.shutdown();
        }
        if let Some(connection) = window_service {
            let _ = connection.close();
        }
    }
    struct Item {
        snapshot: Snapshot,
        sender: mpsc::SyncSender<Action>,
        shared: Arc<Shared>,
        last_activation: Option<Instant>,
    }
    impl Item {
        fn send(&self, action: Action) {
            let action = if let Some(token) =
                ksni::current_xdg_activation_token().filter(|s| s.len() <= 4096)
            {
                Action::Activate {
                    action: Box::new(action),
                    token,
                }
            } else {
                action
            };
            let _ = self.sender.try_send(action);
        }
    }
    impl ksni::Tray for Item {
        fn id(&self) -> String {
            "app.fluxer.Native".into()
        }
        fn title(&self) -> String {
            "Fluxer Native".into()
        }
        fn category(&self) -> ksni::Category {
            ksni::Category::Communications
        }
        fn activate(&mut self, _x: i32, _y: i32) {
            let double_click = self
                .last_activation
                .is_some_and(|last| last.elapsed() < Duration::from_millis(400));
            self.last_activation = Some(Instant::now());
            self.send(if double_click {
                Action::Show
            } else {
                Action::ToggleActive
            });
        }
        fn secondary_activate(&mut self, _x: i32, _y: i32) {
            self.send(Action::Show);
        }
        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            vec![icon(self.snapshot.colored, self.snapshot.unread)]
        }
        fn tool_tip(&self) -> ksni::ToolTip {
            ksni::ToolTip {
                title: "Fluxer Native".into(),
                description: if self.snapshot.unread {
                    "Unread messages".into()
                } else {
                    String::new()
                },
                ..Default::default()
            }
        }
        fn watcher_offline(&self, _reason: ksni::OfflineReason) -> bool {
            self.shared.available.store(false, Ordering::Release);
            *self.shared.error.lock().unwrap() =
                Some("System tray disconnected; the native window will be restored.".into());
            false // Recreate only after successful registration; never claim availability before it.
        }
        fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
            use ksni::menu::{RadioGroup, RadioItem, StandardItem, SubMenu};
            let item = |label: &str, action: Action| {
                StandardItem {
                    label: label.into(),
                    activate: Box::new(move |this: &mut Self| this.send(action.clone())),
                    ..Default::default()
                }
                .into()
            };
            let account = self.snapshot.account;
            let mut menu = vec![
                item(
                    if self.snapshot.visible {
                        "Hide Fluxer Native"
                    } else {
                        "Show Fluxer Native"
                    },
                    Action::Toggle,
                ),
                item("Open Settings", Action::Settings),
                ksni::MenuItem::Separator,
                SubMenu {
                    label: "Set Status".into(),
                    enabled: account.is_some(),
                    submenu: vec![
                        RadioGroup {
                            selected: self.snapshot.presence,
                            options: ["Online", "Idle", "Do Not Disturb", "Invisible"]
                                .map(|label| RadioItem {
                                    label: label.into(),
                                    ..Default::default()
                                })
                                .to_vec(),
                            select: Box::new(move |this: &mut Self, index| {
                                if let Some(account) = account {
                                    this.send(Action::Presence { account, index })
                                }
                            }),
                        }
                        .into(),
                    ],
                    ..Default::default()
                }
                .into(),
            ];
            if let Some(expected) = self.snapshot.call.clone() {
                menu.push(ksni::MenuItem::Separator);
                for (label, action) in [
                    (
                        if expected.muted {
                            "Unmute Microphone"
                        } else {
                            "Mute Microphone"
                        },
                        CallAction::ToggleMute,
                    ),
                    (
                        if expected.deafened {
                            "Undeafen"
                        } else {
                            "Deafen"
                        },
                        CallAction::ToggleDeaf,
                    ),
                    ("Disconnect Voice", CallAction::Disconnect),
                ] {
                    menu.push(item(
                        label,
                        Action::Call {
                            expected: expected.clone(),
                            action,
                        },
                    ));
                }
            }
            menu.push(ksni::MenuItem::Separator);
            menu.push(item("Copy Build Info", Action::CopyBuild));
            if self.snapshot.can_restart {
                menu.push(item("Restart Fluxer Native", Action::Restart));
            }
            menu.push(item("Quit Fluxer Native", Action::Quit));
            menu
        }
    }
    fn icon(colored: bool, unread: bool) -> ksni::Icon {
        use std::sync::LazyLock;
        static COLOR: LazyLock<image::RgbaImage> = LazyLock::new(|| {
            image::load_from_memory(include_bytes!(
                "../../../../fluxer_desktop/build_resources/icons-stable/48x48.png"
            ))
            .unwrap()
            .into_rgba8()
        });
        static MONO: LazyLock<image::RgbaImage> = LazyLock::new(|| {
            let mut pixels = image::load_from_memory(include_bytes!(
                "../../../../fluxer_desktop/build_resources/icons-stable/FluxerTrayTemplate@2x.png"
            ))
            .unwrap()
            .into_rgba8();
            for pixel in pixels.pixels_mut() {
                pixel.0 = [255, 255, 255, pixel[3]];
            }
            pixels
        });
        let original = if colored { &*COLOR } else { &*MONO };
        let mut pixels =
            image::imageops::resize(original, 48, 48, image::imageops::FilterType::Lanczos3);
        if unread {
            // Source TrayIconImage.ts: a 0.22-radius dot and 0.0625 clear gap.
            let radius = 48. * 0.22;
            let center = (48. - radius, radius);
            let cut = radius + 48. * 0.0625;
            for (x, y, pixel) in pixels.enumerate_pixels_mut() {
                let d = ((x as f32 + 0.5 - center.0).powi(2) + (y as f32 + 0.5 - center.1).powi(2))
                    .sqrt();
                if d < cut {
                    *pixel = if d < radius {
                        image::Rgba([221, 0, 0, 255])
                    } else {
                        image::Rgba([0, 0, 0, 0])
                    };
                }
            }
        }
        ksni::Icon {
            width: 48,
            height: 48,
            data: pixels
                .pixels()
                .flat_map(|p| [p[3], p[0], p[1], p[2]])
                .collect(),
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn icon_is_network_argb_and_only_unread_adds_the_source_red_indicator() {
            let plain = icon(false, false);
            let alert = icon(false, true);
            assert_eq!(plain.data.len(), 48 * 48 * 4);
            assert_ne!(plain.data, alert.data);
            assert!(
                plain
                    .data
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|p| p[0] == 0 || p[1..] == [255, 255, 255])
            );
            assert!(alert.data.as_chunks::<4>().0.contains(&[255, 221, 0, 0]));
        }
        #[test]
        fn menu_dispatch_keeps_the_observed_call_identity() {
            use ksni::Tray;
            let (sender, receiver) = mpsc::sync_channel(32);
            let shared = Arc::new(Shared {
                snapshot: Mutex::new(Snapshot::default()),
                available: AtomicBool::new(false),
                running: AtomicBool::new(false),
                error: Mutex::new(None),
            });
            let call = CallControlSnapshot {
                account_id: 1.into(),
                generation: 7,
                channel_id: 2.into(),
                status: fluxer_client::VoiceStatus::Connected {
                    channel_id: 2.into(),
                },
                muted: false,
                deafened: false,
                server_muted: false,
                server_deafened: false,
                screen_sharing: false,
            };
            let mut tray = Item {
                snapshot: Snapshot {
                    call: Some(call.clone()),
                    ..Default::default()
                },
                sender,
                shared,
                last_activation: None,
            };
            let menu = tray.menu();
            let mute = menu
                .into_iter()
                .find_map(|i| match i {
                    ksni::MenuItem::Standard(i) if i.label == "Mute Microphone" => Some(i),
                    _ => None,
                })
                .unwrap();
            tray.snapshot.call = None;
            (mute.activate)(&mut tray);
            assert!(
                matches!(receiver.try_recv().unwrap(),Action::Call{expected,action:CallAction::ToggleMute} if expected==call)
            );
        }
    }
}
