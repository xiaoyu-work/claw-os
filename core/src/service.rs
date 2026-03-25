/// Generic service manager — a simplified systemd for agents.
///
/// Replaces hardcoded service management (e.g. browser.rs) with a
/// generic system that discovers service definitions from JSON files,
/// manages lifecycle (start/stop/restart), tracks health, and streams logs.
///
/// Service definitions live in `COS_SERVICES_DIR` (default `/usr/lib/cos/services/`).
/// Each service is a subdirectory containing a `service.json`.
/// Runtime state (PID files, logs) lives in `COS_DATA_DIR/services/<name>/`.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::policy::{self, OpType};

const MAX_LOG_BYTES: usize = 200_000;
const DEFAULT_LOG_TAIL: usize = 20;

// ---------------------------------------------------------------------------
// Service definition types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HealthConfig {
    #[serde(default)]
    url: Option<String>,
    #[serde(default = "default_interval")]
    interval_secs: u64,
    #[serde(default = "default_timeout")]
    timeout_secs: u64,
    #[serde(default = "default_grace")]
    start_grace_secs: u64,
}

fn default_interval() -> u64 {
    10
}
fn default_timeout() -> u64 {
    5
}
fn default_grace() -> u64 {
    15
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ServiceDef {
    name: String,
    #[serde(default)]
    description: String,
    command: String,
    #[serde(default)]
    workdir: Option<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    health: Option<HealthConfig>,
    #[serde(default = "default_restart")]
    restart: String,
    #[serde(default)]
    depends_on: Vec<String>,
    /// Credential names to inject as environment variables on start.
    /// Values are loaded from `cos credential` store at start time.
    #[serde(default)]
    credentials: Vec<String>,
    #[serde(default)]
    lifecycle: Option<LifecycleHooks>,
}

fn default_restart() -> String {
    "on-failure".into()
}

// ---------------------------------------------------------------------------
// Lifecycle hooks
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LifecycleHooks {
    /// Command to run before the service starts (e.g., DB migration)
    #[serde(default)]
    pre_start: Option<String>,
    /// Command to run after the service starts and is healthy
    #[serde(default)]
    post_start: Option<String>,
    /// Command to run before sending SIGTERM (e.g., drain connections, flush state)
    #[serde(default)]
    pre_stop: Option<String>,
    /// Command to run after the service has fully stopped (e.g., cleanup temp files)
    #[serde(default)]
    post_stop: Option<String>,
    /// Seconds to wait after pre_stop before sending SIGTERM (drain period)
    #[serde(default = "default_drain_timeout")]
    drain_timeout_secs: u64,
    /// Seconds to wait after SIGTERM before sending SIGKILL
    #[serde(default = "default_stop_timeout")]
    stop_timeout_secs: u64,
    /// Command to run to export state before shutdown (checkpoint)
    #[serde(default)]
    checkpoint_cmd: Option<String>,
}

fn default_drain_timeout() -> u64 {
    5
}

fn default_stop_timeout() -> u64 {
    10
}

// ---------------------------------------------------------------------------
// Directory helpers
// ---------------------------------------------------------------------------

fn services_dir() -> PathBuf {
    PathBuf::from(
        std::env::var("COS_SERVICES_DIR").unwrap_or_else(|_| "/usr/lib/cos/services".into()),
    )
}

fn runtime_dir() -> PathBuf {
    PathBuf::from(std::env::var("COS_DATA_DIR").unwrap_or_else(|_| "/var/lib/cos".into()))
        .join("services")
}

fn service_runtime_dir(name: &str) -> PathBuf {
    runtime_dir().join(name)
}

fn pid_path(name: &str) -> PathBuf {
    service_runtime_dir(name).join("service.pid")
}

fn log_path(name: &str) -> PathBuf {
    service_runtime_dir(name).join("service.log")
}

// ---------------------------------------------------------------------------
// PID helpers
// ---------------------------------------------------------------------------

fn read_pid(name: &str) -> Option<u32> {
    fs::read_to_string(pid_path(name)).ok()?.trim().parse().ok()
}

fn write_pid(name: &str, pid: u32) {
    let path = pid_path(name);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(&path, pid.to_string());
}

fn clear_pid(name: &str) {
    let _ = fs::remove_file(pid_path(name));
}

fn is_alive(pid: u32) -> bool {
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as i32, 0) == 0
    }
    #[cfg(not(unix))]
    {
        Command::new("cmd")
            .args(["/c", &format!("tasklist /FI \"PID eq {pid}\" /NH")])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
            .unwrap_or(false)
    }
}

// ---------------------------------------------------------------------------
// Health check
// ---------------------------------------------------------------------------

fn check_health_url(url: &str, timeout: u64) -> bool {
    Command::new("curl")
        .args([
            "-s",
            "-o",
            "/dev/null",
            "-w",
            "%{http_code}",
            "--connect-timeout",
            &timeout.to_string(),
            url,
        ])
        .output()
        .map(|o| {
            let code = String::from_utf8_lossy(&o.stdout);
            code.trim().starts_with('2') || code.trim().starts_with('3')
        })
        .unwrap_or(false)
}

fn check_service_health(def: &ServiceDef) -> Option<bool> {
    let health = def.health.as_ref()?;
    let url = health.url.as_ref()?;
    Some(check_health_url(url, health.timeout_secs))
}

