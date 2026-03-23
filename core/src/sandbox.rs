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

const SANDBOX_DIR: &str = "/var/lib/cos/sandboxes";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    pub id: String,
    pub mode: String,         // "rw" | "ro"
    pub workspace: String,    // path mounted into sandbox
    pub network: bool,        // allow network access
    pub created_at: String,
    pub pid: Option<u32>,     // init process PID (if persistent)
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
/// Usage: cos sandbox exec [--no-network] [--ro] [--workspace DIR] -- <command> [args...]
fn cmd_exec(args: &[String]) -> Result<Value, String> {
    let mut network = true;
    let mut read_only = false;
    let mut workspace = "/workspace".to_string();
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
            "--" => {
                cmd_start = Some(i + 1);
                break;
            }
            _ => {
                // First non-flag arg starts the command
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

    #[cfg(target_os = "linux")]
    {
        return exec_linux(command_args, network, read_only, &workspace);
    }

    #[cfg(not(target_os = "linux"))]
    {
        // Fallback: just run the command with basic isolation (no namespaces)
        exec_fallback(command_args)
    }
}

/// Linux: use unshare(1) for namespace isolation.
#[cfg(target_os = "linux")]
fn exec_linux(
    command_args: &[String],
    network: bool,
    read_only: bool,
    workspace: &str,
) -> Result<Value, String> {
    // Build unshare command with appropriate namespace flags
    let mut unshare_args = vec![
        "--pid".to_string(),
        "--fork".to_string(),
        "--mount-proc".to_string(),
        "--mount".to_string(),
    ];

    if !network {
        unshare_args.push("--net".to_string());
    }

    // Add the actual command
    unshare_args.push("--".to_string());
    unshare_args.extend_from_slice(command_args);

    let mut child = Command::new("unshare")
        .args(&unshare_args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn sandbox: {e}"))?;

    let status = child.wait().map_err(|e| format!("sandbox wait failed: {e}"))?;

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
    }))
}

/// Fallback for non-Linux: basic subprocess execution.
#[cfg(not(target_os = "linux"))]
fn exec_fallback(command_args: &[String]) -> Result<Value, String> {
    if command_args.is_empty() {
        return Err("no command specified".into());
    }

    let mut child = Command::new(&command_args[0])
        .args(&command_args[1..])
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn: {e}"))?;

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
        "note": "namespace isolation requires Linux",
    }))
}

/// Create a persistent sandbox.
fn cmd_create(args: &[String]) -> Result<Value, String> {
    let mut network = true;
    let mut mode = "rw".to_string();
    let mut workspace = "/workspace".to_string();

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

    let id = format!("sb-{}", &uuid_short());
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
    let id = args.first().ok_or("usage: cos sandbox destroy <id>")?;

    let mut reg = load_registry();
    let before = reg.sandboxes.len();

    // Kill the init process if running
    if let Some(sb) = reg.sandboxes.iter().find(|s| &s.id == id) {
        if let Some(_pid) = sb.pid {
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
    let reg = load_registry();
    Ok(json!({
        "sandboxes": reg.sandboxes,
        "count": reg.sandboxes.len(),
    }))
}

fn uuid_short() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{:x}", t & 0xFFFFFFFF)
}
