use serde_json::{json, Value};
use std::env;

use crate::policy::{self, OpType};

/// Built-in system information (replaces Python sys app for basic queries).
pub fn run(command: &str, args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Read).map_err(|v| v.to_string())?;
    match command {
        "info" => cmd_info(),
        "env" => cmd_env(args),
        "resources" => cmd_resources(),
        "uptime" => cmd_uptime(),
        "proc" => cmd_proc(),
        "mounts" => cmd_mounts(),
        "net" => cmd_net(),
        "cgroup" => cmd_cgroup(),
        _ => Err(format!("unknown command: {command}")),
    }
}

fn cmd_info() -> Result<Value, String> {
    Ok(json!({
        "name": "claw-os",
        "version": env::var("COS_VERSION").unwrap_or_else(|_| "unknown".into()),
        "platform": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "hostname": hostname(),
        "pid": std::process::id(),
    }))
}

fn cmd_env(args: &[String]) -> Result<Value, String> {
    let vars: std::collections::BTreeMap<String, String> = if let Some(pattern) = args.first() {
        let pat = pattern.to_lowercase();
        env::vars()
            .filter(|(k, _)| k.to_lowercase().contains(&pat))
            .collect()
    } else {
        env::vars().collect()
    };
    Ok(json!({
        "env": vars,
        "count": vars.len(),
    }))
}

fn cmd_resources() -> Result<Value, String> {
    let mut result = json!({});

    // Disk usage for den
    #[cfg(unix)]
    {
        use std::ffi::CString;
        let workspace = env::var("DEN").unwrap_or_else(|_| "/den".into());
        let c_path = CString::new(workspace.as_bytes())
            .map_err(|e| format!("invalid workspace path for CString: {e}"))?;
        unsafe {
            let mut stat: libc::statvfs = std::mem::zeroed();
            if libc::statvfs(c_path.as_ptr(), &mut stat) == 0 {
                let total = stat.f_blocks as u64 * stat.f_frsize as u64;
                let free = stat.f_bavail as u64 * stat.f_frsize as u64;
                let used = total - free;
                result["disk"] = json!({
                    "path": workspace,
                    "total_mb": total / (1024 * 1024),
                    "used_mb": used / (1024 * 1024),
                    "free_mb": free / (1024 * 1024),
                });
            }
        }
    }

    // Memory from /proc/meminfo
    #[cfg(target_os = "linux")]
    {
        if let Ok(contents) = std::fs::read_to_string("/proc/meminfo") {
            let mut total_kb = 0u64;
            let mut available_kb = 0u64;
            for line in contents.lines() {
                if let Some(val) = line.strip_prefix("MemTotal:") {
                    total_kb = parse_kb(val);
                } else if let Some(val) = line.strip_prefix("MemAvailable:") {
                    available_kb = parse_kb(val);
                }
            }
            let used_kb = total_kb.saturating_sub(available_kb);
            result["memory"] = json!({
                "total_mb": total_kb / 1024,
                "used_mb": used_kb / 1024,
                "available_mb": available_kb / 1024,
            });
        }
    }

    Ok(result)
}

fn cmd_uptime() -> Result<Value, String> {
    #[cfg(target_os = "linux")]
    {
        if let Ok(contents) = std::fs::read_to_string("/proc/uptime") {
            if let Some(secs_str) = contents.split_whitespace().next() {
                if let Ok(secs) = secs_str.parse::<f64>() {
                    let s = secs as u64;
                    let days = s / 86400;
                    let hours = (s % 86400) / 3600;
                    let minutes = (s % 3600) / 60;
                    return Ok(json!({
                        "uptime_seconds": s,
                        "formatted": format!("{days}d {hours}h {minutes}m"),
                    }));
                }
            }
        }
    }
    Err("could not read uptime".into())
}

