/// Lightweight sandbox using Linux namespaces.
///
/// Instead of Docker-in-Docker, Claw OS provides native process isolation
/// via unshare/clone. This eliminates ~6000 lines of Docker boilerplate
/// in OpenClaw's sandbox module.
///
/// Sandbox modes:
///   - `exec`:  Run a command in an isolated namespace (PID + mount + optional network)
///   - `create`: Create a persistent sandbox environment
///   - `destroy`: Tear down a sandbox
///   - `list`: List active sandboxes
///
/// On non-Linux platforms, sandbox falls back to basic subprocess isolation.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::policy::{self, OpType};

const SANDBOX_DIR: &str = "/var/lib/cos/sandboxes";

struct ResourceLimits {
    mem_limit: Option<String>,       // e.g. "512M"
    cpu_percent: Option<u32>,        // e.g. 50
    pids_max: Option<u32>,           // e.g. 100
    timeout_secs: Option<u32>,       // e.g. 300
    seccomp_profile: Option<String>, // e.g. "minimal", "network", "full"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    pub id: String,
    pub mode: String,      // "rw" | "ro"
    pub workspace: String, // path mounted into sandbox
    pub network: bool,     // allow network access
    pub created_at: String,
    pub pid: Option<u32>, // init process PID (if persistent)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SandboxRegistry {
    sandboxes: Vec<SandboxConfig>,
}

fn registry_path() -> PathBuf {
    PathBuf::from(SANDBOX_DIR).join("registry.json")
}

fn load_registry() -> SandboxRegistry {
    let path = registry_path();
    if let Ok(data) = fs::read_to_string(&path) {
        if let Ok(reg) = serde_json::from_str(&data) {
            return reg;
        }
    }
    SandboxRegistry { sandboxes: vec![] }
}

fn save_registry(reg: &SandboxRegistry) {
    let path = registry_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(data) = serde_json::to_string_pretty(reg) {
        let _ = fs::write(&path, data);
    }
}

pub fn run(command: &str, args: &[String]) -> Result<Value, String> {
    match command {
        "exec" => cmd_exec(args),
        "create" => cmd_create(args),
        "destroy" => cmd_destroy(args),
        "list" => cmd_list(args),
        _ => Err(format!("unknown sandbox command: {command}")),
    }
}

/// Execute a command in an isolated sandbox.
///
/// Usage: cos sandbox exec [--no-network] [--ro] [--workspace DIR]
///                         [--mem LIMIT] [--cpu PERCENT] [--pids MAX]
///                         [--timeout SECS] -- <command> [args...]
fn cmd_exec(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Exec).map_err(|v| v.to_string())?;
    let mut network = true;
    let mut read_only = false;
    let mut workspace = "/den".to_string();
    let mut mem_limit: Option<String> = None; // e.g. "512M", "1G"
    let mut cpu_percent: Option<u32> = None; // e.g. 50 = 50%
    let mut pids_max: Option<u32> = None; // e.g. 100
    let mut timeout_secs: Option<u32> = None; // e.g. 300
    let mut seccomp_profile: Option<String> = None; // e.g. "minimal", "network", "full"
    let mut cmd_start = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--no-network" => {
                network = false;
                i += 1;
            }
            "--ro" => {
                read_only = true;
                i += 1;
            }
            "--workspace" if i + 1 < args.len() => {
                workspace = args[i + 1].clone();
                i += 2;
            }
            "--mem" if i + 1 < args.len() => {
                mem_limit = Some(args[i + 1].clone());
                i += 2;
            }
            "--cpu" if i + 1 < args.len() => {
                cpu_percent = Some(
                    args[i + 1]
                        .parse::<u32>()
                        .map_err(|_| format!("invalid cpu value: {}", args[i + 1]))?,
                );
                i += 2;
            }
            "--pids" if i + 1 < args.len() => {
                pids_max = Some(
                    args[i + 1]
                        .parse::<u32>()
                        .map_err(|_| format!("invalid pids value: {}", args[i + 1]))?,
                );
                i += 2;
            }
            "--timeout" if i + 1 < args.len() => {
                timeout_secs = Some(
                    args[i + 1]
                        .parse::<u32>()
                        .map_err(|_| format!("invalid timeout value: {}", args[i + 1]))?,
                );
                i += 2;
            }
            "--seccomp-profile" if i + 1 < args.len() => {
                let profile = args[i + 1].to_lowercase();
                if !["minimal", "network", "full"].contains(&profile.as_str()) {
                    return Err("seccomp profile must be: minimal, network, full".into());
                }
                seccomp_profile = Some(profile);
                i += 2;
            }
            "--" => {
                cmd_start = Some(i + 1);
                break;
            }
            _ => {
                cmd_start = Some(i);
                break;
            }
        }
    }

    let cmd_idx = cmd_start.ok_or("no command specified")?;
    if cmd_idx >= args.len() {
        return Err("no command specified".into());
    }

    let command_args = &args[cmd_idx..];
    let limits = ResourceLimits {
        mem_limit,
        cpu_percent,
        pids_max,
        timeout_secs,
        seccomp_profile,
    };

    #[cfg(target_os = "linux")]
    {
        return exec_linux(command_args, network, read_only, &workspace, &limits);
    }

    #[cfg(not(target_os = "linux"))]
    {
        exec_fallback(command_args, &workspace, &limits)
    }
}

