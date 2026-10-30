// SPDX-License-Identifier: AGPL-3.0-or-later

//! The main window's frame and background, from `createWindow` in
//! fluxer_desktop/src/main/Window.ts: `getTitleBarWindowOptions`, `getWindowBackgroundColor`,
//! `getWindowShadowOptions` and the macOS traffic-light position.
//!
//! Custom chrome (macOS always, elsewhere without the native title bar) is an undecorated window
//! on Linux and Windows, which the renderer's titlebar drags through the bridge (bridge/drag.ts)
//! and tao resizes from its edges. macOS keeps the system frame with the title bar overlaid on
//! the content, as Electron's `titleBarStyle: 'hidden'` did; that path is not verified here.

use tauri::window::Color;
use tauri::{AppHandle, WebviewWindowBuilder, Wry};

use crate::config::ActiveWindowBehavior;

/// `THEME_WINDOW_BACKGROUND_COLORS` by preboot theme, `DEFAULT_WINDOW_BACKGROUND_COLOR`
/// otherwise, and fully transparent with transparency on.
fn background_color(preboot_theme: Option<&str>, transparent: bool) -> Color {
    if transparent {
        return Color(0, 0, 0, 0);
    }
    match preboot_theme {
        Some("light") => Color(0xeb, 0xec, 0xef, 0xff),
        Some("coal") => Color(0x02, 0x02, 0x03, 0xff),
        Some("dark_legacy") => Color(0x19, 0x1b, 0x20, 0xff),
        _ => Color(0x1a, 0x18, 0x1e, 0xff),
    }
}

type Builder<'a> = WebviewWindowBuilder<'a, Wry, AppHandle>;

pub fn apply<'a>(
    builder: Builder<'a>,
    active: ActiveWindowBehavior,
    preboot_theme: Option<&str>,
) -> Builder<'a> {
    let builder = builder
        .background_color(background_color(preboot_theme, active.allow_transparency))
        .transparent(active.allow_transparency)
        .decorations(cfg!(target_os = "macos") || active.use_native_title_bar)
        .resizable(true);
    apply_platform(builder, active)
}

/// `hasShadow: false` for a transparent window. GTK draws no shadow of its own, so Tauri ignores
/// it there.
#[cfg(target_os = "linux")]
fn apply_platform(builder: Builder<'_>, active: ActiveWindowBehavior) -> Builder<'_> {
    builder.shadow(!active.allow_transparency)
}

#[cfg(windows)]
fn apply_platform(builder: Builder<'_>, _active: ActiveWindowBehavior) -> Builder<'_> {
    builder
}

#[cfg(target_os = "macos")]
fn apply_platform(builder: Builder<'_>, _active: ActiveWindowBehavior) -> Builder<'_> {
    builder
        .title_bar_style(tauri::TitleBarStyle::Overlay)
        .hidden_title(true)
        // `CUSTOM_TITLEBAR_TRAFFIC_LIGHT_POSITION`: centred in the 32 px titlebar.
        .traffic_light_position(tauri::LogicalPosition::new(12.0, 9.0))
        .accept_first_mouse(true)
}
