use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

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
    collect_headless_runtime(state);
    collect_headless_shells(state);
    collect_headless_workspaces(state);
    collect_headless_recent_files(state);
    collect_headless_services(state);
    collect_headless_package_activity(state);
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

fn collect_headless_runtime(state: &DaemonState) {
    let kernel_release = read_trimmed(PathBuf::from("/proc/sys/kernel/osrelease"));
    let kernel_version = read_trimmed(PathBuf::from("/proc/version"));
    state.update_context(
        "clawd.headless.runtime".to_string(),
        json!({
            "headless": is_headless_environment(),
            "wsl": wsl_summary(kernel_release.as_deref(), kernel_version.as_deref()),
            "systemd": systemd_summary(),
            "login_sessions": logged_in_users_snapshot(32),
            "paths": {
                "home_dirs": human_home_dirs(32).into_iter().map(|path| json!(path)).collect::<Vec<_>>(),
                "tmp": path_info(PathBuf::from("/tmp")),
                "wsl_windows_users": path_info(PathBuf::from("/mnt/c/Users")),
            },
        }),
        json!({
            "kind": "builtin",
            "collector": "headless_runtime",
            "capability": "sys.observe",
            "mode": "readonly",
            "platform": "linux_headless",
        }),
    );
}

fn collect_headless_shells(state: &DaemonState) {
    let shells = shell_process_snapshot(128);
    let count = shells.len();
    state.update_context(
        "clawd.headless.shells".to_string(),
        json!({
            "shells": shells,
            "count": count,
            "truncated": count >= 128,
        }),
        json!({
            "kind": "builtin",
            "collector": "headless_shells",
            "capability": "proc.observe",
            "mode": "readonly",
            "platform": "linux_headless",
        }),
    );
}

fn collect_headless_workspaces(state: &DaemonState) {
    let workspaces = active_git_workspaces_snapshot(64);
    let count = workspaces.len();
    state.update_context(
        "clawd.headless.workspaces".to_string(),
        json!({
            "workspaces": workspaces,
            "count": count,
            "truncated": count >= 64,
        }),
        json!({
            "kind": "builtin",
            "collector": "headless_workspaces",
            "capability": "sys.observe",
            "mode": "readonly",
            "platform": "linux_headless",
        }),
    );
}

fn collect_headless_recent_files(state: &DaemonState) {
    let roots = recent_file_roots(32);
    let root_count = roots.len();
    let snapshot = recent_files_snapshot(roots, 100, 3, 4_000);
    state.update_context(
        "clawd.headless.recent_files".to_string(),
        json!({
            "files": snapshot.files,
            "roots": snapshot.roots,
            "root_count": root_count,
            "visited_entries": snapshot.visited_entries,
            "count": snapshot.count,
            "truncated": snapshot.truncated,
        }),
        json!({
            "kind": "builtin",
            "collector": "headless_recent_files",
            "capability": "fs.meta",
            "mode": "readonly",
            "platform": "linux_headless",
            "max_depth": 3,
        }),
    );
}

fn collect_headless_services(state: &DaemonState) {
    state.update_context(
        "clawd.headless.services".to_string(),
        system_services_snapshot(100),
        json!({
            "kind": "builtin",
            "collector": "headless_services",
            "capability": "sys.observe",
            "mode": "readonly",
            "platform": "linux_headless",
        }),
    );
}

