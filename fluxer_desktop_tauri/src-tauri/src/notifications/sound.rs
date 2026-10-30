// SPDX-License-Identifier: AGPL-3.0-or-later

//! `notification-sound-allowed`, the port of NotificationState.ts. Notifications are always
//! shown silent; the renderer plays its own sound unless the OS is in a do-not-disturb state:
//!
//! - Linux: the notification server's `Inhibited` property is true, or GNOME's
//!   `org.gnome.desktop.notifications show-banners` is false. Electron read `Inhibited` through
//!   `gdbus` (then `qdbus6`, `qdbus-qt6`, `qdbus`); this shell asks the session bus itself.
//!   `gsettings` is still run as a command. Each source gets 1 s.
//! - Windows: `SHQueryUserNotificationState` reports not-present, presentation mode or quiet
//!   time.
//! - macOS: a Focus mode is on that does not allow the app (see [`super::sound_macos`]).
//!
//! The answer is shared for 1 s; anything that fails allows the sound.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use futures_util::FutureExt as _;
use futures_util::future::{BoxFuture, Shared};

/// `POLICY_CACHE_TTL_MS`.
const CACHE_TTL: Duration = Duration::from_secs(1);
/// `COMMAND_TIMEOUT_MS`.
#[cfg(target_os = "linux")]
const QUERY_TIMEOUT: Duration = Duration::from_secs(1);

type Policy = Shared<BoxFuture<'static, bool>>;

#[derive(Default)]
pub struct SoundPolicy {
    cached: Mutex<Option<(Instant, Policy)>>,
}

impl SoundPolicy {
    /// `shouldPlayNotificationSound`.
    pub async fn allows_sound(&self) -> bool {
        let policy = {
            let mut cached = self.cached.lock().expect("notification sound policy lock");
            let now = Instant::now();
            match cached.as_ref() {
                Some((expires_at, policy)) if *expires_at > now => policy.clone(),
                _ => {
                    let policy = compute().boxed().shared();
                    *cached = Some((now + CACHE_TTL, policy.clone()));
                    policy
                }
            }
        };
        policy.await
    }
}

#[cfg(target_os = "linux")]
async fn compute() -> bool {
    let (inhibited, show_banners) = tokio::join!(
        linux::notifications_inhibited(),
        linux::gnome_show_banners()
    );
    linux_allows(inhibited, show_banners)
}

#[cfg(windows)]
async fn compute() -> bool {
    let state = tauri::async_runtime::spawn_blocking(fluxer_win_shell::get_user_notification_state)
        .await
        .ok()
        .and_then(Result::ok);
    windows_allows(state.as_deref())
}

#[cfg(target_os = "macos")]
async fn compute() -> bool {
    tauri::async_runtime::spawn_blocking(super::sound_macos::allows_now)
        .await
        .unwrap_or(true)
}

/// `getLinuxNotificationSoundPolicy`: `None` is a source that gave no answer.
#[cfg(any(target_os = "linux", test))]
fn linux_allows(inhibited: Option<bool>, show_banners: Option<bool>) -> bool {
    inhibited != Some(true) && show_banners != Some(false)
}

/// `getWindowsNotificationSoundPolicy`: a state label of `fluxer_win_shell`, `None` when the
/// query failed.
#[cfg(any(windows, test))]
fn windows_allows(state: Option<&str>) -> bool {
    !matches!(
        state,
        Some("not-present" | "presentation-mode" | "quiet-time")
    )
}

/// `parseBooleanOutput`: `\btrue\b`, else `\bfalse\b`, case-insensitively.
#[cfg(any(target_os = "linux", test))]
fn parse_boolean_output(output: &str) -> Option<bool> {
    let output = output.to_ascii_lowercase();
    let is_word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    let contains_word = |word: &str| {
        output.match_indices(word).any(|(start, _)| {
            let end = start + word.len();
            (start == 0 || !is_word(output.as_bytes()[start - 1]))
                && output
                    .as_bytes()
                    .get(end)
                    .is_none_or(|&byte| !is_word(byte))
        })
    };
    if contains_word("true") {
        Some(true)
    } else if contains_word("false") {
        Some(false)
    } else {
        None
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::io::Read as _;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use zbus::Connection;
    use zbus::zvariant::OwnedValue;

    use super::{QUERY_TIMEOUT, parse_boolean_output};

    const SERVICE: &str = "org.freedesktop.Notifications";
    const PATH: &str = "/org/freedesktop/Notifications";
    const PROPERTIES_INTERFACE: &str = "org.freedesktop.DBus.Properties";
    const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(10);

    /// `readFreedesktopNotificationsInhibited`.
    pub async fn notifications_inhibited() -> Option<bool> {
        let query = async {
            let connection = Connection::session().await.ok()?;
            let reply = connection
                .call_method(
                    Some(SERVICE),
                    PATH,
                    Some(PROPERTIES_INTERFACE),
                    "Get",
                    &(SERVICE, "Inhibited"),
                )
                .await
                .ok()?;
            let value: OwnedValue = reply.body().deserialize().ok()?;
            bool::try_from(value).ok()
        };
        tokio::time::timeout(QUERY_TIMEOUT, query)
            .await
            .ok()
            .flatten()
    }

    /// `readGnomeShowBanners`: `gsettings`, killed when it outlives the timeout.
    pub async fn gnome_show_banners() -> Option<bool> {
        tauri::async_runtime::spawn_blocking(read_show_banners)
            .await
            .ok()
            .flatten()
    }

    fn read_show_banners() -> Option<bool> {
        let mut child = Command::new("gsettings")
            .args(["get", "org.gnome.desktop.notifications", "show-banners"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let deadline = Instant::now() + QUERY_TIMEOUT;
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => {
                    let mut output = String::new();
                    child.stdout.take()?.read_to_string(&mut output).ok()?;
                    return parse_boolean_output(output.trim());
                }
                Ok(None) if Instant::now() < deadline => std::thread::sleep(EXIT_POLL_INTERVAL),
                Ok(Some(_)) => return None,
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_suppresses_when_inhibited_or_banners_are_off() {
        assert!(linux_allows(None, None));
        assert!(linux_allows(Some(false), Some(true)));
        assert!(!linux_allows(Some(true), None));
        assert!(!linux_allows(Some(true), Some(true)));
        assert!(!linux_allows(None, Some(false)));
        assert!(!linux_allows(Some(false), Some(false)));
    }

    #[test]
    fn windows_suppresses_only_away_presenting_and_quiet_states() {
        for state in ["not-present", "presentation-mode", "quiet-time"] {
            assert!(!windows_allows(Some(state)), "{state}");
        }
        for state in [
            "busy",
            "running-d3d-full-screen",
            "accepts-notifications",
            "app",
            "unknown",
        ] {
            assert!(windows_allows(Some(state)), "{state}");
        }
        assert!(windows_allows(None));
    }

    #[test]
    fn command_output_booleans_are_whole_words() {
        assert_eq!(parse_boolean_output("(<true>,)"), Some(true));
        assert_eq!(parse_boolean_output("false"), Some(false));
        assert_eq!(parse_boolean_output("TRUE"), Some(true));
        assert_eq!(parse_boolean_output("true false"), Some(true));
        assert_eq!(parse_boolean_output("untrue_value falsehood"), None);
        assert_eq!(parse_boolean_output("No such schema"), None);
        assert_eq!(parse_boolean_output(""), None);
    }
}
