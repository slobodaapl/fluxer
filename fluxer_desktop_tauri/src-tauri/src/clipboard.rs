// SPDX-License-Identifier: AGPL-3.0-or-later

//! Writing text to the system clipboard for the shell's own menus (Electron's
//! `clipboard.writeText`). On Linux GTK owns the clipboard, so the write runs on the GTK main
//! thread; elsewhere arboard writes it. Failures are logged, as Electron logged them.

use tauri::AppHandle;

#[cfg(target_os = "linux")]
pub fn write_text(app: &AppHandle, text: &str) {
    let text = text.to_owned();
    let scheduled = app.run_on_main_thread(move || {
        gtk::Clipboard::get(&gtk::gdk::SELECTION_CLIPBOARD).set_text(&text);
    });
    if let Err(error) = scheduled {
        tracing::error!(%error, "Failed to write to the clipboard");
    }
}

#[cfg(not(target_os = "linux"))]
pub fn write_text(_app: &AppHandle, text: &str) {
    let written = arboard::Clipboard::new().and_then(|mut clipboard| clipboard.set_text(text));
    if let Err(error) = written {
        tracing::error!(%error, "Failed to write to the clipboard");
    }
}
