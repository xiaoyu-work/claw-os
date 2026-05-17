use serde_json::{json, Value};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::state::DaemonState;
use super::system_journal;

pub fn snapshot(state: &DaemonState) -> Result<Value, String> {
    refresh_builtin_sources(state);
    let entries = state
        .context_snapshot()
        .into_iter()
        .map(|entry| {
            json!({
                "source": entry.source,
                "updated_at": entry.updated_at,
                "payload": entry.payload,
                "metadata": entry.metadata,
            })
        })
        .collect::<Vec<_>>();

    Ok(json!({
        "schema": 1,
        "entries": entries,
    }))
}

pub fn sources(state: &DaemonState) -> Result<Value, String> {
    refresh_builtin_sources(state);
    let sources = state
        .context_snapshot()
        .into_iter()
        .map(|entry| {
            json!({
                "source": entry.source,
                "updated_at": entry.updated_at,
                "metadata": entry.metadata,
            })
        })
        .collect::<Vec<_>>();

    Ok(json!({
        "schema": 1,
        "sources": sources,
    }))
}

pub fn update(state: &DaemonState, params: Value) -> Result<Value, String> {
    let source = required_string(&params, "source")?;
    let payload = params.get("payload").cloned().unwrap_or(Value::Null);
    let metadata = params.get("metadata").cloned().unwrap_or_else(|| json!({}));

    state.update_context(source.clone(), payload, metadata);

    Ok(json!({
        "accepted": true,
        "source": source,
    }))
}

pub fn refresh_builtin_sources(state: &DaemonState) {
    collect_session_environment(state);
    collect_system_overview(state);
    collect_system_inventory(state);
    collect_system_processes(state);
    collect_system_mounts(state);
    collect_system_users(state);
    collect_system_packages(state);
    collect_system_audit_sources(state);
    collect_system_operations(state);
}

fn collect_session_environment(state: &DaemonState) {
    let keys = [
        "XDG_CURRENT_DESKTOP",
        "XDG_SESSION_TYPE",
        "DESKTOP_SESSION",
        "WAYLAND_DISPLAY",
        "DISPLAY",
        "XDG_RUNTIME_DIR",
        "COS_RUNTIME_DIR",
        "COS_DATA_DIR",
        "SHELL",
        "LANG",
    ];
    let mut env = serde_json::Map::new();
    for key in keys {
        if let Ok(value) = std::env::var(key) {
            if !value.is_empty() {
                env.insert(key.to_string(), Value::String(value));
            }
        }
    }
    state.update_context(
        "clawd.environment".to_string(),
        Value::Object(env),
        json!({
            "kind": "builtin",
            "collector": "session_environment",
        }),
    );
}

fn collect_system_overview(state: &DaemonState) {
    state.update_context(
        "clawd.system".to_string(),
        json!({
            "os": std::env::consts::OS,
            "family": std::env::consts::FAMILY,
            "arch": std::env::consts::ARCH,
            "hostname": command_stdout("hostname", []),
            "kernel": command_stdout("uname", ["-srm"]),
            "os_release": os_release_summary(),
            "uptime_seconds": linux_uptime_seconds(),
            "clawd_pid": std::process::id(),
        }),
        json!({
            "kind": "builtin",
            "collector": "system_overview",
            "capability": "sys.observe",
            "mode": "readonly",
        }),
    );
}

fn collect_system_inventory(state: &DaemonState) {
    state.update_context(
        "clawd.system.inventory".to_string(),
        json!({
            "systemd": {
                "systemctl_available": command_exists("systemctl"),
                "user_session": std::env::var_os("XDG_RUNTIME_DIR").is_some(),
            },
            "packages": {
                "dpkg_available": command_exists("dpkg"),
                "apt_cache_available": command_exists("apt-cache"),
                "apt_get_available": command_exists("apt-get"),
            },
            "processes": {
                "procfs_available": std::path::Path::new("/proc").is_dir(),
                "ps_available": command_exists("ps"),
            },
        }),
        json!({
            "kind": "builtin",
            "collector": "system_inventory",
            "capability": "sys.observe",
            "mode": "readonly",
        }),
    );
}

