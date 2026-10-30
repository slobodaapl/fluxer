// SPDX-License-Identifier: AGPL-3.0-or-later

//! Windows autostart: a value under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` named
//! after this shell's AppUserModelID, which none of Electron's value names (its AUMIDs, the
//! `velopack.*` ids, the app names) can equal. It launches the Velopack stub next to the
//! install's `current` directory when a release build has one, else this executable.
//!
//! Electron also called `app.setLoginItemSettings`, which writes the same Run value and, for an
//! enable, deletes the value's `Explorer\StartupApproved\Run` entry (Task Manager's switch);
//! that deletion is done here directly. An entry Task Manager switched off reads as disabled.
//! Its byte format is not documented: a first byte with the low bit clear (`02`, `06`) is
//! taken as enabled, as is a missing entry.
//!
//! Electron matched Run values to its launch items with a case-insensitive, normalised path;
//! this port compares the value with the command it would write, which was Electron's fallback.

#[cfg(windows)]
use tauri::AppHandle;

use std::path::{Path, PathBuf};

#[cfg(windows)]
use super::AUTOSTART_ARG;
#[cfg(windows)]
use crate::build_channel::BuildChannel;

/// `quoteWindowsCommandArg`: the MSVCRT quoting `CommandLineToArgvW` reads back.
fn quote_command_arg(value: &str) -> String {
    // JavaScript's `\s` also matches the byte order mark.
    if !value.is_empty()
        && !value
            .chars()
            .any(|c| c.is_whitespace() || c == '\u{feff}' || c == '"')
    {
        return value.to_owned();
    }
    let mut quoted = String::from('"');
    let mut backslashes = 0;
    for c in value.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                quoted.push_str(&"\\".repeat(backslashes * 2 + 1));
                quoted.push('"');
                backslashes = 0;
            }
            _ => {
                quoted.push_str(&"\\".repeat(backslashes));
                backslashes = 0;
                quoted.push(c);
            }
        }
    }
    quoted.push_str(&"\\".repeat(backslashes * 2));
    quoted.push('"');
    quoted
}

/// `buildWindowsRunCommand`.
fn run_command(path: &str, args: &[&str]) -> String {
    std::iter::once(path)
        .chain(args.iter().copied())
        .map(quote_command_arg)
        .collect::<Vec<_>>()
        .join(" ")
}

/// `getWindowsLoginItemPath`: `<root>\<exe>` for `<root>\current\<exe>` when that stub exists in
/// a release build (Electron: `app.isPackaged`).
fn login_item_path(exe: &Path, release_build: bool) -> PathBuf {
    let stub = exe
        .parent()
        .and_then(Path::parent)
        .zip(exe.file_name())
        .map(|(root, name)| root.join(name));
    match stub {
        Some(stub) if release_build && stub != exe && stub.exists() => stub,
        _ => exe.to_owned(),
    }
}

/// A `StartupApproved\Run` entry's verdict.
fn approved(data: &[u8]) -> bool {
    data.first().is_none_or(|state| state & 1 == 0)
}

/// The Run commands that count as this app's login item: each launch path, with and without
/// `--autostart`.
#[cfg(windows)]
fn candidate_commands(exe: &Path) -> Vec<String> {
    let primary = login_item_path(exe, !cfg!(debug_assertions));
    let mut paths = vec![primary.to_string_lossy().into_owned()];
    let exe = exe.to_string_lossy().into_owned();
    if !paths.contains(&exe) {
        paths.push(exe);
    }
    paths
        .iter()
        .flat_map(|path| [run_command(path, &[AUTOSTART_ARG]), run_command(path, &[])])
        .collect()
}

#[cfg(windows)]
fn value_name() -> &'static str {
    BuildChannel::current().app_user_model_id()
}

/// Electron's Windows enable never failed: a failed write is logged.
#[cfg(windows)]
pub async fn enable(_app: &AppHandle) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let path = login_item_path(&exe, !cfg!(debug_assertions));
    let command = run_command(&path.to_string_lossy(), &[AUTOSTART_ARG]);
    if let Err(error) = fluxer_win_shell::set_current_user_run_value(value_name(), &command) {
        tracing::warn!(%error, "Failed to write the Windows Run key");
    }
    if let Err(error) = startup_approved::delete(value_name()) {
        tracing::warn!(%error, "Failed to clear the Windows StartupApproved entry");
    }
    Ok(())
}

#[cfg(windows)]
pub async fn disable(_app: &AppHandle) -> Result<(), String> {
    if let Err(error) = fluxer_win_shell::delete_current_user_run_value(value_name()) {
        tracing::warn!(%error, "Failed to remove Windows Run key");
    }
    if let Err(error) = startup_approved::delete(value_name()) {
        tracing::warn!(%error, "Failed to clear the Windows StartupApproved entry");
    }
    Ok(())
}

