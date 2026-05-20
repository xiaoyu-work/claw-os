//! `cos agent serve` — built-in web UI.
//!
//! Terminal output (`cos agent chat`) is a deliberately plain-text
//! channel — see the header doc on [`crate::agent::display`]: the
//! pure-functional formatter never emits ANSI control codes, so the
//! same renderer is reusable from headless contexts (the gateway,
//! `cos agent ask --full`, mcp-server) that have no tty.
//!
//! For WSL and headless-Linux hosts that lacks a rich tty, plain text
//! is the *only* thing the REPL can produce — no folding for long
//! tool results, no syntax highlighting, no images, no clickable
//! links, no sysinfo dashboard. The fix is not to teach the terminal
//! tricks it doesn't have; it's to expose a second front-end that
//! happens to live in the user's browser instead.
//!
//! `cos agent serve [--bind ADDR] [--port N] [--detach]` boots a tiny
//! axum server that:
//!
//! * Streams chat turns over Server-Sent Events (one request per turn,
//!   long-lived response).
//! * Surfaces the same lifecycle verbs the CLI has (`ls`, `show`,
//!   `stop`, `undo`, `resume`) as JSON endpoints.
//! * Renders an `inbox` view fed by clawd's append-only context
//!   events log (see [`crate::paths::context_events_log_path`]) and a
//!   `sysinfo` dashboard fed by [`crate::sysinfo`].
//! * Surfaces the approval queue ([`crate::approvals`]) so a user can
//!   actually answer the pending consent prompts that block agent
//!   work in clawd-routed setups.
//!
//! Auth is intentionally minimal: a one-shot 32-byte hex token loaded
//! from `$COS_DATA_DIR/agent/web/serve.token` (auto-generated on
//! first run) is required as `?t=<token>` or `Authorization: Bearer
//! <token>`. By default the server only binds `127.0.0.1`. Exposing
//! to other interfaces (`--bind 0.0.0.0`) is allowed but the token
//! gate stays on. There is no multi-user support — this server
//! represents the local user, period.

pub mod assets;
pub mod auth;
pub mod routes;
pub mod server;
pub mod sse;
pub mod state;

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// Where the daemon writes its PID file. Co-located with `serve.token`
/// under `$COS_DATA_DIR/agent/web/` so a single directory carries the
/// whole serve session: token, pid, log.
fn pid_path() -> PathBuf {
    auth::token_dir().join("serve.pid")
}

/// Default log path when daemonised. Override with `--log`.
fn default_log_path() -> PathBuf {
    auth::token_dir().join("serve.log")
}