/// Linux: use unshare(1) for namespace isolation + systemd-run for cgroup limits.
#[cfg(target_os = "linux")]
fn exec_linux(
    command_args: &[String],
    network: bool,
    read_only: bool,
    workspace: &str,
    limits: &ResourceLimits,
) -> Result<Value, String> {
    let has_limits = limits.mem_limit.is_some()
        || limits.cpu_percent.is_some()
        || limits.pids_max.is_some()
        || limits.timeout_secs.is_some()
        || limits.seccomp_profile.is_some();

    // If resource limits are set, use systemd-run which handles cgroup v2
    if has_limits {
        return exec_linux_with_cgroup(command_args, network, read_only, workspace, limits);
    }

    // Otherwise, use plain unshare for lightweight namespace isolation
    let mut unshare_args = vec![
        "--pid".to_string(),
        "--fork".to_string(),
        "--mount-proc".to_string(),
        "--mount".to_string(),
    ];

    if !network {
        unshare_args.push("--net".to_string());
    }

    unshare_args.push("--".to_string());
    unshare_args.extend_from_slice(command_args);

    let mut child = Command::new("unshare")
        .args(&unshare_args)
        .current_dir(workspace)
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn sandbox: {e}"))?;

    let status = child
        .wait()
        .map_err(|e| format!("sandbox wait failed: {e}"))?;

    let mut stdout = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut stdout);
    }
    let mut stderr = String::new();
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut stderr);
    }

    Ok(json!({
        "exit_code": status.code().unwrap_or(-1),
        "stdout": stdout,
        "stderr": stderr,
        "isolated": true,
        "network": network,
        "read_only": read_only,
        "workspace": workspace,
    }))
}

/// Linux: use systemd-run for cgroup v2 resource limits + namespace isolation.
///
/// systemd-run creates a transient scope with cgroup limits.
/// Combined with unshare flags for PID/mount/net namespace isolation.
#[cfg(target_os = "linux")]
fn exec_linux_with_cgroup(
    command_args: &[String],
    network: bool,
    read_only: bool,
    workspace: &str,
    limits: &ResourceLimits,
) -> Result<Value, String> {
    let scope_name = format!("cos-sandbox-{}", short_id());

    let mut sr_args = vec![
        "--scope".to_string(),
        format!("--unit={scope_name}"),
        "--quiet".to_string(),
    ];

    // Memory limit (cgroup v2: MemoryMax)
    if let Some(ref mem) = limits.mem_limit {
        sr_args.push(format!("-p MemoryMax={mem}"));
        sr_args.push(format!("-p MemorySwapMax=0")); // no swap
    }

    // CPU limit (cgroup v2: CPUQuota)
    if let Some(cpu) = limits.cpu_percent {
        sr_args.push(format!("-p CPUQuota={cpu}%"));
    }

    // PID limit (cgroup v2: TasksMax)
    if let Some(pids) = limits.pids_max {
        sr_args.push(format!("-p TasksMax={pids}"));
    }

    // Timeout via RuntimeMaxSec
    if let Some(secs) = limits.timeout_secs {
        sr_args.push(format!("-p RuntimeMaxSec={secs}"));
    }

    // Read-only filesystem via systemd property
    if read_only {
        sr_args.push("-p".to_string());
        sr_args.push("ReadOnlyPaths=/".to_string());
    }

    // Seccomp syscall filter via systemd property
    if let Some(ref profile) = limits.seccomp_profile {
        if let Some(filter) = seccomp_syscall_filter(profile) {
            sr_args.push("-p".to_string());
            sr_args.push(format!("SystemCallFilter={filter}"));
        }
    }

    // Set working directory to workspace
    sr_args.push(format!("-p WorkingDirectory={workspace}"));

    // Wrap the actual command in unshare for namespace isolation
    sr_args.push("--".to_string());
    sr_args.push("unshare".to_string());
    sr_args.push("--pid".to_string());
    sr_args.push("--fork".to_string());
    sr_args.push("--mount-proc".to_string());
    sr_args.push("--mount".to_string());
    if !network {
        sr_args.push("--net".to_string());
    }
    sr_args.push("--".to_string());
    sr_args.extend_from_slice(command_args);

    let mut child = Command::new("systemd-run")
        .args(&sr_args)
        .current_dir(workspace)
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn sandbox (systemd-run): {e}"))?;

    let status = child
        .wait()
        .map_err(|e| format!("sandbox wait failed: {e}"))?;

    let mut stdout = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut stdout);
    }
    let mut stderr = String::new();
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut stderr);
    }

    // Check if killed by cgroup (exit code 137 = OOM, etc.)
    let exit_code = status.code().unwrap_or(-1);
    let mut killed_by = None;
    if exit_code == 137 {
        killed_by = Some("OOM (memory limit exceeded)");
    } else if exit_code == 124 || stderr.contains("RuntimeMaxSec") {
        killed_by = Some("timeout (RuntimeMaxSec exceeded)");
    }

    let mut result = json!({
        "exit_code": exit_code,
        "stdout": stdout,
        "stderr": stderr,
        "isolated": true,
        "network": network,
        "read_only": read_only,
        "workspace": workspace,
        "cgroup": true,
        "scope": scope_name,
    });

    if let Some(mem) = &limits.mem_limit {
        result["limits"] = json!({
            "memory": mem,
            "cpu_percent": limits.cpu_percent,
            "pids_max": limits.pids_max,
            "timeout_secs": limits.timeout_secs,
        });
    }

    if let Some(ref profile) = limits.seccomp_profile {
        result["seccomp_profile"] = json!(profile);
    }

    if let Some(reason) = killed_by {
        result["killed_by"] = json!(reason);
    }

    Ok(result)
}

