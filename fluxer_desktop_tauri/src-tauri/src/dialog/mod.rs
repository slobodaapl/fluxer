// SPDX-License-Identifier: AGPL-3.0-or-later

//! Native dialogs, the part of Electron's `dialog` module the shell uses: `showOpenDialog`,
//! `showSaveDialog` and `showMessageBoxSync`.
//!
//! Linux builds them with GTK 3 (the toolkit Tauri already runs): `GtkFileChooserNative`, which
//! hands off to the desktop portal under Flatpak or `GTK_USE_PORTAL=1`, and `GtkMessageDialog`.
//! Windows and macOS use `rfd`. File dialogs are modal to the calling window and are built on
//! the main thread; the caller awaits the answer. A dialog that cannot be shown is logged and
//! reads as cancelled.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(not(target_os = "linux"))]
mod rfd_backend;

#[cfg(target_os = "linux")]
use linux as backend;
#[cfg(not(target_os = "linux"))]
use rfd_backend as backend;

use std::path::PathBuf;

use tauri::Window;
use tokio::sync::oneshot;

/// One entry of Electron's `filters`.
pub struct FileFilter {
    pub name: String,
    pub extensions: &'static [&'static str],
}

/// The `openFile`/`multiSelections`/`openDirectory` properties a caller passes.
pub enum OpenTarget {
    Files { multiple: bool },
    Directory,
}

pub struct OpenDialogOptions {
    pub title: String,
    pub target: OpenTarget,
    pub filter: Option<FileFilter>,
}

/// Electron's `defaultPath`, already split into the folder and the name the dialog starts at.
pub struct SaveDialogOptions {
    pub folder: Option<PathBuf>,
    pub file_name: Option<String>,
}

/// `showMessageBoxSync` with `type: 'error'`, the only type a ported caller uses so far.
pub struct MessageBoxOptions {
    pub title: String,
    pub message: String,
    pub detail: String,
    pub buttons: Vec<String>,
    #[cfg_attr(
        not(target_os = "linux"),
        expect(dead_code, reason = "rfd always defaults to the first button")
    )]
    pub default_id: usize,
    pub cancel_id: usize,
}

/// `showOpenDialog(window, options)`; an empty list when cancelled.
pub async fn show_open_dialog(window: &Window, options: OpenDialogOptions) -> Vec<PathBuf> {
    present(window, move |window, reply| {
        backend::open(window, options, reply)
    })
    .await
    .unwrap_or_default()
}

/// `showSaveDialog(window, options)`; `None` when cancelled. The platform dialog asks before
/// replacing an existing file.
pub async fn show_save_dialog(window: &Window, options: SaveDialogOptions) -> Option<PathBuf> {
    present(window, move |window, reply| {
        backend::save(window, options, reply)
    })
    .await
}

/// `showMessageBoxSync` without a parent window: the index of the button pressed, or
/// `cancel_id` when the box was dismissed. Must run on the main thread.
pub fn show_message_box_sync(options: &MessageBoxOptions) -> usize {
    backend::message_box_sync(options)
}

/// Runs `show` on the main thread and waits for the answer it sends; `None` when the dialog
/// was cancelled or could not be shown.
async fn present<T: Send + 'static>(
    window: &Window,
    show: impl FnOnce(&Window, oneshot::Sender<Option<T>>) + Send + 'static,
) -> Option<T> {
    let (reply, answer) = oneshot::channel();
    let target = window.clone();
    if let Err(error) = window.run_on_main_thread(move || show(&target, reply)) {
        tracing::error!(%error, "Failed to schedule a native dialog");
        return None;
    }
    answer.await.ok().flatten()
}
