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
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::policy::{self, OpType};

const MAX_OUTPUT_BYTES: usize = 2_000_000;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SessionInfo {
    pub session_id: String,
    pub pid: u32,
    pub command: Vec<String>,
    pub started_at: String,
    pub stdout_path: String,
    pub stderr_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
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
        "wait" => cmd_wait(args),
        "signal" => cmd_signal(args),
        "result" => cmd_result(args),
        "stats" => cmd_stats(args),
        "renice" => cmd_renice(args),
        _ => Err(format!("unknown proc command: {command}")),
    }
}

fn cmd_spawn(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Exec).map_err(|v| v.to_string())?;
    let mut session_id = None;
    let mut group = None;
    let mut parent = None;
    let mut workdir = None;
    let mut tier: Option<u8> = None;
    let mut scope: Option<String> = None;
    let mut priority: Option<String> = None;
    let mut isolated_workspace = false;
    let mut cmd_start = 0;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--session" if i + 1 < args.len() => {
                session_id = Some(args[i + 1].clone());
                i += 2;
            }
            "--group" if i + 1 < args.len() => {
                group = Some(args[i + 1].clone());
                i += 2;
            }
            "--parent" if i + 1 < args.len() => {
                parent = Some(args[i + 1].clone());
                i += 2;
            }
            "--workdir" if i + 1 < args.len() => {
                workdir = Some(args[i + 1].clone());
                i += 2;
            }
            "--workspace" if i + 1 < args.len() && args[i + 1] == "isolated" => {
                isolated_workspace = true;
                i += 2;
            }
            "--tier" if i + 1 < args.len() => {
                tier = Some(args[i + 1].parse::<u8>().map_err(|_| "tier must be 0-3".to_string())?);
                i += 2;
            }
            "--scope" if i + 1 < args.len() => {
                scope = Some(args[i + 1].clone());
                i += 2;
            }
            "--priority" if i + 1 < args.len() => {
                let p = args[i + 1].to_lowercase();
                if !["low", "normal", "high", "realtime"].contains(&p.as_str()) {
                    return Err("priority must be: low, normal, high, realtime".into());
                }
                priority = Some(p);
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

    // Validate tier value (0-3 only)
    if let Some(t) = tier {
        if t > 3 {
            return Err("tier must be 0-3 (0=ROOT, 1=OPERATE, 2=CREATE, 3=OBSERVE)".into());
        }
    }

    // Enforce inheritance rules when parent is set
    if let Some(ref parent_sid) = parent {
        let reg = load_registry();
        if let Some(parent_info) = reg.sessions.iter().find(|s| &s.session_id == parent_sid) {
            // Tier inheritance: child tier must be >= parent tier (more restricted)
            if let (Some(parent_tier), Some(child_tier)) = (parent_info.tier, tier) {
                if child_tier < parent_tier {
                    return Err(format!(
                        "cannot escalate tier: parent '{}' has tier {} but child requested tier {}. Child tier must be >= parent tier.",
                        parent_sid, parent_tier, child_tier
                    ));
                }
            }
            // If parent has tier but child doesn't specify, inherit parent's tier
            if parent_info.tier.is_some() && tier.is_none() {
                tier = parent_info.tier;
            }

            // Scope inheritance: child scope must be within parent scope
            if let (Some(ref parent_scope), Some(ref child_scope)) = (&parent_info.scope, &scope) {
                if !child_scope.starts_with(parent_scope.as_str()) {
                    return Err(format!(
                        "cannot widen scope: parent '{}' is scoped to '{}' but child requested scope '{}'",
                        parent_sid, parent_scope, child_scope
                    ));
                }
            }
            // If parent has scope but child doesn't specify, inherit parent's scope
            if parent_info.scope.is_some() && scope.is_none() {
                scope = parent_info.scope.clone();
            }
        }
    }

    // Guardrails: check for rapid respawn and destructive commands
    let reg_check = load_registry();
    let rapid_warning = check_rapid_respawn(&reg_check, command_args);
    let destructive_warning = check_destructive(command_args);
    drop(reg_check);

    let sid = session_id.unwrap_or_else(|| format!("proc-{}", short_id()));
    let dir = proc_dir();
    let _ = fs::create_dir_all(&dir);

    // Handle isolated workspace
    if isolated_workspace {
        let ws_dir = PathBuf::from(
            std::env::var("COS_DATA_DIR").unwrap_or_else(|_| "/var/lib/cos".into()),
        )
        .join("sessions")
        .join(&sid)
        .join("workspace");
        fs::create_dir_all(&ws_dir)
            .map_err(|e| format!("failed to create isolated workspace: {e}"))?;
        workdir = Some(ws_dir.to_string_lossy().to_string());
    }

    let stdout_path = dir.join(format!("{sid}.stdout"));
    let stderr_path = dir.join(format!("{sid}.stderr"));

    let stdout_file = fs::File::create(&stdout_path)
        .map_err(|e| format!("failed to create stdout file: {e}"))?;
    let stderr_file = fs::File::create(&stderr_path)
        .map_err(|e| format!("failed to create stderr file: {e}"))?;

    // Apply process priority via nice (Unix only)
    #[cfg(unix)]
    let (actual_cmd, actual_args) = if let Some(ref prio) = priority {
        let nice_val = match prio.as_str() {
            "low" => "10",
            "normal" => "0",
            "high" => "-5",
            "realtime" => "-10",
            _ => "0",
        };
        let mut nice_args = vec!["-n".to_string(), nice_val.to_string()];
        nice_args.extend_from_slice(command_args);
        ("nice".to_string(), nice_args)
    } else {
        (command_args[0].clone(), command_args[1..].to_vec())
    };

    #[cfg(not(unix))]
    let (actual_cmd, actual_args) = (command_args[0].clone(), command_args[1..].to_vec());

    let mut cmd = Command::new(&actual_cmd);
    cmd.args(&actual_args)
        .stdin(Stdio::null())
        .stdout(stdout_file)
        .stderr(stderr_file)
        // Agent-native: suppress all interactive prompts
        .env("DEBIAN_FRONTEND", "noninteractive")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("CI", "true")
        .env("PAGER", "cat")
        .env("GIT_PAGER", "cat")
        .env("PIP_NO_INPUT", "1")
        .env("NPM_CONFIG_YES", "true")
        .env("PYTHONDONTWRITEBYTECODE", "1");

    if let Some(ref wd) = workdir {
        cmd.current_dir(wd);
    }

    // Inject session ID so child process can be identified by policy module
    cmd.env("COS_SESSION", &sid);

    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }

    let child = cmd.spawn()
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
        group: group.clone(),
        parent: parent.clone(),
        workdir: workdir.clone(),
        exit_code: None,
        ended_at: None,
        tier,
        scope: scope.clone(),
        priority: priority.clone(),
    };

    let mut reg = load_registry();
    reg.sessions.push(info);
    save_registry(&reg);

    // Detach -- process keeps running after cos exits
    std::mem::forget(child);

    let mut result = json!({
        "session_id": sid,
        "pid": pid,
        "command": command_args,
        "started_at": now,
    });
    if let Some(g) = group { result["group"] = json!(g); }
    if let Some(p) = parent { result["parent"] = json!(p); }
    if let Some(w) = workdir { result["workdir"] = json!(w); }
    if let Some(t) = tier { result["tier"] = json!(t); }
    if let Some(ref s) = scope { result["scope"] = json!(s); }
    if let Some(ref pr) = priority { result["priority"] = json!(pr); }
    let mut warnings = Vec::new();
    if let Some(w) = rapid_warning { warnings.push(w); }
    if let Some(w) = destructive_warning { warnings.push(w); }
    if !warnings.is_empty() { result["warnings"] = json!(warnings); }

    Ok(result)
}

