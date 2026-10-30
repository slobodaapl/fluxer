// SPDX-License-Identifier: AGPL-3.0-or-later

//! The shell process from `getrusage` and `proc_pidinfo`, and the `os` values from sysctl.
//! Written against the macOS headers; not yet built or run on macOS.

use std::ffi::{CStr, c_void};
use std::time::Duration;

use super::{ProcessSample, SystemSample};

pub(super) fn system_sample() -> SystemSample {
    // SAFETY: sysconf has no preconditions.
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(0) as u64;
    SystemSample {
        cpu_model: sysctl_string(c"machdep.cpu.brand_string"),
        cpu_speed_mhz: sysctl_value::<u64>(c"hw.cpufrequency").map_or(0, |hz| hz / 1_000_000),
        cpu_count: sysctl_value::<libc::c_int>(c"hw.ncpu").map_or(0, |count| count.max(0) as usize),
        total_memory_bytes: sysctl_value::<u64>(c"hw.memsize").unwrap_or(0),
        free_memory_bytes: sysctl_value::<u32>(c"vm.page_free_count")
            .map_or(0, |pages| u64::from(pages) * page_size),
    }
}

pub(super) fn process_samples() -> Vec<ProcessSample> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: `usage` is a rusage the call fills; zeroed is a valid rusage if it fails.
    let usage = unsafe {
        libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr());
        usage.assume_init()
    };
    let mut task = std::mem::MaybeUninit::<libc::proc_taskinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
    // SAFETY: `task` is a proc_taskinfo of `size` bytes.
    let written = unsafe {
        libc::proc_pidinfo(
            std::process::id() as libc::c_int,
            libc::PROC_PIDTASKINFO,
            0,
            task.as_mut_ptr().cast::<c_void>(),
            size,
        )
    };
    // SAFETY: zeroed is a valid proc_taskinfo whether or not the call wrote it.
    let task = unsafe { task.assume_init() };
    let resident_kb = if written == size {
        task.pti_resident_size / 1_024
    } else {
        0
    };
    vec![ProcessSample {
        pid: std::process::id(),
        started: 0,
        kind: "Browser",
        name: None,
        cpu_time: timeval(usage.ru_utime) + timeval(usage.ru_stime),
        working_set_kb: resident_kb,
        // macOS reports ru_maxrss in bytes.
        peak_working_set_kb: usage.ru_maxrss.max(0) as u64 / 1_024,
        private_kb: None,
    }]
}

fn timeval(time: libc::timeval) -> Duration {
    Duration::from_secs(time.tv_sec.max(0) as u64)
        + Duration::from_micros(time.tv_usec.max(0) as u64)
}

fn sysctl_value<T: Copy + Default>(name: &CStr) -> Option<T> {
    let mut value = T::default();
    let mut size = std::mem::size_of::<T>();
    // SAFETY: `value` is `size` bytes the call may fill.
    let status = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            (&raw mut value).cast::<c_void>(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    (status == 0 && size == std::mem::size_of::<T>()).then_some(value)
}

fn sysctl_string(name: &CStr) -> Option<String> {
    let mut buffer = [0u8; 256];
    let mut size = buffer.len();
    // SAFETY: `buffer` is `size` bytes the call may fill.
    let status = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            buffer.as_mut_ptr().cast::<c_void>(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if status != 0 {
        return None;
    }
    CStr::from_bytes_until_nul(&buffer[..size.min(buffer.len())])
        .ok()
        .map(|text| text.to_string_lossy().into_owned())
}
