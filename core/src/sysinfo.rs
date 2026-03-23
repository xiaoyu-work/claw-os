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
        let c_path = CString::new(workspace.as_bytes()).unwrap();
        unsafe {
            let mut stat: libc::statvfs = std::mem::zeroed();
            if libc::statvfs(c_path.as_ptr(), &mut stat) == 0 {
                let total = stat.f_blocks * stat.f_frsize as u64;
                let free = stat.f_bavail * stat.f_frsize as u64;
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
