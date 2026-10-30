// SPDX-License-Identifier: AGPL-3.0-or-later

//! The macOS application menu: `buildTemplate` of Menu.ts as a model, then as a Tauri menu that
//! replaces Tauri's default one, rebuilt when the native strings change. Not verified on macOS;
//! the model and the menu built from it are tested on every platform.
//!
//! Differences from Electron's template, each for a missing capability:
//! - "Check for updates" is left out: there is no updater.
//! - Troubleshooting has no "Reset app data and restart": this shell has no app data reset.
//! - Electron's hidden `Cmd+=` zoom item: muda has no hidden items, so the visible "Zoom in"
//!   takes `Cmd+=` and Electron's `Cmd+Plus` (shifted on most layouts) is not bound.
//! - The `pasteAndMatchStyle`, `delete`, `startSpeaking` and `stopSpeaking` roles are plain
//!   items sending the responder-chain action, always enabled.
//! - Electron's nested `window` role item is the Window menu itself, which macOS fills with the
//!   window list.
//!
//! The Help title keeps Electron's zero-width non-joiner, which stops macOS from adding its
//! help search to the menu.

use tauri::menu::{IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu, WINDOW_SUBMENU_ID};
use tauri::{AppHandle, Runtime};

use crate::build_channel::BuildChannel;
use crate::i18n::MainI18n;

const ID_PREFIX: &str = "app-menu.";
/// `MACOS_HELP_MENU_TITLE_AUTODETECT_OPT_OUT`.
const HELP_TITLE_SUFFIX: char = '\u{200C}';

/// The items this shell handles itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Preferences,
    Reload,
    ForceReload,
    ToggleDevtools,
    ActualSize,
    ZoomIn,
    ZoomOut,
    Website,
    GitHub,
    ReportIssue,
    ToggleHardwareAcceleration,
    TroubleshootingReload,
    PasteAndMatchStyle,
    Delete,
    StartSpeaking,
    StopSpeaking,
}

impl Command {
    const ALL: [Self; 16] = [
        Self::Preferences,
        Self::Reload,
        Self::ForceReload,
        Self::ToggleDevtools,
        Self::ActualSize,
        Self::ZoomIn,
        Self::ZoomOut,
        Self::Website,
        Self::GitHub,
        Self::ReportIssue,
        Self::ToggleHardwareAcceleration,
        Self::TroubleshootingReload,
        Self::PasteAndMatchStyle,
        Self::Delete,
        Self::StartSpeaking,
        Self::StopSpeaking,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Preferences => "preferences",
            Self::Reload => "reload",
            Self::ForceReload => "force-reload",
            Self::ToggleDevtools => "toggle-devtools",
            Self::ActualSize => "actual-size",
            Self::ZoomIn => "zoom-in",
            Self::ZoomOut => "zoom-out",
            Self::Website => "website",
            Self::GitHub => "github",
            Self::ReportIssue => "report-issue",
            Self::ToggleHardwareAcceleration => "toggle-hardware-acceleration",
            Self::TroubleshootingReload => "troubleshooting-reload",
            Self::PasteAndMatchStyle => "paste-and-match-style",
            Self::Delete => "delete",
            Self::StartSpeaking => "start-speaking",
            Self::StopSpeaking => "stop-speaking",
        }
    }

    fn menu_id(self) -> String {
        format!("{ID_PREFIX}{}", self.name())
    }

    /// The command of an application menu item id; other menus' ids are not the menu's.
    fn from_menu_id(id: &str) -> Option<Self> {
        let name = id.strip_prefix(ID_PREFIX)?;
        Self::ALL.into_iter().find(|command| command.name() == name)
    }
}

/// Electron roles with a Tauri predefined item; `None` labels keep the system's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Role {
    About(String),
    Services,
    Hide(String),
    HideOthers,
    Unhide,
    Quit(String),
    Close,
    Undo(String),
    Redo(String),
    Cut(String),
    Copy(String),
    Paste(String),
    SelectAll(String),
    ToggleFullScreen(String),
    Minimize(String),
    Zoom(String),
    Front,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    Submenu {
        id: Option<&'static str>,
        label: String,
        entries: Vec<Entry>,
    },
    Item {
        command: Command,
        label: String,
        accelerator: Option<&'static str>,
    },
    Role(Role),
    Separator,
}