fn collect_headless_package_activity(state: &DaemonState) {
    state.update_context(
        "clawd.headless.package_activity".to_string(),
        apt_history_snapshot(40),
        json!({
            "kind": "builtin",
            "collector": "headless_package_activity",
            "capability": "sys.observe",
            "mode": "readonly",
            "platform": "linux_headless",
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

#[derive(Debug, Clone)]
struct TerminalProcess {
    pid: u32,
    ppid: Option<u32>,
    name: String,
    state: Option<String>,
    uid: Option<String>,
    tty_nr: Option<i64>,
    cwd: Option<PathBuf>,
    workspace: Option<GitWorkspace>,
}

#[derive(Debug, Clone)]
struct GitWorkspace {
    root: PathBuf,
    git_dir: PathBuf,
    branch: Option<String>,
    head: Option<String>,
}

#[derive(Debug, Clone)]
struct RecentRoot {
    label: String,
    path: PathBuf,
}

#[derive(Debug)]
struct RecentFileEntry {
    modified_epoch_ms: u64,
    payload: Value,
}

#[derive(Debug)]
struct RecentFilesSnapshot {
    files: Vec<Value>,
    roots: Vec<Value>,
    visited_entries: usize,
    count: usize,
    truncated: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct ProcStatSummary {
    state: Option<String>,
    ppid: Option<u32>,
    pgrp: Option<i32>,
    session: Option<i32>,
    tty_nr: Option<i64>,
}

fn is_headless_environment() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_none() && std::env::var_os("DISPLAY").is_none()
}

fn wsl_summary(kernel_release: Option<&str>, kernel_version: Option<&str>) -> Value {
    let detected = is_wsl_kernel(kernel_release, kernel_version)
        || std::env::var_os("WSL_DISTRO_NAME").is_some()
        || std::env::var_os("WSL_INTEROP").is_some();
    json!({
        "detected": detected,
        "distro": std::env::var("WSL_DISTRO_NAME").ok(),
        "interop_socket": std::env::var("WSL_INTEROP").ok(),
        "kernel_release": kernel_release,
        "kernel_version": kernel_version,
        "windows_mount_c": path_info(PathBuf::from("/mnt/c")),
    })
}

fn is_wsl_kernel(kernel_release: Option<&str>, kernel_version: Option<&str>) -> bool {
    [kernel_release, kernel_version]
        .into_iter()
        .flatten()
        .any(|value| {
            let lower = value.to_ascii_lowercase();
            lower.contains("microsoft") || lower.contains("wsl")
        })
}

fn systemd_summary() -> Value {
    let pid1 = read_trimmed(PathBuf::from("/proc/1/comm"));
    let running = command_output_text("systemctl", ["is-system-running", "--no-pager"])
        .map(|(_, output)| output);
    json!({
        "systemctl_available": command_exists("systemctl"),
        "pid1": pid1,
        "pid1_is_systemd": pid1.as_deref() == Some("systemd"),
        "state": running,
    })
}

fn logged_in_users_snapshot(limit: usize) -> Vec<Value> {
    let Some(raw) = command_stdout("who", []) else {
        return Vec::new();
    };
    raw.lines()
        .take(limit)
        .filter_map(parse_who_line)
        .collect::<Vec<_>>()
}

fn parse_who_line(line: &str) -> Option<Value> {
    let parts = line.split_whitespace().collect::<Vec<_>>();
    if parts.len() < 3 {
        return None;
    }
    let login_at = match parts.get(3) {
        Some(time) => format!("{} {}", parts[2], time),
        None => parts[2].to_string(),
    };
    let remote = line
        .split_once('(')
        .and_then(|(_, rest)| rest.split_once(')').map(|(remote, _)| remote.to_string()));
    Some(json!({
        "user": parts[0],
        "tty": parts[1],
        "login_at": login_at,
        "remote": remote,
    }))
}

fn shell_process_snapshot(limit: usize) -> Vec<Value> {
    terminal_processes(limit)
        .into_iter()
        .map(|process| {
            json!({
                "pid": process.pid,
                "ppid": process.ppid,
                "name": process.name,
                "state": process.state,
                "uid": process.uid,
                "tty_nr": process.tty_nr,
                "cwd": process.cwd,
                "workspace": process.workspace.map(git_workspace_value),
            })
        })
        .collect()
}

fn terminal_processes(limit: usize) -> Vec<TerminalProcess> {
    let proc_root = Path::new("/proc");
    let Ok(read) = fs::read_dir(proc_root) else {
        return Vec::new();
    };
    let mut pids = read
        .flatten()
        .filter_map(|entry| entry.file_name().to_string_lossy().parse::<u32>().ok())
        .collect::<Vec<_>>();
    pids.sort_unstable();

    let mut processes = Vec::new();
    for pid in pids {
        let dir = proc_root.join(pid.to_string());
        let stat = parse_proc_stat(&fs::read_to_string(dir.join("stat")).unwrap_or_default());
        let status = fs::read_to_string(dir.join("status")).unwrap_or_default();
        let name = read_trimmed(dir.join("comm"))
            .or_else(|| status_field(&status, "Name"))
            .unwrap_or_default();
        let tty_nr = stat.tty_nr;
        if !is_terminal_process(&name, tty_nr) {
            continue;
        }
        let cwd = fs::read_link(dir.join("cwd")).ok();
        let workspace = cwd.as_deref().and_then(git_workspace_info);
        processes.push(TerminalProcess {
            pid,
            ppid: stat
                .ppid
                .or_else(|| status_field(&status, "PPid").and_then(|value| value.parse().ok())),
            name,
            state: status_field(&status, "State").or(stat.state),
            uid: status_field(&status, "Uid")
                .and_then(|value| value.split_whitespace().next().map(ToOwned::to_owned)),
            tty_nr,
            cwd,
            workspace,
        });
        if processes.len() >= limit {
            break;
        }
    }
    processes
}

fn is_terminal_process(name: &str, tty_nr: Option<i64>) -> bool {
    let lower = name.to_ascii_lowercase();
    let known_session_process = matches!(
        lower.as_str(),
        "bash"
            | "zsh"
            | "fish"
            | "sh"
            | "dash"
            | "nu"
            | "tmux"
            | "screen"
            | "ssh"
            | "sshd"
            | "login"
    ) || lower.starts_with("tmux");
    known_session_process || tty_nr.unwrap_or_default() != 0
}

fn active_git_workspaces_snapshot(limit: usize) -> Vec<Value> {
    let mut by_root = BTreeMap::<PathBuf, (GitWorkspace, Vec<u32>)>::new();
    for process in terminal_processes(256) {
        let Some(workspace) = process.workspace else {
            continue;
        };
        by_root
            .entry(workspace.root.clone())
            .and_modify(|(_, pids)| pids.push(process.pid))
            .or_insert_with(|| (workspace, vec![process.pid]));
        if by_root.len() >= limit {
            break;
        }
    }
    by_root
        .into_values()
        .map(|(workspace, pids)| {
            let mut value = git_workspace_value(workspace);
            if let Some(object) = value.as_object_mut() {
                object.insert("terminal_pids".to_string(), json!(pids));
            }
            value
        })
        .collect()
}

fn git_workspace_value(workspace: GitWorkspace) -> Value {
    json!({
        "root": workspace.root,
        "git_dir": workspace.git_dir,
        "branch": workspace.branch,
        "head": workspace.head,
    })
}

fn git_workspace_info(path: &Path) -> Option<GitWorkspace> {
    let root = find_git_root(path)?;
    let git_dir = git_dir_for_root(&root)?;
    let (branch, head) = git_head_summary(&git_dir);
    Some(GitWorkspace {
        root,
        git_dir,
        branch,
        head,
    })
}

fn find_git_root(path: &Path) -> Option<PathBuf> {
    let mut current = if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent()?.to_path_buf()
    };
    loop {
        if current.join(".git").exists() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}

fn git_dir_for_root(root: &Path) -> Option<PathBuf> {
    let dot_git = root.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }
    let raw = fs::read_to_string(&dot_git).ok()?;
    let git_dir = raw.trim().strip_prefix("gitdir:")?.trim();
    let path = PathBuf::from(git_dir);
    Some(if path.is_absolute() {
        path
    } else {
        root.join(path)
    })
}

fn git_head_summary(git_dir: &Path) -> (Option<String>, Option<String>) {
    let Some(head) = read_trimmed(git_dir.join("HEAD")) else {
        return (None, None);
    };
    if let Some(branch) = head.strip_prefix("ref: refs/heads/") {
        return (Some(branch.to_string()), None);
    }
    (None, Some(head.chars().take(12).collect()))
}

fn recent_file_roots(limit: usize) -> Vec<RecentRoot> {
    let mut roots = Vec::new();
    let mut seen = HashSet::new();
    for home in human_home_dirs(32) {
        add_recent_root(
            &mut roots,
            &mut seen,
            "downloads",
            home.join("Downloads"),
            limit,
        );
    }
    for downloads in wsl_windows_download_dirs(16) {
        add_recent_root(
            &mut roots,
            &mut seen,
            "wsl_windows_downloads",
            downloads,
            limit,
        );
    }
    add_recent_root(&mut roots, &mut seen, "tmp", PathBuf::from("/tmp"), limit);
    for process in terminal_processes(128) {
        if let Some(workspace) = process.workspace {
            add_recent_root(
                &mut roots,
                &mut seen,
                "active_workspace",
                workspace.root,
                limit,
            );
        } else if let Some(cwd) = process.cwd {
            add_recent_root(&mut roots, &mut seen, "terminal_cwd", cwd, limit);
        }
    }
    roots
}

fn add_recent_root(
    roots: &mut Vec<RecentRoot>,
    seen: &mut HashSet<String>,
    label: &str,
    path: PathBuf,
    limit: usize,
) {
    if roots.len() >= limit || !path.is_dir() {
        return;
    }
    let key_path = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
    let key = key_path.to_string_lossy().to_string();
    if seen.insert(key) {
        roots.push(RecentRoot {
            label: label.to_string(),
            path,
        });
    }
}

fn recent_files_snapshot(
    roots: Vec<RecentRoot>,
    limit: usize,
    max_depth: usize,
    max_visited: usize,
) -> RecentFilesSnapshot {
    let roots_payload = roots
        .iter()
        .map(|root| {
            json!({
                "label": root.label,
                "path": root.path,
            })
        })
        .collect::<Vec<_>>();
    let mut entries = Vec::new();
    let mut visited_entries = 0usize;
    let mut truncated = false;
    for root in &roots {
        scan_recent_dir(
            root,
            &root.path,
            max_depth,
            max_visited,
            &mut visited_entries,
            &mut entries,
            &mut truncated,
        );
        if truncated {
            break;
        }
    }
    entries.sort_by(|left, right| right.modified_epoch_ms.cmp(&left.modified_epoch_ms));
    let count = entries.len();
    let truncated = truncated || count > limit;
    let files = entries
        .into_iter()
        .take(limit)
        .map(|entry| entry.payload)
        .collect();
    RecentFilesSnapshot {
        files,
        roots: roots_payload,
        visited_entries,
        count,
        truncated,
    }
}

fn scan_recent_dir(
    root: &RecentRoot,
    dir: &Path,
    depth_remaining: usize,
    max_visited: usize,
    visited_entries: &mut usize,
    entries: &mut Vec<RecentFileEntry>,
    truncated: &mut bool,
) {
    if *truncated {
        return;
    }
    let Ok(read) = fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        if *visited_entries >= max_visited {
            *truncated = true;
            return;
        }
        *visited_entries += 1;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            if depth_remaining > 0 && !skip_recent_dir(&name) {
                scan_recent_dir(
                    root,
                    &path,
                    depth_remaining - 1,
                    max_visited,
                    visited_entries,
                    entries,
                    truncated,
                );
            }
            continue;
        }
        if !(file_type.is_file() || file_type.is_symlink()) {
            continue;
        }
        let metadata = entry.metadata().or_else(|_| fs::symlink_metadata(&path));
        let Ok(metadata) = metadata else {
            continue;
        };
        let modified = metadata.modified().ok();
        let modified_epoch_ms = modified.and_then(system_time_epoch_ms).unwrap_or_default();
        entries.push(RecentFileEntry {
            modified_epoch_ms,
            payload: json!({
                "root": root.label,
                "path": path,
                "kind": if file_type.is_symlink() { "symlink" } else { "file" },
                "bytes": metadata.len(),
                "modified_at": modified.and_then(system_time_rfc3339),
                "modified_epoch_ms": modified_epoch_ms,
            }),
        });
    }
}

fn skip_recent_dir(name: &str) -> bool {
    name.starts_with('.')
        || matches!(
            name,
            "node_modules"
                | "target"
                | "vendor"
                | "dist"
                | "build"
                | "__pycache__"
                | ".git"
                | ".cache"
        )
}

fn human_home_dirs(limit: usize) -> Vec<PathBuf> {
    let raw = match fs::read_to_string("/etc/passwd") {
        Ok(raw) => raw,
        Err(_) => return Vec::new(),
    };
    let mut homes = Vec::new();
    for line in raw.lines() {
        let parts = line.split(':').collect::<Vec<_>>();
        if parts.len() < 7 {
            continue;
        }
        let Ok(uid) = parts[2].parse::<u32>() else {
            continue;
        };
        if !(1000..60000).contains(&uid) {
            continue;
        }
        let shell = parts[6];
        if shell.ends_with("/false") || shell.ends_with("/nologin") {
            continue;
        }
        let home = PathBuf::from(parts[5]);
        if home.is_dir() {
            homes.push(home);
        }
        if homes.len() >= limit {
            break;
        }
    }
    homes
}

fn wsl_windows_download_dirs(limit: usize) -> Vec<PathBuf> {
    let users_root = Path::new("/mnt/c/Users");
    let Ok(read) = fs::read_dir(users_root) else {
        return Vec::new();
    };
    read.flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            if matches!(
                name.as_str(),
                "All Users" | "Default" | "Default User" | "Public"
            ) {
                return None;
            }
            let downloads = entry.path().join("Downloads");
            downloads.is_dir().then_some(downloads)
        })
        .take(limit)
        .collect()
}

