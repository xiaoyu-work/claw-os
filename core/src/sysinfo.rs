use serde_json::{json, Value};
use std::env;

use crate::caps::{require_or_json, Scope, Verb};

/// Built-in system information (replaces Python sys app for basic queries).
///
/// All commands are Linux-only and read from `/proc`, `/sys`, or shell out
/// to standard system tools (journalctl, systemctl, apt, who, dmesg,
/// coredumpctl). On non-Linux platforms the commands that need Linux
/// kernel surfaces return an explicit "requires Linux" error so the
/// shape of the response is always machine-readable.
///
/// Capability: every subcommand is read-only system observation, so the
/// gate is [`Verb::SYS_OBSERVE`] (catalog: "Inspect system state … without
/// changing them", Risk::Low). The previous gate was `Verb::SYS_KERNEL`
/// ("Load kernel modules … reserved for trusted system tools",
/// Risk::Critical), which mis-classified read-only ops like `loadavg` /
/// `resources` / `uptime` as kernel-module loading and made
/// clawd-routed agent jobs fail with `verb-not-granted: sys.kernel` even
/// though [`crate::clawd::system_caps::readonly_task_caps`] already
/// grants `SYS_OBSERVE`. The cron/netfilter/checkpoint sites that
/// *do* mutate kernel state still use `SYS_KERNEL`.
pub fn run(command: &str, args: &[String]) -> Result<Value, String> {
    require_or_json(Verb::SYS_OBSERVE, Scope::wild()).map_err(|v| v.to_string())?;
    match command {
        // identity / environment
        "info" => cmd_info(),
        "env" => cmd_env(args),
        "uptime" => cmd_uptime(),
        "who" => cmd_who(),
        "desktop" => cmd_desktop(),

        // resources / load
        "resources" => cmd_resources(),
        "loadavg" => cmd_loadavg(),
        "sensors" => cmd_sensors(),
        "cgroup" => cmd_cgroup(),

        // processes
        "proc" => cmd_proc(),
        "top" => cmd_top(args),
        "threads" => cmd_threads(args),
        "port" => cmd_port(args),

        // network
        "net" => cmd_net(),
        "net_rate" => cmd_net_rate(args),

        // storage
        "mounts" => cmd_mounts(),
        "disk_io" => cmd_disk_io(args),
        "largest_files" => cmd_largest_files(args),

        // logs
        "journal" => cmd_journal(args),
        "dmesg" => cmd_dmesg(args),

        // systemd
        "services" => cmd_services(args),
        "failed_units" => cmd_failed_units(),
        "coredumps" => cmd_coredumps(args),

        // packages
        "pkg_updates" => cmd_pkg_updates(),

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
    let include_secrets = args.iter().any(|a| a == "--include-secrets");

    // Build the raw map first, then redact unless the caller opted
    // in via --include-secrets. Without this, any caller with the
    // (broad) `sysinfo` capability could exfiltrate `OPENAI_API_KEY`,
    // `AWS_SECRET_ACCESS_KEY`, GitHub PATs, etc. inherited from the
    // parent shell. The redaction policy mirrors the cred-name
    // patterns elsewhere in the codebase.
    let raw: std::collections::BTreeMap<String, String> =
        if let Some(pattern) = args.iter().find(|a| !a.starts_with("--")) {
            let pat = pattern.to_lowercase();
            env::vars()
                .filter(|(k, _)| k.to_lowercase().contains(&pat))
                .collect()
        } else {
            env::vars().collect()
        };

    let mut redacted = 0_usize;
    let vars: std::collections::BTreeMap<String, String> = raw
        .into_iter()
        .map(|(k, v)| {
            if include_secrets || !looks_like_secret_key(&k) {
                (k, v)
            } else {
                redacted += 1;
                (k, "***REDACTED***".into())
            }
        })
        .collect();
    Ok(json!({
        "env": vars,
        "count": vars.len(),
        "redacted_count": redacted,
        "include_secrets": include_secrets,
    }))
}

/// Heuristic match for environment variable NAMES that almost
/// certainly hold credentials. Substring (case-insensitive) hits on
/// well-known suffixes/prefixes. Conservatively over-redacts: a
/// developer wanting the unredacted view can re-run with
/// `--include-secrets`.
fn looks_like_secret_key(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    const SUFFIX_HITS: &[&str] = &[
        "_API_KEY",
        "_TOKEN",
        "_SECRET",
        "_PASSWORD",
        "_PASS",
        "_PRIVATE_KEY",
        "_CREDENTIALS",
        "_AUTH",
    ];
    const FULL_HITS: &[&str] = &[
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "GITHUB_TOKEN",
        "GOOGLE_API_KEY",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
        "AWS_ACCESS_KEY_ID",
        "GH_TOKEN",
    ];
    const PREFIX_HITS: &[&str] = &[
        "OPENAI_",
        "ANTHROPIC_",
        "GOOGLE_",
        "AWS_",
        "AZURE_",
        "GITHUB_",
        "GH_",
        "STRIPE_",
        "TWILIO_",
    ];
    if FULL_HITS.iter().any(|n| upper == *n) {
        return true;
    }
    if SUFFIX_HITS.iter().any(|s| upper.ends_with(s)) {
        return true;
    }
    if PREFIX_HITS.iter().any(|p| upper.starts_with(p))
        && (upper.contains("KEY") || upper.contains("SECRET") || upper.contains("TOKEN"))
    {
        return true;
    }
    false
}

fn cmd_resources() -> Result<Value, String> {
    let mut result = json!({});

    // Disk usage for the agent home (writable workspace).
    #[cfg(unix)]
    {
        use std::ffi::CString;
        let workspace = env::var("COS_HOME")
            .or_else(|_| env::var("HOME"))
            .unwrap_or_else(|_| "/root".into());
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

                let (comm, fields) = match parse_proc_stat(&stat) {
                    Some(t) => t,
                    None => continue,
                };
                // Need state(0), utime(11), stime(12), vsize(20), rss(21).
                if fields.len() < 22 {
                    continue;
                }

                let state = fields[0];
                let utime = fields[11].parse::<u64>().unwrap_or(0);
                let stime = fields[12].parse::<u64>().unwrap_or(0);
                let vsize = fields[20].parse::<u64>().unwrap_or(0);
                let rss_pages = fields[21].parse::<i64>().unwrap_or(0);

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

// ============================================================================
// New commands (P0 / P1 / P2 system-introspection coverage).
//
// All Linux-specific paths gated with `#[cfg(target_os = "linux")]`. The
// fallback arms return an explicit "requires Linux ..." error so the agent
// sees a structured failure rather than a panic.
// ============================================================================

fn cmd_desktop() -> Result<Value, String> {
    let pick = |k: &str| env::var(k).ok();
    Ok(json!({
        "desktop": pick("XDG_CURRENT_DESKTOP"),
        "session_type": pick("XDG_SESSION_TYPE"),
        "session_desktop": pick("XDG_SESSION_DESKTOP"),
        "wayland_display": pick("WAYLAND_DISPLAY"),
        "x_display": pick("DISPLAY"),
        "seat": pick("XDG_SEAT"),
        "vt": pick("XDG_VTNR"),
        "user": pick("USER"),
        "runtime_dir": pick("XDG_RUNTIME_DIR"),
    }))
}

fn cmd_loadavg() -> Result<Value, String> {
    #[cfg(target_os = "linux")]
    {
        let s = std::fs::read_to_string("/proc/loadavg")
            .map_err(|e| format!("read /proc/loadavg: {e}"))?;
        let parts: Vec<&str> = s.split_whitespace().collect();
        if parts.len() < 5 {
            return Err("malformed /proc/loadavg".into());
        }
        let load1: f64 = parts[0].parse().unwrap_or(0.0);
        let load5: f64 = parts[1].parse().unwrap_or(0.0);
        let load15: f64 = parts[2].parse().unwrap_or(0.0);
        let tasks: Vec<&str> = parts[3].split('/').collect();
        let running: u64 = tasks.first().and_then(|x| x.parse().ok()).unwrap_or(0);
        let total: u64 = tasks.get(1).and_then(|x| x.parse().ok()).unwrap_or(0);
        let last_pid: u64 = parts[4].parse().unwrap_or(0);
        let cores = num_cores();
        return Ok(json!({
            "load_1min": load1,
            "load_5min": load5,
            "load_15min": load15,
            "running_tasks": running,
            "total_tasks": total,
            "last_pid": last_pid,
            "cores": cores,
            "load_per_core_1min": (load1 / cores.max(1) as f64 * 100.0).round() / 100.0,
        }));
    }
    #[cfg(not(target_os = "linux"))]
    Err("sys loadavg requires Linux /proc filesystem".into())
}

fn cmd_top(args: &[String]) -> Result<Value, String> {
    #[cfg(target_os = "linux")]
    {
        let top_n: usize = read_arg(args, "--top")
            .and_then(|s| s.parse().ok())
            .unwrap_or(10);
        let by = read_arg(args, "--by").unwrap_or("cpu").to_string();
        let interval_ms: u64 = read_arg(args, "--interval")
            .and_then(|s| s.parse().ok())
            .unwrap_or(500);

        let snap1 = sample_proc_stats()?;
        std::thread::sleep(std::time::Duration::from_millis(interval_ms));
        let snap2 = sample_proc_stats()?;

        let clk = clk_tck().max(1);
        let dt_secs = interval_ms as f64 / 1000.0;

        let mut rows: Vec<Value> = Vec::new();
        for (pid, s2) in &snap2 {
            if let Some(s1) = snap1.get(pid) {
                let cpu_ticks_delta = (s2.utime + s2.stime).saturating_sub(s1.utime + s1.stime);
                let cpu_pct = (cpu_ticks_delta as f64 / clk as f64 / dt_secs) * 100.0;
                rows.push(json!({
                    "pid": pid,
                    "name": s2.comm,
                    "state": state_name(&s2.state),
                    "cpu_percent": (cpu_pct * 10.0).round() / 10.0,
                    "rss_mb": s2.rss_bytes / (1024 * 1024),
                    "rss_bytes": s2.rss_bytes,
                }));
            }
        }

        rows.sort_by(|a, b| {
            let av = if by == "mem" {
                a["rss_bytes"].as_u64().unwrap_or(0) as f64
            } else {
                a["cpu_percent"].as_f64().unwrap_or(0.0) * 1_000_000.0
            };
            let bv = if by == "mem" {
                b["rss_bytes"].as_u64().unwrap_or(0) as f64
            } else {
                b["cpu_percent"].as_f64().unwrap_or(0.0) * 1_000_000.0
            };
            bv.partial_cmp(&av).unwrap_or(std::cmp::Ordering::Equal)
        });
        rows.truncate(top_n);

        return Ok(json!({
            "interval_ms": interval_ms,
            "by": by,
            "top": top_n,
            "processes": rows,
        }));
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        Err("sys top requires Linux /proc filesystem".into())
    }
}

fn cmd_threads(args: &[String]) -> Result<Value, String> {
    #[cfg(target_os = "linux")]
    {
        let pid: u32 = args
            .first()
            .ok_or("usage: threads <pid>")?
            .parse()
            .map_err(|_| "invalid pid".to_string())?;
        let task_dir = format!("/proc/{pid}/task");
        let entries =
            std::fs::read_dir(&task_dir).map_err(|e| format!("read {task_dir}: {e}"))?;
        let mut threads: Vec<Value> = Vec::new();
        for entry in entries.flatten() {
            let tid_name = entry.file_name().to_string_lossy().to_string();
            let tid: u32 = match tid_name.parse() {
                Ok(t) => t,
                Err(_) => continue,
            };
            let stat_path = format!("/proc/{pid}/task/{tid}/stat");
            let stat = match std::fs::read_to_string(&stat_path) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let (comm, fields) = match parse_proc_stat(&stat) {
                Some(t) => t,
                None => continue,
            };
            if fields.len() < 13 {
                continue;
            }
            let state = fields[0];
            let utime = fields[11].parse::<u64>().unwrap_or(0);
            let stime = fields[12].parse::<u64>().unwrap_or(0);
            threads.push(json!({
                "tid": tid,
                "name": comm,
                "state": state_name(state),
                "cpu_ticks": utime + stime,
                "cpu_ms": (utime + stime) * 1000 / clk_tck().max(1),
            }));
        }
        threads.sort_by(|a, b| {
            a["tid"]
                .as_u64()
                .unwrap_or(0)
                .cmp(&b["tid"].as_u64().unwrap_or(0))
        });
        let count = threads.len();
        return Ok(json!({
            "pid": pid,
            "thread_count": count,
            "threads": threads,
        }));
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        Err("sys threads requires Linux /proc filesystem".into())
    }
}

fn cmd_port(args: &[String]) -> Result<Value, String> {
    #[cfg(target_os = "linux")]
    {
        let port: u16 = args
            .first()
            .ok_or("usage: port <port>")?
            .parse()
            .map_err(|_| "invalid port number".to_string())?;
        let port_hex = format!("{port:04X}");

        struct SockHit {
            proto: &'static str,
            local: String,
            remote: String,
            state: &'static str,
            inode: u64,
        }
        let mut hits: Vec<SockHit> = Vec::new();
        for (path, proto) in &[
            ("/proc/net/tcp", "tcp"),
            ("/proc/net/tcp6", "tcp6"),
            ("/proc/net/udp", "udp"),
            ("/proc/net/udp6", "udp6"),
        ] {
            let content = match std::fs::read_to_string(path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            for line in content.lines().skip(1) {
                let f: Vec<&str> = line.split_whitespace().collect();
                if f.len() < 10 {
                    continue;
                }
                let local = f[1];
                if !local.ends_with(&format!(":{port_hex}")) {
                    continue;
                }
                let state_hex = f[3];
                let state = match state_hex {
                    "01" => "ESTABLISHED",
                    "02" => "SYN_SENT",
                    "06" => "TIME_WAIT",
                    "07" => "CLOSE",
                    "0A" => "LISTEN",
                    _ => "UNKNOWN",
                };
                let inode: u64 = f[9].parse().unwrap_or(0);
                hits.push(SockHit {
                    proto,
                    local: local.to_string(),
                    remote: f[2].to_string(),
                    state,
                    inode,
                });
            }
        }

        let mut matches: Vec<Value> = Vec::new();
        for hit in &hits {
            let socket_link = format!("socket:[{}]", hit.inode);
            let mut found: Vec<Value> = Vec::new();
            if let Ok(proc_dir) = std::fs::read_dir("/proc") {
                for pid_entry in proc_dir.flatten() {
                    let pid_name = pid_entry.file_name().to_string_lossy().to_string();
                    let pid: u32 = match pid_name.parse() {
                        Ok(p) => p,
                        Err(_) => continue,
                    };
                    let fd_dir = format!("/proc/{pid}/fd");
                    let fds = match std::fs::read_dir(&fd_dir) {
                        Ok(d) => d,
                        Err(_) => continue,
                    };
                    let mut matched = false;
                    for fd in fds.flatten() {
                        if let Ok(target) = std::fs::read_link(fd.path()) {
                            if target.to_string_lossy() == socket_link {
                                matched = true;
                                break;
                            }
                        }
                    }
                    if matched {
                        let comm = std::fs::read_to_string(format!("/proc/{pid}/comm"))
                            .unwrap_or_default()
                            .trim()
                            .to_string();
                        let cmdline = std::fs::read_to_string(format!("/proc/{pid}/cmdline"))
                            .unwrap_or_default()
                            .replace('\0', " ")
                            .trim()
                            .to_string();
                        found.push(json!({"pid": pid, "name": comm, "cmdline": cmdline}));
                    }
                }
            }
            matches.push(json!({
                "protocol": hit.proto,
                "local": hit.local,
                "remote": hit.remote,
                "state": hit.state,
                "inode": hit.inode,
                "processes": found,
            }));
        }

        return Ok(json!({"port": port, "matches": matches, "count": hits.len()}));
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        Err("sys port requires Linux /proc filesystem".into())
    }
}

fn cmd_sensors() -> Result<Value, String> {
    #[cfg(target_os = "linux")]
    {
        let mut result = json!({});

        // /sys/class/power_supply (batteries + AC adapters)
        let mut batteries: Vec<Value> = Vec::new();
        let mut ac_adapters: Vec<Value> = Vec::new();
        if let Ok(entries) = std::fs::read_dir("/sys/class/power_supply") {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                let base = entry.path();
                let typ = read_str_trim(&base.join("type"));
                match typ.as_deref() {
                    Some("Battery") => {
                        let cap = read_u64(&base.join("capacity"));
                        let energy_now = read_u64(&base.join("energy_now"));
                        let energy_full = read_u64(&base.join("energy_full"));
                        let power_now = read_u64(&base.join("power_now"));
                        let status = read_str_trim(&base.join("status"));
                        // Estimated runtime (only meaningful while discharging).
                        let runtime_min = match (status.as_deref(), energy_now, power_now) {
                            (Some("Discharging"), Some(e), Some(p)) if p > 0 => {
                                Some(e * 60 / p)
                            }
                            _ => None,
                        };
                        batteries.push(json!({
                            "name": name,
                            "status": status,
                            "capacity_percent": cap,
                            "energy_now_uwh": energy_now,
                            "energy_full_uwh": energy_full,
                            "power_now_uw": power_now,
                            "voltage_now_uv": read_u64(&base.join("voltage_now")),
                            "cycle_count": read_u64(&base.join("cycle_count")),
                            "technology": read_str_trim(&base.join("technology")),
                            "model_name": read_str_trim(&base.join("model_name")),
                            "estimated_runtime_minutes": runtime_min,
                        }));
                    }
                    Some("Mains") | Some("USB") | Some("UPS") => {
                        ac_adapters.push(json!({
                            "name": name,
                            "type": typ,
                            "online": read_u64(&base.join("online")).map(|v| v == 1),
                        }));
                    }
                    _ => {}
                }
            }
        }
        if !batteries.is_empty() {
            result["battery"] = json!(batteries);
        }
        if !ac_adapters.is_empty() {
            result["ac_adapter"] = json!(ac_adapters);
        }

        // /sys/class/thermal/thermal_zone*
        let mut thermal: Vec<Value> = Vec::new();
        if let Ok(entries) = std::fs::read_dir("/sys/class/thermal") {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if !name.starts_with("thermal_zone") {
                    continue;
                }
                let base = entry.path();
                let typ = read_str_trim(&base.join("type"));
                let temp_millideg = read_u64(&base.join("temp"));
                thermal.push(json!({
                    "zone": name,
                    "type": typ,
                    "temp_celsius": temp_millideg.map(|v| (v as f64 / 1000.0 * 10.0).round() / 10.0),
                }));
            }
        }
        if !thermal.is_empty() {
            result["thermal"] = json!(thermal);
        }

        // /sys/class/hwmon/* (fans + extra temps)
        let mut hwmon_devices: Vec<Value> = Vec::new();
        if let Ok(entries) = std::fs::read_dir("/sys/class/hwmon") {
            for entry in entries.flatten() {
                let base = entry.path();
                let dev_name = read_str_trim(&base.join("name"));
                let mut readings: Vec<Value> = Vec::new();
                if let Ok(items) = std::fs::read_dir(&base) {
                    for item in items.flatten() {
                        let fname = item.file_name().to_string_lossy().to_string();
                        if let Some(idx) = fname
                            .strip_prefix("fan")
                            .and_then(|x| x.strip_suffix("_input"))
                        {
                            if let Some(rpm) = read_u64(&item.path()) {
                                let label = read_str_trim(&base.join(format!("fan{idx}_label")));
                                readings.push(json!({
                                    "kind": "fan",
                                    "index": idx,
                                    "label": label,
                                    "rpm": rpm,
                                }));
                            }
                        } else if let Some(idx) = fname
                            .strip_prefix("temp")
                            .and_then(|x| x.strip_suffix("_input"))
                        {
                            if let Some(milli) = read_u64(&item.path()) {
                                let label =
                                    read_str_trim(&base.join(format!("temp{idx}_label")));
                                readings.push(json!({
                                    "kind": "temp",
                                    "index": idx,
                                    "label": label,
                                    "celsius": (milli as f64 / 1000.0 * 10.0).round() / 10.0,
                                }));
                            }
                        }
                    }
                }
                if !readings.is_empty() {
                    hwmon_devices.push(json!({
                        "name": dev_name,
                        "readings": readings,
                    }));
                }
            }
        }
        if !hwmon_devices.is_empty() {
            result["hwmon"] = json!(hwmon_devices);
        }

        return Ok(result);
    }
    #[cfg(not(target_os = "linux"))]
    Err("sys sensors requires Linux /sys filesystem".into())
}

fn cmd_journal(args: &[String]) -> Result<Value, String> {
    #[cfg(target_os = "linux")]
    {
        let lines: u64 = read_arg(args, "--lines")
            .and_then(|s| s.parse().ok())
            .unwrap_or(100);
        let mut cmd_args: Vec<String> = vec![
            "-o".into(),
            "json".into(),
            "--no-pager".into(),
            "-n".into(),
            lines.to_string(),
        ];
        if let Some(unit) = read_arg(args, "--unit") {
            cmd_args.push("-u".into());
            cmd_args.push(unit.to_string());
        }
        if let Some(since) = read_arg(args, "--since") {
            cmd_args.push("--since".into());
            cmd_args.push(since.to_string());
        }
        if let Some(prio) = read_arg(args, "--priority") {
            cmd_args.push("-p".into());
            cmd_args.push(prio.to_string());
        }
        if has_flag(args, "--kernel") {
            cmd_args.push("-k".into());
        }
        let refs: Vec<&str> = cmd_args.iter().map(|s| s.as_str()).collect();
        let stdout = run_cmd("journalctl", &refs)?;

        let mut entries: Vec<Value> = Vec::new();
        for line in stdout.lines() {
            if line.is_empty() {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<Value>(line) {
                entries.push(json!({
                    "timestamp_us": v.get("__REALTIME_TIMESTAMP"),
                    "unit": v.get("_SYSTEMD_UNIT").or_else(|| v.get("UNIT")),
                    "priority": v.get("PRIORITY"),
                    "pid": v.get("_PID"),
                    "comm": v.get("_COMM"),
                    "exe": v.get("_EXE"),
                    "message": v.get("MESSAGE"),
                }));
            }
        }
        let count = entries.len();
        return Ok(json!({"entries": entries, "count": count}));
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        Err("sys journal requires Linux systemd".into())
    }
}

fn cmd_dmesg(args: &[String]) -> Result<Value, String> {
    #[cfg(target_os = "linux")]
    {
        let lines: usize = read_arg(args, "--lines")
            .and_then(|s| s.parse().ok())
            .unwrap_or(100);

        // Newer util-linux supports --json
        if let Ok(stdout) = run_cmd("dmesg", &["--json", "--no-pager"]) {
            if let Ok(v) = serde_json::from_str::<Value>(&stdout) {
                if let Some(arr) = v.get("dmesg").and_then(|a| a.as_array()) {
                    let n = arr.len();
                    let start = n.saturating_sub(lines);
                    let taken: Vec<Value> = arr[start..].to_vec();
                    return Ok(json!({
                        "entries": taken,
                        "count": n.min(lines),
                        "format": "json",
                    }));
                }
            }
        }

        // Fallback: best-effort text
        let stdout = run_cmd(
            "dmesg",
            &["--color=never", "--time-format=iso", "--no-pager"],
        )?;
        let all: Vec<&str> = stdout.lines().collect();
        let start = all.len().saturating_sub(lines);
        let entries: Vec<Value> = all[start..]
            .iter()
            .map(|l| json!({"raw": l.trim()}))
            .collect();
        let count = entries.len();
        return Ok(json!({"entries": entries, "count": count, "format": "raw"}));
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        Err("sys dmesg requires Linux kernel".into())
    }
}

fn cmd_services(args: &[String]) -> Result<Value, String> {
    #[cfg(target_os = "linux")]
    {
        let mut cmd_args: Vec<String> = vec![
            "list-units".into(),
            "--all".into(),
            "--no-pager".into(),
            "--no-legend".into(),
            "--output=json".into(),
            "--plain".into(),
        ];
        if has_flag(args, "--failed-only") || has_flag(args, "--failed") {
            cmd_args.push("--state=failed".into());
        }
        if let Some(t) = read_arg(args, "--type") {
            cmd_args.push("--type".into());
            cmd_args.push(t.into());
        }
        if let Some(state) = read_arg(args, "--state") {
            cmd_args.push("--state".into());
            cmd_args.push(state.into());
        }
        let refs: Vec<&str> = cmd_args.iter().map(|s| s.as_str()).collect();
        let stdout = run_cmd("systemctl", &refs)?;
        let v: Value = serde_json::from_str(stdout.trim())
            .map_err(|e| format!("parse systemctl json: {e}"))?;
        let count = v.as_array().map(|a| a.len()).unwrap_or(0);
        return Ok(json!({"units": v, "count": count}));
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        Err("sys services requires Linux systemd".into())
    }
}

fn cmd_failed_units() -> Result<Value, String> {
    cmd_services(&["--failed-only".into()])
}

fn cmd_coredumps(args: &[String]) -> Result<Value, String> {
    #[cfg(target_os = "linux")]
    {
        let lines: usize = read_arg(args, "--lines")
            .and_then(|s| s.parse().ok())
            .unwrap_or(50);

        if let Ok(stdout) = run_cmd(
            "coredumpctl",
            &[
                "list",
                "--no-pager",
                "--no-legend",
                "--json=short",
                "--reverse",
            ],
        ) {
            if let Ok(v) = serde_json::from_str::<Value>(stdout.trim()) {
                if let Some(arr) = v.as_array() {
                    let trimmed: Vec<Value> = arr.iter().take(lines).cloned().collect();
                    let count = trimmed.len();
                    return Ok(json!({"coredumps": trimmed, "count": count, "format": "json"}));
                }
            }
        }

        let stdout = run_cmd(
            "coredumpctl",
            &["list", "--no-pager", "--no-legend", "--reverse"],
        )?;
        let entries: Vec<Value> = stdout
            .lines()
            .take(lines)
            .map(|l| json!({"raw": l.trim()}))
            .collect();
        let count = entries.len();
        return Ok(json!({"coredumps": entries, "count": count, "format": "raw"}));
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        Err("sys coredumps requires Linux systemd-coredump".into())
    }
}

fn cmd_who() -> Result<Value, String> {
    #[cfg(target_os = "linux")]
    {
        let stdout = run_cmd("who", &["-H"])?;
        let mut sessions: Vec<Value> = Vec::new();
        for (i, line) in stdout.lines().enumerate() {
            // First line is the header from `-H`
            if i == 0 {
                continue;
            }
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 4 {
                continue;
            }
            sessions.push(json!({
                "user": f[0],
                "tty": f[1],
                "login_date": f[2],
                "login_time": f[3],
                "from": f.get(4).copied(),
            }));
        }
        let count = sessions.len();
        return Ok(json!({"sessions": sessions, "count": count}));
    }
    #[cfg(not(target_os = "linux"))]
    Err("sys who requires Linux utmp".into())
}

fn cmd_pkg_updates() -> Result<Value, String> {
    #[cfg(target_os = "linux")]
    {
        let stdout = run_cmd("apt", &["list", "--upgradable"])?;
        let mut packages: Vec<Value> = Vec::new();
        for line in stdout.lines().skip(1) {
            if line.is_empty() {
                continue;
            }
            if let Some(slash) = line.find('/') {
                let pkg = &line[..slash];
                let rest = &line[slash + 1..];
                let mut parts = rest.split_whitespace();
                let codename = parts.next().unwrap_or("");
                let version = parts.next().unwrap_or("");
                let arch = parts.next().unwrap_or("");
                let old_version = if let Some(start) = line.find("[upgradable from:") {
                    line[start + "[upgradable from:".len()..]
                        .trim()
                        .trim_end_matches(']')
                        .trim()
                        .to_string()
                } else {
                    String::new()
                };
                packages.push(json!({
                    "package": pkg,
                    "codename": codename,
                    "new_version": version,
                    "arch": arch,
                    "old_version": old_version,
                }));
            }
        }
        let count = packages.len();
        return Ok(json!({"upgradable": packages, "count": count}));
    }
    #[cfg(not(target_os = "linux"))]
    Err("sys pkg_updates requires Debian-based Linux with apt".into())
}

fn cmd_disk_io(args: &[String]) -> Result<Value, String> {
    #[cfg(target_os = "linux")]
    {
        let interval_ms: u64 = read_arg(args, "--interval")
            .and_then(|s| s.parse().ok())
            .unwrap_or(500);
        let snap1 = read_diskstats()?;
        std::thread::sleep(std::time::Duration::from_millis(interval_ms));
        let snap2 = read_diskstats()?;
        let dt = (interval_ms as f64 / 1000.0).max(0.001);
        let mut disks: Vec<Value> = Vec::new();
        for (name, s2) in &snap2 {
            if let Some(s1) = snap1.get(name) {
                let read_bytes = (s2.read_sectors.saturating_sub(s1.read_sectors)) * 512;
                let write_bytes = (s2.write_sectors.saturating_sub(s1.write_sectors)) * 512;
                disks.push(json!({
                    "device": name,
                    "read_kb_per_sec": ((read_bytes as f64 / dt / 1024.0) * 10.0).round() / 10.0,
                    "write_kb_per_sec": ((write_bytes as f64 / dt / 1024.0) * 10.0).round() / 10.0,
                    "reads_delta": s2.reads.saturating_sub(s1.reads),
                    "writes_delta": s2.writes.saturating_sub(s1.writes),
                }));
            }
        }
        disks.sort_by(|a, b| {
            let av = a["read_kb_per_sec"].as_f64().unwrap_or(0.0)
                + a["write_kb_per_sec"].as_f64().unwrap_or(0.0);
            let bv = b["read_kb_per_sec"].as_f64().unwrap_or(0.0)
                + b["write_kb_per_sec"].as_f64().unwrap_or(0.0);
            bv.partial_cmp(&av).unwrap_or(std::cmp::Ordering::Equal)
        });
        return Ok(json!({"interval_ms": interval_ms, "disks": disks}));
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        Err("sys disk_io requires Linux /proc filesystem".into())
    }
}

fn cmd_net_rate(args: &[String]) -> Result<Value, String> {
    #[cfg(target_os = "linux")]
    {
        let interval_ms: u64 = read_arg(args, "--interval")
            .and_then(|s| s.parse().ok())
            .unwrap_or(500);
        let snap1 = read_net_dev()?;
        std::thread::sleep(std::time::Duration::from_millis(interval_ms));
        let snap2 = read_net_dev()?;
        let dt = (interval_ms as f64 / 1000.0).max(0.001);
        let mut ifaces: Vec<Value> = Vec::new();
        for (name, s2) in &snap2 {
            if let Some(s1) = snap1.get(name) {
                let rx = s2.rx_bytes.saturating_sub(s1.rx_bytes);
                let tx = s2.tx_bytes.saturating_sub(s1.tx_bytes);
                ifaces.push(json!({
                    "name": name,
                    "rx_kb_per_sec": ((rx as f64 / dt / 1024.0) * 10.0).round() / 10.0,
                    "tx_kb_per_sec": ((tx as f64 / dt / 1024.0) * 10.0).round() / 10.0,
                }));
            }
        }
        ifaces.sort_by(|a, b| {
            let av = a["rx_kb_per_sec"].as_f64().unwrap_or(0.0)
                + a["tx_kb_per_sec"].as_f64().unwrap_or(0.0);
            let bv = b["rx_kb_per_sec"].as_f64().unwrap_or(0.0)
                + b["tx_kb_per_sec"].as_f64().unwrap_or(0.0);
            bv.partial_cmp(&av).unwrap_or(std::cmp::Ordering::Equal)
        });
        return Ok(json!({"interval_ms": interval_ms, "interfaces": ifaces}));
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        Err("sys net_rate requires Linux /proc filesystem".into())
    }
}

fn cmd_largest_files(args: &[String]) -> Result<Value, String> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::MetadataExt;
        let path: String = args
            .iter()
            .find(|a| !a.starts_with("--"))
            .cloned()
            .unwrap_or_else(|| "/".to_string());
        let top: usize = read_arg(args, "--top")
            .and_then(|s| s.parse().ok())
            .unwrap_or(20);
        let min_mb: u64 = read_arg(args, "--min-mb")
            .and_then(|s| s.parse().ok())
            .unwrap_or(50);
        let min_bytes = min_mb * 1024 * 1024;

        let root_md = std::fs::metadata(&path).map_err(|e| format!("stat {path}: {e}"))?;
        let root_dev = root_md.dev();

        let mut heap: std::collections::BinaryHeap<std::cmp::Reverse<(u64, String)>> =
            std::collections::BinaryHeap::new();
        let mut stack: Vec<std::path::PathBuf> = vec![std::path::PathBuf::from(&path)];
        let mut scanned_dirs = 0usize;
        let mut scanned_files = 0usize;
        while let Some(dir) = stack.pop() {
            let read_dir = match std::fs::read_dir(&dir) {
                Ok(r) => r,
                Err(_) => continue,
            };
            scanned_dirs += 1;
            for entry in read_dir.flatten() {
                let p = entry.path();
                let md = match entry.metadata() {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                if md.dev() != root_dev {
                    continue;
                }
                if md.is_dir() {
                    stack.push(p);
                } else if md.is_file() {
                    scanned_files += 1;
                    let size = md.len();
                    if size >= min_bytes {
                        heap.push(std::cmp::Reverse((size, p.to_string_lossy().to_string())));
                        if heap.len() > top {
                            heap.pop();
                        }
                    }
                }
            }
        }
        let mut results: Vec<(u64, String)> = heap.into_iter().map(|r| r.0).collect();
        results.sort_by(|a, b| b.0.cmp(&a.0));
        let files: Vec<Value> = results
            .into_iter()
            .map(|(size, path)| {
                json!({
                    "path": path,
                    "size_bytes": size,
                    "size_mb": size / (1024 * 1024),
                })
            })
            .collect();
        return Ok(json!({
            "search_root": path,
            "top": top,
            "min_mb": min_mb,
            "files": files,
            "scanned_dirs": scanned_dirs,
            "scanned_files": scanned_files,
        }));
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        Err("sys largest_files requires Linux".into())
    }
}

// ----- shared helpers (Linux) -----

/// Parse a `/proc/<pid>/stat` line into (comm, fields_after_comm).
///
/// /proc stat format: `pid (comm) state ppid ...` where `comm` is up
/// to 16 chars and may itself contain spaces or `(`/`)`. Splitting on
/// whitespace and indexing into fields[1] (the previous code) breaks
/// for any process named `foo bar`, `(weird` or `proc) name`. We must
/// find the LAST `)` to delimit comm, since the rest of the line
/// has no parens. Returns None if the line is malformed.
#[cfg(target_os = "linux")]
fn parse_proc_stat(stat: &str) -> Option<(String, Vec<&str>)> {
    let lparen = stat.find('(')?;
    let rparen = stat.rfind(')')?;
    if rparen <= lparen {
        return None;
    }
    let comm = stat[lparen + 1..rparen].to_string();
    // Fields AFTER the closing paren are positionally fields[2..]
    // in `man 5 proc`. Index 0 of our slice is `state`, etc.
    let tail = stat[rparen + 1..].trim();
    let fields: Vec<&str> = tail.split_whitespace().collect();
    Some((comm, fields))
}

#[cfg(target_os = "linux")]
fn read_arg<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    let prefix = format!("{flag}=");
    let mut iter = args.iter();
    while let Some(a) = iter.next() {
        if a == flag {
            return iter.next().map(String::as_str);
        }
        if let Some(rest) = a.strip_prefix(&prefix) {
            return Some(rest);
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

#[cfg(target_os = "linux")]
fn run_cmd(cmd: &str, args: &[&str]) -> Result<String, String> {
    use std::process::Command;
    let out = Command::new(cmd)
        .args(args)
        .output()
        .map_err(|e| format!("failed to spawn {cmd}: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        return Err(format!(
            "{cmd} exited {}: {}",
            out.status,
            stderr.trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

#[cfg(target_os = "linux")]
fn clk_tck() -> u64 {
    let v = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if v <= 0 {
        100
    } else {
        v as u64
    }
}

#[cfg(target_os = "linux")]
fn num_cores() -> u64 {
    let v = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };
    if v <= 0 {
        1
    } else {
        v as u64
    }
}

#[cfg(target_os = "linux")]
fn state_name(s: &str) -> &'static str {
    match s {
        "R" => "running",
        "S" => "sleeping",
        "D" => "disk_wait",
        "Z" => "zombie",
        "T" => "stopped",
        "t" => "tracing_stop",
        "X" | "x" => "dead",
        _ => "unknown",
    }
}

#[cfg(target_os = "linux")]
fn read_str_trim(path: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}

#[cfg(target_os = "linux")]
fn read_u64(path: &std::path::Path) -> Option<u64> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

#[cfg(target_os = "linux")]
#[derive(Clone)]
struct ProcSnap {
    comm: String,
    state: String,
    utime: u64,
    stime: u64,
    rss_bytes: u64,
}

#[cfg(target_os = "linux")]
fn sample_proc_stats() -> Result<std::collections::HashMap<u32, ProcSnap>, String> {
    let entries = std::fs::read_dir("/proc").map_err(|e| format!("read /proc: {e}"))?;
    let mut map = std::collections::HashMap::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let pid: u32 = match name.parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let (comm, fields) = match parse_proc_stat(&stat) {
            Some(t) => t,
            None => continue,
        };
        if fields.len() < 22 {
            continue;
        }
        let state = fields[0].to_string();
        let utime = fields[11].parse::<u64>().unwrap_or(0);
        let stime = fields[12].parse::<u64>().unwrap_or(0);
        let rss_pages = fields[21].parse::<u64>().unwrap_or(0);
        map.insert(
            pid,
            ProcSnap {
                comm,
                state,
                utime,
                stime,
                rss_bytes: rss_pages * 4096,
            },
        );
    }
    Ok(map)
}

#[cfg(target_os = "linux")]
struct DiskStat {
    reads: u64,
    read_sectors: u64,
    writes: u64,
    write_sectors: u64,
}

#[cfg(target_os = "linux")]
fn read_diskstats() -> Result<std::collections::HashMap<String, DiskStat>, String> {
    let s = std::fs::read_to_string("/proc/diskstats")
        .map_err(|e| format!("read /proc/diskstats: {e}"))?;
    let mut map = std::collections::HashMap::new();
    for line in s.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 14 {
            continue;
        }
        let name = f[2].to_string();
        let reads = f[3].parse().unwrap_or(0);
        let read_sectors = f[5].parse().unwrap_or(0);
        let writes = f[7].parse().unwrap_or(0);
        let write_sectors = f[9].parse().unwrap_or(0);
        map.insert(
            name,
            DiskStat {
                reads,
                read_sectors,
                writes,
                write_sectors,
            },
        );
    }
    Ok(map)
}

#[cfg(target_os = "linux")]
struct NetStat {
    rx_bytes: u64,
    tx_bytes: u64,
}

#[cfg(target_os = "linux")]
fn read_net_dev() -> Result<std::collections::HashMap<String, NetStat>, String> {
    let s = std::fs::read_to_string("/proc/net/dev")
        .map_err(|e| format!("read /proc/net/dev: {e}"))?;
    let mut map = std::collections::HashMap::new();
    for line in s.lines().skip(2) {
        let parts: Vec<&str> = line.split(':').collect();
        if parts.len() != 2 {
            continue;
        }
        let iface = parts[0].trim().to_string();
        let stats: Vec<u64> = parts[1]
            .split_whitespace()
            .filter_map(|x| x.parse().ok())
            .collect();
        if stats.len() < 9 {
            continue;
        }
        map.insert(
            iface,
            NetStat {
                rx_bytes: stats[0],
                tx_bytes: stats[8],
            },
        );
    }
    Ok(map)
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

// ----- tests -----

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn read_arg_long_form() {
        let args = vec!["--top".to_string(), "25".to_string()];
        assert_eq!(read_arg(&args, "--top"), Some("25"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn read_arg_equals_form() {
        let args = vec!["--lines=50".to_string()];
        assert_eq!(read_arg(&args, "--lines"), Some("50"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn read_arg_missing() {
        let args = vec!["--other".to_string(), "v".to_string()];
        assert_eq!(read_arg(&args, "--top"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn has_flag_works() {
        let args = vec!["--failed-only".to_string()];
        assert!(has_flag(&args, "--failed-only"));
        assert!(!has_flag(&args, "--failed"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn state_name_maps_common_codes() {
        assert_eq!(state_name("R"), "running");
        assert_eq!(state_name("S"), "sleeping");
        assert_eq!(state_name("Z"), "zombie");
        assert_eq!(state_name("?"), "unknown");
    }

    #[test]
    fn desktop_command_returns_object() {
        let v = cmd_desktop().expect("desktop should always succeed");
        assert!(v.is_object());
        // env vars may be unset; we just verify the keys exist as JSON keys.
        for key in [
            "desktop",
            "session_type",
            "wayland_display",
            "x_display",
            "seat",
            "vt",
            "user",
            "runtime_dir",
        ] {
            assert!(v.get(key).is_some(), "missing key {key}");
        }
    }

    #[test]
    fn unknown_command_errors() {
        let r = run("definitely-not-a-real-command", &[]);
        assert!(r.is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn loadavg_smoke() {
        // /proc/loadavg should always exist on Linux test runners.
        let v = cmd_loadavg().expect("loadavg");
        assert!(v["load_1min"].as_f64().is_some());
        assert!(v["cores"].as_u64().unwrap_or(0) >= 1);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn sample_proc_stats_returns_self() {
        let pid = std::process::id();
        let map = sample_proc_stats().expect("sample");
        assert!(map.contains_key(&pid), "current process should be visible");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn clk_tck_is_sensible() {
        let v = clk_tck();
        assert!(v >= 50 && v <= 10_000, "clk_tck out of range: {v}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn num_cores_is_at_least_one() {
        assert!(num_cores() >= 1);
    }

    // -----------------------------------------------------------------
    // Capability gate
    // -----------------------------------------------------------------

    /// Regression: the top-level `run()` gate is read-only system
    /// observation, NOT kernel-module loading. Before this fix every
    /// subcommand asked for [`Verb::SYS_KERNEL`] ("Load kernel modules
    /// … reserved for trusted system tools", Risk::Critical), which
    /// the clawd-routed agent doesn't have by default. The correct
    /// verb per `caps::catalog` is [`Verb::SYS_OBSERVE`] ("Inspect
    /// system state … without changing them", Risk::Low) — already
    /// granted by [`crate::clawd::system_caps::readonly_task_caps`].
    /// This test fails closed if anyone re-classifies the gate as a
    /// privileged verb again.
    #[test]
    fn run_clears_gate_with_sys_observe_only() {
        use crate::caps::{Cap, CapSet, Role};
        use crate::proc::{deregister_session, register_session, SessionInfo};

        let _lock = crate::caps::test_env_lock::env_lock();

        // Redirect COS_DATA_DIR so the registry write lands in a
        // tempdir, isolated from any concurrent test and from the
        // real per-user proc/registry.json.
        let tmp = tempfile::tempdir().expect("tempdir");
        let prev_data = env::var_os("COS_DATA_DIR");
        env::set_var("COS_DATA_DIR", tmp.path());

        let prev_sess = env::var_os("COS_SESSION");
        let prev_perms = env::var_os("COS_PERMS_MODE");
        env::remove_var("COS_PERMS_MODE");

        // Build a session that holds SYS_OBSERVE only — mirrors what
        // `clawd::system_caps::readonly_task_caps` hands out. PID is
        // our own so the ancestry check in caps::enforcement passes
        // without a real fork.
        let session_id = format!("sysinfo-cap-test-{}", std::process::id());
        let mut caps = CapSet::new();
        caps.insert(Cap::new(Verb::SYS_OBSERVE, Scope::Wild));
        let info = SessionInfo {
            session_id: session_id.clone(),
            pid: std::process::id(),
            command: vec!["sysinfo-cap-test".into()],
            started_at: chrono::Utc::now().to_rfc3339(),
            stdout_path: String::new(),
            stderr_path: String::new(),
            group: None,
            parent: None,
            workdir: None,
            exit_code: None,
            ended_at: None,
            tier: None,
            scope: None,
            priority: None,
            caps: Some(caps),
            role: Some(Role::Observer.name().to_string()),
            app_id: None,
            pending_bind: false,
            start_time_ticks: None,
        };
        register_session(info).expect("register session");
        env::set_var("COS_SESSION", &session_id);

        // Bogus command name so we hit the dispatch's "unknown
        // command" arm immediately after the cap gate clears. If the
        // gate is still on SYS_KERNEL, this errors with
        // "permission denied" / "verb-not-granted" instead.
        let result = run("__definitely-not-a-real-command__", &[]);

        // Restore env BEFORE asserting so a panic doesn't leak state
        // into other tests that share the lock.
        deregister_session(&session_id);
        match prev_sess {
            Some(v) => env::set_var("COS_SESSION", v),
            None => env::remove_var("COS_SESSION"),
        }
        match prev_perms {
            Some(v) => env::set_var("COS_PERMS_MODE", v),
            None => env::remove_var("COS_PERMS_MODE"),
        }
        match prev_data {
            Some(v) => env::set_var("COS_DATA_DIR", v),
            None => env::remove_var("COS_DATA_DIR"),
        }

        let err = result.expect_err("dispatch should error on bogus command");
        let lower = err.to_lowercase();
        assert!(
            !lower.contains("permission denied") && !lower.contains("not granted"),
            "SYS_OBSERVE should be sufficient to clear the run() cap gate, but got: {err}"
        );
        assert!(
            lower.contains("unknown command"),
            "expected to reach command dispatch (unknown command arm), got: {err}"
        );
    }
}
