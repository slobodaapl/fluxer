// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::Serialize;

use crate::build_channel::BuildChannel;

/// Mirrors `DesktopInfo` in fluxer_app/src/types/electron.d.ts. Electron, Node and Chromium
/// fields stay in the shape for renderer compatibility; this shell reports the webview instead.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopInfo {
    version: String,
    channel: &'static str,
    arch: &'static str,
    hardware_arch: &'static str,
    running_under_rosetta: bool,
    os: &'static str,
    os_version: String,
    electron_version: &'static str,
    chrome_version: String,
    node_version: &'static str,
    wayland_session: bool,
    portable: bool,
    flatpak: bool,
    flatpak_app_id: Option<String>,
    chromium_runtime: ChromiumRuntimeInfo,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ChromiumRuntimeInfo {
    enable_features: [&'static str; 0],
    disable_features: [&'static str; 0],
    switches: [&'static str; 0],
}

/// `process.platform` spelling.
pub const fn node_platform() -> &'static str {
    if cfg!(target_os = "windows") {
        "win32"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    }
}

/// `process.arch` spelling.
const fn node_arch() -> &'static str {
    if cfg!(target_arch = "x86_64") {
        "x64"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        std::env::consts::ARCH
    }
}

#[cfg(unix)]
fn os_release() -> String {
    let mut uts = std::mem::MaybeUninit::<libc::utsname>::uninit();
    // SAFETY: uname fills the struct on success and we only read it then.
    if unsafe { libc::uname(uts.as_mut_ptr()) } != 0 {
        return String::new();
    }
    let uts = unsafe { uts.assume_init() };
    // SAFETY: release is NUL-terminated by uname.
    unsafe { std::ffi::CStr::from_ptr(uts.release.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

#[cfg(not(unix))]
fn os_release() -> String {
    String::new()
}

pub fn wayland_session() -> bool {
    cfg!(target_os = "linux")
        && (std::env::var_os("WAYLAND_DISPLAY").is_some()
            || std::env::var("XDG_SESSION_TYPE").is_ok_and(|kind| kind == "wayland"))
}

#[tauri::command]
pub fn get_desktop_info(app: tauri::AppHandle) -> DesktopInfo {
    let flatpak_app_id = std::env::var("FLATPAK_ID").ok().filter(|id| !id.is_empty());
    DesktopInfo {
        version: app.package_info().version.to_string(),
        channel: BuildChannel::current().as_str(),
        arch: node_arch(),
        hardware_arch: node_arch(),
        running_under_rosetta: false,
        os: node_platform(),
        os_version: os_release(),
        electron_version: "",
        chrome_version: tauri::webview_version().unwrap_or_default(),
        node_version: "",
        wayland_session: wayland_session(),
        portable: false,
        flatpak: flatpak_app_id.is_some(),
        flatpak_app_id,
        chromium_runtime: ChromiumRuntimeInfo {
            enable_features: [],
            disable_features: [],
            switches: [],
        },
    }
}
