// SPDX-License-Identifier: AGPL-3.0-or-later

//! One running instance: `requestSingleInstanceLock` and the `second-instance` listener of
//! fluxer_desktop/src/main/index.ts with `handleSecondInstance` of DeepLinks.ts, on
//! tauri-plugin-single-instance. A second launch hands its argv to the first and exits during
//! the app build, before this shell's setup runs.
//!
//! Electron's lock was keyed on the user data directory. The plugin's is not:
//! - Linux: the D-Bus name `<desktop id>.SingleInstance` on the session bus. Without a session
//!   bus the plugin logs a warning and each launch runs as its own instance.
//! - Windows: a named mutex and a message window keyed on the bundle identifier of
//!   tauri.conf.json, which is the same for every channel.
//! - macOS: a Unix socket `/tmp/<identifier>_si.sock` (the identifier with `.` and `-` as
//!   `_`), shared by every user of the machine, so one user's instance can receive another's
//!   launch. Not verified on macOS.
//!
//! Electron set its second-instance sink before the app was ready, so an early launch never
//! waited for the window: its link took the initial-link slot, a task was dropped and a plain
//! launch showed nothing. This port does the same rather than buffer.

use tauri::plugin::TauriPlugin;
use tauri::{AppHandle, Manager, Wry};

use crate::build_channel::BuildChannel;
use crate::config::launch;
use crate::deep_link;
use crate::jump_list::{self, Task};
use crate::window::{self, policy::MainWindowPolicy};

/// What a second launch asks for, first match wins.
#[derive(Debug, PartialEq, Eq)]
enum Request<'a> {
    Update,
    Task(Task),
    DeepLink(&'a str),
    /// A Squirrel installer hook, which Electron ignored.
    Ignore,
    Show,
}

fn request<'a>(args: &'a [String], scheme: &str) -> Request<'a> {
    if launch::requests_desktop_update(args) {
        return Request::Update;
    }
    if let Some(task) = jump_list::task_from_args(args) {
        return Request::Task(task);
    }
    if let Some(link) = deep_link::from_args(args, scheme) {
        return Request::DeepLink(link);
    }
    if args.iter().any(|arg| arg.starts_with("--squirrel-")) {
        return Request::Ignore;
    }
    Request::Show
}

/// The plugin, to be registered before every other plugin. The callback runs on the plugin's
/// thread (the D-Bus executor on Linux), so the launch is handled on the main thread.
pub fn plugin() -> TauriPlugin<Wry> {
    tauri_plugin_single_instance::Builder::new()
        .dbus_id(BuildChannel::current().desktop_id())
        .callback(|app, args, _cwd| {
            let handle = app.clone();
            if let Err(error) =
                app.run_on_main_thread(move || handle_second_instance(&handle, &args))
            {
                tracing::warn!(%error, "Dropped a second instance launch");
            }
        })
        .build()
}

/// `handleSecondInstance`, ignored once the app is quitting.
fn handle_second_instance(app: &AppHandle, args: &[String]) {
    if app.state::<MainWindowPolicy>().is_quitting() {
        tracing::info!("Ignoring a second instance launch because the app is quitting");
        return;
    }
    match request(args, BuildChannel::current().app_protocol()) {
        Request::Update => {
            tracing::info!("--fluxer-update needs the desktop updater, which this shell lacks");
            window::show_main_window(app);
        }
        Request::Task(task) => jump_list::dispatch_task(app, task),
        Request::DeepLink(link) => deep_link::dispatch(app, link),
        Request::Ignore => {}
        Request::Show => window::show_main_window(app),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn a_second_launch_is_read_in_electrons_order() {
        let cases: [(&[&str], Request<'_>); 7] = [
            (
                &[
                    "app",
                    "--fluxer-update",
                    "--fluxer-task=new-dm",
                    "fluxer://a",
                ],
                Request::Update,
            ),
            (
                &["app", "fluxer://invite/x", "--fluxer-task=open-settings"],
                Request::Task(Task::OpenSettings),
            ),
            (
                &["app", "--fluxer-task=unknown", "fluxer://invite/x"],
                Request::DeepLink("fluxer://invite/x"),
            ),
            (
                &["app", "--url", "--", "fluxer://invite/y"],
                Request::DeepLink("fluxer://invite/y"),
            ),
            (&["app", "--squirrel-firstrun"], Request::Ignore),
            (&["app", "--autostart"], Request::Show),
            (&["app", "https://fluxer.app/invite/x"], Request::Show),
        ];
        for (argv, expected) in cases {
            let argv = args(argv);
            assert_eq!(request(&argv, "fluxer"), expected, "{argv:?}");
        }
    }
}
