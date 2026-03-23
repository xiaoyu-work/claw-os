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
}

fn default_restart() -> String {
    "on-failure".into()
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
    PathBuf::from(
        std::env::var("COS_DATA_DIR").unwrap_or_else(|_| "/var/lib/cos".into()),
    )
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
    services
        .get(name)
        .cloned()
        .ok_or_else(|| {
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

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub fn run(command: &str, args: &[String]) -> Result<Value, String> {
    match command {
        "start" => cmd_start(args),
        "stop" => cmd_stop(args),
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

    // Ensure runtime directory exists
    let rt_dir = service_runtime_dir(name);
    let _ = fs::create_dir_all(&rt_dir);

    // Prepare log file
    let log = log_path(name);
    let log_file = fs::File::create(&log)
        .map_err(|e| format!("failed to create log file: {e}"))?;
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

    // If health check is configured, wait for it to pass
    let healthy = if let Some(ref health) = def.health {
        if health.url.is_some() {
            let grace = health.start_grace_secs;
            let polls = grace * 2; // poll every 500ms
            let mut ready = false;
            for _ in 0..polls {
                std::thread::sleep(std::time::Duration::from_millis(500));
                if check_service_health(&def) == Some(true) {
                    ready = true;
                    break;
                }
            }
            Some(ready)
        } else {
            None
        }
    } else {
        None
    };

    let status = match healthy {
        Some(true) => "running",
        Some(false) => "starting",
        None => "running",
    };

    Ok(json!({
        "name": name,
        "status": status,
        "pid": pid,
        "healthy": healthy,
        "log": log.to_string_lossy(),
    }))
}

/// Stop a service by name.
fn cmd_stop(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::System).map_err(|v| v.to_string())?;
    let name = args.first().ok_or("usage: cos service stop <name>")?;

    let pid = read_pid(name)
        .ok_or_else(|| format!("service {name} is not running (no PID file)"))?;

    kill_pid(pid);
    clear_pid(name);

    Ok(json!({
        "name": name,
        "status": "stopped",
        "pid": pid,
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
    let name = args.first().ok_or("usage: cos service logs <name> [--tail N]")?;

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

    let def = ServiceDef {
        name: name.clone(),
        description,
        command,
        workdir,
        env: BTreeMap::new(),
        health,
        restart: default_restart(),
        depends_on: Vec::new(),
    };

    // Write service.json
    let svc_dir = services_dir().join(&name);
    let _ = fs::create_dir_all(&svc_dir);

    let manifest_path = svc_dir.join("service.json");
    let data = serde_json::to_string_pretty(&def)
        .map_err(|e| format!("failed to serialize service definition: {e}"))?;

    fs::write(&manifest_path, &data)
        .map_err(|e| format!("failed to write service.json: {e}"))?;

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
    }

    #[test]
    fn test_deserialize_service_def_full() {
        let json = r#"{
            "name": "browser",
            "description": "Jina Reader",
            "command": "npx tsx src/index.ts",
            "workdir": "/opt/jina-reader",
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
        assert_eq!(def.description, "Jina Reader");
        assert_eq!(def.command, "npx tsx src/index.ts");
        assert_eq!(def.workdir.as_deref(), Some("/opt/jina-reader"));
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
        };

        let json_str = serde_json::to_string(&def).unwrap();
        let restored: ServiceDef = serde_json::from_str(&json_str).unwrap();
        assert_eq!(restored.name, def.name);
        assert_eq!(restored.command, def.command);
        assert_eq!(restored.env.get("FOO").unwrap(), "bar");
    }
}