/// `buildTemplate` for macOS. `hardware_acceleration_disabled` is the launch's troubleshooting
/// setting, which names the Troubleshooting toggle.
pub fn entries(i18n: &MainI18n, hardware_acceleration_disabled: bool) -> Vec<Entry> {
    let app_name = BuildChannel::current().app_name();
    let t = |key: &str| i18n.t(key, &[("appName", app_name)]);
    let item = |command, key: &str, accelerator| Entry::Item {
        command,
        label: t(key),
        accelerator,
    };
    let submenu = |label: String, entries| Entry::Submenu {
        id: None,
        label,
        entries,
    };
    vec![
        submenu(
            app_name.to_owned(),
            vec![
                Entry::Role(Role::About(t("desktop.appMenu.about"))),
                Entry::Separator,
                item(
                    Command::Preferences,
                    "desktop.appMenu.preferences",
                    Some("Cmd+,"),
                ),
                Entry::Separator,
                Entry::Role(Role::Services),
                Entry::Separator,
                Entry::Role(Role::Hide(t("desktop.appMenu.hide"))),
                Entry::Role(Role::HideOthers),
                Entry::Role(Role::Unhide),
                Entry::Separator,
                Entry::Role(Role::Quit(t("desktop.appMenu.quit"))),
            ],
        ),
        submenu(t("desktop.appMenu.file"), vec![Entry::Role(Role::Close)]),
        submenu(
            t("desktop.appMenu.edit"),
            vec![
                Entry::Role(Role::Undo(t("desktop.appMenu.undo"))),
                Entry::Role(Role::Redo(t("desktop.appMenu.redo"))),
                Entry::Separator,
                Entry::Role(Role::Cut(t("desktop.appMenu.cut"))),
                Entry::Role(Role::Copy(t("desktop.appMenu.copy"))),
                Entry::Role(Role::Paste(t("desktop.appMenu.paste"))),
                item(
                    Command::PasteAndMatchStyle,
                    "desktop.appMenu.pasteAndMatchStyle",
                    Some("Alt+Shift+Cmd+V"),
                ),
                item(Command::Delete, "desktop.appMenu.delete", None),
                Entry::Role(Role::SelectAll(t("desktop.appMenu.selectAll"))),
                Entry::Separator,
                submenu(
                    t("desktop.appMenu.speech"),
                    vec![
                        item(
                            Command::StartSpeaking,
                            "desktop.appMenu.startSpeaking",
                            None,
                        ),
                        item(Command::StopSpeaking, "desktop.appMenu.stopSpeaking", None),
                    ],
                ),
            ],
        ),
        submenu(
            t("desktop.appMenu.view"),
            vec![
                item(Command::Reload, "desktop.appMenu.reload", Some("Cmd+R")),
                item(
                    Command::ForceReload,
                    "desktop.appMenu.forceReload",
                    Some("Shift+Cmd+R"),
                ),
                item(
                    Command::ToggleDevtools,
                    "desktop.appMenu.toggleDeveloperTools",
                    Some("Alt+Cmd+I"),
                ),
                Entry::Separator,
                item(
                    Command::ActualSize,
                    "desktop.appMenu.actualSize",
                    Some("Cmd+0"),
                ),
                item(Command::ZoomIn, "desktop.appMenu.zoomIn", Some("Cmd+=")),
                item(Command::ZoomOut, "desktop.appMenu.zoomOut", Some("Cmd+-")),
                Entry::Separator,
                Entry::Role(Role::ToggleFullScreen(t(
                    "desktop.appMenu.toggleFullScreen",
                ))),
            ],
        ),
        Entry::Submenu {
            id: Some(WINDOW_SUBMENU_ID),
            label: t("desktop.appMenu.window"),
            entries: vec![
                Entry::Role(Role::Minimize(t("desktop.appMenu.minimize"))),
                Entry::Role(Role::Zoom(t("desktop.appMenu.zoomWindow"))),
                Entry::Separator,
                Entry::Role(Role::Front),
            ],
        },
        submenu(
            format!("{}{HELP_TITLE_SUFFIX}", t("desktop.appMenu.help")),
            vec![
                item(Command::Website, "desktop.appMenu.website", None),
                item(Command::GitHub, "desktop.appMenu.github", None),
                Entry::Separator,
                item(Command::ReportIssue, "desktop.appMenu.reportIssue", None),
                Entry::Separator,
                submenu(
                    t("desktop.appMenu.troubleshooting"),
                    vec![
                        item(
                            Command::ToggleHardwareAcceleration,
                            if hardware_acceleration_disabled {
                                "desktop.troubleshooting.enableHardwareAccelerationAndRestart"
                            } else {
                                "desktop.troubleshooting.disableHardwareAccelerationAndRestart"
                            },
                            None,
                        ),
                        item(
                            Command::TroubleshootingReload,
                            "desktop.troubleshooting.reload",
                            None,
                        ),
                    ],
                ),
            ],
        ),
    ]
}

