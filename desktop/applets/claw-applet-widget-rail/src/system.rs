// SPDX-License-Identifier: GPL-3.0-only

use crate::policy::{self, Scope};
use serde::Deserialize;
use std::{
    fs,
    time::{Duration, Instant},
};
use tokio::{process::Command, time::timeout};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SystemSummary {
    pub cpu_percent: Option<f32>,
    pub memory: Option<Usage>,
    pub storage: Option<Usage>,
    pub network_down_bps: Option<u64>,
    pub network_up_bps: Option<u64>,
    pub fallback: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub used_mb: u64,
    pub total_mb: u64,
}

impl Usage {
    pub fn percent(self) -> u64 {
        if self.total_mb == 0 {
            0
        } else {
            self.used_mb.saturating_mul(100) / self.total_mb
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RawSample {
    cpu: Option<CpuTimes>,
    network: Option<NetworkTotals>,
    sampled_at: Instant,
}

impl Default for RawSample {
    fn default() -> Self {
        Self {
            cpu: None,
            network: None,
            sampled_at: Instant::now(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct CpuTimes {
    idle: u64,
    total: u64,
}

#[derive(Debug, Clone, Copy)]
struct NetworkTotals {
    received: u64,
    transmitted: u64,
}

#[derive(Debug, Deserialize)]
struct Resources {
    #[serde(default)]
    memory: Option<ResourceUsage>,
    #[serde(default)]
    disk: Option<ResourceUsage>,
}

#[derive(Debug, Deserialize)]
struct ResourceUsage {
    total_mb: u64,
    used_mb: u64,
}

pub async fn load(previous: RawSample) -> Result<(SystemSummary, RawSample), String> {
    policy::require("sys.observe", Scope::Wild).await?;

    let current = RawSample {
        cpu: read_cpu_times().ok(),
        network: read_network_totals().ok(),
        sampled_at: Instant::now(),
    };

    let (mut summary, fallback) = match load_cos_resources().await {
        Ok(summary) => (summary, false),
        Err(cos_error) => match load_linux_resources().await {
            Some(summary) => (summary, true),
            None => return Err(cos_error),
        },
    };
    summary.fallback = fallback;
    summary.cpu_percent = cpu_percent(previous.cpu, current.cpu);
    if let (Some(old), Some(new)) = (previous.network, current.network) {
        let elapsed = current
            .sampled_at
            .saturating_duration_since(previous.sampled_at);
        summary.network_down_bps = rate(old.received, new.received, elapsed);
        summary.network_up_bps = rate(old.transmitted, new.transmitted, elapsed);
    }

    Ok((summary, current))
}

async fn load_cos_resources() -> Result<SystemSummary, String> {
    let mut command = Command::new(cos_binary());
    command.args(["sys", "resources"]).kill_on_drop(true);
    let output = timeout(COMMAND_TIMEOUT, command.output())
        .await
        .map_err(|_| "System telemetry timed out.".to_string())?
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                "The ClawOS system service is not installed.".to_string()
            } else {
                format!("Could not start system telemetry: {error}")
            }
        })?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if detail.is_empty() {
            format!("System telemetry exited with {}", output.status)
        } else {
            detail
        });
    }
    parse_resources(&output.stdout)
}

fn parse_resources(raw: &[u8]) -> Result<SystemSummary, String> {
    let resources: Resources = serde_json::from_slice(raw)
        .map_err(|error| format!("System telemetry returned an unreadable response: {error}"))?;
    let memory = resources.memory.map(|usage| Usage {
        used_mb: usage.used_mb,
        total_mb: usage.total_mb,
    });
    let storage = resources.disk.map(|usage| Usage {
        used_mb: usage.used_mb,
        total_mb: usage.total_mb,
    });
    if memory.is_none() && storage.is_none() {
        return Err("System telemetry did not include memory or storage data.".to_string());
    }
    Ok(SystemSummary {
        memory,
        storage,
        ..Default::default()
    })
}

async fn load_linux_resources() -> Option<SystemSummary> {
    let memory = fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|raw| parse_meminfo(&raw));
    let storage = load_df_usage().await;
    (memory.is_some() || storage.is_some()).then_some(SystemSummary {
        memory,
        storage,
        ..Default::default()
    })
}

fn parse_meminfo(raw: &str) -> Option<Usage> {
    let mut total_kb = None;
    let mut available_kb = None;
    for line in raw.lines() {
        if let Some(value) = line.strip_prefix("MemTotal:") {
            total_kb = parse_kb(value);
        } else if let Some(value) = line.strip_prefix("MemAvailable:") {
            available_kb = parse_kb(value);
        }
    }
    let total_kb = total_kb?;
    let available_kb = available_kb?;
    Some(Usage {
        used_mb: total_kb.saturating_sub(available_kb) / 1024,
        total_mb: total_kb / 1024,
    })
}

fn parse_kb(value: &str) -> Option<u64> {
    value.split_whitespace().next()?.parse().ok()
}

async fn load_df_usage() -> Option<Usage> {
    let path = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
    let mut command = Command::new("df");
    command.args(["-Pk", &path]).kill_on_drop(true);
    let output = timeout(COMMAND_TIMEOUT, command.output())
        .await
        .ok()?
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_df(&String::from_utf8_lossy(&output.stdout))
}

fn parse_df(raw: &str) -> Option<Usage> {
    let line = raw.lines().filter(|line| !line.trim().is_empty()).last()?;
    let fields: Vec<&str> = line.split_whitespace().collect();
    if fields.len() < 6 {
        return None;
    }
    let total_kb: u64 = fields[1].parse().ok()?;
    let used_kb: u64 = fields[2].parse().ok()?;
    Some(Usage {
        used_mb: used_kb / 1024,
        total_mb: total_kb / 1024,
    })
}

fn read_cpu_times() -> Result<CpuTimes, String> {
    let raw = fs::read_to_string("/proc/stat").map_err(|error| error.to_string())?;
    parse_cpu_times(&raw).ok_or_else(|| "Could not parse /proc/stat.".to_string())
}

fn parse_cpu_times(raw: &str) -> Option<CpuTimes> {
    let fields: Vec<u64> = raw
        .lines()
        .find(|line| line.starts_with("cpu "))?
        .split_whitespace()
        .skip(1)
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    if fields.len() < 4 {
        return None;
    }
    let idle = fields[3].saturating_add(fields.get(4).copied().unwrap_or_default());
    Some(CpuTimes {
        idle,
        total: fields.into_iter().sum(),
    })
}

fn cpu_percent(previous: Option<CpuTimes>, current: Option<CpuTimes>) -> Option<f32> {
    let (previous, current) = (previous?, current?);
    let total = current.total.saturating_sub(previous.total);
    if total == 0 {
        return None;
    }
    let idle = current.idle.saturating_sub(previous.idle);
    Some((100.0 * (total.saturating_sub(idle)) as f32 / total as f32).clamp(0.0, 100.0))
}

fn rate(previous: u64, current: u64, elapsed: Duration) -> Option<u64> {
    if elapsed.is_zero() {
        return None;
    }
    Some((current.saturating_sub(previous) as f64 / elapsed.as_secs_f64()) as u64)
}

fn read_network_totals() -> Result<NetworkTotals, String> {
    let raw = fs::read_to_string("/proc/net/dev").map_err(|error| error.to_string())?;
    parse_network_totals(&raw).ok_or_else(|| "Could not parse /proc/net/dev.".to_string())
}

fn parse_network_totals(raw: &str) -> Option<NetworkTotals> {
    let mut totals = NetworkTotals {
        received: 0,
        transmitted: 0,
    };
    let mut found = false;
    for line in raw.lines().skip(2) {
        let (interface, values) = line.split_once(':')?;
        if interface.trim() == "lo" {
            continue;
        }
        let fields: Vec<&str> = values.split_whitespace().collect();
        if fields.len() < 16 {
            continue;
        }
        totals.received = totals.received.saturating_add(fields[0].parse().ok()?);
        totals.transmitted = totals.transmitted.saturating_add(fields[8].parse().ok()?);
        found = true;
    }
    found.then_some(totals)
}

fn cos_binary() -> String {
    std::env::var("COS_BIN").unwrap_or_else(|_| "cos".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cos_resources_response() {
        let summary = parse_resources(
            br#"{
                "disk":{"path":"/home/claw","total_mb":100000,"used_mb":42000,"free_mb":58000},
                "memory":{"total_mb":16000,"used_mb":6000,"available_mb":10000}
            }"#,
        )
        .unwrap();
        assert_eq!(summary.memory.unwrap().percent(), 37);
        assert_eq!(summary.storage.unwrap().percent(), 42);
    }

    #[test]
    fn parses_linux_memory_fallback() {
        let usage =
            parse_meminfo("MemTotal:       8192000 kB\nMemAvailable:   2048000 kB\n").unwrap();
        assert_eq!(usage.total_mb, 8000);
        assert_eq!(usage.used_mb, 6000);
        assert_eq!(usage.percent(), 75);
    }

    #[test]
    fn calculates_cpu_delta() {
        let old = parse_cpu_times("cpu  100 0 50 850 0 0 0 0\n").unwrap();
        let new = parse_cpu_times("cpu  140 0 60 900 0 0 0 0\n").unwrap();
        assert_eq!(cpu_percent(Some(old), Some(new)), Some(50.0));
    }

    #[test]
    fn parses_network_and_ignores_loopback() {
        let raw = "Inter-| Receive | Transmit\n face |bytes packets errs drop fifo frame compressed multicast|bytes packets errs drop fifo colls carrier compressed\n  lo: 100 0 0 0 0 0 0 0 100 0 0 0 0 0 0 0\neth0: 2048 0 0 0 0 0 0 0 4096 0 0 0 0 0 0 0\n";
        let totals = parse_network_totals(raw).unwrap();
        assert_eq!(totals.received, 2048);
        assert_eq!(totals.transmitted, 4096);
    }

    #[test]
    fn calculates_network_rate_from_actual_elapsed_time() {
        assert_eq!(rate(1_000, 4_000, Duration::from_millis(1500)), Some(2_000));
        assert_eq!(rate(1_000, 4_000, Duration::ZERO), None);
    }

    #[test]
    fn parses_df_fallback() {
        let usage = parse_df(
            "Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/root 104857600 52428800 52428800 50% /\n",
        )
        .unwrap();
        assert_eq!(usage.total_mb, 102400);
        assert_eq!(usage.used_mb, 51200);
    }
}
