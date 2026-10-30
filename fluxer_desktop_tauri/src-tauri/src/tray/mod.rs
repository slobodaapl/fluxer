// SPDX-License-Identifier: AGPL-3.0-or-later

//! The tray icon, the port of fluxer_desktop/src/main/DesktopTray.ts: the icon (coloured or
//! monochrome, with an unread dot), its tooltip, its menu, and the actions it sends the
//! renderer. Left click toggles the main window (outside macOS), double click shows it.
//!
//! The `showTrayIcon` setting adds or removes the icon, except on Linux, where the icon cannot
//! come back once created and a change waits for a restart (the pending-restart flag of the
//! window behavior settings follows the same rule). On Linux menu rebuilds are coalesced and
//! menu actions run 25 ms after the click, as in Electron. Quit and restart from the tray
//! force the process out if shutdown has not finished 5 s later.
//!
//! The Linux icon is a StatusNotifierItem (tray-icon's ksni backend); without a
//! StatusNotifierWatcher on the session bus there is no tray icon, where Chromium fell back to
//! an XEmbed icon. Electron's macOS position GUID has no Tauri counterpart. A main window that
//! is gone is not recreated: this shell keeps one main window for the process.
//!
//! Tauri tray and menu calls are made with the tray's lock released: off the main thread they
//! wait for it.

mod actions;
pub mod commands;
mod icon;
mod menu;
mod state;
#[cfg(windows)]
mod windows_theme;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Webview};

use self::actions::{PendingActions, TrayAction};
use self::icon::{Appearance, Platform};
use self::menu::Command;
use self::state::TrayRuntimeState;
use crate::build_channel::BuildChannel;
use crate::config::DesktopConfig;
use crate::documents::{Caller, DocumentOwner, DocumentRegistry};
use crate::events::BridgeEvents;
use crate::i18n::MainI18n;
use crate::png_image::{self, RgbaImage};
use crate::unread_badge;
use crate::window::{self, MAIN_WINDOW_LABEL, policy::MainWindowPolicy};

const TRAY_ID: &str = "fluxer-tray";
const TRAY_ACTION_EVENT: &str = "tray-action";
const OPEN_SETTINGS_EVENT: &str = "open-settings";
/// `LINUX_TRAY_MENU_ACTION_DELAY_MS`.
const LINUX_MENU_ACTION_DELAY: Duration = Duration::from_millis(25);
/// `TRAY_FORCED_EXIT_DELAY_MS`.
const FORCED_EXIT_DELAY: Duration = Duration::from_secs(5);
const RUNTIME_STATE_CONTEXT: &str = "tray-runtime-state-update";

type IconKey = (Appearance, bool);

#[derive(Default)]
struct Inner {
    icon: Option<TrayIcon>,
    linux_created_in_process: bool,
    linux_recreate_warning_logged: bool,
    state: TrayRuntimeState,
    /// `trayUnreadBadge`.
    unread_badge: i64,
    /// `trayBaseIcon`; retried while it fails to decode.
    base: Option<RgbaImage>,
    images: HashMap<IconKey, RgbaImage>,
    applied: Option<IconKey>,
    pending: PendingActions,
    /// The document that last reported tray state (`trayActionBridgeWebContentsId`).
    bridge: Option<DocumentOwner>,
    /// An `open-settings` waiting for the main window's page to load.
    open_settings_pending: bool,
}

#[derive(Default)]
pub struct DesktopTray {
    inner: Mutex<Inner>,
    menu_refresh_scheduled: AtomicBool,
    flush_scheduled: AtomicBool,
    restart_requested: AtomicBool,
    forced_exit_armed: AtomicBool,
}

fn lock(app: &AppHandle) -> MutexGuard<'_, Inner> {
    // The tray is managed for the app's lifetime, so the guard can borrow the handle.
    app.state::<DesktopTray>()
        .inner()
        .inner
        .lock()
        .expect("desktop tray lock")
}

fn tray(app: &AppHandle) -> &DesktopTray {
    app.state::<DesktopTray>().inner()
}

