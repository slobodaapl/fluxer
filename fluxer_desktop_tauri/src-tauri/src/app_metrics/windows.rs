// SPDX-License-Identifier: AGPL-3.0-or-later

//! The shell process from `GetProcessTimes` and `GetProcessMemoryInfo`, and the `os` values from
//! where libuv reads them: the CentralProcessor registry key, `GetSystemInfo` and
//! `GlobalMemoryStatusEx`.

use std::time::Duration;

use windows::Win32::Foundation::{ERROR_SUCCESS, FILETIME};
use windows::Win32::System::ProcessStatus::{
    GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};
use windows::Win32::System::Registry::{
    HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW,
};
use windows::Win32::System::SystemInformation::{
    GetSystemInfo, GlobalMemoryStatusEx, MEMORYSTATUSEX, SYSTEM_INFO,
};
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
use windows::core::{PCWSTR, w};

use super::{ProcessSample, SystemSample};

const CPU_KEY: PCWSTR = w!("HARDWARE\\DESCRIPTION\\System\\CentralProcessor\\0");
const CPU_NAME_VALUE: PCWSTR = w!("ProcessorNameString");
const CPU_MHZ_VALUE: PCWSTR = w!("~MHz");
/// FILETIME counts 100-nanosecond intervals.
const FILETIME_NANOS: u64 = 100;

pub(super) fn system_sample() -> SystemSample {
    let mut system = SYSTEM_INFO::default();
    // SAFETY: `system` is a SYSTEM_INFO the call fills.
    unsafe { GetSystemInfo(&mut system) };
    let mut memory = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    // SAFETY: `memory` is a MEMORYSTATUSEX with `dwLength` set.
    let memory_read = unsafe { GlobalMemoryStatusEx(&mut memory) }.is_ok();
    SystemSample {
        cpu_model: cpu_name(),
        cpu_speed_mhz: cpu_mhz().map_or(0, u64::from),
        cpu_count: system.dwNumberOfProcessors as usize,
        total_memory_bytes: if memory_read { memory.ullTotalPhys } else { 0 },
        free_memory_bytes: if memory_read { memory.ullAvailPhys } else { 0 },
    }
}

pub(super) fn process_samples() -> Vec<ProcessSample> {
    // SAFETY: the pseudo handle of the current process needs no closing.
    let process = unsafe { GetCurrentProcess() };
    let [mut created, mut exited, mut kernel, mut user] = [FILETIME::default(); 4];
    // SAFETY: the four FILETIMEs outlive the call.
    let times_read =
        unsafe { GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) }
            .is_ok();
    let mut counters = PROCESS_MEMORY_COUNTERS_EX {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    // SAFETY: `counters` is a PROCESS_MEMORY_COUNTERS_EX whose `cb` tells the call so.
    let memory_read = unsafe {
        GetProcessMemoryInfo(
            process,
            (&raw mut counters).cast::<PROCESS_MEMORY_COUNTERS>(),
            counters.cb,
        )
    }
    .is_ok();
    let kb = |bytes: usize| if memory_read { bytes as u64 / 1_024 } else { 0 };
    vec![ProcessSample {
        pid: std::process::id(),
        started: if times_read { filetime(created) } else { 0 },
        kind: "Browser",
        name: None,
        cpu_time: if times_read {
            Duration::from_nanos((filetime(kernel) + filetime(user)) * FILETIME_NANOS)
        } else {
            Duration::ZERO
        },
        working_set_kb: kb(counters.WorkingSetSize),
        peak_working_set_kb: kb(counters.PeakWorkingSetSize),
        private_kb: memory_read.then_some(counters.PrivateUsage as u64 / 1_024),
    }]
}

fn filetime(time: FILETIME) -> u64 {
    (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
}

fn cpu_name() -> Option<String> {
    let mut buffer = [0u16; 256];
    let mut size = std::mem::size_of_val(&buffer) as u32;
    // SAFETY: `buffer` and `size` outlive the call and describe the buffer in bytes.
    let read = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            CPU_KEY,
            CPU_NAME_VALUE,
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&raw mut size),
        )
    };
    if read != ERROR_SUCCESS {
        return None;
    }
    let len = buffer
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..len]))
}

fn cpu_mhz() -> Option<u32> {
    let mut value = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: `value` and `size` outlive the call and describe a DWORD buffer.
    let read = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            CPU_KEY,
            CPU_MHZ_VALUE,
            RRF_RT_REG_DWORD,
            None,
            Some((&raw mut value).cast()),
            Some(&raw mut size),
        )
    };
    (read == ERROR_SUCCESS).then_some(value)
}
