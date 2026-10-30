// SPDX-License-Identifier: AGPL-3.0-or-later

//! Launch tasks (`--fluxer-task=<id>`), the port of fluxer_desktop/src/main/JumpList.ts with
//! `dispatchJumpListTask` and `consumeInitialJumpListTask` of DeepLinks.ts. The Windows jump
//! list and the macOS dock menu launch the app with a task; a running app gets it from the
//! second instance.
//!
//! The renderer reports whether direct messages can be started (`setDirectMessagesAvailable`);
//! the Windows jump list and the macOS dock menu drop "New direct message" while they cannot.
//! Linux, where Electron rewrote its launcher entry's desktop actions, has no such entry here
//! before the cutover, so the member is absent there.

use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{AppHandle, Manager};
#[cfg(any(windows, target_os = "macos"))]
use tauri::{State, Webview};

use crate::events::BridgeEvents;
use crate::tray;
use crate::window::{self, MAIN_WINDOW_LABEL};

/// `TASK_ARG_PREFIX`.
pub const TASK_ARG_PREFIX: &str = "--fluxer-task=";
const NEW_DM_EVENT: &str = "jump-list-new-dm";

/// `JumpListTaskId`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Task {
    OpenSettings,
    NewDm,
    OpenAccountSwitcher,
}

impl Task {
    const ALL: [Self; 3] = [Self::OpenSettings, Self::NewDm, Self::OpenAccountSwitcher];

    pub const fn id(self) -> &'static str {
        match self {
            Self::OpenSettings => "open-settings",
            Self::NewDm => "new-dm",
            Self::OpenAccountSwitcher => "open-account-switcher",
        }
    }
}

/// `parseJumpListTaskFromArgv`: the first `--fluxer-task=` argument with a known id.
pub fn task_from_args(args: &[String]) -> Option<Task> {
    args.iter()
        .filter_map(|arg| arg.strip_prefix(TASK_ARG_PREFIX))
        .find_map(|id| Task::ALL.into_iter().find(|task| task.id() == id))
}

pub struct JumpList {
    /// `directMessagesAvailable`.
    #[cfg(any(windows, target_os = "macos"))]
    direct_messages_available: AtomicBool,
    /// A launch `new-dm` task waiting for the main window's first page load.
    launch_new_dm_pending: AtomicBool,
}

impl Default for JumpList {
    fn default() -> Self {
        Self {
            #[cfg(any(windows, target_os = "macos"))]
            direct_messages_available: AtomicBool::new(true),
            launch_new_dm_pending: AtomicBool::new(false),
        }
    }
}

impl JumpList {
    /// `isDirectMessagesAvailable`.
    #[cfg(any(windows, target_os = "macos"))]
    pub fn direct_messages_available(&self) -> bool {
        self.direct_messages_available.load(Ordering::SeqCst)
    }
}

/// `dispatchJumpListTask`, also the macOS dock menu's clicks. Settings and new-DM need the main
/// window, which is shown first; "open settings" waits for a loading page, as the tray's does.
pub fn dispatch_task(app: &AppHandle, task: Task) {
    match task {
        Task::OpenAccountSwitcher => tray::request_account_switcher(app),
        _ if app.get_webview_window(MAIN_WINDOW_LABEL).is_none() => {}
        Task::OpenSettings => tray::open_settings(app),
        Task::NewDm => {
            window::show_main_window(app);
            app.state::<BridgeEvents>()
                .emit(MAIN_WINDOW_LABEL, NEW_DM_EVENT, ());
        }
    }
}

/// `consumeInitialJumpListTask` and its `did-finish-load` listener: this launch's own task,
/// once the main window exists. The account switcher is asked for at once; settings and new-DM
/// go to the first page that finishes loading, without showing the window.
pub fn launch_task(app: &AppHandle, args: &[String]) {
    match task_from_args(args) {
        Some(Task::OpenAccountSwitcher) => tray::request_account_switcher(app),
        Some(Task::OpenSettings) => tray::send_open_settings(app),
        Some(Task::NewDm) => app
            .state::<JumpList>()
            .launch_new_dm_pending
            .store(true, Ordering::SeqCst),
        None => {}
    }
}

/// The main window's page finished loading.
pub fn page_loaded(app: &AppHandle) {
    if app
        .state::<JumpList>()
        .launch_new_dm_pending
        .swap(false, Ordering::SeqCst)
    {
        app.state::<BridgeEvents>()
            .emit(MAIN_WINDOW_LABEL, NEW_DM_EVENT, ());
    }
}