/// Runs `task` on the main thread later, like Electron's `setTimeout(task, 0)`.
fn defer(app: &AppHandle, delay: Duration, task: impl FnOnce(&AppHandle) + Send + 'static) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        let handle = app.clone();
        if let Err(error) = app.run_on_main_thread(move || task(&handle)) {
            tracing::debug!(%error, "Dropped a deferred tray task");
        }
    });
}

fn main_window(app: &AppHandle) -> Option<tauri::WebviewWindow> {
    app.get_webview_window(MAIN_WINDOW_LABEL)
}

/// Shown and not minimized.
fn window_visible(app: &AppHandle) -> bool {
    main_window(app).is_some_and(|window| {
        window.is_visible().unwrap_or(false) && !window.is_minimized().unwrap_or(false)
    })
}

fn has_icon(app: &AppHandle) -> bool {
    lock(app).icon.is_some()
}

#[cfg(windows)]
fn dark_system_ui() -> bool {
    windows_theme::uses_dark_system_ui()
}

#[cfg(not(windows))]
fn dark_system_ui() -> bool {
    false
}

/// `resolveTrayIcon`: the composed image for the current appearance and badge, cached.
fn resolve_icon(app: &AppHandle, inner: &mut Inner) -> Option<(IconKey, RgbaImage)> {
    if inner.base.is_none() {
        match png_image::decode(icon::BASE_ICON) {
            Ok(base) => inner.base = Some(base),
            Err(error) => tracing::warn!(%error, "Tray icon candidate could not be decoded"),
        }
    }
    let base = inner.base.as_ref()?;
    let colored = app
        .state::<Arc<DesktopConfig>>()
        .window_behavior()
        .colored_tray_icon;
    let appearance = icon::appearance(Platform::CURRENT, colored, dark_system_ui());
    let key = (appearance, inner.unread_badge != 0);
    let image = inner
        .images
        .entry(key)
        .or_insert_with(|| icon::render(base, appearance, key.1, Platform::CURRENT));
    Some((key, image.clone()))
}

/// `getTrayToolTip`.
fn tooltip(app: &AppHandle, unread_badge: i64) -> String {
    let app_name = BuildChannel::current().app_name();
    match unread_badge::describe(&app.state::<MainI18n>(), unread_badge) {
        Some(description) => format!("{app_name} - {description}"),
        None => app_name.to_owned(),
    }
}

fn build_menu(app: &AppHandle) -> tauri::Result<tauri::menu::Menu<tauri::Wry>> {
    let visible = window_visible(app);
    let state = lock(app).state.clone();
    menu::build(app, &menu::entries(&state, visible, &app.state::<MainI18n>()))
}

/// `syncTrayIcon`.
fn sync_icon(app: &AppHandle, force: bool) {
    let (icon, image, badge) = {
        let mut inner = lock(app);
        let Some(icon) = inner.icon.clone() else {
            return;
        };
        let image = match resolve_icon(app, &mut inner) {
            Some((key, image)) if force || inner.applied != Some(key) => {
                inner.applied = Some(key);
                Some(image)
            }
            Some(_) => None,
            None => {
                tracing::warn!("Tray icon asset could not be reloaded; keeping existing tray image");
                None
            }
        };
        (icon, image, inner.unread_badge)
    };
    if let Some(image) = image
        && let Err(error) = icon.set_icon(Some(image.as_image()))
    {
        tracing::warn!(%error, "Failed to update the tray icon");
    }
    if let Err(error) = icon.set_tooltip(Some(tooltip(app, badge))) {
        tracing::warn!(%error, "Failed to update the tray tooltip");
    }
}

/// `setCurrentTrayContextMenu`.
fn set_current_menu(app: &AppHandle) {
    let Some(icon) = lock(app).icon.clone() else {
        return;
    };
    if let Err(error) = build_menu(app).and_then(|menu| icon.set_menu(Some(menu))) {
        tracing::warn!(%error, "Failed to refresh desktop tray context menu");
    }
}

