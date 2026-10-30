// SPDX-License-Identifier: AGPL-3.0-or-later

//! Command-line options DesktopConfig.ts, DesktopDebugInfo.ts and LaunchOptions.ts read from
//! `process.argv`.

use crate::instance::endpoint::{EndpointKind, normalize_instance_endpoint};

const INSTANCE_ENDPOINT_ARGS: [&str; 1] = ["--fluxer-instance"];
const SAFE_MODE_ARGS: [&str; 1] = ["--fluxer-safe-mode"];
const RESET_WINDOW_STATE_ARGS: [&str; 1] = ["--fluxer-reset-window-state"];
const AUTOSTART_ARG: &str = "--autostart";
const DISABLE_HARDWARE_ACCELERATION_ARGS: [&str; 2] = [
    "--fluxer-disable-gpu",
    "--fluxer-disable-hardware-acceleration",
];
const DISABLE_MODULE_UPDATES_ARGS: [&str; 2] = [
    "--fluxer-disable-module-updates",
    "--disable-module-updates",
];
const DISABLE_NATIVE_GATEWAY_ARGS: [&str; 1] = ["--fluxer-disable-native-gateway"];
const DISABLE_NATIVE_GATEWAY_ENV: &str = "FLUXER_DISABLE_NATIVE_GATEWAY";
const DISABLE_MODULE_UPDATES_ENV: &str = "FLUXER_DISABLE_MODULE_UPDATES";
const DESKTOP_UPDATE_ARGS: [&str; 1] = ["--fluxer-update"];
const OPEN_DEVTOOLS_ARGS: [&str; 2] = ["--fluxer-devtools", "--fluxer-open-devtools"];
const RENDERER_CONSOLE_LOG_ARGS: [&str; 1] = ["--fluxer-log-renderer-console"];
#[cfg(any(target_os = "linux", test))]
const DISABLE_NATIVE_NOTIFICATIONS_ARGS: [&str; 1] = ["--fluxer-disable-native-notifications"];
#[cfg(any(target_os = "linux", test))]
const NATIVE_NOTIFICATIONS_MODE_ARGS: [&str; 2] =
    ["--fluxer-native-notifications", "--fluxer-notifications"];
#[cfg(any(target_os = "linux", test))]
const DISABLE_NATIVE_NOTIFICATIONS_ENV: &str = "FLUXER_DISABLE_NATIVE_NOTIFICATIONS";
#[cfg(any(target_os = "linux", test))]
const NATIVE_NOTIFICATIONS_MODE_ENV: &str = "FLUXER_NATIVE_NOTIFICATIONS";
#[cfg(any(target_os = "linux", test))]
const DISABLE_LINUX_PORTALS_ARGS: [&str; 1] = ["--fluxer-disable-linux-portals"];
#[cfg(any(target_os = "linux", test))]
const LINUX_PORTALS_MODE_ARGS: [&str; 2] = ["--fluxer-linux-portals", "--fluxer-portals"];
#[cfg(any(target_os = "linux", test))]
const DISABLE_LINUX_PORTALS_ENV: &str = "FLUXER_DISABLE_LINUX_PORTALS";
#[cfg(any(target_os = "linux", test))]
const LINUX_PORTALS_MODE_ENV: &str = "FLUXER_LINUX_PORTALS";

/// The process arguments as `process.argv` held them (lossy for non-UTF-8 arguments).
pub fn process_args() -> Vec<String> {
    std::env::args_os()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect()
}

/// `hasFlag`: the flag alone or as `flag=value`.
fn has_flag(args: &[String], flags: &[&str]) -> bool {
    args.iter().any(|arg| {
        flags.iter().any(|flag| {
            arg == flag
                || arg
                    .strip_prefix(flag)
                    .is_some_and(|rest| rest.starts_with('='))
        })
    })
}

/// `truthyEnv`.
fn truthy_env(name: &str) -> bool {
    is_truthy_env_value(std::env::var(name).ok().as_deref())
}

