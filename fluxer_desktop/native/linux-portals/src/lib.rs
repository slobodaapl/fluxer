// SPDX-License-Identifier: AGPL-3.0-or-later

pub mod background;
pub mod global_shortcuts;
pub mod gnome_shell;
pub mod kwin;
#[cfg(target_os = "linux")]
pub mod portal;
pub mod session_monitor;
pub mod settings;
pub mod x11;

#[cfg(all(feature = "napi", target_os = "linux"))]
pub mod napi_surface;