/// `refreshDesktopTrayMenu`: the menu again, once per main-loop turn on Linux.
pub fn refresh_menu(app: &AppHandle) {
    if !has_icon(app) {
        return;
    }
    if cfg!(target_os = "linux") {
        if tray(app).menu_refresh_scheduled.swap(true, Ordering::SeqCst) {
            return;
        }
        defer(app, Duration::ZERO, |app| {
            tray(app).menu_refresh_scheduled.store(false, Ordering::SeqCst);
            set_current_menu(app);
        });
        return;
    }
    set_current_menu(app);
}

/// `createTray`.
fn create_icon(app: &AppHandle) -> Option<TrayIcon> {
    let Some(((appearance, indicator), image)) = resolve_icon(app, &mut lock(app)) else {
        tracing::warn!("Tray icon asset could not be resolved; tray disabled");
        return None;
    };
    let badge = lock(app).unread_badge;
    let built = build_menu(app).and_then(|menu| {
        TrayIconBuilder::with_id(TRAY_ID)
            .icon(image.as_image())
            .tooltip(tooltip(app, badge))
            .menu(&menu)
            .show_menu_on_left_click(cfg!(target_os = "macos"))
            .icon_as_template(appearance == Appearance::Template)
            .on_tray_icon_event(|icon, event| on_tray_icon_event(icon.app_handle(), event))
            .build(app)
    });
    match built {
        Ok(icon) => {
            let mut inner = lock(app);
            inner.applied = Some((appearance, indicator));
            if cfg!(target_os = "linux") {
                inner.linux_created_in_process = true;
                inner.linux_recreate_warning_logged = false;
            }
            Some(icon)
        }
        Err(error) => {
            tracing::error!(%error, "Failed to create tray icon");
            None
        }
    }
}

/// `canCreateTray`: never while the Linux lifecycle lock holds, nor twice on Linux.
fn can_create(inner: &mut Inner, lifecycle_locked: bool) -> bool {
    if lifecycle_locked {
        return false;
    }
    if !cfg!(target_os = "linux") || !inner.linux_created_in_process {
        return true;
    }
    if !inner.linux_recreate_warning_logged {
        inner.linux_recreate_warning_logged = true;
        tracing::warn!("Suppressing Linux tray recreation after a tray was already created in this process");
    }
    false
}

fn set_icon(app: &AppHandle, icon: Option<TrayIcon>) {
    let active = icon.is_some();
    let previous = std::mem::replace(&mut lock(app).icon, icon);
    if previous.is_some() {
        // The app keeps its own handle; both go for the icon to be removed.
        drop(app.remove_tray_by_id(TRAY_ID));
        drop(previous);
    }
    app.state::<MainWindowPolicy>().set_tray_active(active);
}

/// `revealHiddenWindowIfTrayUnavailable`.
fn reveal_if_unavailable(app: &AppHandle) {
    if has_icon(app) {
        return;
    }
    if main_window(app).is_some_and(|window| !window.is_visible().unwrap_or(true)) {
        window::show_main_window(app);
    }
}

/// `applyDesktopWindowBehaviorSettings`.
pub fn apply_window_behavior(app: &AppHandle) {
    let config = app.state::<Arc<DesktopConfig>>();
    let show = config.window_behavior().show_tray_icon;
    let lifecycle_locked = cfg!(target_os = "linux") && show != config.launch_show_tray_icon();
    if !show {
        if !lifecycle_locked {
            set_icon(app, None);
        }
        reveal_if_unavailable(app);
        return;
    }
    if has_icon(app) {
        sync_icon(app, true);
    } else if can_create(&mut lock(app), lifecycle_locked) {
        let icon = create_icon(app);
        set_icon(app, icon);
    }
    refresh_menu(app);
    reveal_if_unavailable(app);
}

/// `initializeDesktopTray`, after the main window exists.
pub fn initialize(app: &AppHandle) {
    app.on_menu_event(|app, event| {
        if let Some(command) = Command::from_menu_id(event.id().as_ref()) {
            run_menu_action(app, command);
        }
    });
    apply_window_behavior(app);
    #[cfg(windows)]
    windows_theme::watch(app.clone(), |app| sync_icon(app, false));
}

