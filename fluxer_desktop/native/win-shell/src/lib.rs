// SPDX-License-Identifier: AGPL-3.0-or-later

#[cfg(feature = "napi")]
pub mod napi_surface;

/// Why a call failed. The napi surface maps `InvalidArg` to `Status::InvalidArg` and `Generic` to
/// `Status::GenericFailure`, with the message unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellError {
    InvalidArg(String),
    Generic(String),
}

impl std::fmt::Display for ShellError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidArg(message) | Self::Generic(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for ShellError {}

pub type Result<T> = std::result::Result<T, ShellError>;

/// A `.lnk` to create. `lnk_path` and `target` must be non-empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortcutOptions {
    pub lnk_path: String,
    pub target: String,
    pub args: Option<String>,
    pub app_user_model_id: Option<String>,
    pub toast_activator_clsid: Option<String>,
    pub icon_path: Option<String>,
    pub icon_index: i32,
    pub working_dir: Option<String>,
    pub description: Option<String>,
}

/// Creates or overwrites a shortcut file. Blocking; initialises COM on the calling thread.
/// Fails with a generic error on platforms other than Windows.
pub fn create_shortcut(opts: &ShortcutOptions) -> Result<()> {
    require_non_empty(&opts.lnk_path, "lnkPath")?;
    require_non_empty(&opts.target, "target")?;
    platform::create_shortcut(opts)
}

/// Sets a string value under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
pub fn set_current_user_run_value(name: &str, command: &str) -> Result<()> {
    require_non_empty(name, "name")?;
    require_non_empty(command, "command")?;
    platform::set_current_user_run_value(name, command)
}

/// Deletes a value from the HKCU Run key. A missing value or key is success.
pub fn delete_current_user_run_value(name: &str) -> Result<()> {
    require_non_empty(name, "name")?;
    platform::delete_current_user_run_value(name)
}

/// Reads a string value from the HKCU Run key; `None` when it does not exist.
pub fn get_current_user_run_value(name: &str) -> Result<Option<String>> {
    require_non_empty(name, "name")?;
    platform::get_current_user_run_value(name)
}

/// One of `not-present`, `busy`, `running-d3d-full-screen`, `presentation-mode`,
/// `accepts-notifications`, `quiet-time`, `app`, `unknown`.
#[cfg(target_os = "windows")]
pub fn get_user_notification_state() -> Result<String> {
    platform::get_user_notification_state()
}

pub(crate) fn require_non_empty(value: &str, property: &'static str) -> Result<()> {
    if value.is_empty() {
        return Err(ShellError::InvalidArg(format!(
            "{property} is required and must be non-empty"
        )));
    }
    Ok(())
}