/// `cos agent serve` entry point — invoked from
/// [`crate::agent::run`]'s match arm.
pub fn serve(args: &[String]) -> Result<Value, String> {
    let mut bind: String = "127.0.0.1".to_string();
    let mut port: u16 = 7878;
    let mut token_override: Option<String> = None;
    let mut open_browser = false;
    let mut detach = false;
    let mut stop_daemon_flag = false;
    let mut status_daemon_flag = false;
    let mut foreground_flag = false;
    let mut log_override: Option<PathBuf> = None;

    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--bind" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| "--bind needs <addr>".to_string())?;
                bind = v.clone();
                i += 2;
            }
            "--port" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| "--port needs <n>".to_string())?;
                port = v
                    .parse()
                    .map_err(|e| format!("--port: {e}"))?;
                i += 2;
            }
            "--token" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| "--token needs <hex>".to_string())?;
                token_override = Some(v.clone());
                i += 2;
            }
            "--open" => {
                open_browser = true;
                i += 1;
            }
            "--detach" | "-d" | "--daemon" => {
                detach = true;
                i += 1;
            }
            "--foreground" | "--fg" => {
                // Used internally by the spawned child so we know not to
                // re-detach. Also useful if the user passes both --detach
                // and --foreground (foreground wins, for debugging).
                foreground_flag = true;
                i += 1;
            }
            "--log" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| "--log needs <path>".to_string())?;
                log_override = Some(PathBuf::from(v));
                i += 2;
            }
            "--stop" => {
                stop_daemon_flag = true;
                i += 1;
            }
            "--status" => {
                status_daemon_flag = true;
                i += 1;
            }
            "--help" | "-h" => {
                return Ok(json!({
                    "command": "cos agent serve",
                    "summary": "Run the built-in web UI for chat / tasks / approvals / sysinfo.",
                    "usage": "cos agent serve [--bind 127.0.0.1] [--port 7878] [--token <hex>] [--open] [--detach|--stop|--status] [--log <path>]",
                    "flags": {
                        "--bind": "Network interface to bind. Default 127.0.0.1 (localhost only).",
                        "--port": "TCP port. Default 7878.",
                        "--token": "Override the persisted access token. Default: load/generate from $COS_DATA_DIR/agent/web/serve.token.",
                        "--open": "Print the URL with the token query parameter so the user can paste it into a browser.",
                        "--detach / -d": "Run in the background. Writes PID to $COS_DATA_DIR/agent/web/serve.pid and logs to serve.log (override with --log).",
                        "--stop": "Stop a previously-detached daemon (SIGTERM, then SIGKILL after 5s).",
                        "--status": "Report whether a detached daemon is running; print URL + PID if so.",
                        "--log": "Path for the detached daemon's log file. Default $COS_DATA_DIR/agent/web/serve.log.",
                    },
                    "url": format!("http://{bind}:{port}/?t=<token>"),
                }));
            }
            other => return Err(format!("unknown flag for `serve`: {other} (try --help)")),
        }
    }

    if stop_daemon_flag {
        return stop_daemon();
    }
    if status_daemon_flag {
        return report_status();
    }
    if detach && !foreground_flag {
        return spawn_detached(args, &bind, port, log_override.as_deref());
    }

    let cfg = crate::config::get().agent.clone();
    // Deliberately do *not* short-circuit on `is_ready`: the UI itself
    // remains useful for inspecting tasks, approvals, inbox, sysinfo,
    // and serves as the place a user discovers they still need to run
    // `cos agent setup llm`. The chat SSE handler surfaces the
    // `is_ready` error inline as a streamed `error` frame, so the
    // user gets actionable feedback in the browser instead of a
    // command that refuses to start.

    let token = match token_override {
        Some(t) => auth::persist_token(&t).map_err(|e| format!("persist token: {e}"))?,
        None => auth::load_or_generate_token().map_err(|e| format!("token: {e}"))?,
    };

    let addr: std::net::SocketAddr = format!("{bind}:{port}")
        .parse()
        .map_err(|e| format!("bad bind {bind}:{port}: {e}"))?;

    let url = format!("http://{}/?t={}", addr, token);
    eprintln!("cos agent serve — listening on {addr}");
    eprintln!("  open: {url}");
    if open_browser {
        let _ = try_open_browser(&url);
    }
    eprintln!("  token persisted at {}", auth::token_path().display());
    if foreground_flag {
        eprintln!("  pid file: {}", pid_path().display());
        eprintln!("  stop with: cos agent serve --stop");
    } else {
        eprintln!("  press Ctrl-C to stop (or run with --detach to background).");
    }

    // Drop any stale PID file from a crashed previous daemon, then claim it.
    if let Err(e) = write_pid_file(std::process::id()) {
        eprintln!("warning: could not write {}: {e}", pid_path().display());
    }
    // Ensure PID file is cleaned up on exit even if we panic; the
    // drop-guard runs before the runtime tears down.
    let _pid_guard = PidGuard;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime: {e}"))?;

    runtime.block_on(async move {
        let state = state::AppState::new(cfg, token);
        let app = server::build_app(state);
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|e| format!("bind {addr}: {e}"))?;
        axum::serve(listener, app.into_make_service())
            .with_graceful_shutdown(shutdown_signal())
            .await
            .map_err(|e| format!("serve: {e}"))?;
        Ok::<_, String>(())
    })?;

    Ok(json!({
        "status": "stopped",
        "bind": bind,
        "port": port,
    }))
}