/// `destroyDesktopTray`.
pub fn destroy(app: &AppHandle) {
    {
        let mut inner = lock(app);
        inner.bridge = None;
        inner.pending.clear();
        inner.open_settings_pending = false;
    }
    set_icon(app, None);
}

/// `setDesktopTrayUnreadBadge`.
pub fn set_unread_badge(app: &AppHandle, badge: i64) {
    {
        let mut inner = lock(app);
        if inner.unread_badge == badge {
            return;
        }
        inner.unread_badge = badge;
    }
    sync_icon(app, false);
}

/// The `onLocaleChange` subscription: tooltip and menu.
pub fn locale_changed(app: &AppHandle) {
    let (icon, badge) = {
        let inner = lock(app);
        (inner.icon.clone(), inner.unread_badge)
    };
    if let Some(icon) = icon
        && let Err(error) = icon.set_tooltip(Some(tooltip(app, badge)))
    {
        tracing::warn!(%error, "Failed to update the tray tooltip");
    }
    refresh_menu(app);
}

/// `updateTrayRuntimeState`: the calling document becomes the one tray actions go to.
pub fn update_runtime_state(
    app: &AppHandle,
    webview: &Webview,
    document_token: String,
    update: &serde_json::Value,
) {
    let owner = app
        .state::<Arc<DocumentRegistry>>()
        .capture_for_read(&Caller::new(webview, document_token), RUNTIME_STATE_CONTEXT)
        .ok();
    let build_info = {
        let mut inner = lock(app);
        inner.bridge = owner;
        inner.state.apply(update)
    };
    if let Some(build_info) = build_info {
        tracing::info!("The renderer reported its build: {build_info}");
    }
    refresh_menu(app);
    schedule_flush(app);
}

/// The `did-finish-load` listeners: a deferred `open-settings` and the pending actions.
pub fn page_loaded(app: &AppHandle) {
    let (open_settings, pending) = {
        let mut inner = lock(app);
        (
            std::mem::take(&mut inner.open_settings_pending),
            !inner.pending.is_empty(),
        )
    };
    if open_settings {
        app.state::<BridgeEvents>()
            .emit(MAIN_WINDOW_LABEL, OPEN_SETTINGS_EVENT, ());
    }
    if pending {
        schedule_flush(app);
    }
}

/// `canSendTrayAction`: the reporting document is the main window's current, loaded one.
fn bridge_ready(app: &AppHandle, inner: &Inner) -> bool {
    main_window(app).is_some()
        && inner.bridge.as_ref().is_some_and(|owner| {
            app.state::<Arc<DocumentRegistry>>().is_current(owner)
        })
}

fn send(app: &AppHandle, action: &TrayAction) -> bool {
    app.state::<BridgeEvents>()
        .emit(MAIN_WINDOW_LABEL, TRAY_ACTION_EVENT, action)
}

/// `dispatchTrayAction`.
fn dispatch(app: &AppHandle, action: TrayAction) {
    let ready = bridge_ready(app, &lock(app));
    if ready && send(app, &action) {
        return;
    }
    tracing::debug!(?action, "Queueing tray action until the renderer bridge is ready");
    if let Some(dropped) = lock(app).pending.push(action) {
        tracing::warn!(
            action = ?dropped,
            limit = actions::MAX_PENDING,
            "Dropped oldest pending tray action because the renderer bridge is not ready"
        );
    }
    if main_window(app).is_none() {
        ensure_main_window_visible(app);
    }
}

fn schedule_flush(app: &AppHandle) {
    if tray(app).flush_scheduled.swap(true, Ordering::SeqCst) {
        return;
    }
    defer(app, Duration::ZERO, |app| {
        tray(app).flush_scheduled.store(false, Ordering::SeqCst);
        flush(app);
    });
}

/// `flushPendingTrayActions`.
fn flush(app: &AppHandle) {
    let queued = {
        let mut inner = lock(app);
        if inner.pending.is_empty() || !bridge_ready(app, &inner) {
            return;
        }
        inner.pending.take()
    };
    let undelivered = actions::deliver(queued, |action| send(app, action));
    if !undelivered.is_empty() {
        lock(app).pending.restore(undelivered);
    }
}

