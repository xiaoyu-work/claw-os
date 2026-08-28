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
//! Auth uses a persistent 32-byte bootstrap secret loaded from
//! `$COS_DATA_DIR/agent/web/serve.token` (auto-generated on first run).
//! The UI exchanges it once at `/api/auth/token`; normal requests use only
//! signed Bearer access tokens. By default the server binds `127.0.0.1`.
//! Exposing
//! to other interfaces (`--bind 0.0.0.0`) requires a PEM certificate and
//! owner-only private key. The bootstrap is exchanged for a signed one-hour
//! access token; API query-string tokens are never accepted. Each
//! instance is bound to exactly one non-root Unix uid
//! and refuses shared or wrong-owner state directories. Multiple desktop
//! users run separate isolated instances.

pub mod assets;
pub mod auth;
pub mod routes;
pub mod server;
pub mod sse;
pub mod state;

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// Where the daemon writes its PID file. Co-located with `serve.token`
/// under `$COS_DATA_DIR/agent/web/` so a single directory carries the
/// whole serve session: token, pid, log.
fn pid_path() -> PathBuf {
    auth::token_dir().join("serve.pid")
}

fn serve_info_path() -> PathBuf {
    auth::token_dir().join("serve.json")
}

fn ready_path() -> PathBuf {
    auth::token_dir().join("serve.ready")
}

/// Default log path when daemonised. Override with `--log`.
fn default_log_path() -> PathBuf {
    auth::token_dir().join("serve.log")
}