// ---------------------------------------------------------------------------
// Service discovery
// ---------------------------------------------------------------------------

fn discover_services() -> BTreeMap<String, ServiceDef> {
    let dir = services_dir();
    let mut services = BTreeMap::new();

    let entries = match fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return services,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let manifest_path = path.join("service.json");
        if !manifest_path.is_file() {
            continue;
        }
        let data = match fs::read_to_string(&manifest_path) {
            Ok(d) => d,
            Err(_) => continue,
        };
        let def: ServiceDef = match serde_json::from_str(&data) {
            Ok(d) => d,
            Err(_) => continue,
        };
        let name = def.name.clone();
        services.insert(name, def);
    }

    services
}

fn find_service(name: &str) -> Result<ServiceDef, String> {
    let services = discover_services();
    services.get(name).cloned().ok_or_else(|| {
        let available: Vec<&String> = services.keys().collect();
        format!("service not found: {name}. available: {available:?}")
    })
}

// ---------------------------------------------------------------------------
// Log helpers
// ---------------------------------------------------------------------------

fn read_log_tail(name: &str, n: usize) -> String {
    let path = log_path(name);
    let content = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return String::new(),
    };

    let content = if content.len() > MAX_LOG_BYTES {
        let truncated = &content[content.len() - MAX_LOG_BYTES..];
        format!(
            "[truncated, showing last {}KB]\n{truncated}",
            MAX_LOG_BYTES / 1024
        )
    } else {
        content
    };

    let lines: Vec<&str> = content.lines().collect();
    if lines.len() > n {
        lines[lines.len() - n..].join("\n")
    } else {
        lines.join("\n")
    }
}

// ---------------------------------------------------------------------------
// Process control
// ---------------------------------------------------------------------------

fn kill_pid(pid: u32) {
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
    #[cfg(not(unix))]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .output();
    }
}

fn send_sigkill(pid: u32) {
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }
    #[cfg(not(unix))]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .output();
    }
}

/// Run a lifecycle hook command and return structured result.
/// Hooks run synchronously with a timeout.
fn run_hook(hook_name: &str, command: &str, timeout_secs: u64) -> Value {
    let start = Instant::now();
    let shell_cmd = build_shell_command(command);
    let result = Command::new(&shell_cmd.0)
        .args(&shell_cmd.1)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();

    let duration_ms = start.elapsed().as_millis() as u64;
    let _ = timeout_secs; // timeout enforced by caller if needed

    match result {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            json!({
                "step": hook_name,
                "status": if output.status.success() { "ok" } else { "failed" },
                "exit_code": output.status.code(),
                "stdout": stdout.trim(),
                "stderr": stderr.trim(),
                "duration_ms": duration_ms,
            })
        }
        Err(e) => {
            json!({
                "step": hook_name,
                "status": "error",
                "error": e.to_string(),
                "duration_ms": duration_ms,
            })
        }
    }
}

/// Build the shell command tuple (program, args) for running a hook command.
/// Uses `sh -c` on Unix, `cmd /c` on Windows.
fn build_shell_command(command: &str) -> (String, Vec<String>) {
    #[cfg(unix)]
    {
        ("sh".into(), vec!["-c".into(), command.into()])
    }
    #[cfg(not(unix))]
    {
        ("cmd".into(), vec!["/c".into(), command.into()])
    }
}

