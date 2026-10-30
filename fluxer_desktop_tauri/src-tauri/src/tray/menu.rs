// SPDX-License-Identifier: AGPL-3.0-or-later

//! The tray menu of `buildTrayMenu` (DesktopTray.ts), as a model built from the tray state and
//! then as a Tauri menu. Items this shell cannot serve yet follow Electron's own rules with the
//! capability absent:
//!
//! - Accounts: Electron shows the submenu for two or more account engine labels; there is no
//!   account engine yet, so there are none.
//! - Update now: shown while a desktop update is available; there is no updater yet.
//! - Check for updates: Electron shows it unless module updates are disabled for the launch, so
//!   with no updater to ask it is left out.
//!
//! The status items are radio items in Electron; muda's tray menus have none, so they are check
//! items checked by the same rule.

use tauri::menu::{CheckMenuItem, IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{AppHandle, Wry};

use super::state::{PresenceStatus, TrayRuntimeState};
use crate::build_channel::BuildChannel;
use crate::i18n::MainI18n;

const ID_PREFIX: &str = "tray.";
/// `FIRST STRONG ISOLATE` and `POP DIRECTIONAL ISOLATE` of `isolateBidiText`.
const FIRST_STRONG_ISOLATE: char = '\u{2068}';
const POP_DIRECTIONAL_ISOLATE: char = '\u{2069}';

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    HideWindow,
    ShowWindow,
    OpenSettings,
    SetStatus(PresenceStatus),
    ToggleMute,
    ToggleDeafen,
    DisconnectVoice,
    CopyBuildInfo,
    Restart,
    Quit,
}

impl Command {
    const ALL: [Self; 13] = [
        Self::HideWindow,
        Self::ShowWindow,
        Self::OpenSettings,
        Self::SetStatus(PresenceStatus::Online),
        Self::SetStatus(PresenceStatus::Idle),
        Self::SetStatus(PresenceStatus::Dnd),
        Self::SetStatus(PresenceStatus::Invisible),
        Self::ToggleMute,
        Self::ToggleDeafen,
        Self::DisconnectVoice,
        Self::CopyBuildInfo,
        Self::Restart,
        Self::Quit,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::HideWindow => "hide-window",
            Self::ShowWindow => "show-window",
            Self::OpenSettings => "open-settings",
            Self::SetStatus(status) => match status {
                PresenceStatus::Online => "status-online",
                PresenceStatus::Idle => "status-idle",
                PresenceStatus::Dnd => "status-dnd",
                PresenceStatus::Invisible => "status-invisible",
            },
            Self::ToggleMute => "toggle-mute",
            Self::ToggleDeafen => "toggle-deafen",
            Self::DisconnectVoice => "disconnect-voice",
            Self::CopyBuildInfo => "copy-build-info",
            Self::Restart => "restart",
            Self::Quit => "quit",
        }
    }

    fn menu_id(self) -> String {
        format!("{ID_PREFIX}{}", self.name())
    }

    /// The command of a tray menu item id; other menus' ids are not the tray's.
    pub fn from_menu_id(id: &str) -> Option<Self> {
        let name = id.strip_prefix(ID_PREFIX)?;
        Self::ALL.into_iter().find(|command| command.name() == name)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    Item { command: Command, label: String },
    Check {
        command: Command,
        label: String,
        checked: bool,
    },
    Separator,
    Submenu { label: String, entries: Vec<Entry> },
}

/// `buildTrayMenu`. `window_visible`: the main window is shown and not minimized.
pub fn entries(state: &TrayRuntimeState, window_visible: bool, i18n: &MainI18n) -> Vec<Entry> {
    let t = |key: &str| i18n.t(key, &[]);
    let item = |command, label| Entry::Item { command, label };
    let status = |label: &str, status: PresenceStatus| Entry::Check {
        command: Command::SetStatus(status),
        label: t(label),
        checked: state.presence_status == Some(status),
    };
    let app_name = [("appName", BuildChannel::current().app_name())];
    let mut entries = vec![
        if window_visible {
            item(Command::HideWindow, i18n.t("desktop.tray.hide", &app_name))
        } else {
            item(Command::ShowWindow, i18n.t("desktop.tray.show", &app_name))
        },
        item(Command::OpenSettings, t("desktop.tray.openSettings")),
        Entry::Separator,
        Entry::Submenu {
            label: t("desktop.tray.status"),
            entries: vec![
                status("desktop.tray.statusOnline", PresenceStatus::Online),
                status("desktop.tray.statusIdle", PresenceStatus::Idle),
                status("desktop.tray.statusDnd", PresenceStatus::Dnd),
                status("desktop.tray.statusInvisible", PresenceStatus::Invisible),
            ],
        },
    ];
    if state.voice_connected {
        entries.push(Entry::Separator);
        entries.push(item(
            Command::ToggleMute,
            t(if state.self_mute {
                "desktop.tray.unmuteMic"
            } else {
                "desktop.tray.muteMic"
            }),
        ));
        entries.push(item(
            Command::ToggleDeafen,
            t(if state.self_deaf {
                "desktop.tray.undeafen"
            } else {
                "desktop.tray.deafen"
            }),
        ));
        let disconnect = match state.voice_channel_label.as_deref() {
            Some(channel) if !channel.is_empty() => {
                let isolated = format!("{FIRST_STRONG_ISOLATE}{channel}{POP_DIRECTIONAL_ISOLATE}");
                i18n.t("desktop.tray.disconnectFrom", &[("channel", &isolated)])
            }
            _ => t("desktop.tray.disconnectVoice"),
        };
        entries.push(item(Command::DisconnectVoice, disconnect));
    }
    entries.push(Entry::Separator);
    if state.build_info.as_deref().is_some_and(|info| !info.is_empty()) {
        entries.push(item(Command::CopyBuildInfo, t("desktop.tray.copyBuildInfo")));
    }
    entries.push(Entry::Separator);
    entries.push(item(Command::Restart, i18n.t("desktop.tray.restart", &app_name)));
    entries.push(item(Command::Quit, i18n.t("desktop.tray.quit", &app_name)));
    entries
}

