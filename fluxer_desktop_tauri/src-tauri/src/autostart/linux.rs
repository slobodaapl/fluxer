// SPDX-License-Identifier: AGPL-3.0-or-later

//! Linux autostart: `<desktop id>.desktop` in `$XDG_CONFIG_HOME/autostart` (`~/.config` without
//! it), or under Flatpak the XDG Background portal, whose last answer is kept in
//! `flatpak-autostart-v1.json`.
//!
//! The entry counts as enabled unless it is `Hidden=true` or its program (`TryExec`, else the
//! first word of `Exec`) is not an executable file; like Electron, a GNOME
//! `X-GNOME-Autostart-enabled=false` still reads as enabled. At start-up an entry of this app's
//! (its `StartupWMClass`) that the user has not hidden or switched off and whose program is gone
//! is rewritten for the running one.
//!
//! The generated-by marker leaves out `fluxer-desktop`, which the Electron shell matches to
//! delete its own legacy entries. Electron's migration of its `fluxer*.desktop` legacy entry has
//! no counterpart: this shell never wrote one.

use std::collections::HashMap;
use std::ffi::{CString, OsStr, OsString};
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use fluxer_linux_portals::background::{RequestOptions, request_background_async};
use tauri::{AppHandle, Manager};

use super::AUTOSTART_ARG;
use crate::build_channel::BuildChannel;
use crate::config::launch;
use crate::i18n::MainI18n;
use crate::storage::private_file;

const GENERATED_MARKER: &str = "# X-Generated-By=app.fluxer.tauri";
const FLATPAK_INFO: &str = "/.flatpak-info";
const FLATPAK_STATE_FILE: &str = "flatpak-autostart-v1.json";
const FLATPAK_COMMAND_ENV: &str = "FLUXER_FLATPAK_COMMAND";
const DEFAULT_FLATPAK_COMMAND: &str = "fluxer";

/// `isFlatpakRuntime` of LinuxSandbox.ts.
fn is_flatpak() -> bool {
    Path::new(FLATPAK_INFO).exists()
}

/// `getLinuxDesktopFilePath`.
fn entry_path(app: &AppHandle) -> Result<PathBuf, String> {
    let base = match std::env::var_os("XDG_CONFIG_HOME").filter(|base| !base.is_empty()) {
        Some(base) => PathBuf::from(base),
        None => app
            .path()
            .home_dir()
            .map_err(|error| error.to_string())?
            .join(".config"),
    };
    Ok(base
        .join("autostart")
        .join(format!("{}.desktop", BuildChannel::current().desktop_id())))
}

/// `getStableLinuxLaunchPath` of LinuxLaunchPath.ts: the AppImage, else this executable.
fn stable_launch_path() -> Result<String, String> {
    if let Some(app_image) = std::env::var_os("APPIMAGE").filter(|path| !path.is_empty()) {
        return Ok(app_image.to_string_lossy().into_owned());
    }
    std::env::current_exe()
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(|error| error.to_string())
}

/// `escapeDesktopValue` of LinuxDesktopEntry.ts.
fn escape_desktop_value(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
        .replace('\r', "\\r")
}

/// `quoteExecArg` of LinuxDesktopEntry.ts.
fn quote_exec_arg(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for c in value.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    quoted.push('"');
    quoted
}

/// `buildLinuxDesktopFileContents`.
fn entry_contents(launch_path: &str, desktop_id: &str, app_name: &str) -> String {
    let exec = format!("{} {AUTOSTART_ARG}", quote_exec_arg(launch_path));
    [
        "[Desktop Entry]",
        GENERATED_MARKER,
        "Type=Application",
        &format!("Name={}", escape_desktop_value(app_name)),
        "Comment=Fluxer",
        &format!("Exec={}", escape_desktop_value(&exec)),
        &format!("TryExec={}", escape_desktop_value(launch_path)),
        "Terminal=false",
        "Categories=Network;InstantMessaging;",
        &format!("StartupWMClass={desktop_id}"),
        "SingleMainWindow=true",
        "X-GNOME-Autostart-enabled=true",
        "StartupNotify=false",
        "",
    ]
    .join("\n")
}

fn current_entry_contents() -> Result<String, String> {
    let channel = BuildChannel::current();
    Ok(entry_contents(
        &stable_launch_path()?,
        channel.desktop_id(),
        channel.app_name(),
    ))
}