/// `direct-messages-availability-set`: a boolean from the privileged renderer document;
/// anything else is dropped.
#[cfg(any(windows, target_os = "macos"))]
#[tauri::command]
pub fn direct_messages_availability_set(
    app: AppHandle,
    webview: Webview,
    documents: State<'_, std::sync::Arc<crate::documents::DocumentRegistry>>,
    jump_list: State<'_, JumpList>,
    available: Option<serde_json::Value>,
    document_token: String,
) {
    let Some(serde_json::Value::Bool(available)) = available else {
        return;
    };
    if documents
        .capture_for_read(
            &crate::documents::Caller::new(&webview, document_token),
            "direct-messages-availability-set",
        )
        .is_err()
    {
        return;
    }
    if jump_list
        .direct_messages_available
        .swap(available, Ordering::SeqCst)
        == available
    {
        return;
    }
    #[cfg(windows)]
    refresh(&app);
    #[cfg(target_os = "macos")]
    crate::dock_menu::refresh(&app);
}

/// A task as the jump list shows it (`Electron.Task` without the program and icon, which are
/// the running executable).
#[cfg(any(windows, test))]
#[derive(Debug, PartialEq, Eq)]
struct UserTask {
    arguments: String,
    title: String,
    description: String,
}

/// `buildTasks`.
#[cfg(any(windows, test))]
fn user_tasks(i18n: &crate::i18n::MainI18n, direct_messages_available: bool) -> Vec<UserTask> {
    let app_name = [(
        "appName",
        crate::build_channel::BuildChannel::current().app_name(),
    )];
    let task = |task: Task, title: &str, description: &str| UserTask {
        arguments: format!("{TASK_ARG_PREFIX}{}", task.id()),
        title: i18n.t(title, &app_name),
        description: i18n.t(description, &app_name),
    };
    let mut tasks = vec![task(
        Task::OpenSettings,
        "desktop.jumpList.openSettings",
        "desktop.jumpList.openSettingsDescription",
    )];
    if direct_messages_available {
        tasks.push(task(
            Task::NewDm,
            "desktop.jumpList.newDirectMessage",
            "desktop.jumpList.newDirectMessageDescription",
        ));
    }
    tasks.push(task(
        Task::OpenAccountSwitcher,
        "desktop.jumpList.chooseAccount",
        "desktop.jumpList.chooseAccountDescription",
    ));
    tasks
}

/// `initializeJumpList`: the tasks now; locale and direct-message changes replace them.
#[cfg(windows)]
pub fn initialize(app: &AppHandle) {
    refresh(app);
}

/// The `onLocaleChange` subscription.
#[cfg(windows)]
pub fn locale_changed(app: &AppHandle) {
    refresh(app);
}

/// `refreshJumpList`, on the main thread, where COM runs single-threaded.
#[cfg(windows)]
fn refresh(app: &AppHandle) {
    let handle = app.clone();
    let scheduled = app.run_on_main_thread(move || {
        let tasks = user_tasks(
            &handle.state::<crate::i18n::MainI18n>(),
            handle.state::<JumpList>().direct_messages_available(),
        );
        let installed = std::env::current_exe()
            .map_err(|error| error.to_string())
            .and_then(|exe| com::set_user_tasks(&exe, &tasks).map_err(|error| error.to_string()));
        if let Err(error) = installed {
            tracing::warn!(%error, "Failed to install Windows JumpList");
        }
    });
    if let Err(error) = scheduled {
        tracing::warn!(%error, "Failed to schedule the Windows JumpList refresh");
    }
}

/// `app.setUserTasks`: the tasks as shell links in the user-tasks category of the process's
/// jump list. The list belongs to the process's AppUserModelID, so it is not set here.
#[cfg(windows)]
mod com {
    use std::mem::ManuallyDrop;
    use std::path::Path;