/// `cos agent serve` entry point — invoked from
/// [`crate::agent::run`]'s match arm.
pub fn serve(args: &[String]) -> Result<Value, String> {
    let _detached_session_guard = DetachedSessionGuard::from_env();
    let mut bind: String = "127.0.0.1".to_string();
    let mut port: u16 = 7878;
    let mut token_override: Option<String> = None;
    let mut open_browser = false;
    let mut detach = false;
    let mut stop_daemon_flag = false;
    let mut status_daemon_flag = false;
    let mut foreground_flag = false;
    let mut log_override: Option<PathBuf> = None;
    let mut tls_cert: Option<PathBuf> = None;
    let mut tls_key: Option<PathBuf> = None;
    let mut rotate_token = false;

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
            "--tls-cert" => {
                let value = args
                    .get(i + 1)
                    .ok_or_else(|| "--tls-cert needs <pem-file>".to_string())?;
                tls_cert = Some(PathBuf::from(value));
                i += 2;
            }
            "--tls-key" => {
                let value = args
                    .get(i + 1)
                    .ok_or_else(|| "--tls-key needs <pem-file>".to_string())?;
                tls_key = Some(PathBuf::from(value));
                i += 2;
            }
            "--rotate-token" => {
                rotate_token = true;
                i += 1;
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
                    "usage": "cos agent serve [--bind 127.0.0.1] [--port 7878] [--tls-cert cert.pem --tls-key key.pem] [--token <hex>] [--rotate-token] [--open] [--detach|--stop|--status] [--log <path>]",
                    "flags": {
                        "--bind": "Network interface to bind. Default 127.0.0.1 (localhost only).",
                        "--port": "TCP port. Default 7878.",
                        "--token": "Override the persistent 64-hex bootstrap secret. Default: load/generate from $COS_DATA_DIR/agent/web/serve.token.",
                        "--open": "Open the UI. Loopback URLs carry the bootstrap in ?t= only for the frontend exchange step.",
                        "--detach / -d": "Run in the background. Writes PID to $COS_DATA_DIR/agent/web/serve.pid and logs to serve.log (override with --log).",
                        "--stop": "Stop a previously-detached daemon (SIGTERM, then SIGKILL after 5s).",
                        "--status": "Report whether a detached daemon is running; print URL + PID if so.",
                        "--log": "Path for the detached daemon's log file. Default $COS_DATA_DIR/agent/web/serve.log.",
                        "--tls-cert": "PEM certificate chain. Required with --tls-key.",
                        "--tls-key": "Owner-only PEM private key. Required with --tls-cert.",
                        "--rotate-token": "Rotate bootstrap and signing secrets, immediately invalidating every issued access token.",
                    },
                    "url": format!("{}://{}/", if tls_cert.is_some() { "https" } else { "http" }, display_address(&bind, port)),
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
    if rotate_token {
        let owner_uid = state::current_owner_uid()?;
        state::validate_owner_storage(owner_uid)?;
        let bootstrap = auth::rotate_tokens()?;
        return Ok(json!({
            "rotated": true,
            "bootstrap_token": bootstrap,
            "bootstrap_path": auth::token_path().display().to_string(),
            "signing_key_path": auth::signing_key_path().display().to_string(),
            "note": "all previously issued access tokens are now invalid",
        }));
    }
    let owner_uid = state::current_owner_uid()?;
    let addr: std::net::SocketAddr = format!("{bind}:{port}")
        .parse()
        .map_err(|e| format!("bad bind {bind}:{port}: {e}"))?;
    let tls_enabled = match (&tls_cert, &tls_key) {
        (Some(_), Some(_)) => true,
        (None, None) => false,
        _ => return Err("--tls-cert and --tls-key must be provided together".to_string()),
    };
    if !addr.ip().is_loopback() && !tls_enabled {
        return Err(
            "non-loopback agent web binds require TLS; provide --tls-cert and --tls-key or bind only to 127.0.0.1"
                .to_string(),
        );
    }
    if detach && !foreground_flag {
        return spawn_detached(
            args,
            &bind,
            port,
            tls_enabled,
            log_override.as_deref(),
        );
    }

    let cfg = crate::config::current_snapshot().agent.clone();
    // Deliberately do *not* short-circuit on `is_ready`: the UI itself
    // remains useful for inspecting tasks, approvals, inbox, sysinfo,
    // and serves as the place a user discovers they still need to run
    // `cos agent setup text`. The chat SSE handler surfaces the
    // `is_ready` error inline as a streamed `error` frame, so the
    // user gets actionable feedback in the browser instead of a
    // command that refuses to start.

    state::validate_owner_storage(owner_uid)?;
    let token = match token_override {
        Some(t) => auth::persist_token(&t).map_err(|e| format!("persist token: {e}"))?,
        None => auth::load_or_generate_token().map_err(|e| format!("token: {e}"))?,
    };
    auth::ensure_signing_key()?;
    let tls_material = match (tls_cert.as_deref(), tls_key.as_deref()) {
        (Some(cert), Some(key)) => Some(read_tls_material(cert, key, owner_uid)?),
        (None, None) => None,
        _ => unreachable!("TLS pairing validated above"),
    };

    let scheme = if tls_enabled { "https" } else { "http" };
    let url = if addr.ip().is_loopback() {
        format!("{scheme}://{addr}/?t={token}")
    } else {
        format!("{scheme}://{addr}/")
    };
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

    let listener = std::net::TcpListener::bind(addr)
        .map_err(|error| format!("bind {addr}: {error}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|error| format!("set nonblocking {addr}: {error}"))?;
    let _ = fs::remove_file(ready_path());
    // Drop any stale PID file from a crashed previous daemon, then claim it.
    if let Err(e) = write_pid_file(std::process::id()) {
        eprintln!("warning: could not write {}: {e}", pid_path().display());
    }
    if let Err(error) = write_serve_info(std::process::id(), addr, scheme) {
        eprintln!(
            "warning: could not write {}: {error}",
            serve_info_path().display()
        );
    }
    // Ensure PID file is cleaned up on exit even if we panic; the
    // drop-guard runs before the runtime tears down.
    let _pid_guard = PidGuard;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime: {e}"))?;

    runtime.block_on(async move {
        let state = state::AppState::new(cfg, owner_uid);
        let app = server::build_app(state);
        if let Some((cert, key)) = tls_material {
            let _ = rustls::crypto::ring::default_provider().install_default();
            let config = axum_server::tls_rustls::RustlsConfig::from_pem(cert, key)
                .await
                .map_err(|error| format!("load TLS certificate/key: {error}"))?;
            let handle = axum_server::Handle::new();
            let shutdown_handle = handle.clone();
            tokio::spawn(async move {
                shutdown_signal().await;
                shutdown_handle.graceful_shutdown(Some(Duration::from_secs(10)));
            });
            let server = axum_server::from_tcp_rustls(listener, config)
                .map_err(|error| format!("prepare TLS listener: {error}"))?;
            write_ready_file(std::process::id())?;
            server
                .handle(handle)
                .serve(app.into_make_service())
                .await
                .map_err(|error| format!("serve TLS: {error}"))?;
        } else {
            let listener = tokio::net::TcpListener::from_std(listener)
                .map_err(|error| format!("adopt listener {addr}: {error}"))?;
            write_ready_file(std::process::id())?;
            axum::serve(listener, app.into_make_service())
                .with_graceful_shutdown(shutdown_signal())
                .await
                .map_err(|e| format!("serve: {e}"))?;
        }
        Ok::<_, String>(())
    })?;

    Ok(json!({
        "status": "stopped",
        "bind": bind,
        "port": port,
    }))
}

fn read_tls_material(
    certificate: &Path,
    private_key: &Path,
    owner_uid: u32,
) -> Result<(Vec<u8>, Vec<u8>), String> {
    let certificate = read_tls_file(certificate, owner_uid, false)?;
    let private_key = read_tls_file(private_key, owner_uid, true)?;
    Ok((certificate, private_key))
}

fn read_tls_file(path: &Path, owner_uid: u32, private_key: bool) -> Result<Vec<u8>, String> {
    const MAX_PEM_BYTES: u64 = 1024 * 1024;
    if !path.is_absolute() {
        return Err(format!("TLS paths must be absolute: {}", path.display()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

        let metadata = fs::symlink_metadata(path)
            .map_err(|error| format!("inspect TLS file {}: {error}", path.display()))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(format!(
                "TLS file must be a regular non-symlink: {}",
                path.display()
            ));
        }
        let mut options = fs::OpenOptions::new();
        options
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        let mut file = options
            .open(path)
            .map_err(|error| format!("open TLS file {}: {error}", path.display()))?;
        let opened = file
            .metadata()
            .map_err(|error| format!("inspect opened TLS file {}: {error}", path.display()))?;
        if !opened.is_file()
            || opened.dev() != metadata.dev()
            || opened.ino() != metadata.ino()
            || opened.len() == 0
            || opened.len() > MAX_PEM_BYTES
        {
            return Err(format!(
                "TLS file changed, is empty, or exceeds {} bytes: {}",
                MAX_PEM_BYTES,
                path.display()
            ));
        }
        if private_key
            && (opened.uid() != owner_uid || opened.permissions().mode() & 0o077 != 0)
        {
            return Err(format!(
                "TLS private key {} must be owned by uid {owner_uid} with no group/other permissions",
                path.display()
            ));
        }
        if !private_key && opened.permissions().mode() & 0o022 != 0 {
            return Err(format!(
                "TLS certificate {} must not be group/other writable",
                path.display()
            ));
        }
        let mut bytes = Vec::with_capacity(opened.len() as usize);
        file.read_to_end(&mut bytes)
            .map_err(|error| format!("read TLS file {}: {error}", path.display()))?;
        Ok(bytes)
    }
    #[cfg(not(unix))]
    {
        let _ = (path, owner_uid, private_key);
        Err("built-in TLS file validation requires Unix".to_string())
    }
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
    tls_enabled: bool,
    log_override: Option<&std::path::Path>,
) -> Result<Value, String> {
    if crate::paths::current_owner_uid_override().is_some() {
        return Err(
            "detached agent serve cannot be started from a routed job"
                .to_string(),
        );
    }
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
        let _ = fs::remove_file(serve_info_path());
        let _ = fs::remove_file(ready_path());
    }

    let dir = auth::token_dir();
    crate::storage::ensure_private_dir(&dir)
        .map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let log_path = log_override
        .map(PathBuf::from)
        .unwrap_or_else(default_log_path);
    let mut log_options = fs::OpenOptions::new();
    log_options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        log_options.mode(0o600);
    }
    let log = log_options
        .open(&log_path)
        .map_err(|e| format!("open log {}: {e}", log_path.display()))?;
    crate::storage::set_private_file(&log_path)
        .map_err(|e| format!("chmod log {}: {e}", log_path.display()))?;
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
    let detached_session = register_detached_session(&child_args)?;
    let mut pending_session =
        PendingDetachedSessionGuard::new(detached_session.clone());

    let mut cmd = std::process::Command::new(&exe);
    cmd.args(&child_args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_clone))
        .stderr(Stdio::from(log))
        .env("COS_SESSION", &detached_session)
        .env("COS_PROC_DATA_DIR", crate::paths::proc_data_dir())
        .env("COS_DETACHED_SESSION", &detached_session);

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

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("spawn detached: {e}"))?;
    let child_pid = child.id();
    if let Err(error) =
        crate::proc::bind_session_process(&detached_session, child_pid)
    {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("bind detached session: {error}"));
    }

    // Wait for the child to either bind successfully (writes its own
    // PID file) or exit fast (we read the log tail for context).
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        if let Some(p) = read_ready_pid() {
            if p as u32 == child_pid && process_alive(p) {
                // Bound successfully.
                let token = auth::load_or_generate_token().unwrap_or_default();
                let scheme = if tls_enabled { "https" } else { "http" };
                let loopback = bind
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|address| address.is_loopback());
                let address = display_address(bind, port);
                let url = if token.is_empty() || !loopback {
                    format!("{scheme}://{address}/")
                } else {
                    format!("{scheme}://{address}/?t={token}")
                };
                pending_session.disarm();
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
        match child.try_wait() {
            Ok(Some(status)) => {
                let tail = tail_log(&log_path, 40);
                return Err(format!(
                    "cos agent serve failed to start (pid {child_pid}, status {status}). \
                     Last log lines from {}:\n{}",
                    log_path.display(),
                    tail
                ));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "inspect detached process {child_pid}: {error}"
                ));
            }
            Ok(None) => {}
        }
        if !process_alive(child_pid as i32) {
            let _ = child.wait();
            let tail = tail_log(&log_path, 40);
            return Err(format!(
                "cos agent serve failed to start (pid {child_pid} exited). \
                 Last log lines from {}:\n{}",
                log_path.display(),
                tail
            ));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
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
        let _ = fs::remove_file(pid_path());
        let _ = fs::remove_file(serve_info_path());
        let _ = fs::remove_file(ready_path());
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
            let _ = fs::remove_file(serve_info_path());
            let _ = fs::remove_file(ready_path());
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
    let _ = fs::remove_file(serve_info_path());
    let _ = fs::remove_file(ready_path());
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
        let _ = fs::remove_file(serve_info_path());
        return Ok(json!({
            "status": "not running",
            "note": format!("stale pid file (pid {pid})"),
        }));
    }
    if read_ready_pid() != Some(pid) {
        return Ok(json!({
            "status": "starting",
            "pid": pid,
            "log": default_log_path().display().to_string(),
        }));
    }
    let token = auth::load_or_generate_token().unwrap_or_default();
    let info = read_serve_info().filter(|info| info["pid"].as_u64() == Some(pid as u64));
    let scheme = info
        .as_ref()
        .and_then(|value| value["scheme"].as_str())
        .unwrap_or("http");
    let address = info
        .as_ref()
        .and_then(|value| value["address"].as_str())
        .unwrap_or("127.0.0.1:7878");
    let loopback = address
        .parse::<std::net::SocketAddr>()
        .is_ok_and(|value| value.ip().is_loopback());
    let url_hint = if loopback {
        format!("{scheme}://{address}/?t={token}")
    } else {
        format!("{scheme}://{address}/")
    };
    Ok(json!({
        "status": "running",
        "pid": pid,
        "token_persisted_at": auth::token_path().display().to_string(),
        "url_hint": url_hint,
        "log": default_log_path().display().to_string(),
        "stop": "cos agent serve --stop",
    }))
}