#[cfg(target_os = "windows")]
mod platform {
    use super::{Result, ShellError, ShortcutOptions};
    use std::ffi::c_void;
    use std::mem::ManuallyDrop;
    use windows::Win32::Foundation::{
        ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, PROPERTYKEY, WIN32_ERROR,
    };
    use windows::Win32::System::Com::StructuredStorage::{
        PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
    };
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, CLSIDFromString, COINIT_APARTMENTTHREADED, CoCreateInstance,
        CoInitializeEx, CoUninitialize, IPersistFile,
    };
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ,
        RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegGetValueW, RegOpenKeyExW, RegSetValueExW,
    };
    use windows::Win32::System::Variant::{VT_CLSID, VT_LPWSTR};
    use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
    use windows::Win32::UI::Shell::{
        IShellLinkW, QUNS_ACCEPTS_NOTIFICATIONS, QUNS_APP, QUNS_BUSY, QUNS_NOT_PRESENT,
        QUNS_PRESENTATION_MODE, QUNS_QUIET_TIME, QUNS_RUNNING_D3D_FULL_SCREEN,
        SHQueryUserNotificationState, ShellLink,
    };
    use windows::core::{GUID, Interface, PCWSTR, PWSTR};

    const CURRENT_USER_RUN_SUBKEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
    const PKEY_APP_USER_MODEL_ID: PROPERTYKEY = PROPERTYKEY {
        fmtid: GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3),
        pid: 5,
    };
    const PKEY_APP_USER_MODEL_TOAST_ACTIVATOR_CLSID: PROPERTYKEY = PROPERTYKEY {
        fmtid: GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3),
        pid: 26,
    };

    pub(super) fn get_user_notification_state() -> Result<String> {
        let state = unsafe { SHQueryUserNotificationState() }
            .map_err(|err| hresult_error("SHQueryUserNotificationState failed", err))?;
        let label = match state.0 {
            value if value == QUNS_NOT_PRESENT.0 => "not-present",
            value if value == QUNS_BUSY.0 => "busy",
            value if value == QUNS_RUNNING_D3D_FULL_SCREEN.0 => "running-d3d-full-screen",
            value if value == QUNS_PRESENTATION_MODE.0 => "presentation-mode",
            value if value == QUNS_ACCEPTS_NOTIFICATIONS.0 => "accepts-notifications",
            value if value == QUNS_QUIET_TIME.0 => "quiet-time",
            value if value == QUNS_APP.0 => "app",
            _ => "unknown",
        };
        Ok(label.to_string())
    }

    pub(super) fn create_shortcut(opts: &ShortcutOptions) -> Result<()> {
        let _com = ComApartment::initialize()?;

        unsafe {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)
                .map_err(|err| hresult_error("CoCreateInstance(CLSID_ShellLink) failed", err))?;

            let target = wide_null(&opts.target);
            link.SetPath(PCWSTR(target.as_ptr()))
                .map_err(|err| hresult_error("IShellLinkW::SetPath failed", err))?;

            if let Some(args) = opts.args.as_deref() {
                let args = wide_null(args);
                link.SetArguments(PCWSTR(args.as_ptr()))
                    .map_err(|err| hresult_error("IShellLinkW::SetArguments failed", err))?;
            }

            if let Some(icon_path) = opts.icon_path.as_deref() {
                let icon_path = wide_null(icon_path);
                link.SetIconLocation(PCWSTR(icon_path.as_ptr()), opts.icon_index)
                    .map_err(|err| hresult_error("IShellLinkW::SetIconLocation failed", err))?;
            }

            if let Some(working_dir) = opts.working_dir.as_deref() {
                let working_dir = wide_null(working_dir);
                link.SetWorkingDirectory(PCWSTR(working_dir.as_ptr()))
                    .map_err(|err| hresult_error("IShellLinkW::SetWorkingDirectory failed", err))?;
            }

            if let Some(description) = opts.description.as_deref() {
                let description = wide_null(description);
                link.SetDescription(PCWSTR(description.as_ptr()))
                    .map_err(|err| hresult_error("IShellLinkW::SetDescription failed", err))?;
            }

            if opts.app_user_model_id.is_some() || opts.toast_activator_clsid.is_some() {
                let property_store: IPropertyStore = link
                    .cast()
                    .map_err(|err| hresult_error("QueryInterface(IPropertyStore) failed", err))?;
                if let Some(app_user_model_id) = opts.app_user_model_id.as_deref() {
                    set_string_property(
                        &property_store,
                        &PKEY_APP_USER_MODEL_ID,
                        app_user_model_id,
                        "IPropertyStore::SetValue(PKEY_AppUserModel_ID) failed",
                    )?;
                }
                if let Some(toast_activator_clsid) = opts.toast_activator_clsid.as_deref() {
                    set_guid_property(
                        &property_store,
                        &PKEY_APP_USER_MODEL_TOAST_ACTIVATOR_CLSID,
                        toast_activator_clsid,
                        "IPropertyStore::SetValue(PKEY_AppUserModel_ToastActivatorCLSID) failed",
                    )?;
                }
                property_store
                    .Commit()
                    .map_err(|err| hresult_error("IPropertyStore::Commit failed", err))?;
            }

            let persist: IPersistFile = link
                .cast()
                .map_err(|err| hresult_error("QueryInterface(IPersistFile) failed", err))?;
            let lnk_path = wide_null(&opts.lnk_path);
            persist
                .Save(PCWSTR(lnk_path.as_ptr()), true)
                .map_err(|err| hresult_error("IPersistFile::Save failed", err))?;
        }

        Ok(())
    }

    pub(super) fn set_current_user_run_value(name: &str, command: &str) -> Result<()> {
        let key = RunKey::create()?;
        let name = wide_null(name);
        let command = wide_null(command);
        let data =
            unsafe { std::slice::from_raw_parts(command.as_ptr().cast::<u8>(), command.len() * 2) };
        let status =
            unsafe { RegSetValueExW(key.raw(), PCWSTR(name.as_ptr()), None, REG_SZ, Some(data)) };
        check_win32(status, "RegSetValueExW(HKCU Run) failed")
    }

    pub(super) fn delete_current_user_run_value(name: &str) -> Result<()> {
        let Some(key) = RunKey::open_for_set()? else {
            return Ok(());
        };
        let name = wide_null(name);
        let status = unsafe { RegDeleteValueW(key.raw(), PCWSTR(name.as_ptr())) };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        check_win32(status, "RegDeleteValueW(HKCU Run) failed")
    }

    pub(super) fn get_current_user_run_value(name: &str) -> Result<Option<String>> {
        let subkey = wide_null(CURRENT_USER_RUN_SUBKEY);
        let name = wide_null(name);
        let mut value_type = REG_SZ;
        let mut byte_len = 0u32;
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                PCWSTR(subkey.as_ptr()),
                PCWSTR(name.as_ptr()),
                RRF_RT_REG_SZ,
                Some(&mut value_type),
                None,
                Some(&mut byte_len),
            )
        };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        check_win32(status, "RegGetValueW(HKCU Run size) failed")?;
        if byte_len == 0 {
            return Ok(Some(String::new()));
        }

        let mut buffer = vec![0u16; byte_len.div_ceil(2) as usize];
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                PCWSTR(subkey.as_ptr()),
                PCWSTR(name.as_ptr()),
                RRF_RT_REG_SZ,
                Some(&mut value_type),
                Some(buffer.as_mut_ptr().cast::<c_void>()),
                Some(&mut byte_len),
            )
        };
        check_win32(status, "RegGetValueW(HKCU Run data) failed")?;
        let mut code_units = (byte_len / 2) as usize;
        while code_units > 0 && buffer[code_units - 1] == 0 {
            code_units -= 1;
        }
        String::from_utf16(&buffer[..code_units])
            .map(Some)
            .map_err(|_| ShellError::Generic("HKCU Run value is not valid UTF-16".to_owned()))
    }

    struct RunKey(HKEY);

    impl RunKey {
        fn create() -> Result<Self> {
            let subkey = wide_null(CURRENT_USER_RUN_SUBKEY);
            let mut key = HKEY::default();
            let status = unsafe {
                RegCreateKeyExW(
                    HKEY_CURRENT_USER,
                    PCWSTR(subkey.as_ptr()),
                    None,
                    PCWSTR::null(),
                    REG_OPTION_NON_VOLATILE,
                    KEY_SET_VALUE,
                    None,
                    &mut key,
                    None,
                )
            };
            check_win32(status, "RegCreateKeyExW(HKCU Run) failed")?;
            Ok(Self(key))
        }

        fn open_for_set() -> Result<Option<Self>> {
            let subkey = wide_null(CURRENT_USER_RUN_SUBKEY);
            let mut key = HKEY::default();
            let status = unsafe {
                RegOpenKeyExW(
                    HKEY_CURRENT_USER,
                    PCWSTR(subkey.as_ptr()),
                    None,
                    KEY_SET_VALUE,
                    &mut key,
                )
            };
            if status == ERROR_FILE_NOT_FOUND {
                return Ok(None);
            }
            check_win32(status, "RegOpenKeyExW(HKCU Run) failed")?;
            Ok(Some(Self(key)))
        }

        fn raw(&self) -> HKEY {
            self.0
        }
    }

    impl Drop for RunKey {
        fn drop(&mut self) {
            unsafe {
                let _ = RegCloseKey(self.0);
            }
        }
    }

    struct ComApartment;

    impl ComApartment {
        fn initialize() -> Result<Self> {
            unsafe {
                CoInitializeEx(None, COINIT_APARTMENTTHREADED)
                    .ok()
                    .map_err(|err| hresult_error("CoInitializeEx failed", err))?;
            }
            Ok(Self)
        }
    }

    impl Drop for ComApartment {
        fn drop(&mut self) {
            unsafe {
                CoUninitialize();
            }
        }
    }

    fn wide_null(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn propvariant_from_wide_string(wide: &mut [u16]) -> PROPVARIANT {
        PROPVARIANT {
            Anonymous: PROPVARIANT_0 {
                Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
                    vt: VT_LPWSTR,
                    wReserved1: 0,
                    wReserved2: 0,
                    wReserved3: 0,
                    Anonymous: PROPVARIANT_0_0_0 {
                        pwszVal: PWSTR(wide.as_mut_ptr()),
                    },
                }),
            },
        }
    }

    fn propvariant_from_guid(guid: &mut GUID) -> PROPVARIANT {
        PROPVARIANT {
            Anonymous: PROPVARIANT_0 {
                Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
                    vt: VT_CLSID,
                    wReserved1: 0,
                    wReserved2: 0,
                    wReserved3: 0,
                    Anonymous: PROPVARIANT_0_0_0 {
                        puuid: guid as *mut GUID,
                    },
                }),
            },
        }
    }

    fn set_string_property(
        property_store: &IPropertyStore,
        key: &PROPERTYKEY,
        value: &str,
        label: &'static str,
    ) -> Result<()> {
        let mut wide = wide_null(value);
        unsafe {
            let prop = ManuallyDrop::new(propvariant_from_wide_string(&mut wide));
            property_store
                .SetValue(key as *const PROPERTYKEY, &*prop as *const PROPVARIANT)
                .map_err(|err| hresult_error(label, err))
        }
    }

    fn set_guid_property(
        property_store: &IPropertyStore,
        key: &PROPERTYKEY,
        value: &str,
        label: &'static str,
    ) -> Result<()> {
        let wide = wide_null(value);
        unsafe {
            let mut guid = CLSIDFromString(PCWSTR(wide.as_ptr()))
                .map_err(|err| hresult_error("CLSIDFromString failed", err))?;
            let prop = ManuallyDrop::new(propvariant_from_guid(&mut guid));
            property_store
                .SetValue(key as *const PROPERTYKEY, &*prop as *const PROPVARIANT)
                .map_err(|err| hresult_error(label, err))
        }
    }

    fn hresult_error(label: &'static str, err: windows::core::Error) -> ShellError {
        ShellError::Generic(format!("{label} (hr 0x{:x})", err.code().0 as u32))
    }

    fn check_win32(status: WIN32_ERROR, label: &'static str) -> Result<()> {
        if status == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(win32_error(label, status))
        }
    }

    fn win32_error(label: &'static str, status: WIN32_ERROR) -> ShellError {
        ShellError::Generic(format!("{label} (win32 {})", status.0))
    }
}

#[cfg(not(target_os = "windows"))]
mod platform {
    use super::{Result, ShellError, ShortcutOptions};

    pub(super) fn create_shortcut(_opts: &ShortcutOptions) -> Result<()> {
        Err(ShellError::Generic(
            "win-shell not supported on this platform".to_owned(),
        ))
    }

    pub(super) fn set_current_user_run_value(_name: &str, _command: &str) -> Result<()> {
        Err(ShellError::Generic(
            "win-shell not supported on this platform".to_owned(),
        ))
    }

    pub(super) fn delete_current_user_run_value(_name: &str) -> Result<()> {
        Err(ShellError::Generic(
            "win-shell not supported on this platform".to_owned(),
        ))
    }

    pub(super) fn get_current_user_run_value(_name: &str) -> Result<Option<String>> {
        Err(ShellError::Generic(
            "win-shell not supported on this platform".to_owned(),
        ))
    }
}