fn is_truthy_env_value(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        ["1", "true", "yes", "on"].contains(&value.trim().to_lowercase().as_str())
    })
}

/// `getNativeGatewayDisableReason`: what turned the native gateway off, if anything.
pub fn native_gateway_disable_reason(args: &[String]) -> Option<&'static str> {
    disable_reason_with_env(
        args,
        std::env::var(DISABLE_NATIVE_GATEWAY_ENV).ok().as_deref(),
    )
}

fn disable_reason_with_env(args: &[String], env_value: Option<&str>) -> Option<&'static str> {
    if has_flag(args, &SAFE_MODE_ARGS) {
        Some(SAFE_MODE_ARGS[0])
    } else if has_flag(args, &DISABLE_NATIVE_GATEWAY_ARGS) {
        Some(DISABLE_NATIVE_GATEWAY_ARGS[0])
    } else if is_truthy_env_value(env_value) {
        Some(DISABLE_NATIVE_GATEWAY_ENV)
    } else {
        None
    }
}

/// `shouldDisableHardwareAccelerationForLaunch`.
pub fn disables_hardware_acceleration(args: &[String]) -> bool {
    has_flag(args, &DISABLE_HARDWARE_ACCELERATION_ARGS) || has_flag(args, &SAFE_MODE_ARGS)
}

/// `shouldDisableModuleUpdatesForLaunch`.
pub fn disables_module_updates(args: &[String]) -> bool {
    has_flag(args, &DISABLE_MODULE_UPDATES_ARGS) || truthy_env(DISABLE_MODULE_UPDATES_ENV)
}

/// `shouldIgnoreWindowStateForLaunch`: safe mode opens the window at its default bounds.
pub fn ignores_window_state(args: &[String]) -> bool {
    has_flag(args, &SAFE_MODE_ARGS)
}

/// `shouldResetWindowStateOnLaunch`.
pub fn resets_window_state(args: &[String]) -> bool {
    has_flag(args, &RESET_WINDOW_STATE_ARGS)
}

/// `isAutostartLaunch` of AutostartLaunch.ts: `--autostart` exactly, as `process.argv.includes`
/// matched it, or a macOS login-item launch.
pub fn is_autostart_launch(args: &[String]) -> bool {
    args.iter().any(|arg| arg == AUTOSTART_ARG) || opened_at_login()
}

/// `app.getLoginItemSettings().wasOpenedAtLogin`, which only macOS reports.
#[cfg(target_os = "macos")]
fn opened_at_login() -> bool {
    crate::autostart::opened_at_login()
}

#[cfg(not(target_os = "macos"))]
fn opened_at_login() -> bool {
    false
}

/// `isDesktopUpdateRequested`.
pub fn requests_desktop_update(args: &[String]) -> bool {
    has_flag(args, &DESKTOP_UPDATE_ARGS)
}

/// `shouldOpenDevToolsOnLaunch` of DesktopDebugInfo.ts.
pub fn opens_devtools(args: &[String]) -> bool {
    has_flag(args, &OPEN_DEVTOOLS_ARGS)
}

/// `shouldForwardRendererConsoleToMainLog` of DesktopDebugInfo.ts.
pub fn forwards_renderer_console(args: &[String]) -> bool {
    has_flag(args, &RENDERER_CONSOLE_LOG_ARGS)
}

/// `getArgValue`: the first argument naming any of `names` wins; a bare flag followed by
/// another `--` option (or nothing) is an empty value.
#[cfg(any(target_os = "linux", test))]
fn arg_value<'a>(args: &'a [String], names: &[&str]) -> Option<&'a str> {
    for (index, arg) in args.iter().enumerate() {
        for name in names {
            if arg == name {
                return Some(
                    args.get(index + 1)
                        .map(String::as_str)
                        .filter(|next| !next.is_empty() && !next.starts_with("--"))
                        .unwrap_or(""),
                );
            }
            if let Some(value) = arg
                .strip_prefix(name)
                .and_then(|rest| rest.strip_prefix('='))
            {
                return Some(value);
            }
        }
    }
    None
}

