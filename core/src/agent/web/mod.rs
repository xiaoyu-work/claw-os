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
//! `cos agent serve [--bind ADDR] [--port N]` boots a tiny axum
//! server that:
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

use serde_json::{json, Value};

/// `cos agent serve` entry point — invoked from
/// [`crate::agent::run`]'s match arm.
pub fn serve(args: &[String]) -> Result<Value, String> {
    let mut bind: String = "127.0.0.1".to_string();
    let mut port: u16 = 7878;
    let mut token_override: Option<String> = None;
    let mut open_browser = false;

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
            "--help" | "-h" => {
                return Ok(json!({
                    "command": "cos agent serve",
                    "summary": "Run the built-in web UI for chat / tasks / approvals / sysinfo.",
                    "usage": "cos agent serve [--bind 127.0.0.1] [--port 7878] [--token <hex>] [--open]",
                    "flags": {
                        "--bind": "Network interface to bind. Default 127.0.0.1 (localhost only).",
                        "--port": "TCP port. Default 7878.",
                        "--token": "Override the persisted access token. Default: load/generate from $COS_DATA_DIR/agent/web/serve.token.",
                        "--open": "Print the URL with the token query parameter so the user can paste it into a browser.",
                    },
                    "url": format!("http://{bind}:{port}/?t=<token>"),
                }));
            }
            other => return Err(format!("unknown flag for `serve`: {other} (try --help)")),
        }
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
    eprintln!("  press Ctrl-C to stop.");

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
