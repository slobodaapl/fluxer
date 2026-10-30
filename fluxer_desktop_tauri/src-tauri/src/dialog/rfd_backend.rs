// SPDX-License-Identifier: AGPL-3.0-or-later

//! Windows and macOS dialogs through `rfd`, parented to the calling window's native handle.
//! rfd's message boxes take at most three buttons and have no `defaultId`: the first button is
//! the default, and a box dismissed without a button press answers `cancelId`.

use std::path::PathBuf;

use futures_util::future::BoxFuture;
use rfd::{
    AsyncFileDialog, FileHandle, MessageButtons, MessageDialog, MessageDialogResult, MessageLevel,
};
use tauri::Window;
use tokio::sync::oneshot::Sender;

use super::{MessageBoxOptions, OpenDialogOptions, OpenTarget, SaveDialogOptions};

fn path_of(handle: FileHandle) -> PathBuf {
    handle.path().to_path_buf()
}

pub(super) fn open(
    window: &Window,
    options: OpenDialogOptions,
    reply: Sender<Option<Vec<PathBuf>>>,
) {
    let mut dialog = AsyncFileDialog::new()
        .set_parent(window)
        .set_title(options.title);
    if let Some(filter) = options.filter {
        dialog = dialog.add_filter(filter.name, filter.extensions);
    }
    // rfd builds a macOS panel when the future is created, so that happens here, on the main
    // thread; the future itself is awaited elsewhere.
    let answer: BoxFuture<'static, Option<Vec<PathBuf>>> = match options.target {
        OpenTarget::Files { multiple: true } => {
            let picking = dialog.pick_files();
            Box::pin(async move {
                picking
                    .await
                    .map(|handles| handles.into_iter().map(path_of).collect())
            })
        }
        OpenTarget::Files { multiple: false } => {
            let picking = dialog.pick_file();
            Box::pin(async move { picking.await.map(|handle| vec![path_of(handle)]) })
        }
        OpenTarget::Directory => {
            let picking = dialog.pick_folder();
            Box::pin(async move { picking.await.map(|handle| vec![path_of(handle)]) })
        }
    };
    tauri::async_runtime::spawn(async move {
        let _ = reply.send(answer.await);
    });
}

pub(super) fn save(window: &Window, options: SaveDialogOptions, reply: Sender<Option<PathBuf>>) {
    let mut dialog = AsyncFileDialog::new().set_parent(window);
    if let Some(folder) = &options.folder {
        dialog = dialog.set_directory(folder);
    }
    if let Some(name) = options.file_name {
        dialog = dialog.set_file_name(name);
    }
    let answer = dialog.save_file();
    tauri::async_runtime::spawn(async move {
        let _ = reply.send(answer.await.map(path_of));
    });
}

pub(super) fn message_box_sync(options: &MessageBoxOptions) -> usize {
    let buttons = match options.buttons.as_slice() {
        [ok] => MessageButtons::OkCustom(ok.clone()),
        [ok, cancel] => MessageButtons::OkCancelCustom(ok.clone(), cancel.clone()),
        [yes, no, cancel] => {
            MessageButtons::YesNoCancelCustom(yes.clone(), no.clone(), cancel.clone())
        }
        _ => panic!("rfd message boxes take one to three buttons"),
    };
    let description = if options.detail.is_empty() {
        options.message.clone()
    } else {
        format!("{}\n\n{}", options.message, options.detail)
    };
    let result = MessageDialog::new()
        .set_level(MessageLevel::Error)
        .set_title(&options.title)
        .set_description(description)
        .set_buttons(buttons)
        .show();
    match result {
        MessageDialogResult::Custom(label) => options
            .buttons
            .iter()
            .position(|button| *button == label)
            .unwrap_or(options.cancel_id),
        _ => options.cancel_id,
    }
}
