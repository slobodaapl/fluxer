// SPDX-License-Identifier: AGPL-3.0-or-later

//! The Windows taskbar theme behind the tray's black or white icon:
//! `nativeTheme.shouldUseDarkColorsForSystemIntegratedUI` and its `updated` event. Chromium
//! read `SystemUsesLightTheme` under the Personalize key (missing means light) and watched the
//! key; so does this module, from a thread that waits on registry change notifications.

use tauri::AppHandle;
use windows::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_NOTIFY, KEY_READ, REG_NOTIFY_CHANGE_LAST_SET, RRF_RT_REG_DWORD,
    RegCloseKey, RegGetValueW, RegNotifyChangeKeyValue, RegOpenKeyExW,
};
use windows::Win32::System::Threading::{CreateEventW, INFINITE, WaitForSingleObject};
use windows::core::w;

const PERSONALIZE_KEY: windows::core::PCWSTR =
    w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
const SYSTEM_USES_LIGHT_THEME: windows::core::PCWSTR = w!("SystemUsesLightTheme");

/// True when the taskbar uses the dark theme.
pub fn uses_dark_system_ui() -> bool {
    let mut value = 1u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: `value` and `size` outlive the call and describe a DWORD buffer.
    let read = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PERSONALIZE_KEY,
            SYSTEM_USES_LIGHT_THEME,
            RRF_RT_REG_DWORD,
            None,
            Some((&raw mut value).cast()),
            Some(&raw mut size),
        )
    };
    read == ERROR_SUCCESS && value == 0
}

/// Calls `changed` on the main thread after every change under the Personalize key.
pub fn watch(app: AppHandle, changed: fn(&AppHandle)) {
    let spawned = std::thread::Builder::new()
        .name("taskbar-theme".into())
        .spawn(move || {
            if let Err(error) = wait_for_changes(&app, changed) {
                tracing::warn!(%error, "Stopped watching the taskbar theme");
            }
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "Failed to watch the taskbar theme");
    }
}

fn wait_for_changes(app: &AppHandle, changed: fn(&AppHandle)) -> windows::core::Result<()> {
    let mut key = HKEY::default();
    // SAFETY: `key` receives the opened handle, closed below.
    unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PERSONALIZE_KEY,
            None,
            KEY_READ | KEY_NOTIFY,
            &raw mut key,
        )
    }
    .ok()?;
    // SAFETY: an unnamed auto-reset event with default security, closed below.
    let event = match unsafe { CreateEventW(None, false, false, None) } {
        Ok(event) => event,
        Err(error) => {
            // SAFETY: `key` was opened above.
            let _ = unsafe { RegCloseKey(key) };
            return Err(error);
        }
    };
    let result = notify_loop(app, key, event, changed);
    // SAFETY: both handles were opened above and are not used after this.
    unsafe {
        let _ = CloseHandle(event);
        let _ = RegCloseKey(key);
    }
    result
}

fn notify_loop(
    app: &AppHandle,
    key: HKEY,
    event: HANDLE,
    changed: fn(&AppHandle),
) -> windows::core::Result<()> {
    loop {
        // SAFETY: `key` and `event` stay open for the whole loop.
        unsafe { RegNotifyChangeKeyValue(key, false, REG_NOTIFY_CHANGE_LAST_SET, Some(event), true) }
            .ok()?;
        // SAFETY: as above; the wait has no timeout.
        unsafe { WaitForSingleObject(event, INFINITE) };
        let handle = app.clone();
        if app.run_on_main_thread(move || changed(&handle)).is_err() {
            return Ok(());
        }
    }
}