fn cmd_status(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Read).map_err(|v| v.to_string())?;
    let sid = args.first().ok_or("usage: cos proc status <session-id>")?;
    let mut reg = load_registry();
    let idx = reg.sessions.iter()
        .position(|s| &s.session_id == sid)
        .ok_or_else(|| format!("session not found: {sid}"))?;

    let alive = is_alive(reg.sessions[idx].pid);
    let status = if alive { "running" } else { "exited" };

    // Auto-capture ended_at when process is first detected as dead
    if !alive && reg.sessions[idx].ended_at.is_none() {
        let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        reg.sessions[idx].ended_at = Some(now);
        save_registry(&reg);
    }

    let info = &reg.sessions[idx];
    let mut result = json!({
        "session_id": info.session_id,
        "pid": info.pid,
        "status": status,
        "command": info.command,
        "started_at": info.started_at,
    });
    if let Some(ref ended) = info.ended_at {
        result["ended_at"] = json!(ended);
    }
    if let Some(t) = info.tier { result["tier"] = json!(t); }
    if let Some(ref s) = info.scope { result["scope"] = json!(s); }

    Ok(result)
}

fn cmd_output(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Read).map_err(|v| v.to_string())?;
    let sid = args.first().ok_or("usage: cos proc output <session-id>")?;
    let mut tail_lines: Option<usize> = None;
    let mut stream = "both".to_string();
    let mut follow = false;
    let mut since_offset: Option<u64> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--tail" if i + 1 < args.len() => { tail_lines = args[i + 1].parse().ok(); i += 2; }
            "--stream" if i + 1 < args.len() => { stream = args[i + 1].clone(); i += 2; }
            "--follow" => { follow = true; i += 1; }
            "--since-offset" if i + 1 < args.len() => { since_offset = args[i + 1].parse().ok(); i += 2; }
            _ => i += 1,
        }
    }

    let reg = load_registry();
    let info = reg.sessions.iter()
        .find(|s| &s.session_id == sid)
        .ok_or_else(|| format!("session not found: {sid}"))?;

    // --since-offset mode: incremental reading from byte offset
    if let Some(offset) = since_offset {
        let (stdout_data, stdout_offset) = if stream == "stdout" || stream == "both" {
            read_from_offset(&info.stdout_path, offset)
        } else {
            (String::new(), offset)
        };
        let (stderr_data, stderr_offset) = if stream == "stderr" || stream == "both" {
            read_from_offset(&info.stderr_path, offset)
        } else {
            (String::new(), offset)
        };
        return Ok(json!({
            "session_id": sid,
            "stdout": stdout_data,
            "stderr": stderr_data,
            "stdout_offset": stdout_offset,
            "stderr_offset": stderr_offset,
            "status": if is_alive(info.pid) { "running" } else { "exited" },
        }));
    }

    // --follow mode: block until process exits, then return all output
    if follow {
        let stdout_path = info.stdout_path.clone();
        let stderr_path = info.stderr_path.clone();
        let pid = info.pid;
        drop(reg);

        while is_alive(pid) {
            thread::sleep(Duration::from_millis(250));
        }

        let mut result = json!({
            "session_id": sid,
            "status": "exited",
        });
        if stream == "stdout" || stream == "both" {
            result["stdout"] = json!(read_capped(&stdout_path, None));
        }
        if stream == "stderr" || stream == "both" {
            result["stderr"] = json!(read_capped(&stderr_path, None));
        }
        return Ok(result);
    }

    // Default mode: read current output
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
    policy::require(OpType::Exec).map_err(|v| v.to_string())?;
    // --group mode: kill all sessions in a group
    if args.len() >= 2 && args[0] == "--group" {
        let group_name = &args[1];
        let reg = load_registry();
        let group_sessions: Vec<&SessionInfo> = reg.sessions.iter()
            .filter(|s| s.group.as_deref() == Some(group_name.as_str()))
            .collect();
        if group_sessions.is_empty() {
            return Err(format!("no sessions in group: {group_name}"));
        }
        let mut killed = Vec::new();
        for info in &group_sessions {
            kill_process(info.pid);
            killed.push(json!({
                "session_id": info.session_id,
                "pid": info.pid,
            }));
        }
        return Ok(json!({
            "group": group_name,
            "status": "killed",
            "sessions": killed,
        }));
    }

    let sid = args.first().ok_or("usage: cos proc kill <session-id>")?;
    let reg = load_registry();
    let info = reg.sessions.iter()
        .find(|s| &s.session_id == sid)
        .ok_or_else(|| format!("session not found: {sid}"))?;

    kill_process(info.pid);

    Ok(json!({
        "session_id": sid,
        "status": "killed",
        "pid": info.pid,
    }))
}

