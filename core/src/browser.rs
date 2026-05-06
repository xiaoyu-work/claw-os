/// Browser service manager — manages the cos-browser CDP server.
///
/// `cos-browser` is the vendored Obscura headless browser (see
/// crates/cos-browser). The CDP server is opt-in: agents can call
/// `cos browser start` to expose ws://localhost:9222 for external
/// Puppeteer/Playwright clients. Plain `cos app web read` does NOT need
/// the service running — it subprocesses cos-browser per request.
///
/// Provides:
/// - start / stop / restart the CDP server
/// - health check (probes /json/version, the standard CDP endpoint)
/// - status with last log lines
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::policy::{self, OpType};

const DEFAULT_CDP_PORT: u16 = 9222;
const HEALTH_TIMEOUT_SECS: u64 = 5;

fn cos_browser_bin() -> String {
    std::env::var("COS_BROWSER_BIN").unwrap_or_else(|_| "cos-browser".into())
}

fn cdp_port() -> u16 {
    std::env::var("COS_BROWSER_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_CDP_PORT)
}

fn cdp_url() -> String {
    format!("http://localhost:{}", cdp_port())
}

fn ws_url() -> String {
    format!("ws://localhost:{}/devtools/browser", cdp_port())
}

fn data_dir() -> PathBuf {
    PathBuf::from(std::env::var("COS_DATA_DIR").unwrap_or_else(|_| "/var/lib/cos".into()))
        .join("browser")
}

fn pid_path() -> PathBuf {
    data_dir().join("cos-browser.pid")
}

fn log_path() -> PathBuf {
    PathBuf::from("/var/log/cos/cos-browser.log")
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
        let _ = pid;
        false
    }
}

/// Probe `/json/version` — Chrome DevTools Protocol's standard discovery
/// endpoint. cos-browser (and any CDP-compatible browser) returns a 200 with
/// JSON describing the protocol version.
fn is_browser_healthy() -> bool {
    let url = format!("{}/json/version", cdp_url());
    Command::new("curl")
        .args([
            "-s",
            "-o",
            "/dev/null",
            "-w",
            "%{http_code}",
            "--connect-timeout",
            &HEALTH_TIMEOUT_SECS.to_string(),
            &url,
        ])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "200")
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

fn parse_start_flags(args: &[String]) -> (bool, Option<String>) {
    let mut stealth = false;
    let mut proxy: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--stealth" => {
                stealth = true;
                i += 1;
            }
            "--proxy" if i + 1 < args.len() => {
                proxy = Some(args[i + 1].clone());
                i += 2;
            }
            _ => i += 1,
        }
    }
    (stealth, proxy)
}

/// Start the cos-browser CDP server.
fn cmd_start(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::System).map_err(|v| v.to_string())?;

    if let Some(pid) = read_pid() {
        if is_process_alive(pid) {
            return Ok(json!({
                "status": "already_running",
                "pid": pid,
                "url": cdp_url(),
                "ws": ws_url(),
            }));
        }
    }

    let bin = cos_browser_bin();
    if which_on_path(&bin).is_none() && !PathBuf::from(&bin).is_file() {
        return Err(format!(
            "cos-browser binary '{bin}' not found on PATH. Install Claw OS or set $COS_BROWSER_BIN."
        ));
    }

    let (stealth, proxy) = parse_start_flags(args);

    let log = log_path();
    if let Some(parent) = log.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::create_dir_all(data_dir());

    let log_file = fs::File::create(&log)
        .map_err(|e| format!("failed to create log file: {e}"))?;
    let log_err = log_file
        .try_clone()
        .map_err(|e| format!("failed to clone log file: {e}"))?;

    let mut cmd = Command::new(&bin);
    cmd.arg("serve").arg("--port").arg(cdp_port().to_string());
    if stealth {
        cmd.arg("--stealth");
    }
    if let Some(ref p) = proxy {
        cmd.arg("--proxy").arg(p);
    }

    let child = cmd
        .stdin(Stdio::null())
        .stdout(log_file)
        .stderr(log_err)
        .spawn()
        .map_err(|e| format!("failed to start cos-browser: {e}"))?;

    let pid = child.id();
    write_pid(pid);
    std::mem::forget(child);

    // Poll up to 15 seconds for the CDP endpoint to come online.
    let mut ready = false;
    for _ in 0..30 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        if is_browser_healthy() {
            ready = true;
            break;
        }
    }

    Ok(json!({
        "status": if ready { "running" } else { "starting" },
        "pid": pid,
        "url": cdp_url(),
        "ws": ws_url(),
        "stealth": stealth,
        "proxy": proxy,
        "ready": ready,
        "log": log.to_string_lossy(),
    }))
}

fn cmd_stop(_args: &[String]) -> Result<Value, String> {
    policy::require(OpType::System).map_err(|v| v.to_string())?;
    let pid = read_pid().ok_or("cos-browser is not running (no PID file)")?;

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

fn cmd_restart(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::System).map_err(|v| v.to_string())?;
    let _ = cmd_stop(&[]);
    std::thread::sleep(std::time::Duration::from_secs(1));
    cmd_start(args)
}

fn cmd_status(_args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Read).map_err(|v| v.to_string())?;
    let pid = read_pid();
    let alive = pid.map(is_process_alive).unwrap_or(false);
    let healthy = if alive { is_browser_healthy() } else { false };

    let bin = cos_browser_bin();
    let installed = which_on_path(&bin).is_some() || PathBuf::from(&bin).is_file();

    let mut result = json!({
        "engine": "cos-browser",
        "binary": bin,
        "installed": installed,
        "running": alive,
        "healthy": healthy,
        "url": cdp_url(),
        "ws": ws_url(),
    });

    if let Some(p) = pid {
        result["pid"] = json!(p);
    }

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

fn cmd_health(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Read).map_err(|v| v.to_string())?;
    let auto_restart = !args.contains(&"--no-restart".to_string());

    if is_browser_healthy() {
        return Ok(json!({
            "healthy": true,
            "url": cdp_url(),
            "ws": ws_url(),
        }));
    }

    if auto_restart {
        let result = cmd_restart(&[])?;
        let healthy = is_browser_healthy();
        return Ok(json!({
            "healthy": healthy,
            "action": "restarted",
            "restart_result": result,
        }));
    }

    Ok(json!({
        "healthy": false,
        "url": cdp_url(),
        "hint": "Run: cos browser restart",
    }))
}

/// Tiny PATH-walking implementation so we don't need a third-party crate
/// in the cos core (which builds for musl and avoids deps that pull
/// platform-specific code).
fn which_on_path(name: &str) -> Option<PathBuf> {
    if PathBuf::from(name).is_absolute() {
        let p = PathBuf::from(name);
        return if p.is_file() { Some(p) } else { None };
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            let exe = dir.join(format!("{name}.exe"));
            if exe.is_file() {
                return Some(exe);
            }
        }
    }
    None
}