pub fn build(app: &AppHandle, entries: &[Entry]) -> tauri::Result<Menu<Wry>> {
    let menu = Menu::new(app)?;
    for entry in entries {
        menu.append(menu_item(app, entry)?.as_ref())?;
    }
    Ok(menu)
}

fn menu_item(app: &AppHandle, entry: &Entry) -> tauri::Result<Box<dyn IsMenuItem<Wry>>> {
    Ok(match entry {
        Entry::Item { command, label } => Box::new(MenuItem::with_id(
            app,
            command.menu_id(),
            label,
            true,
            None::<&str>,
        )?),
        Entry::Check {
            command,
            label,
            checked,
        } => Box::new(CheckMenuItem::with_id(
            app,
            command.menu_id(),
            label,
            true,
            *checked,
            None::<&str>,
        )?),
        Entry::Separator => Box::new(PredefinedMenuItem::separator(app)?),
        Entry::Submenu { label, entries } => {
            let submenu = Submenu::new(app, label, true)?;
            for entry in entries {
                submenu.append(menu_item(app, entry)?.as_ref())?;
            }
            Box::new(submenu)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn labels(entries: &[Entry]) -> Vec<String> {
        entries
            .iter()
            .map(|entry| match entry {
                Entry::Item { label, .. } | Entry::Check { label, .. } => label.clone(),
                Entry::Separator => "---".to_owned(),
                Entry::Submenu { label, .. } => format!("{label} >"),
            })
            .collect()
    }

    fn i18n() -> (tempfile::TempDir, MainI18n) {
        let directory = tempfile::tempdir().unwrap();
        let i18n = MainI18n::load(directory.path());
        (directory, i18n)
    }

    #[test]
    fn the_idle_menu_offers_window_settings_status_restart_and_quit() {
        let (_directory, i18n) = i18n();
        let app = BuildChannel::current().app_name();
        let menu = entries(&TrayRuntimeState::default(), false, &i18n);
        assert_eq!(
            labels(&menu),
            [
                format!("Show {app}"),
                "Open settings".to_owned(),
                "---".to_owned(),
                "Status >".to_owned(),
                "---".to_owned(),
                "---".to_owned(),
                format!("Restart {app}"),
                format!("Quit {app}"),
            ]
        );
        let Entry::Submenu { entries: statuses, .. } = &menu[3] else {
            panic!("the status submenu is fourth");
        };
        assert_eq!(
            labels(statuses),
            ["Online", "Idle", "Do not disturb", "Invisible"]
        );
        assert!(statuses
            .iter()
            .all(|entry| matches!(entry, Entry::Check { checked: false, .. })));
        assert_eq!(
            entries(&TrayRuntimeState::default(), true, &i18n)[0],
            Entry::Item {
                command: Command::HideWindow,
                label: format!("Hide {app}")
            }
        );
    }

    #[test]
    fn voice_items_follow_mute_deafen_and_the_channel() {
        let (_directory, i18n) = i18n();
        let mut state = TrayRuntimeState::default();
        state.apply(&json!({
            "voiceConnected": true,
            "selfMute": true,
            "selfDeaf": false,
            "voiceChannelLabel": "general",
            "presenceStatus": "dnd",
            "buildInfo": "build 1",
        }));
        let menu = entries(&state, true, &i18n);
        assert_eq!(
            labels(&menu)[4..],
            [
                "---",
                "Unmute microphone",
                "Deafen",
                "Disconnect from \u{2068}general\u{2069}",
                "---",
                "Copy build info",
                "---",
                &format!("Restart {}", BuildChannel::current().app_name()),
                &format!("Quit {}", BuildChannel::current().app_name()),
            ]
        );
        let Entry::Submenu { entries: statuses, .. } = &menu[3] else {
            panic!("the status submenu is fourth");
        };
        let checked: Vec<bool> = statuses
            .iter()
            .map(|entry| matches!(entry, Entry::Check { checked: true, .. }))
            .collect();
        assert_eq!(checked, [false, false, true, false]);
        // An empty channel label is no label.
        state.apply(&json!({"voiceChannelLabel": "", "selfMute": false, "selfDeaf": true}));
        let menu = labels(&entries(&state, true, &i18n));
        assert_eq!(menu[5..8], ["Mute microphone", "Undeafen", "Disconnect from voice"]);
    }

    #[test]
    fn labels_come_from_the_renderer_strings() {
        let (_directory, i18n) = i18n();
        i18n.set(
            "fr",
            json!({"desktop.tray.quit": "Quitter {appName}", "desktop.tray.openSettings": "Réglages"})
                .as_object()
                .unwrap()
                .clone(),
        );
        let menu = labels(&entries(&TrayRuntimeState::default(), false, &i18n));
        assert_eq!(menu[1], "Réglages");
        assert_eq!(
            menu.last().unwrap(),
            &format!("Quitter {}", BuildChannel::current().app_name())
        );
    }

    #[test]
    fn menu_ids_round_trip_and_foreign_ids_are_ignored() {
        for command in Command::ALL {
            assert_eq!(Command::from_menu_id(&command.menu_id()), Some(command));
        }
        assert_eq!(Command::from_menu_id("quit"), None);
        assert_eq!(Command::from_menu_id("tray.unknown"), None);
    }
}