fn cmd_list(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Read).map_err(|v| v.to_string())?;
    let mut reg = load_registry();
    let mut group_filter: Option<&str> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--group" if i + 1 < args.len() => {
                group_filter = Some(&args[i + 1]);
                i += 2;
            }
            _ => i += 1,
        }
    }

    let infos: Vec<Value> = reg.sessions.iter()
        .filter(|s| {
            if let Some(g) = group_filter {
                s.group.as_deref() == Some(g)
            } else {
                true
            }
        })
        .map(|s| {
            let mut v = json!({
                "session_id": s.session_id,
                "pid": s.pid,
                "command": s.command,
                "status": if is_alive(s.pid) { "running" } else { "exited" },
                "started_at": s.started_at,
            });
            if let Some(ref g) = s.group { v["group"] = json!(g); }
            if let Some(ref p) = s.parent { v["parent"] = json!(p); }
            if let Some(ref w) = s.workdir { v["workdir"] = json!(w); }
            if let Some(t) = s.tier { v["tier"] = json!(t); }
            if let Some(ref sc) = s.scope { v["scope"] = json!(sc); }
            v
        })
        .collect();

    // Prune dead sessions from registry
    reg.sessions.retain(|s| is_alive(s.pid));
    save_registry(&reg);

    Ok(json!({ "sessions": infos, "count": infos.len() }))
}

