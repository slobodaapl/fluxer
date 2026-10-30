// SPDX-License-Identifier: AGPL-3.0-or-later

//! `get-app-metrics` of IpcHandlers.ts: `getAppMetricsSnapshot` of PlatformInfo.ts, which joined
//! `os.cpus()`, `os.totalmem()`, `os.freemem()` and `app.getAppMetrics()`. The shell process is
//! the `Browser` entry. Linux adds the WebKit processes below it; Windows and macOS report the
//! shell process alone, since WebView2 and WKWebView processes are not enumerated here. Memory is
//! in kilobytes and CPU usage is the percentage of one core since the previous call (0 the first
//! time a process is seen), as Electron reported them.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
use self::linux as platform;
#[cfg(target_os = "macos")]
use self::macos as platform;
#[cfg(windows)]
use self::windows as platform;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::error::BridgeError;

const BYTES_PER_MB: f64 = 1_048_576.0;

/// One process as the platform reads it.
struct ProcessSample {
    pid: u32,
    /// Tells a reused pid from the process that held it: a start time in the platform's unit.
    started: u64,
    kind: &'static str,
    name: Option<&'static str>,
    cpu_time: Duration,
    working_set_kb: u64,
    peak_working_set_kb: u64,
    private_kb: Option<u64>,
}

/// The `os` module values the snapshot carries.
struct SystemSample {
    cpu_model: Option<String>,
    cpu_speed_mhz: u64,
    cpu_count: usize,
    total_memory_bytes: u64,
    free_memory_bytes: u64,
}

