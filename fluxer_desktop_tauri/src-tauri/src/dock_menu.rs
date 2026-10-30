// SPDX-License-Identifier: AGPL-3.0-or-later

//! The macOS dock menu, the port of fluxer_desktop/src/main/DockMenu.ts: "Open settings" and,
//! while direct messages can be started, "New direct message", each showing the main window
//! first. Rebuilt when the native strings or the direct-message availability change.
//!
//! Electron put the account switch items (two or more signed-in accounts) above them and
//! rebuilt on account and tray privacy changes. There is no account engine yet, so there are no
//! account items, and the privacy listener, which only renamed them, has nothing to rebuild.
//!
//! Tauri has no dock menu, so the app delegate (tao's) gets an `applicationDockMenu:` method
//! returning a muda menu, whose clicks arrive as menu events. Not verified on macOS; the item
//! model is tested on every platform.

use crate::i18n::MainI18n;
use crate::jump_list::Task;

const ID_PREFIX: &str = "dock.";

/// `buildDockMenu`'s items.
fn entries(i18n: &MainI18n, direct_messages_available: bool) -> Vec<(Task, String)> {
    let mut entries = vec![(
        Task::OpenSettings,
        i18n.t("desktop.jumpList.openSettings", &[]),
    )];
    if direct_messages_available {
        entries.push((
            Task::NewDm,
            i18n.t("desktop.jumpList.newDirectMessage", &[]),
        ));
    }
    entries
}

fn menu_id(task: Task) -> String {
    format!("{ID_PREFIX}{}", task.id())
}

fn task_from_menu_id(id: &str) -> Option<Task> {
    let id = id.strip_prefix(ID_PREFIX)?;
    [Task::OpenSettings, Task::NewDm]
        .into_iter()
        .find(|task| task.id() == id)
}

#[cfg(target_os = "macos")]
mod delegate {
    use std::cell::RefCell;

    use muda::{ContextMenu, Menu, MenuItem};
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, Imp, Sel};
    use objc2::{class, msg_send, sel};
    use tauri::{AppHandle, Manager};

    use super::{entries, menu_id, task_from_menu_id};
    use crate::i18n::MainI18n;
    use crate::jump_list::{self, JumpList};

    thread_local! {
        /// The menu the dock shows; AppKit asks for it on the main thread.
        static DOCK_MENU: RefCell<Option<Menu>> = const { RefCell::new(None) };
    }

    extern "C-unwind" fn application_dock_menu(
        _delegate: &AnyObject,
        _selector: Sel,
        _application: *mut AnyObject,
    ) -> *mut AnyObject {
        DOCK_MENU.with_borrow(|menu| {
            menu.as_ref()
                .map_or(std::ptr::null_mut(), |menu| menu.ns_menu().cast())
        })
    }

    /// Adds `applicationDockMenu:` to the app delegate's class.
    fn add_delegate_method() -> bool {
        // SAFETY: the shared application's delegate is tao's, alive for the process; the
        // method's type encoding (`@@:@`) matches `application_dock_menu`.
        unsafe {
            let app: Option<Retained<AnyObject>> =
                msg_send![class!(NSApplication), sharedApplication];
            let Some(app) = app else {
                return false;
            };
            let delegate: Option<Retained<AnyObject>> = msg_send![&*app, delegate];
            let Some(delegate) = delegate else {
                return false;
            };
            let class = (*delegate).class();
            let imp: Imp = std::mem::transmute::<
                extern "C-unwind" fn(&AnyObject, Sel, *mut AnyObject) -> *mut AnyObject,
                Imp,
            >(application_dock_menu);
            objc2::ffi::class_addMethod(
                (class as *const objc2::runtime::AnyClass).cast_mut(),
                sel!(applicationDockMenu:),
                imp,
                c"@@:@".as_ptr(),
            )
            .as_bool()
        }
    }

    /// `initializeDockMenu`.
    pub fn install(app: &AppHandle) {
        if !add_delegate_method() {
            tracing::warn!("Failed to install macOS dock menu");
            return;
        }
        app.on_menu_event(|app, event| {
            if let Some(task) = task_from_menu_id(event.id().as_ref()) {
                jump_list::dispatch_task(app, task);
            }
        });
        refresh(app);
    }

    /// `refreshDockMenu`, on the main thread.
    pub fn refresh(app: &AppHandle) {
        let handle = app.clone();
        let scheduled = app.run_on_main_thread(move || {
            let items = entries(
                &handle.state::<MainI18n>(),
                handle.state::<JumpList>().direct_messages_available(),
            );
            let menu = Menu::new();
            for (task, label) in items {
                if let Err(error) =
                    menu.append(&MenuItem::with_id(menu_id(task), label, true, None))
                {
                    tracing::warn!(%error, "Failed to install macOS dock menu");
                    return;
                }
            }
            DOCK_MENU.with_borrow_mut(|current| *current = Some(menu));
        });
        if let Err(error) = scheduled {
            tracing::warn!(%error, "Failed to schedule the macOS dock menu refresh");
        }
    }
}

#[cfg(target_os = "macos")]
pub use self::delegate::{install, refresh};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_dm_is_offered_only_while_direct_messages_are_available() {
        let directory = tempfile::tempdir().unwrap();
        let i18n = MainI18n::load(directory.path());
        assert_eq!(
            entries(&i18n, true),
            [
                (Task::OpenSettings, "Open settings".to_owned()),
                (Task::NewDm, "New direct message".to_owned()),
            ]
        );
        assert_eq!(
            entries(&i18n, false),
            [(Task::OpenSettings, "Open settings".to_owned())]
        );
    }

    #[test]
    fn dock_item_ids_name_their_task() {
        for task in [Task::OpenSettings, Task::NewDm] {
            assert_eq!(task_from_menu_id(&menu_id(task)), Some(task));
        }
        assert_eq!(task_from_menu_id("dock.open-account-switcher"), None);
        assert_eq!(task_from_menu_id("tray.open-settings"), None);
    }
}