fn kill_process(pid: u32) {
    #[cfg(unix)]
    unsafe {
        // Negative PID sends signal to the process group (works with setsid)
        libc::kill(-(pid as i32), libc::SIGTERM);
        // Also signal the individual process in case it wasn't a session leader
        libc::kill(pid as i32, libc::SIGTERM);
    }
    #[cfg(not(unix))]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .output();
    }
}

fn cmd_wait(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Read).map_err(|v| v.to_string())?;
    let mut timeout: Option<u64> = None;
    let mut group_name: Option<&str> = None;
    let mut session_id: Option<&str> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--timeout" if i + 1 < args.len() => {
                timeout = args[i + 1].parse().ok();
                i += 2;
            }
            "--group" if i + 1 < args.len() => {
                group_name = Some(&args[i + 1]);
                i += 2;
            }
            _ => {
                if session_id.is_none() {
                    session_id = Some(&args[i]);
                }
                i += 1;
            }
        }
    }

    let reg = load_registry();

    // Collect PIDs and session IDs to wait on
    let targets: Vec<(String, u32)> = if let Some(g) = group_name {
        reg.sessions.iter()
            .filter(|s| s.group.as_deref() == Some(g))
            .map(|s| (s.session_id.clone(), s.pid))
            .collect()
    } else if let Some(sid) = session_id {
        let info = reg.sessions.iter()
            .find(|s| s.session_id == sid)
            .ok_or_else(|| format!("session not found: {sid}"))?;
        vec![(info.session_id.clone(), info.pid)]
    } else {
        return Err("usage: cos proc wait <session-id> [--timeout N] or --group <name>".into());
    };

    drop(reg);

    if targets.is_empty() {
        return Err("no matching sessions to wait on".into());
    }

    let start = SystemTime::now();
    let timeout_dur = timeout.map(Duration::from_secs);

    loop {
        let all_dead = targets.iter().all(|(_, pid)| !is_alive(*pid));
        if all_dead {
            // Auto-capture ended_at for all exited sessions
            let mut reg = load_registry();
            let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
            for (sid, _) in &targets {
                if let Some(info) = reg.sessions.iter_mut().find(|s| &s.session_id == sid) {
                    if info.ended_at.is_none() {
                        info.ended_at = Some(now.clone());
                    }
                }
            }
            save_registry(&reg);

            // Build results with output tails for each exited session
            let reg = load_registry();
            let results: Vec<Value> = targets.iter()
                .map(|(sid, pid)| {
                    let mut v = json!({
                        "session_id": sid,
                        "pid": pid,
                        "status": "exited",
                    });
                    if let Some(info) = reg.sessions.iter().find(|s| &s.session_id == sid) {
                        let stdout_tail = read_capped(&info.stdout_path, Some(10));
                        let stderr_tail = read_capped(&info.stderr_path, Some(10));
                        if !stdout_tail.is_empty() { v["stdout_tail"] = json!(stdout_tail); }
                        if !stderr_tail.is_empty() { v["stderr_tail"] = json!(stderr_tail); }
                    }
                    v
                })
                .collect();
            return Ok(json!({
                "status": "exited",
                "sessions": results,
            }));
        }

        if let Some(td) = timeout_dur {
            let elapsed = start.elapsed().unwrap_or_default();
            if elapsed >= td {
                let results: Vec<Value> = targets.iter()
                    .map(|(sid, pid)| json!({
                        "session_id": sid,
                        "pid": pid,
                        "status": if is_alive(*pid) { "running" } else { "exited" },
                    }))
                    .collect();
                return Ok(json!({
                    "status": "timeout",
                    "elapsed_secs": elapsed.as_secs(),
                    "sessions": results,
                }));
            }
        }

        thread::sleep(Duration::from_millis(250));
    }
}

