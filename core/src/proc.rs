/// Agent-aware process session manager.
///
/// Tracks processes by session ID with persistent registry,
/// output buffering with caps, and queryable status.
/// Registry is stored on disk so sessions survive cos restarts.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_OUTPUT_BYTES: usize = 200_000;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SessionInfo {
    pub session_id: String,
    pub pid: u32,
    pub command: Vec<String>,
    pub started_at: String,
    pub stdout_path: String,
    pub stderr_path: String,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct Registry {
    sessions: Vec<SessionInfo>,
}

fn proc_dir() -> PathBuf {
    PathBuf::from(
        std::env::var("COS_DATA_DIR").unwrap_or_else(|_| "/var/lib/cos".into()),
    )
    .join("proc")
}

fn registry_path() -> PathBuf {
    proc_dir().join("registry.json")
}

fn load_registry() -> Registry {
    let path = registry_path();
    if let Ok(data) = fs::read_to_string(&path) {
        if let Ok(reg) = serde_json::from_str(&data) {
            return reg;
        }
    }
    Registry::default()
}

fn save_registry(reg: &Registry) {
    let path = registry_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(data) = serde_json::to_string_pretty(reg) {
        let _ = fs::write(&path, data);
    }
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

pub fn run(command: &str, args: &[String]) -> Result<Value, String> {
    match command {
        "spawn" => cmd_spawn(args),
        "status" => cmd_status(args),
        "output" => cmd_output(args),
        "kill" => cmd_kill(args),
        "list" => cmd_list(args),
        _ => Err(format!("unknown proc command: {command}")),
    }
}

fn cmd_spawn(args: &[String]) -> Result<Value, String> {
    let mut session_id = None;
    let mut cmd_start = 0;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--session" if i + 1 < args.len() => {
                session_id = Some(args[i + 1].clone());
                i += 2;
            }
            "--" => { cmd_start = i + 1; break; }
            _ => { cmd_start = i; break; }
        }
    }

    if cmd_start >= args.len() {
        return Err("no command specified".into());
    }

    let command_args = &args[cmd_start..];
    let sid = session_id.unwrap_or_else(|| format!("proc-{}", short_id()));
    let dir = proc_dir();
    let _ = fs::create_dir_all(&dir);

    let stdout_path = dir.join(format!("{sid}.stdout"));
    let stderr_path = dir.join(format!("{sid}.stderr"));

    let stdout_file = fs::File::create(&stdout_path)
        .map_err(|e| format!("failed to create stdout file: {e}"))?;
    let stderr_file = fs::File::create(&stderr_path)
        .map_err(|e| format!("failed to create stderr file: {e}"))?;

    let child = Command::new(&command_args[0])
        .args(&command_args[1..])
        .stdin(Stdio::null())
        .stdout(stdout_file)
        .stderr(stderr_file)
        .spawn()
        .map_err(|e| format!("failed to spawn: {e}"))?;

    let pid = child.id();
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    let info = SessionInfo {
        session_id: sid.clone(),
        pid,
        command: command_args.to_vec(),
        started_at: now.clone(),
        stdout_path: stdout_path.to_string_lossy().to_string(),
        stderr_path: stderr_path.to_string_lossy().to_string(),
    };

    let mut reg = load_registry();
    reg.sessions.push(info);
    save_registry(&reg);

    // Detach — process keeps running after cos exits
    std::mem::forget(child);

    Ok(json!({
        "session_id": sid,
        "pid": pid,
        "command": command_args,
        "started_at": now,
    }))
}

fn cmd_status(args: &[String]) -> Result<Value, String> {
    let sid = args.first().ok_or("usage: cos proc status <session-id>")?;
    let reg = load_registry();
    let info = reg.sessions.iter()
        .find(|s| &s.session_id == sid)
        .ok_or_else(|| format!("session not found: {sid}"))?;

    Ok(json!({
        "session_id": info.session_id,
        "pid": info.pid,
        "status": if is_alive(info.pid) { "running" } else { "exited" },
        "command": info.command,
        "started_at": info.started_at,
    }))
}

fn cmd_output(args: &[String]) -> Result<Value, String> {
    let sid = args.first().ok_or("usage: cos proc output <session-id>")?;
    let mut tail_lines: Option<usize> = None;
    let mut stream = "both".to_string();

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--tail" if i + 1 < args.len() => { tail_lines = args[i + 1].parse().ok(); i += 2; }
            "--stream" if i + 1 < args.len() => { stream = args[i + 1].clone(); i += 2; }
            _ => i += 1,
        }
    }

    let reg = load_registry();
    let info = reg.sessions.iter()
        .find(|s| &s.session_id == sid)
        .ok_or_else(|| format!("session not found: {sid}"))?;

    let mut result = json!({
        "session_id": sid,
        "status": if is_alive(info.pid) { "running" } else { "exited" },
    });

    if stream == "stdout" || stream == "both" {
        result["stdout"] = json!(read_capped(&info.stdout_path, tail_lines));
    }
    if stream == "stderr" || stream == "both" {
        result["stderr"] = json!(read_capped(&info.stderr_path, tail_lines));
    }

    Ok(result)
}

fn cmd_kill(args: &[String]) -> Result<Value, String> {
    let sid = args.first().ok_or("usage: cos proc kill <session-id>")?;
    let reg = load_registry();
    let info = reg.sessions.iter()
        .find(|s| &s.session_id == sid)
        .ok_or_else(|| format!("session not found: {sid}"))?;

    #[cfg(unix)]
    unsafe { libc::kill(info.pid as i32, libc::SIGTERM); }
    #[cfg(not(unix))]
    { let _ = Command::new("taskkill").args(["/PID", &info.pid.to_string(), "/F"]).output(); }

    Ok(json!({
        "session_id": sid,
        "status": "killed",
        "pid": info.pid,
    }))
}

fn cmd_list(_args: &[String]) -> Result<Value, String> {
    let mut reg = load_registry();

    let infos: Vec<Value> = reg.sessions.iter()
        .map(|s| json!({
            "session_id": s.session_id,
            "pid": s.pid,
            "command": s.command,
            "status": if is_alive(s.pid) { "running" } else { "exited" },
            "started_at": s.started_at,
        }))
        .collect();

    // Prune dead sessions from registry
    reg.sessions.retain(|s| is_alive(s.pid));
    save_registry(&reg);

    Ok(json!({ "sessions": infos, "count": infos.len() }))
}

fn read_capped(path: &str, tail_lines: Option<usize>) -> String {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return String::new(),
    };

    let content = if content.len() > MAX_OUTPUT_BYTES {
        let truncated = &content[content.len() - MAX_OUTPUT_BYTES..];
        format!("[truncated, showing last {}KB]\n{truncated}", MAX_OUTPUT_BYTES / 1024)
    } else {
        content
    };

    if let Some(n) = tail_lines {
        let lines: Vec<&str> = content.lines().collect();
        if lines.len() > n {
            return lines[lines.len() - n..].join("\n");
        }
    }
    content
}

fn short_id() -> String {
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{:x}", t & 0xFFFFFFFF)
}