/// `normalizeMode` over `auto`/`off`: whether the mode is `auto`, with Electron's error for an
/// empty or unknown value.
#[cfg(any(target_os = "linux", test))]
fn toggle_mode(value: Option<&str>, option: &str) -> Result<Option<bool>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value.trim().to_lowercase().as_str() {
        "" => Err(format!("{option} requires a value: auto, off")),
        "auto" => Ok(Some(true)),
        "off" => Ok(Some(false)),
        _ => Err(format!("{option} must be one of: auto, off")),
    }
}

/// `getLinuxPortalsMode` is `auto`. Electron threw on an empty or unknown mode from inside the
/// portal binding's loader, which failed the call that needed the portal; the error is returned
/// for that caller. `env` reads one environment variable.
#[cfg(any(target_os = "linux", test))]
pub fn linux_portals_enabled(
    args: &[String],
    env: impl Fn(&str) -> Option<String>,
) -> Result<bool, String> {
    if has_flag(args, &SAFE_MODE_ARGS)
        || has_flag(args, &DISABLE_LINUX_PORTALS_ARGS)
        || is_truthy_env_value(env(DISABLE_LINUX_PORTALS_ENV).as_deref())
    {
        return Ok(false);
    }
    if let Some(enabled) = toggle_mode(
        arg_value(args, &LINUX_PORTALS_MODE_ARGS),
        LINUX_PORTALS_MODE_ARGS[0],
    )? {
        return Ok(enabled);
    }
    Ok(toggle_mode(
        env(LINUX_PORTALS_MODE_ENV).as_deref(),
        LINUX_PORTALS_MODE_ENV,
    )?
    .unwrap_or(true))
}

/// `getLaunchInstanceEndpointOverride`. The last occurrence wins; a bare flag followed by
/// another `--` option (or nothing) is an empty value.
///
/// Electron accepted the flag only when `process.defaultApp` was set, that is when running
/// unpackaged through `electron .`. The analogue here is a debug build (`cargo run`,
/// `tauri dev`), passed in as `development_build` (`cfg!(debug_assertions)`). The build
/// channel is not used: a release build without `FLUXER_BUILD_CHANNEL` is also `development`.
pub fn instance_endpoint_override(
    args: &[String],
    development_build: bool,
) -> Result<Option<String>, &'static str> {
    let mut raw: Option<&str> = None;
    for (index, arg) in args.iter().enumerate() {
        for name in INSTANCE_ENDPOINT_ARGS {
            if arg == name {
                raw = Some(
                    args.get(index + 1)
                        .map(String::as_str)
                        .filter(|next| !next.is_empty() && !next.starts_with("--"))
                        .unwrap_or(""),
                );
            } else if let Some(value) = arg
                .strip_prefix(name)
                .and_then(|rest| rest.strip_prefix('='))
            {
                raw = Some(value);
            }
        }
    }
    let Some(raw) = raw else {
        return Ok(None);
    };
    if !development_build {
        return Err("--fluxer-instance is only accepted in a development build");
    }
    match normalize_instance_endpoint(crate::js::js_trim(raw), EndpointKind::Api) {
        Some(endpoint) if endpoint.contains("://") => Ok(Some(endpoint)),
        _ => Err("--fluxer-instance requires an absolute http(s) API endpoint"),
    }
}

/// `NativeNotificationsMode` of LaunchOptions.ts.
#[cfg(any(target_os = "linux", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeNotificationsMode {
    Auto,
    Off,
    Native,
    Electron,
}