fn cmd_signal(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Exec).map_err(|v| v.to_string())?;
    if args.len() < 2 {
        return Err("usage: cos proc signal <session-id> <signal-name>".into());
    }
    let sid = &args[0];
    let signal_name = args[1].to_uppercase();

    let reg = load_registry();
    let info = reg.sessions.iter()
        .find(|s| &s.session_id == sid)
        .ok_or_else(|| format!("session not found: {sid}"))?;

    let pid = info.pid;

    #[cfg(unix)]
    {
        let signum = match signal_name.as_str() {
            "TERM" => libc::SIGTERM,
            "KILL" => libc::SIGKILL,
            "HUP" => libc::SIGHUP,
            "USR1" => libc::SIGUSR1,
            "USR2" => libc::SIGUSR2,
            "STOP" => libc::SIGSTOP,
            "CONT" => libc::SIGCONT,
            _ => return Err(format!(
                "unsupported signal: {signal_name}. Supported: TERM, KILL, HUP, USR1, USR2, STOP, CONT"
            )),
        };
        let ret = unsafe { libc::kill(pid as i32, signum) };
        if ret != 0 {
            return Err(format!("failed to send signal {signal_name} to pid {pid}"));
        }
    }

    #[cfg(not(unix))]
    {
        match signal_name.as_str() {
            "TERM" | "KILL" => {
                let _ = Command::new("taskkill")
                    .args(["/PID", &pid.to_string(), "/F"])
                    .output();
            }
            _ => return Err(format!("signal {signal_name} not supported on Windows")),
        }
    }

    Ok(json!({
        "session_id": sid,
        "pid": pid,
        "signal": signal_name,
        "status": "sent",
    }))
}

