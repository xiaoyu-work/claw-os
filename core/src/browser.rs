/// Browser service manager — manages Jina Reader lifecycle.
///
/// Provides:
/// - Start/stop/restart the Reader service
/// - Health checking with auto-restart on crash
/// - Status reporting for the agent
///
/// This moves Reader lifecycle from a fragile shell script (cos-init)
/// into a proper service manager that can be queried and controlled.
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::policy::{self, OpType};

const READER_DIR: &str = "/opt/cos-browser-engine";
const DEFAULT_READER_URL: &str = "http://localhost:3000";
const HEALTH_TIMEOUT_SECS: u64 = 5;

fn reader_url() -> String {
    std::env::var("COS_BROWSER_URL").unwrap_or_else(|_| DEFAULT_READER_URL.into())
}

fn pid_path() -> PathBuf {
    PathBuf::from(std::env::var("COS_DATA_DIR").unwrap_or_else(|_| "/var/lib/cos".into()))
        .join("browser")
        .join("reader.pid")
}

fn log_path() -> PathBuf {
    PathBuf::from("/var/log/cos/reader.log")
}

fn read_pid() -> Option<u32> {
    fs::read_to_string(pid_path()).ok()?.trim().parse().ok()
}

fn write_pid(pid: u32) {
    let path = pid_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(&path, pid.to_string());
}

fn clear_pid() {
    let _ = fs::remove_file(pid_path());
}

fn is_process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as i32, 0) == 0
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// Check if Reader HTTP endpoint is responding.
fn is_reader_healthy() -> bool {
    // Use curl for simplicity (available in the rootfs)
    Command::new("curl")
        .args([
            "-s",
            "-o",
            "/dev/null",
            "-w",
            "%{http_code}",
            "--connect-timeout",
            &HEALTH_TIMEOUT_SECS.to_string(),
            &reader_url(),
        ])
        .output()
        .map(|o| {
            let code = String::from_utf8_lossy(&o.stdout);
            code.trim().starts_with('2') || code.trim().starts_with('3')
        })
        .unwrap_or(false)
}

pub fn run(command: &str, args: &[String]) -> Result<Value, String> {
    match command {
        "start" => cmd_start(args),
        "stop" => cmd_stop(args),
        "restart" => cmd_restart(args),
        "status" => cmd_status(args),
        "health" => cmd_health(args),
        _ => Err(format!("unknown browser command: {command}")),
    }
}

/// Start the Jina Reader service.
fn cmd_start(_args: &[String]) -> Result<Value, String> {
    policy::require(OpType::System).map_err(|v| v.to_string())?;
    // Check if already running
    if let Some(pid) = read_pid() {
        if is_process_alive(pid) {
            return Ok(json!({
                "status": "already_running",
                "pid": pid,
                "url": reader_url(),
            }));
        }
    }

    // Check if Reader is installed
    if !PathBuf::from(READER_DIR).is_dir() {
        return Err("Browser engine not installed at /opt/cos-browser-engine".into());
    }

    // Start Reader
    let log = log_path();
    if let Some(parent) = log.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let log_file = fs::File::create(&log).map_err(|e| format!("failed to create log file: {e}"))?;
    let log_err = log_file
        .try_clone()
        .map_err(|e| format!("failed to clone log file: {e}"))?;

    let child = Command::new("node")
        .args(["index.js"])
        .current_dir(READER_DIR)
        .env("PORT", "3000")
        .env("PUPPETEER_SKIP_DOWNLOAD", "true")
        .stdin(Stdio::null())
        .stdout(log_file)
        .stderr(log_err)
        .spawn()
        .map_err(|e| format!("failed to start browser engine: {e}"))?;

    let pid = child.id();
    write_pid(pid);

    // Detach the child process
    std::mem::forget(child);

    // Wait for Reader to be ready (max 15 seconds)
    let mut ready = false;
    for _ in 0..30 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        if is_reader_healthy() {
            ready = true;
            break;
        }
    }

    Ok(json!({
        "status": if ready { "running" } else { "starting" },
        "pid": pid,
        "url": reader_url(),
        "ready": ready,
        "log": log.to_string_lossy(),
    }))
}

/// Stop the Reader service.
fn cmd_stop(_args: &[String]) -> Result<Value, String> {
    policy::require(OpType::System).map_err(|v| v.to_string())?;
    let pid = read_pid().ok_or("Reader is not running (no PID file)")?;

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

    clear_pid();

    Ok(json!({
        "status": "stopped",
        "pid": pid,
    }))
}

/// Restart the Reader service.
fn cmd_restart(_args: &[String]) -> Result<Value, String> {
    policy::require(OpType::System).map_err(|v| v.to_string())?;
    let _ = cmd_stop(&[]);
    std::thread::sleep(std::time::Duration::from_secs(1));
    cmd_start(&[])
}

/// Show Reader status.
fn cmd_status(_args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Read).map_err(|v| v.to_string())?;
    let pid = read_pid();
    let alive = pid.map(|p| is_process_alive(p)).unwrap_or(false);
    let healthy = if alive { is_reader_healthy() } else { false };

    let installed = PathBuf::from(READER_DIR).is_dir();

    let mut result = json!({
        "installed": installed,
        "running": alive,
        "healthy": healthy,
        "url": reader_url(),
    });

    if let Some(p) = pid {
        result["pid"] = json!(p);
    }

    // Include last few lines of log
    let log = log_path();
    if log.is_file() {
        if let Ok(content) = fs::read_to_string(&log) {
            let lines: Vec<&str> = content.lines().collect();
            let tail: Vec<&str> = if lines.len() > 10 {
                lines[lines.len() - 10..].to_vec()
            } else {
                lines
            };
            result["log_tail"] = json!(tail.join("\n"));
        }
    }

    Ok(result)
}

/// Health check — returns ok if Reader is responding, auto-restarts if not.
fn cmd_health(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Read).map_err(|v| v.to_string())?;
    let auto_restart = !args.contains(&"--no-restart".to_string());

    if is_reader_healthy() {
        return Ok(json!({
            "healthy": true,
            "url": reader_url(),
        }));
    }

    // Not healthy
    if auto_restart {
        // Try to restart
        let result = cmd_restart(&[])?;
        let healthy = is_reader_healthy();
        return Ok(json!({
            "healthy": healthy,
            "action": "restarted",
            "restart_result": result,
        }));
    }

    Ok(json!({
        "healthy": false,
        "url": reader_url(),
        "hint": "Run: cos browser restart",
    }))
}