fn system_services_snapshot(limit: usize) -> Value {
    let pid1 = read_trimmed(PathBuf::from("/proc/1/comm"));
    let Some((ok, raw)) = command_output_text(
        "systemctl",
        [
            "list-units",
            "--type=service",
            "--state=running,failed",
            "--no-legend",
            "--no-pager",
            "--plain",
        ],
    ) else {
        return json!({
            "available": false,
            "pid1": pid1,
            "services": [],
            "count": 0,
            "truncated": false,
        });
    };
    let services = raw
        .lines()
        .take(limit)
        .filter_map(parse_systemctl_service_line)
        .collect::<Vec<_>>();
    json!({
        "available": true,
        "systemctl_ok": ok,
        "pid1": pid1,
        "pid1_is_systemd": pid1.as_deref() == Some("systemd"),
        "services": services,
        "count": services.len(),
        "truncated": raw.lines().count() > limit,
    })
}

fn parse_systemctl_service_line(line: &str) -> Option<Value> {
    let parts = line.split_whitespace().collect::<Vec<_>>();
    if parts.len() < 4 {
        return None;
    }
    Some(json!({
        "unit": parts[0],
        "load": parts[1],
        "active": parts[2],
        "sub": parts[3],
        "description": parts.get(4..).unwrap_or_default().join(" "),
    }))
}

