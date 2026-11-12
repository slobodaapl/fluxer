// SPDX-License-Identifier: AGPL-3.0-or-later
//! Initialize the pinned WebRTC archive's weak PipeWire forwarding symbols
//! before pipewire-rs calls them. Otherwise pw_init resolves to a WebRTC stub
//! whose function pointer is still null, even with libpipewire linked.
use crate::Result;

pub(crate) fn ensure() -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        use crate::MediaError;
        use std::sync::OnceLock;
        unsafe extern "C" {
            fn fluxer_initialize_webrtc_pipewire() -> bool;
        }
        static LOADED: OnceLock<bool> = OnceLock::new();
        // SDK modules/portal/pipewire_utils.h explicitly documents this as a
        // library loader: no server connection or physical capture is opened.
        let loaded = LOADED.get_or_init(|| unsafe { fluxer_initialize_webrtc_pipewire() });
        if !loaded {
            return Err(MediaError::Audio("Unable to load PipeWire through the native WebRTC runtime; check the PipeWire installation".into()));
        }
    }
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    #[test]
    fn loader_initializes_weak_symbols_before_direct_pipewire_calls() {
        use std::ffi::{CStr, c_char, c_int};
        unsafe extern "C" {
            fn pw_init(argc: *mut c_int, argv: *mut *mut *mut c_char);
            fn pw_get_library_version() -> *const c_char;
        }
        ensure().expect("load system PipeWire library through WebRTC");
        ensure().expect("loading is idempotent");
        // These are precisely the bindings that previously jumped through
        // uninitialized WebRTC function pointers. No server/audio is accessed.
        unsafe {
            pw_init(std::ptr::null_mut(), std::ptr::null_mut());
            let version = pw_get_library_version();
            assert!(!version.is_null());
            assert!(
                CStr::from_ptr(version)
                    .to_bytes()
                    .iter()
                    .any(u8::is_ascii_digit)
            );
        }
    }
}
