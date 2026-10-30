// SPDX-License-Identifier: AGPL-3.0-or-later

//! macOS autostart: the app as a login item through `SMAppService.mainAppService`, which is what
//! Electron's `app.setLoginItemSettings({openAtLogin})` uses on macOS 13 and later. Before macOS
//! 13 the class is missing and autostart is unavailable (enable logs, enabled reads false);
//! Electron fell back to the deprecated login-items API there. Registration failures are
//! logged, as Electron's call never threw.
//!
//! A login-item launch carries no `--autostart`; it is recognised by the launch's Apple event,
//! which is how Electron's `wasOpenedAtLogin` reads it. Not verified on macOS.

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2::{class, msg_send};
use objc2_service_management::{SMAppService, SMAppServiceStatus};
use tauri::AppHandle;

/// `kAEOpenApplication`, `keyAEPropData` and `keyAELaunchedAsLogInItem`.
const OPEN_APPLICATION_EVENT: u32 = u32::from_be_bytes(*b"oapp");
const PROPERTY_DATA_KEYWORD: u32 = u32::from_be_bytes(*b"prdt");
const LAUNCHED_AS_LOGIN_ITEM: u32 = u32::from_be_bytes(*b"lgit");

fn main_app_service() -> Option<Retained<SMAppService>> {
    if AnyClass::get(c"SMAppService").is_none() {
        tracing::warn!("Login items need macOS 13 or later");
        return None;
    }
    // SAFETY: the class exists; `mainAppService` takes no arguments.
    Some(unsafe { SMAppService::mainAppService() })
}

pub async fn enable(_app: &AppHandle) -> Result<(), String> {
    if let Some(service) = main_app_service()
        // SAFETY: a plain method call on a live service.
        && let Err(error) = unsafe { service.registerAndReturnError() }
    {
        tracing::warn!(%error, "Failed to register the login item");
    }
    Ok(())
}

pub async fn disable(_app: &AppHandle) -> Result<(), String> {
    if let Some(service) = main_app_service()
        // SAFETY: a plain method call on a live service.
        && let Err(error) = unsafe { service.unregisterAndReturnError() }
    {
        tracing::warn!(%error, "Failed to unregister the login item");
    }
    Ok(())
}

/// `getLoginItemSettings().openAtLogin`.
pub fn is_enabled(_app: &AppHandle) -> bool {
    // SAFETY: a plain method call on a live service.
    main_app_service()
        .is_some_and(|service| unsafe { service.status() } == SMAppServiceStatus::Enabled)
}

/// Whether the Apple event being handled (the launch's, during start-up) opened the app as a
/// login item.
pub fn opened_at_login() -> bool {
    // SAFETY: AppKit's shared Apple event manager and the descriptors it returns; each message
    // matches the documented selector and return type, and nil replies end the check.
    unsafe {
        let manager: Option<Retained<AnyObject>> =
            msg_send![class!(NSAppleEventManager), sharedAppleEventManager];
        let Some(manager) = manager else {
            return false;
        };
        let event: Option<Retained<AnyObject>> = msg_send![&*manager, currentAppleEvent];
        let Some(event) = event else {
            return false;
        };
        let event_id: u32 = msg_send![&*event, eventID];
        if event_id != OPEN_APPLICATION_EVENT {
            return false;
        }
        let property: Option<Retained<AnyObject>> =
            msg_send![&*event, paramDescriptorForKeyword: PROPERTY_DATA_KEYWORD];
        property.is_some_and(|property| {
            let code: u32 = msg_send![&*property, enumCodeValue];
            code == LAUNCHED_AS_LOGIN_ITEM
        })
    }
}
