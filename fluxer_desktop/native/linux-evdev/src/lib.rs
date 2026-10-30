#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

// SPDX-License-Identifier: AGPL-3.0-or-later

mod keymap;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
mod logind;

#[cfg(feature = "napi")]
pub mod napi_surface;

#[cfg(target_os = "linux")]
pub use linux::{
    EvdevError, EvdevHook, EventSink, KeyKind, MouseKind, NativeEvent, Result, is_keyboard_readable,
};

#[cfg(not(target_os = "linux"))]
pub fn is_keyboard_readable() -> bool {
    false
}