fn apt_history_snapshot(limit: usize) -> Value {
    let paths = [
        PathBuf::from("/var/log/apt/history.log.1"),
        PathBuf::from("/var/log/apt/history.log"),
    ];
    let mut events = Vec::new();
    for path in &paths {
        let Ok(raw) = fs::read_to_string(path) else {
            continue;
        };
        for block in raw.split("\n\n") {
            if let Some(event) = parse_apt_history_block(path, block) {
                events.push(event);
            }
        }
    }
    let count = events.len();
    events.reverse();
    events.truncate(limit);
    json!({
        "available": count > 0,
        "events": events,
        "count": count,
        "truncated": count > limit,
    })
}

fn parse_apt_history_block(path: &Path, block: &str) -> Option<Value> {
    let mut values = BTreeMap::new();
    for line in block.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        values.insert(key.trim().to_string(), value.trim().to_string());
    }
    if values.is_empty() {
        return None;
    }
    Some(json!({
        "source_log": path,
        "start_date": values.get("Start-Date"),
        "end_date": values.get("End-Date"),
        "commandline": values.get("Commandline"),
        "install": values.get("Install"),
        "upgrade": values.get("Upgrade"),
        "remove": values.get("Remove"),
        "purge": values.get("Purge"),
    }))
}

fn parse_proc_stat(raw: &str) -> ProcStatSummary {
    let Some(close) = raw.rfind(") ") else {
        return ProcStatSummary::default();
    };
    let fields = raw[close + 2..].split_whitespace().collect::<Vec<_>>();
    ProcStatSummary {
        state: fields.first().map(|value| (*value).to_string()),
        ppid: fields.get(1).and_then(|value| value.parse().ok()),
        pgrp: fields.get(2).and_then(|value| value.parse().ok()),
        session: fields.get(3).and_then(|value| value.parse().ok()),
        tty_nr: fields.get(4).and_then(|value| value.parse().ok()),
    }
}