fn display_address(bind: &str, port: u16) -> String {
    format!("{bind}:{port}")
        .parse::<std::net::SocketAddr>()
        .map(|address| address.to_string())
        .unwrap_or_else(|_| format!("{bind}:{port}"))
}

fn write_pid_file(pid: u32) -> Result<(), String> {
    let dir = auth::token_dir();
    crate::storage::ensure_private_dir(&dir)
        .map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let path = pid_path();
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    let mut f = options
        .open(&path)
        .map_err(|e| format!("create {}: {e}", path.display()))?;
    f.write_all(format!("{pid}\n").as_bytes())
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(())
}

fn write_serve_info(
    pid: u32,
    address: std::net::SocketAddr,
    scheme: &str,
) -> Result<(), String> {
    let data = serde_json::to_vec(&json!({
        "pid": pid,
        "address": address.to_string(),
        "scheme": scheme,
        "tls": scheme == "https",
    }))
    .map_err(|error| format!("serialize serve state: {error}"))?;
    crate::agent::util::atomic_write_with_fsync(&serve_info_path(), &data)
        .map_err(|error| format!("write {}: {error}", serve_info_path().display()))
}

fn read_serve_info() -> Option<Value> {
    let data = fs::read(serve_info_path()).ok()?;
    serde_json::from_slice(&data).ok()
}

