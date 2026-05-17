use serde_json::{json, Value};
use std::ffi::OsStr;
use std::path::PathBuf;
use std::process::Command;

use super::state::DaemonState;

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