/// `ensureMainWindowVisible`.
fn ensure_main_window_visible(app: &AppHandle) {
    window::show_main_window(app);
    refresh_menu(app);
}

/// `toggleMainWindow`.
fn toggle_main_window(app: &AppHandle) {
    if window_visible(app) {
        window::show::hide_main_window(app);
    } else {
        ensure_main_window_visible(app);
    }
    refresh_menu(app);
}

fn on_tray_icon_event(app: &AppHandle, event: TrayIconEvent) {
    match event {
        TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } if !cfg!(target_os = "macos") => toggle_main_window(app),
        TrayIconEvent::DoubleClick {
            button: MouseButton::Left,
            ..
        } => ensure_main_window_visible(app),
        _ => {}
    }
}

/// `runTrayMenuAction`.
fn run_menu_action(app: &AppHandle, command: Command) {
    if cfg!(target_os = "linux") {
        defer(app, LINUX_MENU_ACTION_DELAY, move |app| run_command(app, command));
    } else {
        run_command(app, command);
    }
}

fn run_command(app: &AppHandle, command: Command) {
    match command {
        Command::HideWindow => {
            window::show::hide_main_window(app);
            refresh_menu(app);
        }
        Command::ShowWindow => ensure_main_window_visible(app),
        Command::OpenSettings => open_settings(app),
        Command::SetStatus(status) => {
            lock(app).state.presence_status = Some(status);
            refresh_menu(app);
            dispatch(app, TrayAction::SetStatus { status });
        }
        Command::ToggleMute => dispatch(app, TrayAction::ToggleMute),
        Command::ToggleDeafen => dispatch(app, TrayAction::ToggleDeafen),
        Command::DisconnectVoice => dispatch(app, TrayAction::DisconnectVoice),
        Command::CopyBuildInfo => {
            let build_info = lock(app).state.build_info.clone().unwrap_or_default();
            crate::clipboard::write_text(app, &build_info);
        }
        Command::Restart => restart(app),
        Command::Quit => quit(app),
    }
}

/// The open-settings item: the main window shown, then [`send_open_settings`]. Jump list tasks
/// and the macOS dock menu open settings the same way.
pub fn open_settings(app: &AppHandle) {
    ensure_main_window_visible(app);
    send_open_settings(app);
}

/// `open-settings` to the main window now, or once its loading page has finished.
pub fn send_open_settings(app: &AppHandle) {
    if main_window(app).is_none() {
        return;
    }
    if app
        .state::<Arc<DocumentRegistry>>()
        .is_loading(MAIN_WINDOW_LABEL)
    {
        lock(app).open_settings_pending = true;
    } else {
        app.state::<BridgeEvents>()
            .emit(MAIN_WINDOW_LABEL, OPEN_SETTINGS_EVENT, ());
    }
}

/// `requestAccountSwitcher`, for the `open-account-switcher` launch task.
pub fn request_account_switcher(app: &AppHandle) {
    ensure_main_window_visible(app);
    dispatch(app, TrayAction::OpenAccountSwitcher);
}

/// `armTrayForcedExit`.
fn arm_forced_exit(app: &AppHandle, reason: &'static str) {
    if tray(app).forced_exit_armed.swap(true, Ordering::SeqCst) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("tray-forced-exit".into())
        .spawn(move || {
            std::thread::sleep(FORCED_EXIT_DELAY);
            tracing::warn!(
                reason,
                timeout_ms = FORCED_EXIT_DELAY.as_millis() as u64,
                "Tray shutdown did not complete before the forced-exit timeout"
            );
            std::process::exit(0);
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "Failed to arm the tray forced exit");
    }
}

/// `quitFromTray`. The exit request marks the app quitting (`setQuitting`).
fn quit(app: &AppHandle) {
    destroy(app);
    app.exit(0);
    arm_forced_exit(app, "quit");
}

/// `restartFromTray`, once.
fn restart(app: &AppHandle) {
    if tray(app).restart_requested.swap(true, Ordering::SeqCst) {
        return;
    }
    destroy(app);
    arm_forced_exit(app, "restart");
    crate::config::commands::relaunch_and_exit(app);
}