fn write_ready_file(pid: u32) -> Result<(), String> {
    crate::agent::util::atomic_write_with_fsync(
        &ready_path(),
        format!("{pid}\n").as_bytes(),
    )
    .map_err(|error| format!("write {}: {error}", ready_path().display()))
}

fn read_ready_pid() -> Option<i32> {
    fs::read_to_string(ready_path()).ok()?.trim().parse().ok()
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
                let _ = fs::remove_file(serve_info_path());
                let _ = fs::remove_file(ready_path());
            }
        }
    }
}

struct DetachedSessionGuard {
    session_id: Option<String>,
}

impl DetachedSessionGuard {
    fn from_env() -> Self {
        Self {
            session_id: std::env::var("COS_DETACHED_SESSION")
                .ok()
                .filter(|session| !session.is_empty()),
        }
    }
}

impl Drop for DetachedSessionGuard {
    fn drop(&mut self) {
        if let Some(session_id) = self.session_id.take() {
            crate::proc::deregister_current_process_session(
                &session_id,
                "agent-web",
            );
        }
    }
}

struct PendingDetachedSessionGuard {
    session_id: Option<String>,
}

impl PendingDetachedSessionGuard {
    fn new(session_id: String) -> Self {
        Self {
            session_id: Some(session_id),
        }
    }