/// `AppMetricsSnapshot` of packages/desktop_ipc/src/DesktopBridgeTypes.ts.
#[derive(Serialize)]
pub struct AppMetricsSnapshot {
    #[serde(rename = "cpuInfo")]
    cpu_info: CpuInfo,
    processes: Vec<ProcessMetrics>,
    #[serde(rename = "totalMemoryMB")]
    total_memory_mb: u64,
    #[serde(rename = "freeMemoryMB")]
    free_memory_mb: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CpuInfo {
    model: String,
    speed: u64,
    cores: usize,
    physical_cores: usize,
}

#[derive(Serialize)]
struct ProcessMetrics {
    cpu: CpuUsage,
    memory: MemoryInfo,
    pid: u32,
    #[serde(rename = "type")]
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'static str>,
}

#[derive(Serialize)]
struct CpuUsage {
    #[serde(rename = "percentCPUUsage")]
    percent_cpu_usage: f64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MemoryInfo {
    working_set_size: u64,
    peak_working_set_size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    private_bytes: Option<u64>,
}

struct CpuReading {
    cpu_time: Duration,
    at: Instant,
}

/// The CPU readings of the previous call, keyed by pid and start time.
#[derive(Default)]
pub struct AppMetrics {
    previous: Mutex<HashMap<(u32, u64), CpuReading>>,
}

impl AppMetrics {
    fn snapshot(
        &self,
        system: SystemSample,
        processes: Vec<ProcessSample>,
        now: Instant,
    ) -> AppMetricsSnapshot {
        let mut previous = self.previous.lock().expect("app metrics lock");
        let mut current = HashMap::with_capacity(processes.len());
        let processes = processes
            .into_iter()
            .map(|sample| {
                let key = (sample.pid, sample.started);
                let percent_cpu_usage = previous
                    .get(&key)
                    .map_or(0.0, |reading| cpu_percent(reading, sample.cpu_time, now));
                current.insert(
                    key,
                    CpuReading {
                        cpu_time: sample.cpu_time,
                        at: now,
                    },
                );
                ProcessMetrics {
                    cpu: CpuUsage { percent_cpu_usage },
                    memory: MemoryInfo {
                        working_set_size: sample.working_set_kb,
                        peak_working_set_size: sample.peak_working_set_kb,
                        private_bytes: sample.private_kb,
                    },
                    pid: sample.pid,
                    kind: sample.kind,
                    name: sample.name,
                }
            })
            .collect();
        *previous = current;
        AppMetricsSnapshot {
            cpu_info: CpuInfo {
                model: system.cpu_model.unwrap_or_else(|| "Unknown".to_owned()),
                speed: system.cpu_speed_mhz,
                cores: system.cpu_count,
                physical_cores: system.cpu_count,
            },
            processes,
            total_memory_mb: rounded_mb(system.total_memory_bytes),
            free_memory_mb: rounded_mb(system.free_memory_bytes),
        }
    }
}

fn cpu_percent(previous: &CpuReading, cpu_time: Duration, now: Instant) -> f64 {
    let wall = now.saturating_duration_since(previous.at).as_secs_f64();
    if wall == 0.0 {
        return 0.0;
    }
    cpu_time.saturating_sub(previous.cpu_time).as_secs_f64() / wall * 100.0
}

/// `Math.round(bytes / 1048576)`.
fn rounded_mb(bytes: u64) -> u64 {
    (bytes as f64 / BYTES_PER_MB).round() as u64
}

#[tauri::command]
pub async fn get_app_metrics(
    metrics: tauri::State<'_, Arc<AppMetrics>>,
) -> Result<AppMetricsSnapshot, BridgeError> {
    let metrics = metrics.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let system = platform::system_sample();
        let processes = platform::process_samples();
        metrics.snapshot(system, processes, Instant::now())
    })
    .await
    .map_err(|error| BridgeError::new("Error", error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn system() -> SystemSample {
        SystemSample {
            cpu_model: Some("AMD Ryzen 9 7950X 16-Core Processor".to_owned()),
            cpu_speed_mhz: 4_500,
            cpu_count: 32,
            total_memory_bytes: 64 * 1_048_576 + 524_288,
            free_memory_bytes: 1_048_575,
        }
    }

    fn sample(pid: u32, started: u64, cpu_ms: u64) -> ProcessSample {
        ProcessSample {
            pid,
            started,
            kind: "Browser",
            name: None,
            cpu_time: Duration::from_millis(cpu_ms),
            working_set_kb: 204_800,
            peak_working_set_kb: 307_200,
            private_kb: None,
        }
    }

    fn cpu_of(snapshot: &AppMetricsSnapshot) -> Vec<f64> {
        snapshot
            .processes
            .iter()
            .map(|process| process.cpu.percent_cpu_usage)
            .collect()
    }

    #[test]
    fn the_payload_has_the_electron_shape() {
        let network = ProcessSample {
            kind: "Utility",
            name: Some("Network Service"),
            private_kb: Some(1_024),
            ..sample(11, 5, 0)
        };
        let snapshot = AppMetrics::default().snapshot(
            system(),
            vec![sample(10, 1, 0), network],
            Instant::now(),
        );
        assert_eq!(
            serde_json::to_value(snapshot).unwrap(),
            serde_json::json!({
                "cpuInfo": {
                    "model": "AMD Ryzen 9 7950X 16-Core Processor",
                    "speed": 4_500,
                    "cores": 32,
                    "physicalCores": 32,
                },
                "processes": [
                    {
                        "cpu": {"percentCPUUsage": 0.0},
                        "memory": {"workingSetSize": 204_800, "peakWorkingSetSize": 307_200},
                        "pid": 10,
                        "type": "Browser",
                    },
                    {
                        "cpu": {"percentCPUUsage": 0.0},
                        "memory": {
                            "workingSetSize": 204_800,
                            "peakWorkingSetSize": 307_200,
                            "privateBytes": 1_024,
                        },
                        "pid": 11,
                        "type": "Utility",
                        "name": "Network Service",
                    },
                ],
                "totalMemoryMB": 65,
                "freeMemoryMB": 1,
            })
        );
    }

    #[test]
    fn cpu_usage_is_the_share_of_one_core_since_the_previous_call() {
        let metrics = AppMetrics::default();
        let start = Instant::now();
        let first = metrics.snapshot(system(), vec![sample(10, 1, 1_000)], start);
        assert_eq!(cpu_of(&first), [0.0]);
        let later = start + Duration::from_secs(2);
        let second = metrics.snapshot(
            system(),
            vec![sample(10, 1, 4_000), sample(12, 1, 500)],
            later,
        );
        // 3 s of CPU over 2 s of wall time is one and a half cores; pid 12 is new.
        assert_eq!(cpu_of(&second), [150.0, 0.0]);
    }

    #[test]
    fn a_reused_pid_starts_over() {
        let metrics = AppMetrics::default();
        let start = Instant::now();
        metrics.snapshot(system(), vec![sample(10, 1, 1_000)], start);
        let reused = metrics.snapshot(
            system(),
            vec![sample(10, 2, 9_000)],
            start + Duration::from_secs(1),
        );
        assert_eq!(cpu_of(&reused), [0.0]);
    }

    #[test]
    fn a_missing_cpu_model_reads_as_unknown() {
        let unnamed = SystemSample {
            cpu_model: None,
            ..system()
        };
        let snapshot = AppMetrics::default().snapshot(unnamed, Vec::new(), Instant::now());
        assert_eq!(snapshot.cpu_info.model, "Unknown");
    }
}
