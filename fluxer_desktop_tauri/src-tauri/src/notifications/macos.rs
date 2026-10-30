// SPDX-License-Identifier: AGPL-3.0-or-later

//! The macOS path of NotificationsIpc.ts, where Electron used its `Notification`: a
//! `UNUserNotificationCenter` request with the title, subtitle and body, no sound, under the
//! notification id (so showing an id again replaces it); closing removes it.
//!
//! Show and close only, until a signed app bundle exists (P8): no authorization request, so
//! nothing appears before the user allows notifications in System Settings; no delegate, so
//! clicks and dismissals are not seen and a target stays until the store evicts it; no icon.
//! `UNUserNotificationCenter` raises an Objective-C exception in a process without a bundle
//! identifier, so such a process shows nothing. Not verified on macOS.

use std::collections::HashSet;
use std::sync::Mutex;

use objc2_foundation::{NSArray, NSBundle, NSString};
use objc2_user_notifications::{
    UNMutableNotificationContent, UNNotificationRequest, UNUserNotificationCenter,
};

use super::NotificationOptions;
use super::targets::NotificationTargets;

#[derive(Default)]
pub struct MacNotifications {
    active: Mutex<HashSet<String>>,
}

/// The notification center, when the process has a bundle identifier.
#[allow(unused_unsafe)]
fn center() -> Option<objc2::rc::Retained<UNUserNotificationCenter>> {
    unsafe { NSBundle::mainBundle().bundleIdentifier() }?;
    Some(unsafe { UNUserNotificationCenter::currentNotificationCenter() })
}

impl MacNotifications {
    #[allow(unused_unsafe)]
    pub fn show(&self, id: &str, options: &NotificationOptions) {
        let Some(center) = center() else {
            tracing::warn!("Notifications need an app bundle on macOS; not shown");
            return;
        };
        self.active
            .lock()
            .expect("macOS notifications lock")
            .insert(id.to_owned());
        unsafe {
            let content = UNMutableNotificationContent::new();
            content.setTitle(&NSString::from_str(&options.title));
            if let Some(subtitle) = options.subtitle() {
                content.setSubtitle(&NSString::from_str(subtitle));
            }
            content.setBody(&NSString::from_str(&options.body));
            let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
                &NSString::from_str(id),
                &content,
                None,
            );
            center.addNotificationRequest_withCompletionHandler(&request, None);
        }
    }

    #[allow(unused_unsafe)]
    pub fn close(&self, id: &str, targets: &mut NotificationTargets) {
        if !self
            .active
            .lock()
            .expect("macOS notifications lock")
            .remove(id)
        {
            return;
        }
        targets.forget(id);
        let Some(center) = center() else {
            return;
        };
        let identifiers = NSArray::from_retained_slice(&[NSString::from_str(id)]);
        unsafe {
            center.removeDeliveredNotificationsWithIdentifiers(&identifiers);
            center.removePendingNotificationRequestsWithIdentifiers(&identifiers);
        }
    }
}