/// `tryParseDesktopEntry`: the keys of the first `[Desktop Entry]` group, values unescaped.
fn parse_entry(contents: &str) -> HashMap<&str, &str> {
    let mut entry = HashMap::new();
    let mut in_desktop_entry = false;
    for line in contents.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            if in_desktop_entry {
                break;
            }
            in_desktop_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_desktop_entry {
            continue;
        }
        if let Some(separator) = line.find('=').filter(|separator| *separator > 0) {
            entry.insert(&line[..separator], &line[separator + 1..]);
        }
    }
    entry
}

fn entry_flag(entry: &HashMap<&str, &str>, key: &str, value: &str) -> bool {
    entry
        .get(key)
        .is_some_and(|actual| actual.trim().eq_ignore_ascii_case(value))
}

/// `parseDesktopExecCommand`: the first word of `Exec`, quote-aware.
fn exec_command(value: Option<&str>) -> Option<String> {
    let input = value?.trim_start();
    let Some(quoted) = input.strip_prefix('"') else {
        return input.split_whitespace().next().map(str::to_owned);
    };
    let mut command = String::new();
    let mut escaping = false;
    for c in quoted.chars() {
        match c {
            _ if escaping => {
                command.push(c);
                escaping = false;
            }
            '\\' => escaping = true,
            '"' => return Some(command),
            _ => command.push(c),
        }
    }
    None
}

fn is_executable(path: &Path) -> bool {
    CString::new(path.as_os_str().as_bytes())
        // SAFETY: `path` is a NUL-terminated string that outlives the call.
        .is_ok_and(|path| unsafe { libc::access(path.as_ptr(), libc::X_OK) } == 0)
}

/// `commandExists`: an absolute path that is executable, or a name found executable on
/// `search_path` (`PATH`).
fn command_exists(command: &str, search_path: Option<&OsStr>) -> bool {
    let command = Path::new(command);
    if command.is_absolute() {
        return is_executable(command);
    }
    search_path.is_some_and(|search_path| {
        std::env::split_paths(search_path)
            .filter(|directory| !directory.as_os_str().is_empty())
            .any(|directory| is_executable(&directory.join(command)))
    })
}

/// `linuxDesktopEntryTargetsExistingCommand`.
fn targets_existing_command(entry: &HashMap<&str, &str>, search_path: Option<&OsStr>) -> bool {
    if let Some(try_exec) = entry.get("TryExec").map(|value| value.trim())
        && !try_exec.is_empty()
    {
        return command_exists(try_exec, search_path);
    }
    exec_command(entry.get("Exec").copied())
        .is_some_and(|command| command_exists(&command, search_path))
}

fn search_path() -> Option<OsString> {
    std::env::var_os("PATH")
}

/// Whether the entry at `path` starts this app at login.
fn entry_enabled(path: &Path, search_path: Option<&OsStr>) -> bool {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return false;
    };
    let entry = parse_entry(&contents);
    !entry_flag(&entry, "Hidden", "true") && targets_existing_command(&entry, search_path)
}

/// `repairLinuxAutostartEntry`'s test: the entry is this app's, wanted, and stale.
fn needs_repair(
    contents: &str,
    desktop_id: &str,
    launch_path: &str,
    search_path: Option<&OsStr>,
) -> bool {
    let entry = parse_entry(contents);
    entry.get("StartupWMClass").map(|value| value.trim()) == Some(desktop_id)
        && !entry_flag(&entry, "Hidden", "true")
        && !entry_flag(&entry, "X-GNOME-Autostart-enabled", "false")
        && !targets_existing_command(&entry, search_path)
        && command_exists(launch_path, search_path)
}