/// Re-execute `cos agent serve …` as a backgrounded child process. The
/// child runs the normal foreground path (we add `--foreground` to its
/// argv so it doesn't try to re-detach) with stdio detached from the
/// caller's terminal and routed to a log file. We wait briefly for the
/// PID file to materialise so the parent can return either a clean
/// "running" message or a meaningful error culled from the log tail.
fn spawn_detached(
    args: &[String],
    bind: &str,
    port: u16,
    log_override: Option<&std::path::Path>,
) -> Result<Value, String> {
    // Refuse to start a second daemon if one is already alive — the
    // user almost certainly forgot to `--stop` first.
    if let Some(pid) = read_pid_file() {
        if process_alive(pid) {
            return Err(format!(
                "cos agent serve is already running (pid {pid}). \
                 Use `cos agent serve --stop` first, or `--status` to inspect."
            ));
        }
        // Stale pid file from a crashed run — remove and continue.
        let _ = fs::remove_file(pid_path());
    }

    let dir = auth::token_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let log_path = log_override
        .map(PathBuf::from)
        .unwrap_or_else(default_log_path);
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| format!("open log {}: {e}", log_path.display()))?;
    let log_clone = log
        .try_clone()
        .map_err(|e| format!("dup log fd: {e}"))?;

    let exe = std::env::current_exe()
        .map_err(|e| format!("locate current exe: {e}"))?;

    // Re-pass every original flag except --detach/-d (and inject
    // --foreground so the child runs the normal serve path).
    let mut child_args: Vec<String> = vec!["agent".into(), "serve".into(), "--foreground".into()];
    for a in args {
        match a.as_str() {
            "--detach" | "-d" | "--daemon" | "--foreground" | "--fg" => {}
            // --open in detached mode is meaningless (no terminal to print to);
            // drop it.
            "--open" => {}
            _ => child_args.push(a.clone()),
        }
    }

    let mut cmd = std::process::Command::new(&exe);
    cmd.args(&child_args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_clone))
        .stderr(Stdio::from(log));
    // Clear the parent's session so the child can bootstrap its own
    // proc-registry session via [`crate::caps::bootstrap`]. If we let
    // the child inherit COS_SESSION, the parent's `SessionGuard::Drop`
    // (running just before `spawn_detached` returns) will deregister
    // the row out from under the daemon, leaving every gated call
    // (sysinfo, etc.) failing with "no active session".
    cmd.env_remove("COS_SESSION");

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Detach from the parent's controlling terminal + process group so
        // Ctrl-C in the caller's shell doesn't reach us, and so the child
        // outlives the calling shell session.
        unsafe {
            cmd.pre_exec(|| {
                // setsid: become session leader of a new session, no
                // controlling tty. Best-effort — if it fails (e.g. already
                // a session leader) we still try to run.
                let _ = libc::setsid();
                Ok(())
            });
        }
    }

    let child = cmd
        .spawn()
        .map_err(|e| format!("spawn detached: {e}"))?;
    let child_pid = child.id();

    // Wait for the child to either bind successfully (writes its own
    // PID file) or exit fast (we read the log tail for context).
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        if let Some(p) = read_pid_file() {
            if p as u32 == child_pid || process_alive(p) {
                // Bound successfully.
                let token = match auth::load_or_generate_token() {
                    Ok(t) => t,
                    Err(_) => String::new(),
                };
                let url = if token.is_empty() {
                    format!("http://{bind}:{port}/")
                } else {
                    format!("http://{bind}:{port}/?t={token}")
                };
                return Ok(json!({
                    "status": "running",
                    "pid": p,
                    "bind": bind,
                    "port": port,
                    "url": url,
                    "log": log_path.display().to_string(),
                    "pid_file": pid_path().display().to_string(),
                    "stop": "cos agent serve --stop",
                }));
            }
        }
        // Child died?
        if !process_alive(child_pid as i32) {
            let tail = tail_log(&log_path, 40);
            return Err(format!(
                "cos agent serve failed to start (pid {child_pid} exited). \
                 Last log lines from {}:\n{}",
                log_path.display(),
                tail
            ));
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "cos agent serve did not become ready within 6s. \
                 Check log: {}",
                log_path.display()
            ));
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