fn system_time_epoch_ms(time: SystemTime) -> Option<u64> {
    let millis = time.duration_since(UNIX_EPOCH).ok()?.as_millis();
    u64::try_from(millis).ok()
}

fn system_time_rfc3339(time: SystemTime) -> Option<String> {
    let datetime: DateTime<Utc> = time.into();
    Some(datetime.to_rfc3339())
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

fn command_output_text<const N: usize>(program: &str, args: [&str; N]) -> Option<(bool, String)> {
    if !command_exists(program) {
        return None;
    }
    let output = Command::new(program).args(args).output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Some((output.status.success(), text))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_wsl_kernel_markers() {
        assert!(is_wsl_kernel(
            Some("5.15.167.4-microsoft-standard-WSL2"),
            None
        ));
        assert!(is_wsl_kernel(None, Some("Linux version with WSL marker")));
        assert!(!is_wsl_kernel(Some("6.8.0-generic"), Some("Linux version")));
    }

    #[test]
    fn parses_proc_stat_fields_after_command_name() {
        let parsed = parse_proc_stat("123 (bash with spaces) S 1 2 3 34816 5 4194304");
        assert_eq!(
            parsed,
            ProcStatSummary {
                state: Some("S".to_string()),
                ppid: Some(1),
                pgrp: Some(2),
                session: Some(3),
                tty_nr: Some(34816),
            }
        );
    }

    #[test]
    fn parses_apt_history_block() {
        let event = parse_apt_history_block(
            Path::new("/var/log/apt/history.log"),
            "Start-Date: 2026-05-17  12:00:00\nCommandline: apt install git\nInstall: git:amd64\nEnd-Date: 2026-05-17  12:00:02\n",
        )
        .expect("apt history event");

        assert_eq!(event["commandline"], "apt install git");
        assert_eq!(event["install"], "git:amd64");
    }

    #[test]
    fn skips_expensive_recent_file_directories() {
        assert!(skip_recent_dir(".git"));
        assert!(skip_recent_dir("node_modules"));
        assert!(!skip_recent_dir("Downloads"));
    }
}