/// `writeFileAtomically` with mode 0644, into a directory created 0700.
fn write_entry(path: &Path, contents: &str) -> std::io::Result<()> {
    let directory = path.parent().expect("the entry path has a directory");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(directory)?;
    let mut suffix = [0u8; 8];
    getrandom::fill(&mut suffix).map_err(|error| std::io::Error::other(error.to_string()))?;
    let suffix: String = suffix.iter().map(|byte| format!("{byte:02x}")).collect();
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(format!(".{suffix}.tmp"));
    let temporary = PathBuf::from(temporary);
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .open(&temporary)
        .and_then(|mut file| {
            file.write_all(contents.as_bytes())?;
            file.sync_all()
        })
        .and_then(|()| std::fs::rename(&temporary, path));
    if let Err(error) = written {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    std::fs::File::open(directory)?.sync_all()
}

/// Removes `path`; a missing file is success, as `fs.rm({force: true})` had it.
fn remove_entry(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// `new Date(since_epoch).toISOString()`.
fn iso_timestamp(since_epoch: Duration) -> String {
    let seconds = since_epoch.as_secs();
    let (days, time) = (seconds / 86_400, seconds % 86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's `civil_from_days`).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        time / 3600,
        time % 3600 / 60,
        time % 60,
        since_epoch.subsec_millis()
    )
}

/// `getFlatpakAutostartCommandline`.
fn flatpak_commandline() -> Vec<String> {
    let command = std::env::var(FLATPAK_COMMAND_ENV)
        .ok()
        .map(|command| command.trim().to_owned())
        .filter(|command| !command.is_empty())
        .unwrap_or_else(|| DEFAULT_FLATPAK_COMMAND.to_owned());
    vec![command, AUTOSTART_ARG.to_owned()]
}

/// `readFlatpakAutostartState`: anything but `enabled: true` reads as disabled.
fn flatpak_state(directory: &Path) -> bool {
    std::fs::read_to_string(directory.join(FLATPAK_STATE_FILE))
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .is_some_and(|state| state.get("enabled") == Some(&serde_json::Value::Bool(true)))
}

/// `setFlatpakAutostart`: the portal's answer must match the request.
async fn set_flatpak(app: &AppHandle, enabled: bool) -> Result<(), String> {
    if !launch::linux_portals_enabled(&launch::process_args(), |name| std::env::var(name).ok())? {
        return Err("XDG Background portal is unavailable.".to_owned());
    }
    let app_name = BuildChannel::current().app_name();
    let options = RequestOptions {
        reason: enabled.then(|| {
            app.state::<MainI18n>()
                .t("desktop.autostart.portalReason", &[("appName", app_name)])
        }),
        autostart: enabled,
        commandline: if enabled {
            flatpak_commandline()
        } else {
            Vec::new()
        },
        dbus_activatable: false,
    };
    let result = request_background_async(options)
        .await
        .map_err(|error| error.to_string())?;
    if result.cancelled() {
        return Err("XDG Background portal request was cancelled.".to_owned());
    }
    if result.autostart != enabled {
        let verb = if enabled { "enable" } else { "disable" };
        return Err(format!("XDG Background portal did not {verb} autostart."));
    }
    let entry = entry_path(app)?;
    if let Err(error) = remove_entry(&entry) {
        tracing::warn!(%error, path = %entry.display(), "Failed to remove sandbox-local Linux autostart entry");
    }
    let directory = app
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    let updated_at = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let state = serde_json::json!({
        "version": 1,
        "enabled": result.autostart,
        "updatedAt": iso_timestamp(updated_at),
    });
    let persisted = std::fs::create_dir_all(&directory).and_then(|()| {
        private_file::write(&directory.join(FLATPAK_STATE_FILE), &state.to_string())
    });
    if let Err(error) = persisted {
        tracing::warn!(%error, "Failed to persist Flatpak autostart state");
    }
    tracing::info!(
        enabled = result.autostart,
        background = result.background,
        response = result.response,
        "Updated Flatpak autostart through XDG Background portal"
    );
    Ok(())
}

pub async fn enable(app: &AppHandle) -> Result<(), String> {
    if is_flatpak() {
        return set_flatpak(app, true).await;
    }
    let path = entry_path(app)?;
    match write_entry(&path, &current_entry_contents()?) {
        Ok(()) => {
            tracing::info!(path = %path.display(), "Wrote Linux autostart entry");
            Ok(())
        }
        Err(error) => {
            tracing::error!(%error, "Failed to write Linux autostart entry");
            Err(error.to_string())
        }
    }
}

pub async fn disable(app: &AppHandle) -> Result<(), String> {
    if is_flatpak() {
        return set_flatpak(app, false).await;
    }
    let path = entry_path(app)?;
    match remove_entry(&path) {
        Ok(()) => {
            tracing::info!(path = %path.display(), "Removed Linux autostart entry");
            Ok(())
        }
        Err(error) => {
            tracing::error!(%error, "Failed to remove Linux autostart entry");
            Err(error.to_string())
        }
    }
}

/// `isLinuxAutostartEnabled`.
pub fn is_enabled(app: &AppHandle) -> bool {
    if is_flatpak() {
        return app
            .path()
            .app_data_dir()
            .is_ok_and(|directory| flatpak_state(&directory));
    }
    entry_path(app).is_ok_and(|path| entry_enabled(&path, search_path().as_deref()))
}

/// `repairLinuxAutostartEntry`; Flatpak entries belong to the portal.
pub fn repair(app: &AppHandle) {
    if is_flatpak() {
        return;
    }
    let Ok(path) = entry_path(app) else {
        return;
    };
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return;
    };
    let Ok(launch_path) = stable_launch_path() else {
        return;
    };
    let desktop_id = BuildChannel::current().desktop_id();
    if !needs_repair(
        &contents,
        desktop_id,
        &launch_path,
        search_path().as_deref(),
    ) {
        return;
    }
    let rewritten = current_entry_contents()
        .and_then(|contents| write_entry(&path, &contents).map_err(|error| error.to_string()));
    match rewritten {
        Ok(()) => tracing::info!(
            exec_path = launch_path,
            "Rewrote a Linux autostart entry whose command no longer exists"
        ),
        Err(error) => tracing::warn!(%error, "Failed to rewrite the stale Linux autostart entry"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    const ID: &str = "app.fluxer.tauri.stable";

    fn executable(directory: &Path, name: &str) -> String {
        let path = directory.join(name);
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn the_entry_launches_the_app_with_the_autostart_argument() {
        let contents = entry_contents("/opt/My \"Fluxer\"/$bin", ID, "Fluxer\nCanary");
        assert_eq!(
            contents,
            "[Desktop Entry]\n\
             # X-Generated-By=app.fluxer.tauri\n\
             Type=Application\n\
             Name=Fluxer\\nCanary\n\
             Comment=Fluxer\n\
             Exec=\"/opt/My \\\\\"Fluxer\\\\\"/\\\\$bin\" --autostart\n\
             TryExec=/opt/My \"Fluxer\"/$bin\n\
             Terminal=false\n\
             Categories=Network;InstantMessaging;\n\
             StartupWMClass=app.fluxer.tauri.stable\n\
             SingleMainWindow=true\n\
             X-GNOME-Autostart-enabled=true\n\
             StartupNotify=false\n"
        );
        // Electron deletes legacy entries whose text contains its own marker.
        assert!(!contents.contains("# X-Generated-By=fluxer-desktop"));
    }

    #[test]
    fn exec_parsing_reads_the_first_word_quote_aware() {
        assert_eq!(
            exec_command(Some("  /usr/bin/app --x")).as_deref(),
            Some("/usr/bin/app")
        );
        assert_eq!(
            exec_command(Some(r#""/opt/a \"b\"/app" --autostart"#)).as_deref(),
            Some(r#"/opt/a "b"/app"#)
        );
        assert_eq!(exec_command(Some(r#""/opt/unterminated"#)), None);
        assert_eq!(exec_command(Some("   ")), None);
        assert_eq!(exec_command(None), None);
    }

    #[test]
    fn only_the_first_desktop_entry_group_is_read() {
        let entry = parse_entry(
            "# c\n[Other]\nHidden=true\n[Desktop Entry]\n Exec = x\nTryExec=/a=b\n=bad\n[Desktop Action a]\nHidden=true\n",
        );
        assert_eq!(entry.get("TryExec"), Some(&"/a=b"));
        // Keys and values keep the spaces around `=`, as Electron's `slice` did.
        assert_eq!(entry.get("Exec "), Some(&" x"));
        assert_eq!(entry.get("Exec"), None);
        assert_eq!(entry.get("Hidden"), None);
    }

    #[test]
    fn an_entry_is_enabled_while_its_program_is_executable_and_it_is_not_hidden() {
        let directory = tempfile::tempdir().unwrap();
        let program = executable(directory.path(), "fluxer");
        let entry = directory
            .path()
            .join("autostart")
            .join(format!("{ID}.desktop"));
        assert!(!entry_enabled(&entry, None), "a missing entry");
        write_entry(&entry, &entry_contents(&program, ID, "Fluxer")).unwrap();
        assert!(entry_enabled(&entry, None));
        let mode = std::fs::metadata(&entry).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode & !0o644, 0, "{mode:o} is wider than 0644");
        assert_eq!(mode & 0o600, 0o600, "{mode:o}");
        let gnome_off = std::fs::read_to_string(&entry).unwrap().replace(
            "X-GNOME-Autostart-enabled=true",
            "X-GNOME-Autostart-enabled=false",
        );
        write_entry(&entry, &gnome_off).unwrap();
        assert!(
            entry_enabled(&entry, None),
            "GNOME's switch is not read, as in Electron"
        );
        write_entry(&entry, &format!("{gnome_off}Hidden=True\n")).unwrap();
        assert!(!entry_enabled(&entry, None));
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_entry(&entry, &entry_contents(&program, ID, "Fluxer")).unwrap();
        assert!(
            !entry_enabled(&entry, None),
            "a program that is not executable"
        );
    }

    #[test]
    fn without_try_exec_the_exec_program_is_looked_up_on_the_search_path() {
        let directory = tempfile::tempdir().unwrap();
        executable(directory.path(), "fluxer-app");
        let entry = directory.path().join("entry.desktop");
        std::fs::write(&entry, "[Desktop Entry]\nExec=fluxer-app --autostart\n").unwrap();
        let search_path = std::env::join_paths(["", directory.path().to_str().unwrap()]).unwrap();
        assert!(entry_enabled(&entry, Some(&search_path)));
        assert!(!entry_enabled(&entry, None));
        std::fs::write(
            &entry,
            "[Desktop Entry]\nTryExec=missing\nExec=fluxer-app\n",
        )
        .unwrap();
        assert!(
            !entry_enabled(&entry, Some(&search_path)),
            "TryExec decides when present"
        );
    }

    #[test]
    fn repair_rewrites_only_this_apps_wanted_entry_whose_program_is_gone() {
        let directory = tempfile::tempdir().unwrap();
        let current = executable(directory.path(), "current");
        let gone = directory.path().join("gone").to_string_lossy().into_owned();
        let stale = entry_contents(&gone, ID, "Fluxer");
        assert!(needs_repair(&stale, ID, &current, None));
        assert!(
            !needs_repair(&entry_contents(&current, ID, "Fluxer"), ID, &current, None),
            "the program still exists"
        );
        assert!(
            !needs_repair(&stale, "app.fluxer.FluxerDesktop", &current, None),
            "another app's entry"
        );
        assert!(!needs_repair(
            &format!("{stale}Hidden=true\n"),
            ID,
            &current,
            None
        ));
        let switched_off = stale.replace(
            "X-GNOME-Autostart-enabled=true",
            "X-GNOME-Autostart-enabled=FALSE",
        );
        assert!(!needs_repair(&switched_off, ID, &current, None));
        assert!(
            !needs_repair(&stale, ID, &gone, None),
            "nothing to point a rewritten entry at"
        );
    }

    #[test]
    fn removing_a_missing_entry_succeeds() {
        let directory = tempfile::tempdir().unwrap();
        let entry = directory.path().join("x.desktop");
        remove_entry(&entry).unwrap();
        std::fs::write(&entry, "").unwrap();
        remove_entry(&entry).unwrap();
        assert!(!entry.exists());
    }

    #[test]
    fn only_an_enabled_true_flatpak_state_reads_as_enabled() {
        let directory = tempfile::tempdir().unwrap();
        assert!(!flatpak_state(directory.path()));
        let state = directory.path().join(FLATPAK_STATE_FILE);
        for (raw, enabled) in [
            (r#"{"version":1,"enabled":true}"#, true),
            (r#"{"enabled":"true"}"#, false),
            (r#"{"enabled":false}"#, false),
            ("not json", false),
        ] {
            std::fs::write(&state, raw).unwrap();
            assert_eq!(flatpak_state(directory.path()), enabled, "{raw}");
        }
    }

    #[test]
    fn timestamps_use_the_javascript_iso_format() {
        assert_eq!(iso_timestamp(Duration::ZERO), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            iso_timestamp(Duration::from_millis(951_782_400_123)),
            "2000-02-29T00:00:00.123Z"
        );
        assert_eq!(
            iso_timestamp(Duration::from_secs(1_793_145_599)),
            "2026-10-27T23:59:59.000Z"
        );
    }
}
