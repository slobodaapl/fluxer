// SPDX-License-Identifier: AGPL-3.0-or-later

//! The Linux launcher badge and progress: what Electron's `app.setBadgeCount` and
//! `setProgressBar` did through Chromium's libunity calls, which acted only while Unity ran.
//! This shell emits libunity's `com.canonical.Unity.LauncherEntry.Update` signal on the session
//! bus itself, so any dock listening for it shows the entry's count and progress, Unity or not.
//! The entry is `application://<desktop id>.desktop`.
//!
//! Visibility follows Chromium's `unity_service` (from memory, not checked against its source):
//! the count shows when it is not zero, the progress while it is strictly between 0 and 1.
//! Signals go out in order from one thread holding one connection; without a session bus they
//! are dropped.

use std::collections::HashMap;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Mutex, MutexGuard};

use zbus::zvariant::Value;

use crate::build_channel::BuildChannel;

const INTERFACE: &str = "com.canonical.Unity.LauncherEntry";
const SIGNAL: &str = "Update";
const PATH_PREFIX: &str = "/com/canonical/unity/launcherentry/";

#[derive(Clone, Copy, Debug, PartialEq)]
struct EntryState {
    count: i64,
    progress: f64,
}

pub struct LauncherEntry {
    state: Mutex<EntryState>,
    signals: Sender<EntryState>,
}

impl Default for LauncherEntry {
    fn default() -> Self {
        let (signals, receiver) = channel::<EntryState>();
        let spawned = std::thread::Builder::new()
            .name("launcher-entry".into())
            .spawn(move || {
                let mut connection = None;
                for state in receiver {
                    emit(&mut connection, state);
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "Failed to start the launcher entry thread");
        }
        Self {
            state: Mutex::new(EntryState {
                count: 0,
                progress: -1.0,
            }),
            signals,
        }
    }
}

impl LauncherEntry {
    fn lock(&self) -> MutexGuard<'_, EntryState> {
        self.state.lock().expect("launcher entry lock")
    }

    fn update(&self, change: impl FnOnce(&mut EntryState)) {
        let mut state = self.lock();
        change(&mut state);
        // The thread outlives every sender, so a send only fails if it never started.
        let _ = self.signals.send(*state);
    }

    /// `app.setBadgeCount(count)`.
    pub fn set_count(&self, count: i64) {
        self.update(|state| state.count = count);
    }

    /// `setProgressBar(progress)`; a negative progress removes the bar.
    pub fn set_progress(&self, progress: f64) {
        self.update(|state| state.progress = progress);
    }
}

fn app_uri() -> String {
    format!("application://{}.desktop", BuildChannel::current().desktop_id())
}

/// libunity's object path for an entry: `g_str_hash` (djb2) of its URI.
fn object_path(app_uri: &str) -> String {
    let hash = app_uri
        .bytes()
        .fold(5381u32, |hash, byte| hash.wrapping_mul(33).wrapping_add(u32::from(byte)));
    format!("{PATH_PREFIX}{hash}")
}

/// The `Update(s app_uri, a{sv} properties)` arguments for `state`.
fn update_body(app_uri: String, state: EntryState) -> (String, HashMap<&'static str, Value<'static>>) {
    let properties = HashMap::from([
        ("count", Value::from(state.count)),
        ("count-visible", Value::from(state.count != 0)),
        ("progress", Value::from(state.progress)),
        (
            "progress-visible",
            Value::from(state.progress > 0.0 && state.progress < 1.0),
        ),
    ]);
    (app_uri, properties)
}

fn emit(connection: &mut Option<zbus::blocking::Connection>, state: EntryState) {
    if connection.is_none() {
        match zbus::blocking::Connection::session() {
            Ok(opened) => *connection = Some(opened),
            Err(error) => {
                tracing::debug!(%error, "No session bus for the launcher entry");
                return;
            }
        }
    }
    let Some(open) = connection.as_ref() else {
        return;
    };
    let uri = app_uri();
    let path = object_path(&uri);
    let sent = open.emit_signal(
        None::<zbus::names::BusName<'_>>,
        path.as_str(),
        INTERFACE,
        SIGNAL,
        &update_body(uri, state),
    );
    if let Err(error) = sent {
        tracing::warn!(%error, "Failed to update the launcher entry");
        *connection = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::OwnedValue;

    /// The signal as a dock receives it: built into a D-Bus message and read back, which
    /// fails unless the body is `(s a{sv})`.
    fn received(state: EntryState) -> (String, HashMap<String, OwnedValue>) {
        let uri = app_uri();
        let message = zbus::Message::signal(object_path(&uri).as_str(), INTERFACE, SIGNAL)
            .unwrap()
            .build(&update_body(uri, state))
            .unwrap();
        assert_eq!(message.header().member().unwrap().as_str(), SIGNAL);
        assert_eq!(message.header().interface().unwrap().as_str(), INTERFACE);
        message.body().deserialize().unwrap()
    }

    fn property<T: TryFrom<OwnedValue>>(properties: &HashMap<String, OwnedValue>, key: &str) -> T
    where
        T::Error: std::fmt::Debug,
    {
        T::try_from(properties[key].try_clone().unwrap()).unwrap()
    }

    #[test]
    fn the_signal_names_the_desktop_entry_and_carries_the_count() {
        let (uri, properties) = received(EntryState {
            count: 7,
            progress: -1.0,
        });
        assert_eq!(
            uri,
            format!("application://{}.desktop", BuildChannel::current().desktop_id())
        );
        assert_eq!(property::<i64>(&properties, "count"), 7);
        assert!(property::<bool>(&properties, "count-visible"));
        assert!(!property::<bool>(&properties, "progress-visible"));
        let (_, cleared) = received(EntryState {
            count: 0,
            progress: -1.0,
        });
        assert!(!property::<bool>(&cleared, "count-visible"));
    }

    #[test]
    fn progress_shows_only_strictly_between_zero_and_one() {
        for (progress, visible) in [(-1.0, false), (0.0, false), (0.25, true), (1.0, false)] {
            let (_, properties) = received(EntryState { count: 0, progress });
            assert_eq!(property::<f64>(&properties, "progress"), progress);
            assert_eq!(
                property::<bool>(&properties, "progress-visible"),
                visible,
                "progress {progress}"
            );
        }
    }
}
