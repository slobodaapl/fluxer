// SPDX-License-Identifier: AGPL-3.0-or-later

//! Linux idle sources, tried in order until one answers; that one is kept until it fails.
//!
//! - MIT-SCREEN-SAVER (`XScreenSaverQueryInfo`), what Chromium reads on X11. Only on an X11
//!   session: under XWayland it sees X clients' input alone.
//! - `org.gnome.Mutter.IdleMonitor.GetIdletime` (GNOME).
//! - `org.freedesktop.ScreenSaver.GetSessionIdleTime` (KDE Plasma). It comes last because other
//!   owners of the name may lack the method or report session idleness instead of input idleness.
//!
//! Wayland compositors outside GNOME and KDE expose idleness only as threshold notifications
//! (`ext-idle-notify-v1`), so there the query rejects.

use std::sync::Mutex;
use std::time::Duration;

use x11rb::connection::Connection as _;
use x11rb::protocol::screensaver::ConnectionExt as _;
use zbus::blocking::Connection;

const DBUS_TIMEOUT: Duration = Duration::from_millis(1_500);
const MUTTER_DESTINATION: &str = "org.gnome.Mutter.IdleMonitor";
const MUTTER_PATH: &str = "/org/gnome/Mutter/IdleMonitor/Core";
const MUTTER_INTERFACE: &str = "org.gnome.Mutter.IdleMonitor";
const SCREENSAVER_DESTINATION: &str = "org.freedesktop.ScreenSaver";
const SCREENSAVER_PATH: &str = "/org/freedesktop/ScreenSaver";
const SCREENSAVER_INTERFACE: &str = "org.freedesktop.ScreenSaver";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Backend {
    XScreenSaver,
    MutterIdleMonitor,
    FreedesktopScreenSaver,
}

impl Backend {
    fn name(self) -> &'static str {
        match self {
            Self::XScreenSaver => "MIT-SCREEN-SAVER",
            Self::MutterIdleMonitor => MUTTER_INTERFACE,
            Self::FreedesktopScreenSaver => SCREENSAVER_INTERFACE,
        }
    }
}

fn candidates(x11_session: bool) -> &'static [Backend] {
    if x11_session {
        &[
            Backend::XScreenSaver,
            Backend::MutterIdleMonitor,
            Backend::FreedesktopScreenSaver,
        ]
    } else {
        &[Backend::MutterIdleMonitor, Backend::FreedesktopScreenSaver]
    }
}

fn x11_session() -> bool {
    std::env::var_os("DISPLAY").is_some() && !crate::desktop_info::wayland_session()
}

#[derive(Default)]
struct State {
    bus: Option<Connection>,
    backend: Option<Backend>,
}

#[derive(Default)]
pub struct SystemIdle {
    state: Mutex<State>,
}

impl SystemIdle {
    pub fn idle_time_ms(&self) -> Result<u64, String> {
        let mut state = self.state.lock().expect("system idle lock");
        let State { bus, backend } = &mut *state;
        query_first_working(backend, candidates(x11_session()), |candidate| {
            query(candidate, bus)
        })
    }
}

/// Asks the remembered backend, or each candidate in order, remembering the first that answers.
/// A remembered backend that fails is forgotten, so the next call probes again.
fn query_first_working(
    remembered: &mut Option<Backend>,
    candidates: &[Backend],
    mut query: impl FnMut(Backend) -> Result<u64, String>,
) -> Result<u64, String> {
    if let Some(backend) = *remembered {
        return query(backend).map_err(|error| {
            *remembered = None;
            format!("{}: {error}", backend.name())
        });
    }
    let mut failures = Vec::new();
    for &backend in candidates {
        match query(backend) {
            Ok(idle_ms) => {
                *remembered = Some(backend);
                return Ok(idle_ms);
            }
            Err(error) => failures.push(format!("{}: {error}", backend.name())),
        }
    }
    Err(format!(
        "no system idle source answered ({})",
        failures.join("; ")
    ))
}