#[cfg(any(target_os = "linux", test))]
impl NativeNotificationsMode {
    const ALL: [Self; 4] = [Self::Auto, Self::Off, Self::Native, Self::Electron];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Off => "off",
            Self::Native => "native",
            Self::Electron => "electron",
        }
    }

    /// `normalizeMode` over the four modes, with Electron's errors.
    fn normalize(value: Option<&str>, option: &str) -> Result<Option<Self>, String> {
        let Some(value) = value else {
            return Ok(None);
        };
        let allowed = Self::ALL.map(Self::as_str).join(", ");
        let normalized = value.trim().to_lowercase();
        if normalized.is_empty() {
            return Err(format!("{option} requires a value: {allowed}"));
        }
        Self::ALL
            .into_iter()
            .find(|mode| mode.as_str() == normalized)
            .map(Some)
            .ok_or_else(|| format!("{option} must be one of: {allowed}"))
    }
}

/// `getNativeNotificationsMode`. `env` reads one environment variable.
#[cfg(any(target_os = "linux", test))]
pub fn native_notifications_mode(
    args: &[String],
    env: impl Fn(&str) -> Option<String>,
) -> Result<NativeNotificationsMode, String> {
    if has_flag(args, &SAFE_MODE_ARGS)
        || has_flag(args, &DISABLE_NATIVE_NOTIFICATIONS_ARGS)
        || is_truthy_env_value(env(DISABLE_NATIVE_NOTIFICATIONS_ENV).as_deref())
    {
        return Ok(NativeNotificationsMode::Off);
    }
    if let Some(mode) = NativeNotificationsMode::normalize(
        arg_value(args, &NATIVE_NOTIFICATIONS_MODE_ARGS),
        NATIVE_NOTIFICATIONS_MODE_ARGS[0],
    )? {
        return Ok(mode);
    }
    Ok(NativeNotificationsMode::normalize(
        env(NATIVE_NOTIFICATIONS_MODE_ENV).as_deref(),
        NATIVE_NOTIFICATIONS_MODE_ENV,
    )?
    .unwrap_or(NativeNotificationsMode::Auto))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn native_gateway_disable_reason_prefers_safe_mode_then_flag_then_env() {
        let both = args(&["--fluxer-disable-native-gateway", "--fluxer-safe-mode"]);
        assert_eq!(
            disable_reason_with_env(&both, Some("1")),
            Some("--fluxer-safe-mode")
        );
        assert_eq!(
            disable_reason_with_env(&args(&["--fluxer-disable-native-gateway=1"]), Some("1")),
            Some("--fluxer-disable-native-gateway")
        );
        assert_eq!(
            disable_reason_with_env(&args(&[]), Some(" TRUE ")),
            Some("FLUXER_DISABLE_NATIVE_GATEWAY")
        );
        assert_eq!(disable_reason_with_env(&args(&["--other"]), None), None);
        assert_eq!(disable_reason_with_env(&args(&[]), Some("0")), None);
    }

    #[test]
    fn truthy_env_values() {
        for value in ["1", "true", "YES", " on "] {
            assert!(is_truthy_env_value(Some(value)), "{value}");
        }
        for value in [None, Some(""), Some("0"), Some("off")] {
            assert!(!is_truthy_env_value(value), "{value:?}");
        }
    }

    #[test]
    fn flags_match_alone_or_with_a_value() {
        assert!(has_flag(
            &args(&["--fluxer-disable-gpu"]),
            &DISABLE_HARDWARE_ACCELERATION_ARGS
        ));
        assert!(has_flag(&args(&["--fluxer-safe-mode=1"]), &SAFE_MODE_ARGS));
        assert!(!has_flag(&args(&["--fluxer-safe-modes"]), &SAFE_MODE_ARGS));
    }

    #[test]
    fn window_state_flags() {
        assert!(ignores_window_state(&args(&["app", "--fluxer-safe-mode"])));
        assert!(!ignores_window_state(&args(&[
            "app",
            "--fluxer-reset-window-state"
        ])));
        assert!(resets_window_state(&args(&[
            "app",
            "--fluxer-reset-window-state=1"
        ])));
        assert!(!resets_window_state(&args(&["app", "--fluxer-safe-mode"])));
    }

    #[test]
    fn autostart_needs_the_exact_argument() {
        assert!(is_autostart_launch(&args(&["app", "--autostart"])));
        for argv in [
            &["app"][..],
            &["app", "--autostart=1"],
            &["app", "--autostarted"],
        ] {
            assert!(!is_autostart_launch(&args(argv)), "{argv:?}");
        }
    }

    #[test]
    fn launch_flags_for_updates_devtools_and_the_renderer_console() {
        assert!(requests_desktop_update(&args(&["app", "--fluxer-update"])));
        assert!(!requests_desktop_update(&args(&[
            "app",
            "--fluxer-updated"
        ])));
        assert!(opens_devtools(&args(&["app", "--fluxer-open-devtools=1"])));
        assert!(opens_devtools(&args(&["app", "--fluxer-devtools"])));
        assert!(!opens_devtools(&args(&["app", "--devtools"])));
        assert!(forwards_renderer_console(&args(&[
            "app",
            "--fluxer-log-renderer-console"
        ])));
        assert!(!forwards_renderer_console(&args(&["app"])));
    }

    #[test]
    fn linux_portals_follow_disables_then_the_argument_then_the_environment() {
        let no_env = |_: &str| None;
        assert_eq!(linux_portals_enabled(&args(&["app"]), no_env), Ok(true));
        for argv in [
            &["app", "--fluxer-safe-mode"][..],
            &["app", "--fluxer-disable-linux-portals"],
            &["app", "--fluxer-portals", "off"],
            &["app", "--fluxer-linux-portals= OFF "],
        ] {
            assert_eq!(
                linux_portals_enabled(&args(argv), no_env),
                Ok(false),
                "{argv:?}"
            );
        }
        let env = |name: &str| match name {
            "FLUXER_DISABLE_LINUX_PORTALS" => Some("yes".to_owned()),
            _ => None,
        };
        assert_eq!(
            linux_portals_enabled(&args(&["app", "--fluxer-portals=auto"]), env),
            Ok(false)
        );
        let env = |name: &str| (name == "FLUXER_LINUX_PORTALS").then(|| "off".to_owned());
        assert_eq!(linux_portals_enabled(&args(&["app"]), env), Ok(false));
        // The argument outranks the environment.
        assert_eq!(
            linux_portals_enabled(&args(&["app", "--fluxer-portals=auto"]), env),
            Ok(true)
        );
    }

    #[test]
    fn linux_portals_reject_empty_and_unknown_modes() {
        let no_env = |_: &str| None;
        assert_eq!(
            linux_portals_enabled(&args(&["app", "--fluxer-portals", "--other"]), no_env),
            Err("--fluxer-linux-portals requires a value: auto, off".to_owned())
        );
        assert_eq!(
            linux_portals_enabled(&args(&["app", "--fluxer-portals=on"]), no_env),
            Err("--fluxer-linux-portals must be one of: auto, off".to_owned())
        );
        let empty = |name: &str| (name == "FLUXER_LINUX_PORTALS").then(String::new);
        assert_eq!(
            linux_portals_enabled(&args(&["app"]), empty),
            Err("FLUXER_LINUX_PORTALS requires a value: auto, off".to_owned())
        );
    }

    #[test]
    fn instance_override_is_absent_without_the_flag() {
        assert_eq!(instance_endpoint_override(&args(&["app"]), false), Ok(None));
    }

    #[test]
    fn instance_override_parses_both_forms_and_the_last_wins() {
        assert_eq!(
            instance_endpoint_override(
                &args(&["app", "--fluxer-instance", "https://a.example/api"]),
                true
            ),
            Ok(Some("https://a.example/api".to_owned()))
        );
        assert_eq!(
            instance_endpoint_override(
                &args(&[
                    "app",
                    "--fluxer-instance=https://a.example/api",
                    "--fluxer-instance= https://b.example/api/ "
                ]),
                true
            ),
            Ok(Some("https://b.example/api".to_owned()))
        );
    }

    #[test]
    fn instance_override_requires_a_development_build() {
        assert_eq!(
            instance_endpoint_override(
                &args(&["app", "--fluxer-instance=https://a.example/api"]),
                false
            ),
            Err("--fluxer-instance is only accepted in a development build")
        );
    }

    #[test]
    fn instance_override_requires_an_absolute_endpoint() {
        let invalid = "--fluxer-instance requires an absolute http(s) API endpoint";
        for argv in [
            &["app", "--fluxer-instance"][..],
            &["app", "--fluxer-instance", "--other"],
            &["app", "--fluxer-instance=/api"],
            &["app", "--fluxer-instance=ftp://a.example"],
        ] {
            assert_eq!(
                instance_endpoint_override(&args(argv), true),
                Err(invalid),
                "{argv:?}"
            );
        }
    }

    #[test]
    fn linux_portals_turn_off_by_switch_mode_or_environment() {
        let no_env = |_: &str| None;
        let env = |values: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                values
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| (*value).to_owned())
            }
        };
        for argv in [
            &["app", "--fluxer-safe-mode"][..],
            &["app", "--fluxer-disable-linux-portals"],
            &["app", "--fluxer-portals", "OFF"],
            &["app", "--fluxer-linux-portals=off"],
        ] {
            assert_eq!(
                linux_portals_enabled(&args(argv), no_env),
                Ok(false),
                "{argv:?}"
            );
        }
        assert_eq!(
            linux_portals_enabled(
                &args(&["app"]),
                env(&[("FLUXER_DISABLE_LINUX_PORTALS", "yes")])
            ),
            Ok(false)
        );
        assert_eq!(
            linux_portals_enabled(&args(&["app"]), env(&[("FLUXER_LINUX_PORTALS", " off ")])),
            Ok(false)
        );
        assert_eq!(linux_portals_enabled(&args(&["app"]), no_env), Ok(true));
    }

    #[test]
    fn native_notifications_follow_disables_then_the_argument_then_the_environment() {
        let no_env = |_: &str| None;
        let mode = |argv: &[&str], env: &dyn Fn(&str) -> Option<String>| {
            native_notifications_mode(&args(argv), env)
        };
        assert_eq!(mode(&["app"], &no_env), Ok(NativeNotificationsMode::Auto));
        assert_eq!(
            mode(&["app", "--fluxer-safe-mode"], &no_env),
            Ok(NativeNotificationsMode::Off)
        );
        let disable =
            |name: &str| (name == "FLUXER_DISABLE_NATIVE_NOTIFICATIONS").then(|| "1".to_owned());
        assert_eq!(
            mode(&["app", "--fluxer-notifications=native"], &disable),
            Ok(NativeNotificationsMode::Off)
        );
        let electron =
            |name: &str| (name == "FLUXER_NATIVE_NOTIFICATIONS").then(|| " Electron ".to_owned());
        assert_eq!(
            mode(&["app"], &electron),
            Ok(NativeNotificationsMode::Electron)
        );
        assert_eq!(
            mode(
                &["app", "--fluxer-native-notifications", "native"],
                &electron
            ),
            Ok(NativeNotificationsMode::Native)
        );
        assert_eq!(
            mode(&["app", "--fluxer-native-notifications=sideways"], &no_env),
            Err(
                "--fluxer-native-notifications must be one of: auto, off, native, electron"
                    .to_owned()
            )
        );
        assert_eq!(
            mode(&["app", "--fluxer-notifications", "--other"], &no_env),
            Err(
                "--fluxer-native-notifications requires a value: auto, off, native, electron"
                    .to_owned()
            )
        );
    }
}
