/// Browser service manager — manages the cos-browser CDP server.
///
/// `cos-browser` is the vendored Obscura headless browser (see
/// crates/cos-browser). The CDP server is opt-in: agents can call
/// `cos browser start` to expose an authenticated ws://localhost:9222
/// endpoint for external Puppeteer/Playwright clients. Plain
/// `cos app web read` does NOT need the service running — it subprocesses
/// cos-browser per request.
///
/// Provides:
/// - start / stop / restart the CDP server
/// - health check (probes /json/version, the standard CDP endpoint)
/// - status with last log lines
use serde_json::{json, Value};
use std::fs;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::caps::{require_or_json, Scope, Verb};

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
    append_auth_token(format!(
        "ws://localhost:{}/devtools/browser",
        cdp_port()
    ))
}

fn discovery_url() -> String {
    append_auth_token(format!("{}/json/version", cdp_url()))
}

fn append_auth_token(url: String) -> String {
    read_auth_token()
        .map(|token| format!("{url}?token={token}"))
        .unwrap_or(url)
}

fn data_dir() -> PathBuf {
    crate::paths::data_dir().join("browser")
}

fn pid_path() -> PathBuf {
    data_dir().join("cos-browser.pid")
}

fn auth_token_path() -> PathBuf {
    data_dir().join("cos-browser.auth-token")
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
    let _ = fs::remove_file(auth_token_path());
}

fn read_auth_token() -> Option<String> {
    fs::read_to_string(auth_token_path())
        .ok()
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty())
}

fn create_auth_token() -> Result<String, String> {
    let mut random = [0u8; 32];
    fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut random))
        .map_err(|err| format!("failed to generate CDP auth token: {err}"))?;
    Ok(random.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn write_auth_token(token: &str) -> Result<(), String> {
    let path = auth_token_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("failed to create browser data directory: {err}"))?;
    }
    let temp_path = path.with_extension(format!(
        "auth-token.{}.tmp",
        &token[..token.len().min(16)]
    ));
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temp_path)
            .map_err(|err| format!("failed to create CDP auth token file: {err}"))?;
        if let Err(err) = file
            .write_all(token.as_bytes())
            .and_then(|_| file.sync_all())
            .and_then(|_| fs::rename(&temp_path, &path))
        {
            let _ = fs::remove_file(&temp_path);
            return Err(format!("failed to persist CDP auth token file: {err}"));
        }
    }
    #[cfg(not(unix))]
    {
        fs::write(&temp_path, token)
            .and_then(|_| fs::rename(&temp_path, &path))
            .map_err(|err| {
                let _ = fs::remove_file(&temp_path);
                format!("failed to write CDP auth token file: {err}")
            })?;
    }
    Ok(())
}

fn is_process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        if std::path::Path::new(&format!("/proc/{pid}")).exists() {
            return true;
        }
    }
    #[cfg(unix)]
    {
        let rc = unsafe { libc::kill(pid as i32, 0) };
        if rc == 0 {
            return true;
        }
        // EPERM => process exists but is owned by a different uid.
        // Don't claim it's gone — that would let a low-privileged
        // launcher "reclaim" the PID file and SIGTERM whatever pid
        // the kernel recycled to next.
        let err = std::io::Error::last_os_error();
        return err.raw_os_error() == Some(libc::EPERM);
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
    let Some(token) = read_auth_token() else {
        return false;
    };
    let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), cdp_port());
    let timeout = std::time::Duration::from_secs(HEALTH_TIMEOUT_SECS);
    let Ok(mut stream) = TcpStream::connect_timeout(&address, timeout) else {
        return false;
    };
    if stream.set_read_timeout(Some(timeout)).is_err()
        || stream.set_write_timeout(Some(timeout)).is_err()
    {
        return false;
    }
    let path = format!("/json/version?token={token}");
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n",
        cdp_port()
    );
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let mut status = [0u8; 12];
    stream.read_exact(&mut status).is_ok() && status == *b"HTTP/1.1 200"
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
    require_or_json(Verb::SYS_SERVICE, Scope::name("cos-browser")).map_err(|v| v.to_string())?;

    if let Some(pid) = read_pid() {
        if is_process_alive(pid) {
            if read_auth_token().is_none() {
                return Err(
                    "running cos-browser instance has no authentication token; stop it before restarting"
                        .to_string(),
                );
            }
            return Ok(json!({
                "status": "already_running",
                "pid": pid,
                "url": discovery_url(),
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
    let auth_token = create_auth_token()?;

    let log = log_path();
    if let Some(parent) = log.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::create_dir_all(data_dir());

    let log_file = fs::File::create(&log).map_err(|e| format!("failed to create log file: {e}"))?;
    let log_err = log_file
        .try_clone()
        .map_err(|e| format!("failed to clone log file: {e}"))?;

    let mut cmd = Command::new(&bin);
    cmd.arg("serve").arg("--port").arg(cdp_port().to_string());
    cmd.env_clear()
        .env(
            "PATH",
            std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".to_string()),
        )
        .env(
            "HOME",
            crate::paths::current_home_override()
                .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
                .unwrap_or_else(|| PathBuf::from("/")),
        )
        .env("COS_BROWSER_AUTH_TOKEN", &auth_token);
    for key in ["LANG", "LC_ALL", "TZ"] {
        if let Ok(value) = std::env::var(key) {
            cmd.env(key, value);
        }
    }
    if stealth {
        cmd.arg("--stealth");
    }
    if let Some(ref p) = proxy {
        cmd.arg("--proxy").arg(p);
    }
    crate::bridge::apply_routed_identity(&mut cmd)?;
    write_auth_token(&auth_token)?;

    let child = match cmd
        .stdin(Stdio::null())
        .stdout(log_file)
        .stderr(log_err)
        .spawn()
    {
        Ok(child) => child,
        Err(err) => {
            let _ = fs::remove_file(auth_token_path());
            return Err(format!("failed to start cos-browser: {err}"));
        }
    };

    let pid = child.id();
    write_pid(pid);
    // Reap the child in a background thread so the kernel doesn't
    // accumulate <defunct> entries once cos-browser exits. Replaces
    // the prior `std::mem::forget(child)` zombie leak.
    std::thread::spawn(move || {
        let mut child = child;
        let _ = child.wait();
    });

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
        "url": discovery_url(),
        "ws": ws_url(),
        "stealth": stealth,
        "proxy": proxy,
        "ready": ready,
        "log": log.to_string_lossy(),
    }))
}

fn cmd_stop(_args: &[String]) -> Result<Value, String> {
    require_or_json(Verb::SYS_SERVICE, Scope::name("cos-browser")).map_err(|v| v.to_string())?;
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
    require_or_json(Verb::SYS_SERVICE, Scope::name("cos-browser")).map_err(|v| v.to_string())?;
    let _ = cmd_stop(&[]);
    std::thread::sleep(std::time::Duration::from_secs(1));
    cmd_start(args)
}

fn cmd_status(_args: &[String]) -> Result<Value, String> {
    require_or_json(Verb::SYS_SERVICE, Scope::name("cos-browser")).map_err(|v| v.to_string())?;
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
        "url": discovery_url(),
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
    require_or_json(Verb::SYS_SERVICE, Scope::name("cos-browser")).map_err(|v| v.to_string())?;
    let auto_restart = !args.contains(&"--no-restart".to_string());

    if is_browser_healthy() {
        return Ok(json!({
            "healthy": true,
            "url": discovery_url(),
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
        "url": discovery_url(),
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