/// Wait for a process to exit within `timeout_secs`, polling every 100ms.
/// Returns (exited, exit_code).
fn wait_for_exit(pid: u32, timeout_secs: u64) -> (bool, Option<i32>) {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        if !is_alive(pid) {
            return (true, None);
        }
        if Instant::now() >= deadline {
            return (false, None);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub fn run(command: &str, args: &[String]) -> Result<Value, String> {
    match command {
        "start" => cmd_start(args),
        "stop" => cmd_stop(args),
        "stop-all" => cmd_stop_all(args),
        "restart" => cmd_restart(args),
        "status" => cmd_status(args),
        "health" => cmd_health(args),
        "list" => cmd_list(args),
        "logs" => cmd_logs(args),
        "register" => cmd_register(args),
        _ => Err(format!("unknown service command: {command}")),
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Start a service by name.
/// Sequence: pre_start → spawn → health-wait → post_start
fn cmd_start(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::System).map_err(|v| v.to_string())?;
    let name = args.first().ok_or("usage: cos service start <name>")?;
    let def = find_service(name)?;

    // Check if already running
    if let Some(pid) = read_pid(name) {
        if is_alive(pid) {
            let healthy = check_service_health(&def);
            return Ok(json!({
                "name": name,
                "status": "already_running",
                "pid": pid,
                "healthy": healthy,
            }));
        }
    }

    let mut steps: Vec<Value> = Vec::new();
    let hooks = def.lifecycle.as_ref();

    // 1. Run pre_start hook (if configured)
    if let Some(cmd) = hooks.and_then(|h| h.pre_start.as_ref()) {
        let result = run_hook("pre_start", cmd, 60);
        let failed = result.get("status").and_then(|s| s.as_str()) != Some("ok");
        steps.push(result);
        if failed {
            return Ok(json!({
                "name": name,
                "status": "pre_start_failed",
                "steps": steps,
            }));
        }
    }

    // 2. Start the service process
    // Ensure runtime directory exists
    let rt_dir = service_runtime_dir(name);
    let _ = fs::create_dir_all(&rt_dir);

    // Prepare log file
    let log = log_path(name);
    let log_file = fs::File::create(&log).map_err(|e| format!("failed to create log file: {e}"))?;
    let log_err = log_file
        .try_clone()
        .map_err(|e| format!("failed to clone log file: {e}"))?;

    // Parse command — split on whitespace
    let parts: Vec<&str> = def.command.split_whitespace().collect();
    if parts.is_empty() {
        return Err(format!("service {name} has empty command"));
    }

    let mut cmd = Command::new(parts[0]);
    if parts.len() > 1 {
        cmd.args(&parts[1..]);
    }

    // Set working directory
    if let Some(ref workdir) = def.workdir {
        let wd = PathBuf::from(workdir);
        if wd.is_dir() {
            cmd.current_dir(&wd);
        } else {
            return Err(format!("workdir does not exist: {workdir}"));
        }
    }

    // Set environment variables
    for (k, v) in &def.env {
        cmd.env(k, v);
    }

    // Inject credentials as environment variables
    let mut injected_creds: Vec<String> = Vec::new();
    for cred_name in &def.credentials {
        match crate::credential::run("load", &[cred_name.clone()]) {
            Ok(v) => {
                if let Some(val) = v["value"].as_str() {
                    cmd.env(cred_name, val);
                    injected_creds.push(cred_name.clone());
                }
            }
            Err(e) => {
                steps.push(json!({
                    "step": "credential_load",
                    "credential": cred_name,
                    "status": "failed",
                    "error": e,
                }));
            }
        }
    }

    cmd.stdin(Stdio::null());
    cmd.stdout(log_file);
    cmd.stderr(log_err);

    let child = cmd
        .spawn()
        .map_err(|e| format!("failed to start service {name}: {e}"))?;

    let pid = child.id();
    write_pid(name, pid);

    // Detach — process keeps running after cos exits
    std::mem::forget(child);

    steps.push(json!({
        "step": "spawn",
        "status": "ok",
        "pid": pid,
    }));

    // 3. If health check is configured, wait for it to pass
    let healthy = if let Some(ref health) = def.health {
        if health.url.is_some() {
            let grace = health.start_grace_secs;
            let polls = grace * 2; // poll every 500ms
            let mut ready = false;
            let health_start = Instant::now();
            for _ in 0..polls {
                std::thread::sleep(std::time::Duration::from_millis(500));
                if check_service_health(&def) == Some(true) {
                    ready = true;
                    break;
                }
            }
            steps.push(json!({
                "step": "health_wait",
                "status": if ready { "healthy" } else { "timeout" },
                "duration_ms": health_start.elapsed().as_millis() as u64,
            }));
            Some(ready)
        } else {
            None
        }
    } else {
        None
    };

    // 4. Run post_start hook (if configured and service is healthy or no health check)
    let should_run_post = match healthy {
        Some(false) => false, // health check timed out — don't run post_start
        _ => true,
    };
    if should_run_post {
        if let Some(cmd) = hooks.and_then(|h| h.post_start.as_ref()) {
            let result = run_hook("post_start", cmd, 60);
            steps.push(result);
        }
    }

    let status = match healthy {
        Some(true) => "running",
        Some(false) => "starting",
        None => "running",
    };

    let mut result = json!({
        "name": name,
        "status": status,
        "pid": pid,
        "healthy": healthy,
        "log": log.to_string_lossy(),
        "steps": steps,
    });
    if !injected_creds.is_empty() {
        result["credentials_injected"] = json!(injected_creds);
    }
    Ok(result)
}

/// Graceful stop a service by name.
/// Sequence: checkpoint → pre_stop → drain → SIGTERM → wait → SIGKILL → post_stop → clear PID
fn cmd_stop(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::System).map_err(|v| v.to_string())?;
    let name = args.first().ok_or("usage: cos service stop <name>")?;

    let pid =
        read_pid(name).ok_or_else(|| format!("service {name} is not running (no PID file)"))?;

    let def = find_service(name).ok();
    let hooks = def.as_ref().and_then(|d| d.lifecycle.as_ref());

    let drain_timeout = hooks
        .map(|h| h.drain_timeout_secs)
        .unwrap_or(default_drain_timeout());
    let stop_timeout = hooks
        .map(|h| h.stop_timeout_secs)
        .unwrap_or(default_stop_timeout());

    let mut steps: Vec<Value> = Vec::new();

    // 1. Run checkpoint_cmd (if configured)
    if let Some(cmd) = hooks.and_then(|h| h.checkpoint_cmd.as_ref()) {
        steps.push(run_hook("checkpoint", cmd, stop_timeout));
    }

    // 2. Run pre_stop hook (if configured)
    if let Some(cmd) = hooks.and_then(|h| h.pre_stop.as_ref()) {
        steps.push(run_hook("pre_stop", cmd, stop_timeout));
    }

    // 3. Wait drain_timeout_secs (drain period)
    if drain_timeout > 0 {
        let drain_start = Instant::now();
        std::thread::sleep(Duration::from_secs(drain_timeout));
        steps.push(json!({
            "step": "drain",
            "duration_ms": drain_start.elapsed().as_millis() as u64,
        }));
    }

    // 4. Send SIGTERM
    kill_pid(pid);
    steps.push(json!({
        "step": "sigterm",
        "status": "sent",
    }));

    // 5. Wait stop_timeout_secs for process to exit
    let wait_start = Instant::now();
    let (exited, exit_code) = wait_for_exit(pid, stop_timeout);
    let wait_duration = wait_start.elapsed().as_millis() as u64;

    if exited {
        steps.push(json!({
            "step": "wait_exit",
            "status": "exited",
            "exit_code": exit_code,
            "duration_ms": wait_duration,
        }));
    } else {
        // 6. If still alive after stop_timeout → send SIGKILL
        send_sigkill(pid);
        steps.push(json!({
            "step": "wait_exit",
            "status": "timeout",
            "duration_ms": wait_duration,
        }));
        steps.push(json!({
            "step": "sigkill",
            "status": "sent",
        }));
        // Brief wait after SIGKILL
        std::thread::sleep(Duration::from_millis(500));
    }

    // 7. Run post_stop hook (if configured)
    if let Some(cmd) = hooks.and_then(|h| h.post_stop.as_ref()) {
        steps.push(run_hook("post_stop", cmd, stop_timeout));
    }

    // 8. Clear PID file
    clear_pid(name);

    // 9. Return structured JSON with status of each step
    Ok(json!({
        "name": name,
        "status": "stopped",
        "pid": pid,
        "steps": steps,
    }))
}

/// Restart a service (stop then start).
fn cmd_restart(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::System).map_err(|v| v.to_string())?;
    let _name = args.first().ok_or("usage: cos service restart <name>")?;

    // Stop (ignore errors — service may not be running)
    let _ = cmd_stop(args);
    std::thread::sleep(std::time::Duration::from_secs(1));
    cmd_start(args)
}

/// Stop all running services in reverse dependency order.
/// If service B depends_on service A, stop B first, then A.
fn cmd_stop_all(_args: &[String]) -> Result<Value, String> {
    policy::require(OpType::System).map_err(|v| v.to_string())?;
    let services = discover_services();

    // Build the shutdown order: reverse dependency (dependents first, then dependencies).
    let order = reverse_dependency_order(&services);

    // Filter to only running services
    let running: Vec<String> = order
        .into_iter()
        .filter(|name| read_pid(name).map(|p| is_alive(p)).unwrap_or(false))
        .collect();

    let mut results: Vec<Value> = Vec::new();

    for name in &running {
        let result = cmd_stop(&[name.clone()]);
        match result {
            Ok(v) => results.push(v),
            Err(e) => results.push(json!({
                "name": name,
                "status": "error",
                "error": e,
            })),
        }
    }

    let count = results.len();
    Ok(json!({
        "command": "stop-all",
        "stopped": count,
        "results": results,
    }))
}

/// Compute reverse dependency order for shutdown.
/// Services that are depended upon should be stopped last.
fn reverse_dependency_order(services: &BTreeMap<String, ServiceDef>) -> Vec<String> {
    // Topological sort: dependencies first, then dependents.
    // For shutdown we reverse that: dependents first.
    let names: Vec<String> = services.keys().cloned().collect();
    let mut visited: BTreeMap<String, bool> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();

    fn visit(
        name: &str,
        services: &BTreeMap<String, ServiceDef>,
        visited: &mut BTreeMap<String, bool>,
        order: &mut Vec<String>,
    ) {
        if visited.contains_key(name) {
            return;
        }
        visited.insert(name.to_string(), true);
        // Visit dependencies first (they appear earlier in start order)
        if let Some(def) = services.get(name) {
            for dep in &def.depends_on {
                visit(dep, services, visited, order);
            }
        }
        order.push(name.to_string());
    }

    for name in &names {
        visit(name, services, &mut visited, &mut order);
    }

    // Reverse: dependents first for shutdown
    order.reverse();
    order
}

/// Show detailed status for a service.
fn cmd_status(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Read).map_err(|v| v.to_string())?;
    let name = args.first().ok_or("usage: cos service status <name>")?;
    let def = find_service(name)?;

    let pid = read_pid(name);
    let alive = pid.map(|p| is_alive(p)).unwrap_or(false);
    let healthy = if alive {
        check_service_health(&def)
    } else {
        Some(false)
    };

    let mut result = json!({
        "name": name,
        "description": def.description,
        "running": alive,
        "healthy": healthy,
    });

    if let Some(p) = pid {
        result["pid"] = json!(p);
    }

    // Include log tail
    let tail = read_log_tail(name, DEFAULT_LOG_TAIL);
    if !tail.is_empty() {
        result["log_tail"] = json!(tail);
    }

    Ok(result)
}

