// SPDX-License-Identifier: AGPL-3.0-or-later

//! `GetLastInputInfo` against `GetTickCount`, as Chromium's idle_win.cc computed it. Both are
//! 32-bit millisecond tick counts, so the difference wraps with them.

use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};

#[derive(Default)]
pub struct SystemIdle;

impl SystemIdle {
    pub fn idle_time_ms(&self) -> Result<u64, String> {
        let mut info = LASTINPUTINFO {
            cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
            dwTime: 0,
        };
        // SAFETY: `info` is a LASTINPUTINFO with `cbSize` set, alive for the call.
        if !unsafe { GetLastInputInfo(&mut info) }.as_bool() {
            return Err(windows::core::Error::from_thread().message());
        }
        // SAFETY: GetTickCount has no preconditions.
        let now = unsafe { GetTickCount() };
        Ok(u64::from(now.wrapping_sub(info.dwTime)))
    }
}
