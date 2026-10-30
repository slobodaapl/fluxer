// SPDX-License-Identifier: AGPL-3.0-or-later

//! `getSpellcheckLaunchMode` from LaunchOptions.ts. `hasFlag`, `getArgValue`, `truthyEnv` and
//! `normalizeMode` are repeated here because config/launch.rs keeps its helpers private.

use super::state::Engine;

const SAFE_MODE_ARGS: [&str; 1] = ["--fluxer-safe-mode"];
const DISABLE_ARGS: [&str; 2] = [
    "--fluxer-disable-spellcheck",
    "--fluxer-disable-linux-spellcheck",
];
const MODE_ARG: &str = "--fluxer-spellcheck";
const DISABLE_ENV: &str = "FLUXER_DISABLE_SPELLCHECK";
const MODE_ENV: &str = "FLUXER_SPELLCHECK";
const MODES: &str = "off, auto, hunspell, system";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchMode {
    Default,
    Off,
    Engine(Engine),
}

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

/// `getArgValue`: the first occurrence wins; a bare flag followed by another `--` option (or
/// nothing) is an empty value.
fn arg_value<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    for (index, arg) in args.iter().enumerate() {
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
    None
}

fn normalize_mode(value: Option<&str>, option: &str) -> Result<Option<LaunchMode>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let normalized = value.trim().to_lowercase();
    if normalized.is_empty() {
        return Err(format!("{option} requires a value: {MODES}"));
    }
    match normalized.as_str() {
        "off" => Ok(Some(LaunchMode::Off)),
        "auto" => Ok(Some(LaunchMode::Engine(Engine::Auto))),
        "hunspell" => Ok(Some(LaunchMode::Engine(Engine::Hunspell))),
        "system" => Ok(Some(LaunchMode::Engine(Engine::System))),
        _ => Err(format!("{option} must be one of: {MODES}")),
    }
}

/// `getSpellcheckLaunchMode`. `env` reads one environment variable. Electron threw on an
/// empty or unknown mode; the caller logs the error and keeps the default mode instead.
pub fn launch_mode(
    args: &[String],
    env: impl Fn(&str) -> Option<String>,
) -> Result<LaunchMode, String> {
    let truthy = env(DISABLE_ENV).is_some_and(|value| {
        ["1", "true", "yes", "on"].contains(&value.trim().to_lowercase().as_str())
    });
    if has_flag(args, &SAFE_MODE_ARGS) || has_flag(args, &DISABLE_ARGS) || truthy {
        return Ok(LaunchMode::Off);
    }
    if let Some(mode) = normalize_mode(arg_value(args, MODE_ARG), MODE_ARG)? {
        return Ok(mode);
    }
    Ok(normalize_mode(env(MODE_ENV).as_deref(), MODE_ENV)?.unwrap_or(LaunchMode::Default))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn safe_mode_and_disable_switches_turn_spellcheck_off() {
        for argv in [
            &["app", "--fluxer-safe-mode"][..],
            &["app", "--fluxer-disable-linux-spellcheck=1"],
            &[
                "app",
                "--fluxer-disable-spellcheck",
                "--fluxer-spellcheck=system",
            ],
        ] {
            assert_eq!(
                launch_mode(&args(argv), no_env),
                Ok(LaunchMode::Off),
                "{argv:?}"
            );
        }
        let env = |name: &str| (name == DISABLE_ENV).then(|| " Yes ".to_owned());
        assert_eq!(launch_mode(&args(&["app"]), env), Ok(LaunchMode::Off));
    }

    #[test]
    fn the_first_mode_argument_wins_over_the_environment() {
        let env = |name: &str| (name == MODE_ENV).then(|| "hunspell".to_owned());
        assert_eq!(
            launch_mode(
                &args(&[
                    "app",
                    "--fluxer-spellcheck",
                    " System ",
                    "--fluxer-spellcheck=off"
                ]),
                env
            ),
            Ok(LaunchMode::Engine(Engine::System))
        );
        assert_eq!(
            launch_mode(&args(&["app"]), env),
            Ok(LaunchMode::Engine(Engine::Hunspell))
        );
        assert_eq!(
            launch_mode(&args(&["app"]), no_env),
            Ok(LaunchMode::Default)
        );
    }

    #[test]
    fn empty_or_unknown_modes_are_errors() {
        assert_eq!(
            launch_mode(&args(&["app", "--fluxer-spellcheck", "--other"]), no_env),
            Err("--fluxer-spellcheck requires a value: off, auto, hunspell, system".to_owned())
        );
        let env = |name: &str| (name == MODE_ENV).then(|| "chromium".to_owned());
        assert_eq!(
            launch_mode(&args(&["app"]), env),
            Err("FLUXER_SPELLCHECK must be one of: off, auto, hunspell, system".to_owned())
        );
    }
}