    fn disarm(&mut self) {
        self.session_id = None;
    }
}

impl Drop for PendingDetachedSessionGuard {
    fn drop(&mut self) {
        if let Some(session_id) = self.session_id.take() {
            crate::proc::deregister_session(&session_id);
        }
    }
}

fn register_detached_session(command: &[String]) -> Result<String, String> {
    let parent = crate::proc::current_session_info_for_caps().ok_or_else(|| {
        "detached agent serve requires a registered parent session".to_string()
    })?;
    crate::caps::enforcement::require_current_session_identity(
        &parent.session_id,
        parent.pid,
    )
    .map_err(|error| {
        format!("detached agent parent identity check failed: {error}")
    })?;
    if parent.app_id.is_some() {
        return Err(
            "an App session cannot detach the system Agent web service"
                .to_string(),
        );
    }
    let caps = parent.caps.clone().ok_or_else(|| {
        "detached agent parent session has no capabilities".to_string()
    })?;
    let session_id = format!("agent-web-{}", uuid::Uuid::new_v4().simple());
    let pid = std::process::id();
    let info = crate::proc::SessionInfo {
        session_id: session_id.clone(),
        pid,
        command: command.to_vec(),
        started_at: chrono::Utc::now().to_rfc3339(),
        stdout_path: String::new(),
        stderr_path: String::new(),
        group: Some("agent-web".to_string()),
        parent: Some(parent.session_id),
        workdir: std::env::current_dir()
            .ok()
            .map(|path| path.to_string_lossy().into_owned()),
        exit_code: None,
        ended_at: None,
        tier: parent.tier,
        scope: parent.scope,
        priority: parent.priority,
        caps: Some(caps),
        transient_caps: None,
        role: parent.role,
        app_id: None,
        pending_bind: false,
        start_time_ticks: crate::proc::read_start_time_ticks_pub(pid),
    };
    crate::proc::register_session(info)?;
    Ok(session_id)
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