/// `isWindowsAutostartEnabled`.
#[cfg(windows)]
pub fn is_enabled(_app: &AppHandle) -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let value = match fluxer_win_shell::get_current_user_run_value(value_name()) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(%error, "Failed to read Windows Run key");
            return false;
        }
    };
    value.is_some_and(|value| candidate_commands(&exe).contains(&value))
        && startup_approved::read(value_name()).is_none_or(|data| approved(&data))
}

#[cfg(windows)]
mod startup_approved {
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows::Win32::System::Registry::{
        HKEY_CURRENT_USER, RRF_RT_REG_BINARY, RegDeleteKeyValueW, RegGetValueW,
    };
    use windows::core::PCWSTR;

    const SUBKEY: &str =
        "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\Run";
    /// The entries Windows writes are 12 bytes.
    const MAX_DATA: usize = 64;

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// The entry's bytes, `None` when there is none (or it cannot be read).
    pub fn read(name: &str) -> Option<Vec<u8>> {
        let subkey = wide(SUBKEY);
        let name = wide(name);
        let mut data = [0u8; MAX_DATA];
        let mut size = MAX_DATA as u32;
        // SAFETY: the strings are NUL-terminated and `data` holds `size` bytes.
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                PCWSTR(subkey.as_ptr()),
                PCWSTR(name.as_ptr()),
                RRF_RT_REG_BINARY,
                None,
                Some(data.as_mut_ptr().cast()),
                Some(&mut size),
            )
        };
        (status == ERROR_SUCCESS).then(|| data[..(size as usize).min(MAX_DATA)].to_vec())
    }

    /// Deletes the entry; a missing one is success.
    pub fn delete(name: &str) -> Result<(), String> {
        let subkey = wide(SUBKEY);
        let name = wide(name);
        // SAFETY: both strings are NUL-terminated and outlive the call.
        let status = unsafe {
            RegDeleteKeyValueW(
                HKEY_CURRENT_USER,
                PCWSTR(subkey.as_ptr()),
                PCWSTR(name.as_ptr()),
            )
        };
        if status == ERROR_SUCCESS || status == ERROR_FILE_NOT_FOUND {
            Ok(())
        } else {
            Err(format!(
                "RegDeleteKeyValueW(StartupApproved) failed: {}",
                status.0
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_are_quoted_the_way_command_line_to_argv_reads_them() {
        for (value, quoted) in [
            ("--autostart", "--autostart"),
            ("", "\"\""),
            (
                r"C:\Program Files\Fluxer\Fluxer.exe",
                r#""C:\Program Files\Fluxer\Fluxer.exe""#,
            ),
            (r#"a"b"#, r#""a\"b""#),
            (r#"a\"b"#, r#""a\\\"b""#),
            (r"C:\dir with space\", r#""C:\dir with space\\""#),
            ("a\u{feff}b", "\"a\u{feff}b\""),
            (r"C:\NoSpace\App.exe", r"C:\NoSpace\App.exe"),
        ] {
            assert_eq!(quote_command_arg(value), quoted, "{value:?}");
        }
    }

    #[test]
    fn the_run_command_quotes_the_path_and_each_argument() {
        assert_eq!(
            run_command(
                r"C:\Users\A B\AppData\Local\fluxer\Fluxer.exe",
                &["--autostart"]
            ),
            r#""C:\Users\A B\AppData\Local\fluxer\Fluxer.exe" --autostart"#
        );
        assert_eq!(run_command(r"C:\x.exe", &[]), r"C:\x.exe");
    }

    #[test]
    fn a_release_build_launches_the_stub_above_its_directory_when_it_exists() {
        let root = tempfile::tempdir().unwrap();
        let current = root.path().join("current");
        std::fs::create_dir(&current).unwrap();
        let exe = current.join("fluxer.exe");
        std::fs::write(&exe, "").unwrap();
        assert_eq!(login_item_path(&exe, true), exe, "no stub yet");
        let stub = root.path().join("fluxer.exe");
        std::fs::write(&stub, "").unwrap();
        assert_eq!(login_item_path(&exe, true), stub);
        assert_eq!(
            login_item_path(&exe, false),
            exe,
            "a debug build runs itself"
        );
    }

    #[test]
    fn task_manager_switches_off_with_an_odd_first_byte() {
        assert!(approved(&[]));
        assert!(approved(&[0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]));
        assert!(approved(&[0x06, 0, 0, 0]));
        assert!(!approved(&[0x03, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]));
        assert!(!approved(&[0x07]));
    }
}