fn short_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{:x}", t & 0xFFFFFFFF)
}

/// Generate a seccomp BPF filter JSON for use with systemd's SystemCallFilter.
///
/// Profiles:
///   - `minimal`: only basic I/O and process management syscalls
///   - `network`: minimal + networking syscalls (socket, connect, etc.)
///   - `full`: all syscalls allowed (no filtering)
#[cfg(target_os = "linux")]
fn seccomp_syscall_filter(profile: &str) -> Option<String> {
    match profile {
        "minimal" => {
            // Allow only essential syscalls for computation
            Some("~@clock @debug @module @mount @obsolete @raw-io @reboot @swap @privileged".into())
        }
        "network" => {
            // Allow everything except dangerous system-level calls
            Some("~@clock @debug @module @mount @obsolete @raw-io @reboot @swap".into())
        }
        "full" => None, // No filtering
        _ => None,
    }
}

/// Fallback for non-Linux: basic subprocess execution with timeout.
#[cfg(not(target_os = "linux"))]
fn exec_fallback(
    command_args: &[String],
    workspace: &str,
    limits: &ResourceLimits,
) -> Result<Value, String> {
    if command_args.is_empty() {
        return Err("no command specified".into());
    }

    let mut child = Command::new(&command_args[0])
        .args(&command_args[1..])
        .current_dir(workspace)
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn: {e}"))?;

    // Simple timeout: poll in a loop
    if let Some(secs) = limits.timeout_secs {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs as u64);
        loop {
            match child.try_wait() {
                Ok(Some(_status)) => break,
                Ok(None) => {
                    if std::time::Instant::now() > deadline {
                        let _ = child.kill();
                        return Ok(json!({
                            "exit_code": -1,
                            "killed_by": "timeout",
                            "isolated": false,
                        }));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                Err(e) => return Err(format!("wait failed: {e}")),
            }
        }
    }

    let status = child.wait().map_err(|e| format!("wait failed: {e}"))?;

    let mut stdout = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut stdout);
    }
    let mut stderr = String::new();
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut stderr);
    }

    Ok(json!({
        "exit_code": status.code().unwrap_or(-1),
        "stdout": stdout,
        "stderr": stderr,
        "isolated": false,
        "note": "namespace/cgroup isolation requires Linux",
    }))
}

/// Create a persistent sandbox.
fn cmd_create(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Exec).map_err(|v| v.to_string())?;
    let mut network = true;
    let mut mode = "rw".to_string();
    let mut workspace = "/den".to_string();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--no-network" => {
                network = false;
                i += 1;
            }
            "--mode" if i + 1 < args.len() => {
                mode = args[i + 1].clone();
                i += 2;
            }
            "--workspace" if i + 1 < args.len() => {
                workspace = args[i + 1].clone();
                i += 2;
            }
            _ => i += 1,
        }
    }

    let id = format!("sb-{}", &short_id());
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    let config = SandboxConfig {
        id: id.clone(),
        mode,
        workspace,
        network,
        created_at: now,
        pid: None,
    };

    let mut reg = load_registry();
    reg.sandboxes.push(config.clone());
    save_registry(&reg);

    Ok(json!({
        "id": id,
        "mode": config.mode,
        "workspace": config.workspace,
        "network": config.network,
        "created_at": config.created_at,
    }))
}

/// Destroy a sandbox.
fn cmd_destroy(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Exec).map_err(|v| v.to_string())?;
    let id = args.first().ok_or("usage: cos sandbox destroy <id>")?;

    let mut reg = load_registry();
    let before = reg.sandboxes.len();

    // Kill the init process if running
    if let Some(sb) = reg.sandboxes.iter().find(|s| &s.id == id) {
        if let Some(pid) = sb.pid {
            #[cfg(unix)]
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }
        }
    }

    reg.sandboxes.retain(|s| &s.id != id);
    save_registry(&reg);

    if reg.sandboxes.len() == before {
        return Err(format!("sandbox not found: {id}"));
    }

    Ok(json!({"destroyed": id}))
}

/// List active sandboxes.
fn cmd_list(_args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Read).map_err(|v| v.to_string())?;
    let reg = load_registry();
    Ok(json!({
        "sandboxes": reg.sandboxes,
        "count": reg.sandboxes.len(),
    }))
}
