#![deny(clippy::all)]

// SPDX-License-Identifier: AGPL-3.0-or-later

pub mod env;
pub mod keymap;
pub mod modifiers;
pub mod mouse;

#[cfg(target_os = "linux")]
mod hook;

#[cfg(feature = "napi")]
pub mod napi_surface;

#[cfg(target_os = "linux")]
pub use hook::{
    DecodedEvent, EventKind, EventSink, InputHook, InputHookError, Result, StartTicket,
    is_available,
};

#[cfg(not(target_os = "linux"))]
pub fn is_available() -> bool {
    false
}
