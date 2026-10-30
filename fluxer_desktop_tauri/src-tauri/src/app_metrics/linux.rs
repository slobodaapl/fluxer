// SPDX-License-Identifier: AGPL-3.0-or-later

//! `/proc` and `/sys` readings. WebKitGTK runs its web and network processes as descendants of
//! the shell (through bwrap when sandboxed), typed the way Electron typed Chromium's renderer and
//! network service. Other descendants are not app processes Electron would have listed.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::time::Duration;

use super::{ProcessSample, SystemSample};

const BROWSER: &str = "Browser";
/// `comm` keeps the first 15 bytes of the executable name.
const WEBKIT_PROCESSES: [(&str, &str, Option<&str>); 3] = [
    ("WebKitWebProces", "Tab", None),
    ("WebKitNetworkPr", "Utility", Some("Network Service")),
    ("WebKitGPUProces", "GPU", None),
];

pub(super) fn system_sample() -> SystemSample {
    let cpuinfo = parse_cpuinfo(&fs::read_to_string("/proc/cpuinfo").unwrap_or_default());
    let scaling_mhz = fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/scaling_cur_freq")
        .ok()
        .and_then(|khz| khz.trim().parse::<u64>().ok())
        .map(|khz| khz / 1_000);
    let meminfo = fs::read_to_string("/proc/meminfo").unwrap_or_default();
    SystemSample {
        cpu_model: cpuinfo.model,
        cpu_speed_mhz: scaling_mhz.or(cpuinfo.mhz).unwrap_or(0),
        cpu_count: cpuinfo.count,
        total_memory_bytes: kilobytes(&meminfo, "MemTotal").unwrap_or(0) * 1_024,
        free_memory_bytes: kilobytes(&meminfo, "MemAvailable").unwrap_or(0) * 1_024,
    }
}