fn stop_daemon() -> Result<Value, String> {
    let Some(pid) = read_pid_file() else {
        return Ok(json!({
            "status": "not running",
            "message": "no pid file found",
            "pid_file": pid_path().display().to_string(),
        }));
    };
    if !process_alive(pid) {
        let _ = fs::remove_file(pid_path());
        return Ok(json!({
            "status": "not running",
            "message": format!("stale pid file (pid {pid} not alive); removed"),
        }));
    }
    #[cfg(unix)]
    unsafe {
        // SIGTERM = graceful shutdown_signal() path in the daemon.
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        return Err("--stop is only supported on Unix-like systems".into());
    }
    // Wait up to 5s for the process to exit, then SIGKILL.
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if !process_alive(pid) {
            let _ = fs::remove_file(pid_path());
            return Ok(json!({
                "status": "stopped",
                "pid": pid,
                "signal": "SIGTERM",
            }));
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGKILL);
    }
    let _ = fs::remove_file(pid_path());
    Ok(json!({
        "status": "stopped",
        "pid": pid,
        "signal": "SIGKILL",
        "note": "process did not exit within 5s of SIGTERM",
    }))
}

fn report_status() -> Result<Value, String> {
    let Some(pid) = read_pid_file() else {
        return Ok(json!({
            "status": "not running",
            "pid_file": pid_path().display().to_string(),
        }));
    };
    if !process_alive(pid) {
        return Ok(json!({
            "status": "not running",
            "note": format!("stale pid file (pid {pid})"),
        }));
    }
    let token = auth::load_or_generate_token().unwrap_or_default();
    Ok(json!({
        "status": "running",
        "pid": pid,
        "token_persisted_at": auth::token_path().display().to_string(),
        "url_hint": format!("http://127.0.0.1:7878/?t={}", token),
        "log": default_log_path().display().to_string(),
        "stop": "cos agent serve --stop",
    }))
}

fn write_pid_file(pid: u32) -> Result<(), String> {
    let dir = auth::token_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let path = pid_path();
    let mut f = fs::File::create(&path).map_err(|e| format!("create {}: {e}", path.display()))?;
    f.write_all(format!("{pid}\n").as_bytes())
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(())
}

fn read_pid_file() -> Option<i32> {
    let s = fs::read_to_string(pid_path()).ok()?;
    s.trim().parse::<i32>().ok()
}

fn process_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    #[cfg(unix)]
    unsafe {
        // kill(pid, 0) returns 0 if the process exists and we have permission
        // to signal it; -1/EPERM also means "alive (just not ours)".
        let r = libc::kill(pid as libc::pid_t, 0);
        if r == 0 {
            return true;
        }
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

fn tail_log(path: &std::path::Path, lines: usize) -> String {
    let Ok(text) = fs::read_to_string(path) else {
        return String::from("(log unavailable)");
    };
    let collected: Vec<&str> = text.lines().collect();
    let start = collected.len().saturating_sub(lines);
    collected[start..].join("\n")
}

struct PidGuard;
impl Drop for PidGuard {
    fn drop(&mut self) {
        // Only delete the pid file if it still points at us — avoids
        // wiping a fresh daemon's pid if we crashed mid-shutdown.
        if let Some(p) = read_pid_file() {
            if p as u32 == std::process::id() {
                let _ = fs::remove_file(pid_path());
            }
        }
    }
}

fn try_open_browser(url: &str) -> Result<(), String> {
    let (program, args): (&str, Vec<&str>) = if cfg!(target_os = "windows") {
        ("cmd", vec!["/C", "start", url])
    } else if cfg!(target_os = "macos") {
        ("open", vec![url])
    } else {
        ("xdg-open", vec![url])
    };
    std::process::Command::new(program)
        .args(&args)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            sig.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    eprintln!("\n[shutdown] draining…");
}
