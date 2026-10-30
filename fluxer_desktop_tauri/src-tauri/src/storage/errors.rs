// SPDX-License-Identifier: AGPL-3.0-or-later

//! The errors DesktopAppStorage.ts threw, with the class names the renderer sees.

use std::fmt::Display;

use crate::error::BridgeError;

pub(super) fn request(context: &str, reason: &str) -> BridgeError {
    BridgeError::new(
        "DesktopAppStoreRequestError",
        format!("Desktop app store request is invalid: {context} {reason}"),
    )
}

pub(super) fn unavailable(reason: &str) -> BridgeError {
    BridgeError::new(
        "DesktopAppStoreUnavailableError",
        format!("Desktop app store is unavailable: {reason}"),
    )
}

pub(super) fn shutdown() -> BridgeError {
    BridgeError::new(
        "DesktopAppStoreShutdownError",
        "Desktop app store is shutting down",
    )
}

pub(super) fn capacity(resource: &str, limit: usize) -> BridgeError {
    BridgeError::new(
        "DesktopAppStoreCapacityError",
        format!("Desktop app store {resource} is limited to {limit}"),
    )
}

pub(super) fn write_refused(refusals: &[String]) -> BridgeError {
    BridgeError::new(
        "DesktopAppStoreWriteRefusedError",
        format!(
            "Desktop app store refused {} write(s): {}",
            refusals.len(),
            refusals.join("; ")
        ),
    )
}

/// A failure below the TS layer: the native store (whose napi errors were plain `Error`s),
/// the file system, or a blocking task.
pub(super) fn native(error: impl Display) -> BridgeError {
    BridgeError::new("Error", error.to_string())
}