pub(super) fn process_samples() -> Vec<ProcessSample> {
    // SAFETY: sysconf has no preconditions.
    let ticks_per_second = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as u64;
    let stats: Vec<Stat> = fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
        .filter_map(|pid| parse_stat(&fs::read_to_string(format!("/proc/{pid}/stat")).ok()?))
        .collect();
    app_processes(&stats, std::process::id())
        .into_iter()
        .map(|(stat, kind, name)| {
            let status =
                fs::read_to_string(format!("/proc/{}/status", stat.pid)).unwrap_or_default();
            ProcessSample {
                pid: stat.pid,
                started: stat.start_ticks,
                kind,
                name,
                cpu_time: Duration::from_secs_f64(stat.cpu_ticks as f64 / ticks_per_second as f64),
                working_set_kb: kilobytes(&status, "VmRSS").unwrap_or(0),
                peak_working_set_kb: kilobytes(&status, "VmHWM").unwrap_or(0),
                private_kb: None,
            }
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Stat {
    pid: u32,
    comm: String,
    ppid: u32,
    /// `utime + stime`.
    cpu_ticks: u64,
    start_ticks: u64,
}

/// `/proc/<pid>/stat`. `comm` may hold spaces and parentheses, so the fields after it are
/// found from the last `)`.
fn parse_stat(text: &str) -> Option<Stat> {
    let (head, rest) = text.rsplit_once(')')?;
    let (pid, comm) = head.split_once(" (")?;
    // Field 3 (state) is index 0 here.
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let field = |number: usize| fields.get(number - 3)?.parse::<u64>().ok();
    Some(Stat {
        pid: pid.trim().parse().ok()?,
        comm: comm.to_owned(),
        ppid: u32::try_from(field(4)?).ok()?,
        cpu_ticks: field(14)? + field(15)?,
        start_ticks: field(22)?,
    })
}

/// The shell process followed by its WebKit descendants, with their Electron process types.
fn app_processes(stats: &[Stat], own_pid: u32) -> Vec<(&Stat, &'static str, Option<&'static str>)> {
    let mut children: HashMap<u32, Vec<&Stat>> = HashMap::new();
    for stat in stats {
        children.entry(stat.ppid).or_default().push(stat);
    }
    let mut processes = Vec::new();
    let Some(own) = stats.iter().find(|stat| stat.pid == own_pid) else {
        return processes;
    };
    processes.push((own, BROWSER, None));
    let mut seen = HashSet::from([own_pid]);
    let mut pending = vec![own_pid];
    while let Some(parent) = pending.pop() {
        for &child in children.get(&parent).into_iter().flatten() {
            if !seen.insert(child.pid) {
                continue;
            }
            pending.push(child.pid);
            if let Some(&(_, kind, name)) = WEBKIT_PROCESSES
                .iter()
                .find(|(comm, _, _)| *comm == child.comm)
            {
                processes.push((child, kind, name));
            }
        }
    }
    processes[1..].sort_by_key(|(stat, _, _)| stat.pid);
    processes
}

/// A `Key:   123 kB` line of `/proc/meminfo` or `/proc/<pid>/status`.
fn kilobytes(text: &str, key: &str) -> Option<u64> {
    text.lines().find_map(|line| {
        let value = line.strip_prefix(key)?.strip_prefix(':')?;
        value.trim().strip_suffix("kB")?.trim().parse().ok()
    })
}

#[derive(Debug, PartialEq)]
struct CpuInfo {
    model: Option<String>,
    mhz: Option<u64>,
    count: usize,
}

/// The processor count, and the first `model name` and `cpu MHz`, of `/proc/cpuinfo`.
fn parse_cpuinfo(text: &str) -> CpuInfo {
    let mut info = CpuInfo {
        model: None,
        mhz: None,
        count: 0,
    };
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "processor" => info.count += 1,
            "model name" if info.model.is_none() => info.model = Some(value.to_owned()),
            "cpu MHz" if info.mhz.is_none() => {
                info.mhz = value.parse::<f64>().ok().map(|mhz| mhz as u64);
            }
            _ => {}
        }
    }
    info
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stat(pid: u32, comm: &str, ppid: u32) -> Stat {
        Stat {
            pid,
            comm: comm.to_owned(),
            ppid,
            cpu_ticks: 0,
            start_ticks: 0,
        }
    }

    #[test]
    fn stat_fields_are_read_after_a_comm_with_spaces_and_parentheses() {
        let text = "4242 (Web Content (x)) S 4100 4242 4100 0 -1 4194560 9000 0 12 0 \
                    250 75 0 0 20 0 31 0 987654 4000000000 51200 18446744073709551615";
        assert_eq!(
            parse_stat(text),
            Some(Stat {
                pid: 4242,
                comm: "Web Content (x)".to_owned(),
                ppid: 4100,
                cpu_ticks: 325,
                start_ticks: 987_654,
            })
        );
        assert_eq!(parse_stat("4242 (truncated) S 1"), None);
    }

    #[test]
    fn the_shell_comes_first_then_its_webkit_descendants_through_bwrap() {
        let stats = [
            stat(1, "systemd", 0),
            stat(100, "fluxer-desktop-t", 1),
            stat(150, "WebKitNetworkPr", 100),
            stat(120, "bwrap", 100),
            stat(121, "bwrap", 120),
            stat(130, "WebKitWebProces", 121),
            stat(140, "xdg-dbus-proxy", 120),
            stat(200, "WebKitWebProces", 1),
        ];
        let listed: Vec<_> = app_processes(&stats, 100)
            .into_iter()
            .map(|(stat, kind, name)| (stat.pid, kind, name))
            .collect();
        assert_eq!(
            listed,
            [
                (100, "Browser", None),
                (130, "Tab", None),
                (150, "Utility", Some("Network Service")),
            ]
        );
        assert!(app_processes(&stats, 999).is_empty());
    }

    #[test]
    fn memory_lines_are_read_in_kilobytes() {
        let status = "Name:\tfluxer\nVmHWM:\t  412340 kB\nVmRSS:\t  398112 kB\nThreads:\t42\n";
        assert_eq!(kilobytes(status, "VmRSS"), Some(398_112));
        assert_eq!(kilobytes(status, "VmHWM"), Some(412_340));
        assert_eq!(kilobytes(status, "VmSwap"), None);
        let meminfo =
            "MemTotal:       65536000 kB\nMemFree:         1000 kB\nMemAvailable:   32768000 kB\n";
        assert_eq!(kilobytes(meminfo, "MemAvailable"), Some(32_768_000));
        assert_eq!(kilobytes(meminfo, "MemTotal"), Some(65_536_000));
    }

    #[test]
    fn cpuinfo_counts_processors_and_takes_the_first_model() {
        let text = "processor\t: 0\nvendor_id\t: AuthenticAMD\nmodel name\t: AMD Ryzen 9 7950X 16-Core Processor\n\
                    cpu MHz\t\t: 3593.250\n\nprocessor\t: 1\nmodel name\t: other\ncpu MHz\t\t: 5000.000\n";
        assert_eq!(
            parse_cpuinfo(text),
            CpuInfo {
                model: Some("AMD Ryzen 9 7950X 16-Core Processor".to_owned()),
                mhz: Some(3_593),
                count: 2,
            }
        );
    }
}