fn collect_system_processes(state: &DaemonState) {
    let processes = linux_process_snapshot(512);
    let count = processes.len();
    state.update_context(
        "clawd.system.processes".to_string(),
        json!({
            "processes": processes,
            "count": count,
            "truncated": count >= 512,
        }),
        json!({
            "kind": "builtin",
            "collector": "system_processes",
            "capability": "proc.observe",
            "mode": "readonly",
        }),
    );
}

fn collect_system_mounts(state: &DaemonState) {
    let mounts = linux_mounts_snapshot(256);
    let count = mounts.len();
    state.update_context(
        "clawd.system.mounts".to_string(),
        json!({
            "mounts": mounts,
            "count": count,
            "truncated": count >= 256,
        }),
        json!({
            "kind": "builtin",
            "collector": "system_mounts",
            "capability": "sys.observe",
            "mode": "readonly",
        }),
    );
}

fn collect_system_users(state: &DaemonState) {
    let users = passwd_snapshot(256);
    let count = users.len();
    state.update_context(
        "clawd.system.users".to_string(),
        json!({
            "users": users,
            "count": count,
            "truncated": count >= 256,
        }),
        json!({
            "kind": "builtin",
            "collector": "system_users",
            "capability": "sys.observe",
            "mode": "readonly",
        }),
    );
}

fn collect_system_packages(state: &DaemonState) {
    let packages = dpkg_status_snapshot(500);
    state.update_context(
        "clawd.system.packages".to_string(),
        packages,
        json!({
            "kind": "builtin",
            "collector": "system_packages",
            "capability": "sys.observe",
            "mode": "readonly",
        }),
    );
}

fn collect_system_audit_sources(state: &DaemonState) {
    state.update_context(
        "clawd.system.audit".to_string(),
        json!({
            "operation_journal": path_info(crate::paths::system_operations_log_path()),
            "capability_audit": path_info(crate::paths::caps_audit_log_path()),
            "agent_audit": path_info(crate::paths::agent_audit_log_path()),
            "clawd_audit": path_info(crate::paths::data_dir().join("clawd").join("audit.jsonl")),
        }),
        json!({
            "kind": "builtin",
            "collector": "system_audit_sources",
            "capability": "sys.observe",
            "mode": "readonly",
        }),
    );
}

fn collect_system_operations(state: &DaemonState) {
    state.update_context(
        "clawd.system.operations".to_string(),
        system_journal::context_payload(50),
        json!({
            "kind": "builtin",
            "collector": "system_operations",
            "capability": "sys.observe",
            "mode": "readonly",
            "persistent": true,
        }),
    );
}

fn os_release_summary() -> Value {
    let raw = match std::fs::read_to_string("/etc/os-release") {
        Ok(raw) => raw,
        Err(_) => return Value::Null,
    };
    let mut out = serde_json::Map::new();
    for line in raw.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if matches!(
            key,
            "ID" | "NAME" | "VERSION_ID" | "VERSION" | "PRETTY_NAME"
        ) {
            out.insert(
                key.to_ascii_lowercase(),
                Value::String(value.trim_matches('"').to_string()),
            );
        }
    }
    Value::Object(out)
}

fn linux_uptime_seconds() -> Option<u64> {
    let raw = std::fs::read_to_string("/proc/uptime").ok()?;
    let first = raw.split_whitespace().next()?;
    let seconds = first.split('.').next()?;
    seconds.parse().ok()
}