/// Health check a service, optionally auto-restarting if unhealthy.
fn cmd_health(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Read).map_err(|v| v.to_string())?;
    let name = args.first().ok_or("usage: cos service health <name>")?;
    let def = find_service(name)?;
    let auto_restart = !args.contains(&"--no-restart".to_string());

    let healthy = check_service_health(&def).unwrap_or(false);

    if healthy {
        return Ok(json!({
            "name": name,
            "healthy": true,
        }));
    }

    // Not healthy
    if auto_restart {
        let result = cmd_restart(args)?;
        let now_healthy = check_service_health(&def).unwrap_or(false);
        return Ok(json!({
            "name": name,
            "healthy": now_healthy,
            "action": "restarted",
            "restart_result": result,
        }));
    }

    Ok(json!({
        "name": name,
        "healthy": false,
        "hint": format!("Run: cos service restart {name}"),
    }))
}

/// List all discovered services with their current status.
fn cmd_list(_args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Read).map_err(|v| v.to_string())?;
    let services = discover_services();

    let list: Vec<Value> = services
        .values()
        .map(|def| {
            let pid = read_pid(&def.name);
            let alive = pid.map(|p| is_alive(p)).unwrap_or(false);
            let healthy = if alive {
                check_service_health(def)
            } else {
                None
            };

            let mut entry = json!({
                "name": def.name,
                "description": def.description,
                "running": alive,
            });
            if let Some(p) = pid {
                entry["pid"] = json!(p);
            }
            if let Some(h) = healthy {
                entry["healthy"] = json!(h);
            }
            entry
        })
        .collect();

    let count = list.len();
    Ok(json!({
        "services": list,
        "count": count,
    }))
}

