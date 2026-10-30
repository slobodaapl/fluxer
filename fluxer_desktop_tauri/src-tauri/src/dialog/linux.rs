// SPDX-License-Identifier: AGPL-3.0-or-later

//! GTK 3 dialogs. File choosers are shown without a nested main loop and answer from their
//! `response` signal; the message box runs one (`gtk_dialog_run`), as `showMessageBoxSync` did.
//! Electron's GTK filters matched an extension in lower and upper case; so do these.

use std::cell::RefCell;
use std::path::PathBuf;

use gtk::prelude::*;
use gtk::{
    ButtonsType, DialogFlags, FileChooserAction, FileChooserNative, MessageDialog, MessageType,
    ResponseType,
};
use tauri::Window;
use tokio::sync::oneshot::Sender;

use super::{MessageBoxOptions, OpenDialogOptions, OpenTarget, SaveDialogOptions};

fn chooser(window: &Window, title: Option<&str>, action: FileChooserAction) -> FileChooserNative {
    let parent = window
        .gtk_window()
        .inspect_err(|error| tracing::warn!(%error, "A native dialog has no parent window"))
        .ok();
    let chooser = FileChooserNative::new(title, parent.as_ref(), action, None, None);
    chooser.set_modal(true);
    chooser
}

/// Shows `chooser` and sends `read(chooser)` once it is accepted, `None` once it is dismissed.
/// GTK does not keep a native dialog alive, so the handler holds it until the answer.
fn present<T: 'static>(
    chooser: FileChooserNative,
    reply: Sender<Option<T>>,
    read: impl Fn(&FileChooserNative) -> Option<T> + 'static,
) {
    let pending = RefCell::new(Some((chooser.clone(), reply)));
    chooser.connect_response(move |chooser, response| {
        let Some((_alive, reply)) = pending.borrow_mut().take() else {
            return;
        };
        let _ = reply.send(
            (response == ResponseType::Accept)
                .then(|| read(chooser))
                .flatten(),
        );
    });
    chooser.show();
}

pub(super) fn open(
    window: &Window,
    options: OpenDialogOptions,
    reply: Sender<Option<Vec<PathBuf>>>,
) {
    let action = match options.target {
        OpenTarget::Files { .. } => FileChooserAction::Open,
        OpenTarget::Directory => FileChooserAction::SelectFolder,
    };
    let chooser = chooser(window, Some(&options.title), action);
    chooser.set_select_multiple(matches!(
        options.target,
        OpenTarget::Files { multiple: true }
    ));
    if let Some(filter) = options.filter {
        let patterns = gtk::FileFilter::new();
        patterns.set_name(Some(&filter.name));
        for extension in filter.extensions {
            patterns.add_pattern(&format!("*.{}", extension.to_ascii_lowercase()));
            patterns.add_pattern(&format!("*.{}", extension.to_ascii_uppercase()));
        }
        chooser.add_filter(patterns);
    }
    present(chooser, reply, |chooser| Some(chooser.filenames()));
}

/// Electron passed its empty `title` through, so the chooser has an empty title.
pub(super) fn save(window: &Window, options: SaveDialogOptions, reply: Sender<Option<PathBuf>>) {
    let chooser = chooser(window, Some(""), FileChooserAction::Save);
    chooser.set_do_overwrite_confirmation(true);
    if let Some(folder) = &options.folder {
        chooser.set_current_folder(folder);
    }
    if let Some(name) = &options.file_name {
        chooser.set_current_name(name);
    }
    present(chooser, reply, FileChooserExt::filename);
}

pub(super) fn message_box_sync(options: &MessageBoxOptions) -> usize {
    let dialog = MessageDialog::new(
        None::<&gtk::Window>,
        DialogFlags::MODAL,
        MessageType::Error,
        ButtonsType::None,
        &options.message,
    );
    dialog.set_title(&options.title);
    if !options.detail.is_empty() {
        dialog.set_secondary_text(Some(&options.detail));
    }
    for (index, label) in options.buttons.iter().enumerate() {
        dialog.add_button(label, response_for(index));
    }
    dialog.set_default_response(response_for(options.default_id));
    let response = dialog.run();
    dialog.close();
    match response {
        ResponseType::Other(index) => usize::from(index),
        _ => options.cancel_id,
    }
}

fn response_for(index: usize) -> ResponseType {
    ResponseType::Other(u16::try_from(index).expect("a message box has few buttons"))
}