pub fn build<R: Runtime>(app: &AppHandle<R>, entries: &[Entry]) -> tauri::Result<Menu<R>> {
    let menu = Menu::new(app)?;
    for entry in entries {
        menu.append(menu_item(app, entry)?.as_ref())?;
    }
    Ok(menu)
}

fn menu_item<R: Runtime>(
    app: &AppHandle<R>,
    entry: &Entry,
) -> tauri::Result<Box<dyn IsMenuItem<R>>> {
    Ok(match entry {
        Entry::Submenu { id, label, entries } => {
            let submenu = match id {
                Some(id) => Submenu::with_id(app, *id, label, true)?,
                None => Submenu::new(app, label, true)?,
            };
            for entry in entries {
                submenu.append(menu_item(app, entry)?.as_ref())?;
            }
            Box::new(submenu)
        }
        Entry::Item {
            command,
            label,
            accelerator,
        } => Box::new(MenuItem::with_id(
            app,
            command.menu_id(),
            label,
            true,
            *accelerator,
        )?),
        Entry::Role(role) => Box::new(predefined(app, role)?),
        Entry::Separator => Box::new(PredefinedMenuItem::separator(app)?),
    })
}

fn predefined<R: Runtime>(app: &AppHandle<R>, role: &Role) -> tauri::Result<PredefinedMenuItem<R>> {
    match role {
        Role::About(label) => PredefinedMenuItem::about(app, Some(label), None),
        Role::Services => PredefinedMenuItem::services(app, None),
        Role::Hide(label) => PredefinedMenuItem::hide(app, Some(label)),
        Role::HideOthers => PredefinedMenuItem::hide_others(app, None),
        Role::Unhide => PredefinedMenuItem::show_all(app, None),
        Role::Quit(label) => PredefinedMenuItem::quit(app, Some(label)),
        Role::Close => PredefinedMenuItem::close_window(app, None),
        Role::Undo(label) => PredefinedMenuItem::undo(app, Some(label)),
        Role::Redo(label) => PredefinedMenuItem::redo(app, Some(label)),
        Role::Cut(label) => PredefinedMenuItem::cut(app, Some(label)),
        Role::Copy(label) => PredefinedMenuItem::copy(app, Some(label)),
        Role::Paste(label) => PredefinedMenuItem::paste(app, Some(label)),
        Role::SelectAll(label) => PredefinedMenuItem::select_all(app, Some(label)),
        Role::ToggleFullScreen(label) => PredefinedMenuItem::fullscreen(app, Some(label)),
        Role::Minimize(label) => PredefinedMenuItem::minimize(app, Some(label)),
        Role::Zoom(label) => PredefinedMenuItem::maximize(app, Some(label)),
        Role::Front => PredefinedMenuItem::bring_all_to_front(app, None),
    }
}