/// Structured process listing — agent-readable equivalent of /proc/*/stat.
///
/// Returns all running processes with PID, name, state, CPU, and memory.
fn cmd_proc() -> Result<Value, String> {
    #[cfg(target_os = "linux")]
    {
        let mut processes: Vec<Value> = Vec::new();
        if let Ok(entries) = std::fs::read_dir("/proc") {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                // Only numeric directories are PIDs
                let pid: u32 = match name.parse() {
                    Ok(p) => p,
                    Err(_) => continue,
                };

                let stat_path = format!("/proc/{pid}/stat");
                let stat = match std::fs::read_to_string(&stat_path) {
                    Ok(s) => s,
                    Err(_) => continue,
                };

                let fields: Vec<&str> = stat.split_whitespace().collect();
                if fields.len() < 24 {
                    continue;
                }

                // comm is in parens, state is after
                let comm = fields[1].trim_matches(|c| c == '(' || c == ')');
                let state = fields[2];
                let utime = fields[13].parse::<u64>().unwrap_or(0);
                let stime = fields[14].parse::<u64>().unwrap_or(0);
                let vsize = fields[22].parse::<u64>().unwrap_or(0);
                let rss_pages = fields[23].parse::<i64>().unwrap_or(0);

                let state_name = match state {
                    "R" => "running",
                    "S" => "sleeping",
                    "D" => "disk_wait",
                    "Z" => "zombie",
                    "T" => "stopped",
                    "t" => "tracing_stop",
                    "X" | "x" => "dead",
                    _ => state,
                };

                processes.push(json!({
                    "pid": pid,
                    "name": comm,
                    "state": state_name,
                    "cpu_ticks": utime + stime,
                    "cpu_ms": (utime + stime) * 10,
                    "virtual_bytes": vsize,
                    "rss_bytes": (rss_pages as u64) * 4096,
                }));
            }
        }

        processes.sort_by(|a, b| {
            let pa = a["pid"].as_u64().unwrap_or(0);
            let pb = b["pid"].as_u64().unwrap_or(0);
            pa.cmp(&pb)
        });

        let count = processes.len();
        return Ok(json!({
            "processes": processes,
            "count": count,
        }));
    }

    #[cfg(not(target_os = "linux"))]
    {
        Err("sys proc requires Linux /proc filesystem".into())
    }
}

/// Structured mount listing — agent-readable equivalent of /proc/mounts.
fn cmd_mounts() -> Result<Value, String> {
    #[cfg(target_os = "linux")]
    {
        let mut mounts: Vec<Value> = Vec::new();
        if let Ok(content) = std::fs::read_to_string("/proc/mounts") {
            for line in content.lines() {
                let fields: Vec<&str> = line.split_whitespace().collect();
                if fields.len() >= 4 {
                    mounts.push(json!({
                        "device": fields[0],
                        "mount_point": fields[1],
                        "filesystem": fields[2],
                        "options": fields[3],
                    }));
                }
            }
        }

        let count = mounts.len();
        return Ok(json!({
            "mounts": mounts,
            "count": count,
        }));
    }

    #[cfg(not(target_os = "linux"))]
    {
        Err("sys mounts requires Linux /proc filesystem".into())
    }
}