fn cmd_result(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Read).map_err(|v| v.to_string())?;
    let sid = args.first().ok_or("usage: cos proc result <session-id>")?;
    let mut reg = load_registry();
    let idx = reg.sessions.iter()
        .position(|s| &s.session_id == sid)
        .ok_or_else(|| format!("session not found: {sid}"))?;

    let alive = is_alive(reg.sessions[idx].pid);
    let status = if alive { "running" } else { "exited" };

    // Auto-capture ended_at if process is dead and not yet recorded
    if !alive && reg.sessions[idx].ended_at.is_none() {
        let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        reg.sessions[idx].ended_at = Some(now);
        save_registry(&reg);
    }

    let info = &reg.sessions[idx];
    let stdout_tail = read_capped(&info.stdout_path, Some(20));
    let stderr_tail = read_capped(&info.stderr_path, Some(20));
    let stdout_bytes = fs::metadata(&info.stdout_path).map(|m| m.len()).unwrap_or(0);
    let stderr_bytes = fs::metadata(&info.stderr_path).map(|m| m.len()).unwrap_or(0);

    // Heuristic: likely success if stderr is empty/small AND stdout doesn't contain error indicators
    let stdout_has_error = stdout_tail.contains("\"error\"") || stdout_tail.contains("permission denied");
    let likely_success = !stdout_has_error
        && (stderr_bytes == 0 || (stdout_bytes > 0 && stderr_bytes < stdout_bytes / 10));

    let mut result = json!({
        "session_id": info.session_id,
        "status": status,
        "started_at": info.started_at,
        "stdout_bytes": stdout_bytes,
        "stderr_bytes": stderr_bytes,
        "likely_success": likely_success,
    });

    if let Some(ref ended) = info.ended_at {
        result["ended_at"] = json!(ended);
        // Calculate duration
        if let Ok(start) = chrono::DateTime::parse_from_rfc3339(
            &info.started_at.replace('Z', "+00:00"),
        ) {
            if let Ok(end) = chrono::DateTime::parse_from_rfc3339(
                &ended.replace('Z', "+00:00"),
            ) {
                let duration = end.signed_duration_since(start);
                result["duration_secs"] = json!(duration.num_seconds());
            }
        }
    }

    if !stdout_tail.is_empty() { result["stdout_tail"] = json!(stdout_tail); }
    if !stderr_tail.is_empty() { result["stderr_tail"] = json!(stderr_tail); }

    Ok(result)
}

fn check_rapid_respawn(reg: &Registry, command_args: &[String]) -> Option<Value> {
    let now = chrono::Utc::now();
    let cutoff = now - chrono::Duration::seconds(60);
    let recent_same = reg.sessions.iter()
        .filter(|s| s.command == command_args)
        .filter(|s| {
            chrono::DateTime::parse_from_rfc3339(
                &s.started_at.replace('Z', "+00:00"),
            )
            .map(|dt| dt > cutoff)
            .unwrap_or(false)
        })
        .count();
    if recent_same >= 5 {
        Some(json!({
            "warning": "rapid_respawn",
            "message": format!(
                "This command has been spawned {} times in the last 60 seconds. Possible infinite loop.",
                recent_same
            ),
            "count": recent_same,
        }))
    } else {
        None
    }
}

fn check_destructive(command_args: &[String]) -> Option<Value> {
    let cmd_str = command_args.join(" ");
    let patterns = [
        ("rm -rf /", "deleting root filesystem"),
        ("rm -rf /*", "deleting root filesystem contents"),
        ("mkfs", "formatting disk"),
        ("dd if=", "raw disk write"),
        ("> /dev/sd", "writing to disk device"),
    ];
    for (pattern, reason) in patterns {
        if cmd_str.contains(pattern) {
            return Some(json!({
                "warning": "destructive_command",
                "message": format!("Potentially destructive operation detected: {reason}"),
                "pattern": pattern,
            }));
        }
    }
    None
}

fn read_from_offset(path: &str, offset: u64) -> (String, u64) {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return (String::new(), offset),
    };
    let file_len = file.metadata().map(|m| m.len()).unwrap_or(0);
    if offset >= file_len {
        return (String::new(), file_len);
    }
    if file.seek(SeekFrom::Start(offset)).is_err() {
        return (String::new(), offset);
    }
    let to_read = (file_len - offset).min(MAX_OUTPUT_BYTES as u64) as usize;
    let mut buf = vec![0u8; to_read];
    let n = match file.read(&mut buf) {
        Ok(n) => n,
        Err(_) => return (String::new(), offset),
    };
    buf.truncate(n);
    let content = String::from_utf8_lossy(&buf).to_string();
    (content, offset + n as u64)
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