#[cfg(target_os = "macos")]
mod install {
    use std::ffi::CStr;
    use std::sync::Arc;

    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, Sel};
    use objc2::{class, msg_send};
    use tauri::{AppHandle, Manager, WebviewWindow};

    use super::{Command, build, entries};
    use crate::config::DesktopConfig;
    use crate::events::BridgeEvents;
    use crate::i18n::MainI18n;
    use crate::menu::{self as window_actions, WindowAction};
    use crate::window::MAIN_WINDOW_LABEL;

    const WEBSITE_URL: &str = "https://fluxer.app";
    const GITHUB_URL: &str = "https://github.com/fluxerapp/fluxer";
    const REPORT_ISSUE_URL: &str = "https://feedback.fluxer.com";

    /// `createApplicationMenu`.
    pub fn install(app: &AppHandle) {
        app.on_menu_event(|app, event| {
            if let Some(command) = Command::from_menu_id(event.id().as_ref()) {
                run(app, command);
            }
        });
        set(app);
    }

    /// The `onLocaleChange` subscription.
    pub fn locale_changed(app: &AppHandle) {
        set(app);
    }

    fn set(app: &AppHandle) {
        let disabled = app
            .state::<Arc<DesktopConfig>>()
            .launch_troubleshooting()
            .disable_hardware_acceleration;
        let menu = build(app, &entries(&app.state::<MainI18n>(), disabled));
        if let Err(error) = menu.and_then(|menu| app.set_menu(menu)) {
            tracing::error!(%error, "Failed to create application menu");
        }
    }

    /// The focused window, else the main one: the window a role or the devtools item acts on.
    fn target_window(app: &AppHandle) -> Option<WebviewWindow> {
        app.webview_windows()
            .into_values()
            .find(|window| window.is_focused().unwrap_or(false))
            .or_else(|| app.get_webview_window(MAIN_WINDOW_LABEL))
    }

    fn send_to_main(app: &AppHandle, event: &'static str) {
        app.state::<BridgeEvents>()
            .emit(MAIN_WINDOW_LABEL, event, ());
    }

    /// `[NSApp sendAction:selector to:nil from:nil]`, what Electron's roles did.
    fn send_action(selector: &CStr) {
        // SAFETY: the shared application, and an action message with nil target and sender.
        unsafe {
            let app: Option<Retained<AnyObject>> =
                msg_send![class!(NSApplication), sharedApplication];
            if let Some(app) = app {
                let _: bool = msg_send![
                    &*app,
                    sendAction: Sel::register(selector),
                    to: std::ptr::null_mut::<AnyObject>(),
                    from: std::ptr::null_mut::<AnyObject>()
                ];
            }
        }
    }

    fn open_external(app: &AppHandle, url: &str) {
        if let Err(error) = crate::external::open_external_url(app, url) {
            tracing::warn!(url, %error, "Failed to open an external URL from the menu");
        }
    }

    fn run(app: &AppHandle, command: Command) {
        let on_target = |action| {
            if let Some(window) = target_window(app) {
                window_actions::run(&window, action);
            }
        };
        match command {
            Command::Preferences => crate::tray::send_open_settings(app),
            Command::Reload => on_target(WindowAction::Reload),
            Command::ForceReload => on_target(WindowAction::ForceReload),
            Command::ToggleDevtools => on_target(WindowAction::ToggleDevtools),
            Command::ActualSize => send_to_main(app, "zoom-reset"),
            Command::ZoomIn => send_to_main(app, "zoom-in"),
            Command::ZoomOut => send_to_main(app, "zoom-out"),
            Command::Website => open_external(app, WEBSITE_URL),
            Command::GitHub => open_external(app, GITHUB_URL),
            Command::ReportIssue => open_external(app, REPORT_ISSUE_URL),
            // `setHardwareAccelerationDisabledAndRestart` stores false on macOS and goes on.
            Command::ToggleHardwareAcceleration => {
                app.state::<Arc<DesktopConfig>>()
                    .set_troubleshooting(|settings| settings.disable_hardware_acceleration = false);
                tracing::info!("Hardware acceleration setting ignored on macOS");
            }
            // `reloadMainWindow`.
            Command::TroubleshootingReload => match app.get_webview_window(MAIN_WINDOW_LABEL) {
                Some(window) => {
                    tracing::info!("Reloading main window (ignoring cache)");
                    window_actions::run(&window, WindowAction::ForceReload);
                }
                None => tracing::warn!("Reload requested but no main window is available"),
            },
            Command::PasteAndMatchStyle => send_action(c"pasteAndMatchStyle:"),
            Command::Delete => send_action(c"delete:"),
            Command::StartSpeaking => send_action(c"startSpeaking:"),
            Command::StopSpeaking => send_action(c"stopSpeaking:"),
        }
    }
}

#[cfg(target_os = "macos")]
pub use self::install::{install, locale_changed};

#[cfg(test)]
mod tests {
    use super::*;
    use tauri::menu::MenuItemKind;

    fn labels(entries: &[Entry]) -> Vec<&str> {
        entries
            .iter()
            .map(|entry| match entry {
                Entry::Submenu { label, .. } | Entry::Item { label, .. } => label.as_str(),
                Entry::Role(_) => "<role>",
                Entry::Separator => "-",
            })
            .collect()
    }