/// Structured network info — agent-readable equivalent of /proc/net/*.
///
/// Returns network interfaces and active TCP connections.
fn cmd_net() -> Result<Value, String> {
    #[cfg(target_os = "linux")]
    {
        let mut result = json!({});

        // Network interfaces from /proc/net/dev
        if let Ok(content) = std::fs::read_to_string("/proc/net/dev") {
            let mut interfaces: Vec<Value> = Vec::new();
            for line in content.lines().skip(2) {
                let parts: Vec<&str> = line.split(':').collect();
                if parts.len() != 2 {
                    continue;
                }
                let iface = parts[0].trim();
                let stats: Vec<u64> = parts[1]
                    .split_whitespace()
                    .filter_map(|s| s.parse().ok())
                    .collect();
                if stats.len() >= 10 {
                    interfaces.push(json!({
                        "name": iface,
                        "rx_bytes": stats[0],
                        "rx_packets": stats[1],
                        "rx_errors": stats[2],
                        "tx_bytes": stats[8],
                        "tx_packets": stats[9],
                        "tx_errors": stats[10],
                    }));
                }
            }
            result["interfaces"] = json!(interfaces);
        }

        // TCP connections from /proc/net/tcp
        if let Ok(content) = std::fs::read_to_string("/proc/net/tcp") {
            let mut connections: Vec<Value> = Vec::new();
            for line in content.lines().skip(1) {
                let fields: Vec<&str> = line.split_whitespace().collect();
                if fields.len() < 4 {
                    continue;
                }
                let state_hex = fields[3];
                let state = match state_hex {
                    "01" => "ESTABLISHED",
                    "02" => "SYN_SENT",
                    "06" => "TIME_WAIT",
                    "0A" => "LISTEN",
                    _ => state_hex,
                };
                connections.push(json!({
                    "local": fields[1],
                    "remote": fields[2],
                    "state": state,
                }));
            }
            result["tcp_connections"] = json!(connections);
            result["tcp_count"] = json!(connections.len());
        }

        return Ok(result);
    }

    #[cfg(not(target_os = "linux"))]
    {
        Err("sys net requires Linux /proc filesystem".into())
    }
}

/// Structured cgroup info — agent-readable equivalent of /sys/fs/cgroup/.
///
/// Returns memory, CPU, and PID limits/usage for the current cgroup.
fn cmd_cgroup() -> Result<Value, String> {
    #[cfg(target_os = "linux")]
    {
        // Find the cgroup for PID 1 (init) or self
        let cgroup_base = "/sys/fs/cgroup";

        let mut result = json!({});

        // Memory
        let mem_max = read_cgroup_val(&format!("{cgroup_base}/memory.max"));
        let mem_current = read_cgroup_val(&format!("{cgroup_base}/memory.current"));
        if mem_current.is_some() {
            result["memory"] = json!({
                "current_bytes": mem_current,
                "max_bytes": mem_max,
                "current_mb": mem_current.map(|v| v / (1024 * 1024)),
                "max_mb": mem_max.map(|v| v / (1024 * 1024)),
            });
        }

        // CPU
        let cpu_stat_path = format!("{cgroup_base}/cpu.stat");
        if let Ok(content) = std::fs::read_to_string(&cpu_stat_path) {
            let mut cpu = json!({});
            for line in content.lines() {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() == 2 {
                    if let Ok(val) = parts[1].parse::<u64>() {
                        cpu[parts[0]] = json!(val);
                    }
                }
            }
            result["cpu"] = cpu;
        }

        // PIDs
        let pids_max = read_cgroup_val(&format!("{cgroup_base}/pids.max"));
        let pids_current = read_cgroup_val(&format!("{cgroup_base}/pids.current"));
        if pids_current.is_some() {
            result["pids"] = json!({
                "current": pids_current,
                "max": pids_max,
            });
        }

        return Ok(result);
    }

    #[cfg(not(target_os = "linux"))]
    {
        Err("sys cgroup requires Linux cgroup v2 filesystem".into())
    }
}

#[cfg(target_os = "linux")]
fn read_cgroup_val(path: &str) -> Option<u64> {
    let content = std::fs::read_to_string(path).ok()?;
    let trimmed = content.trim();
    if trimmed == "max" {
        return None; // "max" means unlimited
    }
    trimmed.parse().ok()
}

#[cfg(target_os = "linux")]
fn parse_kb(val: &str) -> u64 {
    val.trim()
        .trim_end_matches("kB")
        .trim()
        .parse::<u64>()
        .unwrap_or(0)
}

fn hostname() -> String {
    #[cfg(unix)]
    {
        use std::ffi::CStr;
        let mut buf = [0u8; 256];
        unsafe {
            if libc::gethostname(buf.as_mut_ptr() as *mut _, buf.len()) == 0 {
                if let Ok(s) = CStr::from_ptr(buf.as_ptr() as *const _).to_str() {
                    return s.to_string();
                }
            }
        }
    }
    env::var("HOSTNAME")
        .or_else(|_| env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "unknown".into())
}