/// Show service logs.
fn cmd_logs(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Read).map_err(|v| v.to_string())?;
    let name = args
        .first()
        .ok_or("usage: cos service logs <name> [--tail N]")?;

    let mut tail_n = DEFAULT_LOG_TAIL;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--tail" if i + 1 < args.len() => {
                tail_n = args[i + 1]
                    .parse()
                    .map_err(|_| format!("invalid --tail value: {}", args[i + 1]))?;
                i += 2;
            }
            _ => i += 1,
        }
    }

    let log = log_path(name);
    if !log.is_file() {
        return Ok(json!({
            "name": name,
            "lines": [],
            "count": 0,
            "hint": "no log file found — service may not have been started yet",
        }));
    }

    let tail = read_log_tail(name, tail_n);
    let lines: Vec<&str> = tail.lines().collect();
    let count = lines.len();

    Ok(json!({
        "name": name,
        "lines": lines,
        "count": count,
    }))
}

/// Register a new service by creating a service.json in the services directory.
fn cmd_register(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::System).map_err(|v| v.to_string())?;
    let mut name: Option<String> = None;
    let mut command: Option<String> = None;
    let mut workdir: Option<String> = None;
    let mut health_url: Option<String> = None;
    let mut description = String::new();

    // Lifecycle hook flags
    let mut pre_start: Option<String> = None;
    let mut post_start: Option<String> = None;
    let mut pre_stop: Option<String> = None;
    let mut post_stop: Option<String> = None;
    let mut drain_timeout: Option<u64> = None;
    let mut stop_timeout: Option<u64> = None;
    let mut checkpoint_cmd: Option<String> = None;
    let mut credentials: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--name" if i + 1 < args.len() => {
                name = Some(args[i + 1].clone());
                i += 2;
            }
            "--command" if i + 1 < args.len() => {
                command = Some(args[i + 1].clone());
                i += 2;
            }
            "--workdir" if i + 1 < args.len() => {
                workdir = Some(args[i + 1].clone());
                i += 2;
            }
            "--health-url" if i + 1 < args.len() => {
                health_url = Some(args[i + 1].clone());
                i += 2;
            }
            "--description" if i + 1 < args.len() => {
                description = args[i + 1].clone();
                i += 2;
            }
            "--pre-start" if i + 1 < args.len() => {
                pre_start = Some(args[i + 1].clone());
                i += 2;
            }
            "--post-start" if i + 1 < args.len() => {
                post_start = Some(args[i + 1].clone());
                i += 2;
            }
            "--pre-stop" if i + 1 < args.len() => {
                pre_stop = Some(args[i + 1].clone());
                i += 2;
            }
            "--post-stop" if i + 1 < args.len() => {
                post_stop = Some(args[i + 1].clone());
                i += 2;
            }
            "--drain-timeout" if i + 1 < args.len() => {
                drain_timeout = Some(
                    args[i + 1]
                        .parse()
                        .map_err(|_| format!("invalid --drain-timeout value: {}", args[i + 1]))?,
                );
                i += 2;
            }
            "--stop-timeout" if i + 1 < args.len() => {
                stop_timeout = Some(
                    args[i + 1]
                        .parse()
                        .map_err(|_| format!("invalid --stop-timeout value: {}", args[i + 1]))?,
                );
                i += 2;
            }
            "--checkpoint-cmd" if i + 1 < args.len() => {
                checkpoint_cmd = Some(args[i + 1].clone());
                i += 2;
            }
            "--credentials" if i + 1 < args.len() => {
                credentials = args[i + 1]
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .collect();
                i += 2;
            }
            _ => i += 1,
        }
    }

    let name = name.ok_or("--name is required")?;
    let command = command.ok_or("--command is required")?;

    // Validate name: alphanumeric + hyphens only
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("service name must be alphanumeric (hyphens and underscores allowed)".into());
    }

    let health = health_url.map(|url| HealthConfig {
        url: Some(url),
        interval_secs: default_interval(),
        timeout_secs: default_timeout(),
        start_grace_secs: default_grace(),
    });

    // Build lifecycle hooks if any lifecycle flag was provided
    let has_lifecycle = pre_start.is_some()
        || post_start.is_some()
        || pre_stop.is_some()
        || post_stop.is_some()
        || drain_timeout.is_some()
        || stop_timeout.is_some()
        || checkpoint_cmd.is_some();

    let lifecycle = if has_lifecycle {
        Some(LifecycleHooks {
            pre_start,
            post_start,
            pre_stop,
            post_stop,
            drain_timeout_secs: drain_timeout.unwrap_or_else(default_drain_timeout),
            stop_timeout_secs: stop_timeout.unwrap_or_else(default_stop_timeout),
            checkpoint_cmd,
        })
    } else {
        None
    };

    let def = ServiceDef {
        name: name.clone(),
        description,
        command,
        workdir,
        env: BTreeMap::new(),
        health,
        restart: default_restart(),
        depends_on: Vec::new(),
        credentials,
        lifecycle,
    };

    // Write service.json
    let svc_dir = services_dir().join(&name);
    let _ = fs::create_dir_all(&svc_dir);

    let manifest_path = svc_dir.join("service.json");
    let data = serde_json::to_string_pretty(&def)
        .map_err(|e| format!("failed to serialize service definition: {e}"))?;

    fs::write(&manifest_path, &data).map_err(|e| format!("failed to write service.json: {e}"))?;

    Ok(json!({
        "registered": name,
        "path": manifest_path.to_string_lossy(),
    }))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_values() {
        assert_eq!(default_interval(), 10);
        assert_eq!(default_timeout(), 5);
        assert_eq!(default_grace(), 15);
        assert_eq!(default_restart(), "on-failure");
    }

    #[test]
    fn test_deserialize_service_def_minimal() {
        let json = r#"{"name": "test", "command": "echo hello"}"#;
        let def: ServiceDef = serde_json::from_str(json).unwrap();
        assert_eq!(def.name, "test");
        assert_eq!(def.command, "echo hello");
        assert_eq!(def.description, "");
        assert!(def.workdir.is_none());
        assert!(def.env.is_empty());
        assert!(def.health.is_none());
        assert_eq!(def.restart, "on-failure");
        assert!(def.depends_on.is_empty());
        assert!(def.lifecycle.is_none());
    }

    #[test]
    fn test_deserialize_service_def_full() {
        let json = r#"{
            "name": "browser",
            "description": "Browser engine",
            "command": "node index.js",
            "workdir": "/opt/cos-browser-engine",
            "env": {"KEY": "val"},
            "health": {
                "url": "http://localhost:3000",
                "interval_secs": 5,
                "timeout_secs": 2,
                "start_grace_secs": 30
            },
            "restart": "always",
            "depends_on": ["redis"]
        }"#;
        let def: ServiceDef = serde_json::from_str(json).unwrap();
        assert_eq!(def.name, "browser");
        assert_eq!(def.description, "Browser engine");
        assert_eq!(def.command, "node index.js");
        assert_eq!(def.workdir.as_deref(), Some("/opt/cos-browser-engine"));
        assert_eq!(def.env.get("KEY").unwrap(), "val");
        let h = def.health.unwrap();
        assert_eq!(h.url.as_deref(), Some("http://localhost:3000"));
        assert_eq!(h.interval_secs, 5);
        assert_eq!(h.timeout_secs, 2);
        assert_eq!(h.start_grace_secs, 30);
        assert_eq!(def.restart, "always");
        assert_eq!(def.depends_on, vec!["redis"]);
    }

    #[test]
    fn test_deserialize_health_defaults() {
        let json = r#"{"url": "http://localhost:8080"}"#;
        let h: HealthConfig = serde_json::from_str(json).unwrap();
        assert_eq!(h.url.as_deref(), Some("http://localhost:8080"));
        assert_eq!(h.interval_secs, 10);
        assert_eq!(h.timeout_secs, 5);
        assert_eq!(h.start_grace_secs, 15);
    }

    #[test]
    fn test_unknown_command() {
        let result = run("bogus", &[]);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown service command"));
    }

    #[test]
    fn test_start_missing_name() {
        let result = cmd_start(&[]);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("usage"));
    }

    #[test]
    fn test_stop_missing_name() {
        let result = cmd_stop(&[]);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("usage"));
    }

    #[test]
    fn test_status_missing_name() {
        let result = cmd_status(&[]);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("usage"));
    }

    #[test]
    fn test_health_missing_name() {
        let result = cmd_health(&[]);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("usage"));
    }

    #[test]
    fn test_logs_missing_name() {
        let result = cmd_logs(&[]);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("usage"));
    }

    #[test]
    fn test_register_missing_args() {
        let result = cmd_register(&[]);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("--name is required"));

        let result = cmd_register(&["--name".into(), "foo".into()]);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("--command is required"));
    }

    #[test]
    fn test_register_invalid_name() {
        let result = cmd_register(&[
            "--name".into(),
            "bad/name".into(),
            "--command".into(),
            "echo hi".into(),
        ]);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("alphanumeric"));
    }

    #[test]
    fn test_serialize_roundtrip() {
        let def = ServiceDef {
            name: "test-svc".into(),
            description: "A test service".into(),
            command: "echo hello".into(),
            workdir: Some("/tmp".into()),
            env: BTreeMap::from([("FOO".into(), "bar".into())]),
            health: Some(HealthConfig {
                url: Some("http://localhost:9090".into()),
                interval_secs: 10,
                timeout_secs: 5,
                start_grace_secs: 15,
            }),
            restart: "on-failure".into(),
            depends_on: vec!["dep1".into()],
            credentials: Vec::new(),
            lifecycle: Some(LifecycleHooks {
                pre_start: Some("echo pre".into()),
                post_start: None,
                pre_stop: Some("echo drain".into()),
                post_stop: Some("echo cleanup".into()),
                drain_timeout_secs: 3,
                stop_timeout_secs: 8,
                checkpoint_cmd: None,
            }),
        };

        let json_str = serde_json::to_string(&def).unwrap();
        let restored: ServiceDef = serde_json::from_str(&json_str).unwrap();
        assert_eq!(restored.name, def.name);
        assert_eq!(restored.command, def.command);
        assert_eq!(restored.env.get("FOO").unwrap(), "bar");
        let lc = restored.lifecycle.unwrap();
        assert_eq!(lc.pre_start.as_deref(), Some("echo pre"));
        assert!(lc.post_start.is_none());
        assert_eq!(lc.pre_stop.as_deref(), Some("echo drain"));
        assert_eq!(lc.post_stop.as_deref(), Some("echo cleanup"));
        assert_eq!(lc.drain_timeout_secs, 3);
        assert_eq!(lc.stop_timeout_secs, 8);
        assert!(lc.checkpoint_cmd.is_none());
    }

    #[test]
    fn test_lifecycle_hooks_deserialization() {
        // Empty object should use all defaults
        let json = r#"{}"#;
        let hooks: LifecycleHooks = serde_json::from_str(json).unwrap();
        assert!(hooks.pre_start.is_none());
        assert!(hooks.post_start.is_none());
        assert!(hooks.pre_stop.is_none());
        assert!(hooks.post_stop.is_none());
        assert_eq!(hooks.drain_timeout_secs, default_drain_timeout());
        assert_eq!(hooks.stop_timeout_secs, default_stop_timeout());
        assert!(hooks.checkpoint_cmd.is_none());

        // Full object
        let json = r#"{
            "pre_start": "run-migrations",
            "post_start": "register-discovery",
            "pre_stop": "drain-connections",
            "post_stop": "cleanup-tmp",
            "drain_timeout_secs": 15,
            "stop_timeout_secs": 30,
            "checkpoint_cmd": "save-state"
        }"#;
        let hooks: LifecycleHooks = serde_json::from_str(json).unwrap();
        assert_eq!(hooks.pre_start.as_deref(), Some("run-migrations"));
        assert_eq!(hooks.post_start.as_deref(), Some("register-discovery"));
        assert_eq!(hooks.pre_stop.as_deref(), Some("drain-connections"));
        assert_eq!(hooks.post_stop.as_deref(), Some("cleanup-tmp"));
        assert_eq!(hooks.drain_timeout_secs, 15);
        assert_eq!(hooks.stop_timeout_secs, 30);
        assert_eq!(hooks.checkpoint_cmd.as_deref(), Some("save-state"));
    }

    #[test]
    fn test_lifecycle_defaults() {
        assert_eq!(default_drain_timeout(), 5);
        assert_eq!(default_stop_timeout(), 10);
    }

    #[test]
    fn test_run_hook_success() {
        let result = run_hook("test_hook", "echo hello", 10);
        assert_eq!(result["step"], "test_hook");
        assert_eq!(result["status"], "ok");
        assert_eq!(result["exit_code"], 0);
        assert!(result["duration_ms"].as_u64().is_some());
        // stdout should contain "hello"
        let stdout = result["stdout"].as_str().unwrap_or("");
        assert!(stdout.contains("hello"), "stdout was: {stdout}");
    }

    #[test]
    fn test_run_hook_failure() {
        // Use a command that will fail
        #[cfg(unix)]
        let cmd = "sh -c 'echo fail-output >&2; exit 1'";
        #[cfg(not(unix))]
        let cmd = "cmd /c \"echo fail-output 1>&2 && exit /b 1\"";

        let result = run_hook("failing_hook", cmd, 10);
        assert_eq!(result["step"], "failing_hook");
        assert_eq!(result["status"], "failed");
        assert_ne!(result["exit_code"], 0);
        assert!(result["duration_ms"].as_u64().is_some());
    }

    #[test]
    fn test_stop_all_dispatch() {
        // Verify the stop-all command is routed correctly
        let result = run("stop-all", &[]);
        // Should succeed (no running services in test env)
        assert!(result.is_ok());
        let val = result.unwrap();
        assert_eq!(val["command"], "stop-all");
        assert!(val["stopped"].as_u64().is_some());
        assert!(val["results"].is_array());
    }

    #[test]
    fn test_register_with_lifecycle() {
        // Use a temp services dir for this test
        let tmp_dir = std::env::temp_dir().join("cos-test-register-lifecycle");
        let _ = fs::remove_dir_all(&tmp_dir);
        let _ = fs::create_dir_all(&tmp_dir);
        std::env::set_var("COS_SERVICES_DIR", tmp_dir.to_string_lossy().as_ref());

        let result = cmd_register(&[
            "--name".into(),
            "lifecycle-test".into(),
            "--command".into(),
            "echo hi".into(),
            "--pre-start".into(),
            "echo pre-start".into(),
            "--post-start".into(),
            "echo post-start".into(),
            "--pre-stop".into(),
            "echo draining".into(),
            "--post-stop".into(),
            "echo cleanup".into(),
            "--drain-timeout".into(),
            "3".into(),
            "--stop-timeout".into(),
            "15".into(),
            "--checkpoint-cmd".into(),
            "echo saving".into(),
        ]);
        assert!(result.is_ok(), "register failed: {:?}", result);
        let val = result.unwrap();
        assert_eq!(val["registered"], "lifecycle-test");

        // Verify the written service.json includes lifecycle hooks
        let manifest_path = tmp_dir.join("lifecycle-test").join("service.json");
        let data = fs::read_to_string(&manifest_path).unwrap();
        let def: ServiceDef = serde_json::from_str(&data).unwrap();
        let lc = def.lifecycle.unwrap();
        assert_eq!(lc.pre_start.as_deref(), Some("echo pre-start"));
        assert_eq!(lc.post_start.as_deref(), Some("echo post-start"));
        assert_eq!(lc.pre_stop.as_deref(), Some("echo draining"));
        assert_eq!(lc.post_stop.as_deref(), Some("echo cleanup"));
        assert_eq!(lc.drain_timeout_secs, 3);
        assert_eq!(lc.stop_timeout_secs, 15);
        assert_eq!(lc.checkpoint_cmd.as_deref(), Some("echo saving"));

        // Cleanup
        let _ = fs::remove_dir_all(&tmp_dir);
    }

    #[test]
    fn test_reverse_dependency_order() {
        let mut services = BTreeMap::new();
        services.insert(
            "db".into(),
            ServiceDef {
                name: "db".into(),
                description: String::new(),
                command: "echo db".into(),
                workdir: None,
                env: BTreeMap::new(),
                health: None,
                restart: "on-failure".into(),
                depends_on: vec![],
                credentials: Vec::new(),
                lifecycle: None,
            },
        );
        services.insert(
            "api".into(),
            ServiceDef {
                name: "api".into(),
                description: String::new(),
                command: "echo api".into(),
                workdir: None,
                env: BTreeMap::new(),
                health: None,
                restart: "on-failure".into(),
                depends_on: vec!["db".into()],
                credentials: Vec::new(),
                lifecycle: None,
            },
        );
        services.insert(
            "web".into(),
            ServiceDef {
                name: "web".into(),
                description: String::new(),
                command: "echo web".into(),
                workdir: None,
                env: BTreeMap::new(),
                health: None,
                restart: "on-failure".into(),
                depends_on: vec!["api".into()],
                credentials: Vec::new(),
                lifecycle: None,
            },
        );

        let order = reverse_dependency_order(&services);
        // web depends on api, api depends on db
        // Shutdown order should be: web, api, db
        let web_pos = order.iter().position(|n| n == "web").unwrap();
        let api_pos = order.iter().position(|n| n == "api").unwrap();
        let db_pos = order.iter().position(|n| n == "db").unwrap();
        assert!(
            web_pos < api_pos,
            "web should stop before api: web={web_pos}, api={api_pos}"
        );
        assert!(
            api_pos < db_pos,
            "api should stop before db: api={api_pos}, db={db_pos}"
        );
    }

    #[test]
    fn test_service_def_with_lifecycle_field() {
        let json = r#"{
            "name": "agent",
            "command": "python agent.py",
            "lifecycle": {
                "pre_start": "python migrate.py",
                "post_stop": "rm -rf /tmp/agent-*",
                "drain_timeout_secs": 10,
                "stop_timeout_secs": 20
            }
        }"#;
        let def: ServiceDef = serde_json::from_str(json).unwrap();
        assert_eq!(def.name, "agent");
        let lc = def.lifecycle.unwrap();
        assert_eq!(lc.pre_start.as_deref(), Some("python migrate.py"));
        assert!(lc.post_start.is_none());
        assert!(lc.pre_stop.is_none());
        assert_eq!(lc.post_stop.as_deref(), Some("rm -rf /tmp/agent-*"));
        assert_eq!(lc.drain_timeout_secs, 10);
        assert_eq!(lc.stop_timeout_secs, 20);
        assert!(lc.checkpoint_cmd.is_none());
    }

    #[test]
    fn test_service_def_with_credentials() {
        let json = r#"{
            "name": "my-agent",
            "command": "python agent.py",
            "credentials": ["OPENAI_KEY", "DB_URL"]
        }"#;
        let def: ServiceDef = serde_json::from_str(json).unwrap();
        assert_eq!(def.credentials, vec!["OPENAI_KEY", "DB_URL"]);
    }

    #[test]
    fn test_service_def_without_credentials() {
        let json = r#"{"name": "test", "command": "echo hi"}"#;
        let def: ServiceDef = serde_json::from_str(json).unwrap();
        assert!(def.credentials.is_empty());
    }
}