    use windows::Win32::Foundation::PROPERTYKEY;
    use windows::Win32::System::Com::StructuredStorage::{
        PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
    };
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        CoUninitialize,
    };
    use windows::Win32::System::Variant::VT_LPWSTR;
    use windows::Win32::UI::Shell::Common::{IObjectArray, IObjectCollection};
    use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
    use windows::Win32::UI::Shell::{
        DestinationList, EnumerableObjectCollection, ICustomDestinationList, IShellLinkW, ShellLink,
    };
    use windows::core::{GUID, Interface, PCWSTR, PWSTR, Result};

    use super::UserTask;

    /// `PKEY_Title`.
    const PKEY_TITLE: PROPERTYKEY = PROPERTYKEY {
        fmtid: GUID::from_u128(0xf29f85e0_4ff9_1068_ab91_08002b27b3d9),
        pid: 2,
    };

    struct Apartment;

    impl Apartment {
        fn enter() -> Result<Self> {
            // SAFETY: balanced by `CoUninitialize` in `drop`; an apartment already entered on
            // this thread reports S_FALSE, which is success.
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.ok()?;
            Ok(Self)
        }
    }

    impl Drop for Apartment {
        fn drop(&mut self) {
            // SAFETY: pairs with the successful `CoInitializeEx` of `enter`.
            unsafe { CoUninitialize() };
        }
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn set_user_tasks(exe: &Path, tasks: &[UserTask]) -> Result<()> {
        let _apartment = Apartment::enter()?;
        let exe = wide(&exe.to_string_lossy());
        // SAFETY: plain COM calls on interfaces created here; every string outlives the call
        // that reads it.
        unsafe {
            let list: ICustomDestinationList =
                CoCreateInstance(&DestinationList, None, CLSCTX_INPROC_SERVER)?;
            let mut min_slots = 0u32;
            let _removed: IObjectArray = list.BeginList(&mut min_slots)?;
            let collection: IObjectCollection =
                CoCreateInstance(&EnumerableObjectCollection, None, CLSCTX_INPROC_SERVER)?;
            for task in tasks {
                collection.AddObject(&shell_link(&exe, task)?)?;
            }
            list.AddUserTasks(&collection.cast::<IObjectArray>()?)?;
            list.CommitList()
        }
    }

    unsafe fn shell_link(exe: &[u16], task: &UserTask) -> Result<IShellLinkW> {
        let arguments = wide(&task.arguments);
        let description = wide(&task.description);
        let mut title = wide(&task.title);
        // SAFETY: see `set_user_tasks`. The title PROPVARIANT borrows `title`, which
        // `SetValue` copies, and is never dropped as an owner.
        unsafe {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            link.SetPath(PCWSTR(exe.as_ptr()))?;
            link.SetArguments(PCWSTR(arguments.as_ptr()))?;
            link.SetIconLocation(PCWSTR(exe.as_ptr()), 0)?;
            link.SetDescription(PCWSTR(description.as_ptr()))?;
            let store: IPropertyStore = link.cast()?;
            let value = ManuallyDrop::new(PROPVARIANT {
                Anonymous: PROPVARIANT_0 {
                    Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
                        vt: VT_LPWSTR,
                        wReserved1: 0,
                        wReserved2: 0,
                        wReserved3: 0,
                        Anonymous: PROPVARIANT_0_0_0 {
                            pwszVal: PWSTR(title.as_mut_ptr()),
                        },
                    }),
                },
            });
            store.SetValue(&PKEY_TITLE, &*value)?;
            store.Commit()?;
            Ok(link)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn the_first_known_task_argument_wins() {
        assert_eq!(
            task_from_args(&args(&[
                "app",
                "--fluxer-task=nope",
                "--fluxer-task=new-dm"
            ])),
            Some(Task::NewDm)
        );
        assert_eq!(
            task_from_args(&args(&[
                "app",
                "--fluxer-task=open-account-switcher",
                "--fluxer-task=open-settings"
            ])),
            Some(Task::OpenAccountSwitcher)
        );
        for argv in [
            &["app"][..],
            &["app", "--fluxer-task", "new-dm"],
            &["app", "--fluxer-task=New-DM"],
            &["app", "--fluxer-task=new-dm "],
        ] {
            assert_eq!(task_from_args(&args(argv)), None, "{argv:?}");
        }
    }

    #[test]
    fn the_jump_list_drops_new_dm_while_direct_messages_are_unavailable() {
        let directory = tempfile::tempdir().unwrap();
        let i18n = crate::i18n::MainI18n::load(directory.path());
        let app_name = crate::build_channel::BuildChannel::current().app_name();
        let tasks = user_tasks(&i18n, true);
        assert_eq!(
            tasks,
            [
                UserTask {
                    arguments: "--fluxer-task=open-settings".to_owned(),
                    title: "Open settings".to_owned(),
                    description: format!("Open {app_name} settings"),
                },
                UserTask {
                    arguments: "--fluxer-task=new-dm".to_owned(),
                    title: "New direct message".to_owned(),
                    description: "Compose a new direct message".to_owned(),
                },
                UserTask {
                    arguments: "--fluxer-task=open-account-switcher".to_owned(),
                    title: "Choose an account".to_owned(),
                    description: "Switch to another signed-in account".to_owned(),
                },
            ]
        );
        let without = user_tasks(&i18n, false);
        assert_eq!(
            without
                .iter()
                .map(|task| task.arguments.as_str())
                .collect::<Vec<_>>(),
            [
                "--fluxer-task=open-settings",
                "--fluxer-task=open-account-switcher"
            ]
        );
    }
}