fn linux_process_snapshot(limit: usize) -> Vec<Value> {
    let proc_root = Path::new("/proc");
    let Ok(read) = std::fs::read_dir(proc_root) else {
        return Vec::new();
    };
    let mut pids = read
        .flatten()
        .filter_map(|entry| entry.file_name().to_string_lossy().parse::<u32>().ok())
        .collect::<Vec<_>>();
    pids.sort_unstable();
    let mut processes = Vec::new();
    for pid in pids.into_iter().take(limit) {
        let dir = proc_root.join(pid.to_string());
        let status = std::fs::read_to_string(dir.join("status")).unwrap_or_default();
        let name = status_field(&status, "Name")
            .or_else(|| read_trimmed(dir.join("comm")))
            .unwrap_or_default();
        let state = status_field(&status, "State").unwrap_or_default();
        let ppid = status_field(&status, "PPid").and_then(|value| value.parse::<u32>().ok());
        let uid = status_field(&status, "Uid")
            .and_then(|value| value.split_whitespace().next().map(ToOwned::to_owned));
        let threads = status_field(&status, "Threads").and_then(|value| value.parse::<u32>().ok());
        processes.push(json!({
            "pid": pid,
            "ppid": ppid,
            "name": name,
            "state": state,
            "uid": uid,
            "threads": threads,
        }));
    }
    processes
}

fn linux_mounts_snapshot(limit: usize) -> Vec<Value> {
    let raw = match std::fs::read_to_string("/proc/mounts") {
        Ok(raw) => raw,
        Err(_) => return Vec::new(),
    };
    raw.lines()
        .take(limit)
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let source = parts.next()?;
            let target = parts.next()?;
            let fs_type = parts.next()?;
            let options = parts.next().unwrap_or("");
            Some(json!({
                "source": unescape_mount_field(source),
                "target": unescape_mount_field(target),
                "fs_type": fs_type,
                "options": options,
            }))
        })
        .collect()
}

fn passwd_snapshot(limit: usize) -> Vec<Value> {
    let raw = match std::fs::read_to_string("/etc/passwd") {
        Ok(raw) => raw,
        Err(_) => return Vec::new(),
    };
    raw.lines()
        .take(limit)
        .filter_map(|line| {
            let parts = line.split(':').collect::<Vec<_>>();
            if parts.len() < 7 {
                return None;
            }
            Some(json!({
                "name": parts[0],
                "uid": parts[2],
                "gid": parts[3],
                "gecos": parts[4],
                "home": parts[5],
                "shell": parts[6],
            }))
        })
        .collect()
}

fn dpkg_status_snapshot(limit: usize) -> Value {
    let raw = match std::fs::read_to_string("/var/lib/dpkg/status") {
        Ok(raw) => raw,
        Err(_) => {
            return json!({
                "available": false,
                "packages": [],
                "count": 0,
                "truncated": false,
            });
        }
    };
    let mut packages = Vec::new();
    let mut total = 0usize;
    for stanza in raw.split("\n\n") {
        if !stanza.contains("Status: install ok installed") {
            continue;
        }
        total += 1;
        if packages.len() >= limit {
            continue;
        }
        let package = stanza_field(stanza, "Package").unwrap_or_default();
        if package.is_empty() {
            continue;
        }
        packages.push(json!({
            "package": package,
            "version": stanza_field(stanza, "Version"),
            "architecture": stanza_field(stanza, "Architecture"),
            "description": stanza_field(stanza, "Description"),
        }));
    }
    json!({
        "available": true,
        "packages": packages,
        "count": total,
        "truncated": total > limit,
    })
}

fn path_info(path: PathBuf) -> Value {
    let metadata = std::fs::metadata(&path).ok();
    json!({
        "path": path,
        "exists": metadata.is_some(),
        "bytes": metadata.map(|metadata| metadata.len()),
    })
}

fn read_trimmed(path: PathBuf) -> Option<String> {
    let value = std::fs::read_to_string(path).ok()?.trim().to_string();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn status_field(raw: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    raw.lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn stanza_field(raw: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    raw.lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn unescape_mount_field(value: &str) -> String {
    value
        .replace("\\040", " ")
        .replace("\\011", "\t")
        .replace("\\012", "\n")
        .replace("\\134", "\\")
}

fn command_stdout<const N: usize>(program: &str, args: [&str; N]) -> Option<String> {
    if !command_exists(program) {
        return None;
    }
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

fn command_exists(program: &str) -> bool {
    let program = OsStr::new(program);
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths)
        .map(|dir| dir.join(PathBuf::from(program)))
        .any(|candidate| candidate.is_file())
}

fn required_string(params: &Value, key: &str) -> Result<String, String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("missing required string parameter: {key}"))
}