fn query(backend: Backend, bus: &mut Option<Connection>) -> Result<u64, String> {
    match backend {
        Backend::XScreenSaver => x11_idle_ms(),
        Backend::MutterIdleMonitor => dbus_call::<u64>(
            bus,
            MUTTER_DESTINATION,
            MUTTER_PATH,
            MUTTER_INTERFACE,
            "GetIdletime",
        ),
        Backend::FreedesktopScreenSaver => dbus_call::<u32>(
            bus,
            SCREENSAVER_DESTINATION,
            SCREENSAVER_PATH,
            SCREENSAVER_INTERFACE,
            "GetSessionIdleTime",
        ),
    }
}

fn x11_idle_ms() -> Result<u64, String> {
    let (connection, screen) = x11rb::connect(None).map_err(|error| error.to_string())?;
    let root = connection.setup().roots[screen].root;
    let info = connection
        .screensaver_query_info(root)
        .map_err(|error| error.to_string())?
        .reply()
        .map_err(|error| error.to_string())?;
    Ok(u64::from(info.ms_since_user_input))
}

/// One method call on the shared session bus connection. A failure other than a method error
/// drops the connection, so the next call reconnects.
fn dbus_call<T>(
    bus: &mut Option<Connection>,
    destination: &str,
    path: &str,
    interface: &str,
    method: &str,
) -> Result<u64, String>
where
    T: for<'de> serde::Deserialize<'de> + zbus::zvariant::Type + Into<u64>,
{
    let connection = match bus {
        Some(connection) => connection,
        None => bus.insert(
            zbus::blocking::connection::Builder::session()
                .and_then(|builder| builder.method_timeout(DBUS_TIMEOUT).build())
                .map_err(|error| error.to_string())?,
        ),
    };
    let reply = connection
        .call_method(Some(destination), path, Some(interface), method, &())
        .and_then(|reply| reply.body().deserialize::<T>());
    match reply {
        Ok(value) => Ok(value.into()),
        Err(error) => {
            if !matches!(error, zbus::Error::MethodError(..) | zbus::Error::FDO(_)) {
                *bus = None;
            }
            Err(error.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x11_is_probed_first_and_only_on_an_x11_session() {
        assert_eq!(candidates(true)[0], Backend::XScreenSaver);
        assert!(!candidates(false).contains(&Backend::XScreenSaver));
        assert_eq!(
            candidates(false),
            &[Backend::MutterIdleMonitor, Backend::FreedesktopScreenSaver]
        );
    }

    #[test]
    fn the_first_answering_backend_is_remembered() {
        let mut remembered = None;
        let mut asked = Vec::new();
        let idle = query_first_working(&mut remembered, candidates(true), |backend| {
            asked.push(backend);
            match backend {
                Backend::MutterIdleMonitor => Ok(4_200),
                _ => Err("unavailable".to_owned()),
            }
        });
        assert_eq!(idle, Ok(4_200));
        assert_eq!(asked, [Backend::XScreenSaver, Backend::MutterIdleMonitor]);
        assert_eq!(remembered, Some(Backend::MutterIdleMonitor));

        asked.clear();
        let idle = query_first_working(&mut remembered, candidates(true), |backend| {
            asked.push(backend);
            Ok(10)
        });
        assert_eq!(idle, Ok(10));
        assert_eq!(asked, [Backend::MutterIdleMonitor]);
    }

    #[test]
    fn a_failing_remembered_backend_rejects_once_then_probing_restarts() {
        let mut remembered = Some(Backend::FreedesktopScreenSaver);
        let idle = query_first_working(&mut remembered, candidates(false), |_| {
            Err("name lost".to_owned())
        });
        assert!(idle.unwrap_err().contains("name lost"));
        assert_eq!(remembered, None);

        let mut asked = Vec::new();
        let idle = query_first_working(&mut remembered, candidates(false), |backend| {
            asked.push(backend);
            Ok(1)
        });
        assert_eq!(idle, Ok(1));
        assert_eq!(asked, [Backend::MutterIdleMonitor]);
    }

    #[test]
    fn no_answering_backend_rejects_with_every_reason() {
        let mut remembered = None;
        let error = query_first_working(&mut remembered, candidates(false), |backend| {
            Err(format!("{backend:?} missing"))
        })
        .unwrap_err();
        assert!(error.contains("MutterIdleMonitor missing"), "{error}");
        assert!(error.contains("FreedesktopScreenSaver missing"), "{error}");
        assert_eq!(remembered, None);
    }
}