    fn submenu(entries: &[Entry], index: usize) -> &[Entry] {
        match &entries[index] {
            Entry::Submenu { entries, .. } => entries,
            other => panic!("not a submenu: {other:?}"),
        }
    }

    fn i18n() -> (tempfile::TempDir, MainI18n) {
        let directory = tempfile::tempdir().unwrap();
        let i18n = MainI18n::load(directory.path());
        (directory, i18n)
    }

    #[test]
    fn the_menus_follow_electrons_macos_template() {
        let (_directory, i18n) = i18n();
        let entries = entries(&i18n, false);
        let app_name = BuildChannel::current().app_name();
        assert_eq!(
            labels(&entries),
            [app_name, "File", "Edit", "View", "Window", "Help\u{200C}"]
        );
        assert_eq!(
            labels(submenu(&entries, 0)),
            [
                "<role>",
                "-",
                "Preferences...",
                "-",
                "<role>",
                "-",
                "<role>",
                "<role>",
                "<role>",
                "-",
                "<role>"
            ]
        );
        assert_eq!(
            submenu(&entries, 0)[0],
            Entry::Role(Role::About(format!("About {app_name}")))
        );
        assert_eq!(
            labels(submenu(&entries, 3)),
            [
                "Reload",
                "Force reload",
                "Toggle developer tools",
                "-",
                "Actual size",
                "Zoom in",
                "Zoom out",
                "-",
                "<role>"
            ]
        );
        assert_eq!(
            labels(submenu(submenu(&entries, 5), 5)),
            ["Disable hardware acceleration and restart", "Reload"]
        );
        let disabled = super::entries(&i18n, true);
        assert_eq!(
            labels(submenu(submenu(&disabled, 5), 5))[0],
            "Enable hardware acceleration and restart"
        );
    }

    #[test]
    fn custom_items_carry_electrons_accelerators() {
        let (_directory, i18n) = i18n();
        let entries = entries(&i18n, false);
        let accelerator = |command: Command| {
            fn find(entries: &[Entry], command: Command) -> Option<Option<&'static str>> {
                entries.iter().find_map(|entry| match entry {
                    Entry::Item {
                        command: found,
                        accelerator,
                        ..
                    } if *found == command => Some(*accelerator),
                    Entry::Submenu { entries, .. } => find(entries, command),
                    _ => None,
                })
            }
            find(&entries, command).expect("the command has an item")
        };
        assert_eq!(accelerator(Command::Preferences), Some("Cmd+,"));
        assert_eq!(accelerator(Command::ToggleDevtools), Some("Alt+Cmd+I"));
        assert_eq!(accelerator(Command::ForceReload), Some("Shift+Cmd+R"));
        assert_eq!(accelerator(Command::ZoomIn), Some("Cmd+="));
        assert_eq!(accelerator(Command::Website), None);
    }

    #[test]
    fn item_ids_round_trip_and_stay_apart_from_the_trays() {
        for command in Command::ALL {
            let id = command.menu_id();
            assert_eq!(Command::from_menu_id(&id), Some(command), "{id}");
            assert!(!id.starts_with("tray."), "{id}");
        }
        assert_eq!(Command::from_menu_id("tray.quit"), None);
        assert_eq!(Command::from_menu_id("app-menu.unknown"), None);
    }

    #[test]
    fn the_built_menu_has_the_models_shape() {
        let (_directory, i18n) = i18n();
        let app = tauri::test::mock_app();
        let menu = build(app.handle(), &entries(&i18n, false)).unwrap();
        let submenus: Vec<_> = menu
            .items()
            .unwrap()
            .into_iter()
            .map(|item| match item {
                MenuItemKind::Submenu(submenu) => submenu,
                _ => panic!("a top-level item is not a submenu"),
            })
            .collect();
        assert_eq!(submenus.len(), 6);
        assert_eq!(submenus[4].id().as_ref(), WINDOW_SUBMENU_ID);
        assert_eq!(submenus[5].text().unwrap(), "Help\u{200C}");
        let view = submenus[3].items().unwrap();
        assert_eq!(view.len(), 9);
        assert_eq!(view[0].id().as_ref(), "app-menu.reload");
        assert!(matches!(view[8], MenuItemKind::Predefined(_)));
    }
}
