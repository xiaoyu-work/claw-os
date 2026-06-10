use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

use crate::agent::service::{self, WorkerOptions};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use super::client_identity::ClientIdentity;
use super::protocol::{encode_response, Request, Response};
use super::state::DaemonState;
use super::{audit, context, context_events, memory, permissions, system_journal, tasks, transactions};

#[derive(Debug, Clone)]
pub struct ServerOptions {
    pub socket_path: PathBuf,
    pub socket_mode: u32,
}

pub async fn run(options: ServerOptions) -> Result<(), String> {
    prepare_socket(&options.socket_path).await?;
    let listener = UnixListener::bind(&options.socket_path)
        .map_err(|err| format!("failed to bind {}: {err}", options.socket_path.display()))?;
    set_socket_permissions(&options.socket_path, options.socket_mode)?;

    tracing::info!(socket = %options.socket_path.display(), "clawd listening");

    let state = DaemonState::new();
    audit::install_runtime_hook();
    context::refresh_builtin_sources(&state);
    spawn_agent_worker();
    spawn_heartbeat();
    loop {
        let (stream, _addr) = listener
            .accept()
            .await
            .map_err(|err| format!("failed to accept clawd client: {err}"))?;
        let client = ClientIdentity::from_stream(&stream);
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(err) = handle_client(stream, state, client).await {
                tracing::warn!(error = %err, "clawd client handler failed");
            }
        });
    }
}

fn spawn_agent_worker() {
    let shutdown = Arc::new(AtomicBool::new(false));
    tokio::task::spawn_blocking(move || {
        let options = WorkerOptions {
            once: false,
            poll_ms: 500,
            max_jobs: None,
        };
        if let Err(err) = service::run_worker_loop(options, shutdown) {
            tracing::error!(error = %err, "clawd agent worker stopped");
        }
    });
}

/// Spawn the system-vitals heartbeat — the cheap, always-on reflex loop
/// that samples kernel vitals, emits `context.event`s on threshold
/// crossings (which the trigger engine may turn into agent jobs), and
/// drives the `cron` / `triggers` schedulers so the daemon is its own
/// clock. The heartbeat never calls the LLM itself. See
/// [`super::heartbeat`].
fn spawn_heartbeat() {
    let cfg = super::heartbeat::HeartbeatConfig::from_env();
    let shutdown = Arc::new(AtomicBool::new(false));
    tokio::spawn(super::heartbeat::run_loop(cfg, shutdown));
}

async fn handle_client(
    stream: UnixStream,
    state: DaemonState,
    client: ClientIdentity,
) -> Result<(), String> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    while let Some(line) = lines
        .next_line()
        .await
        .map_err(|err| format!("failed reading clawd client line: {err}"))?
    {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let started = Instant::now();
        let response = match serde_json::from_str::<Request>(line) {
            Ok(request) => {
                let response = dispatch(request.clone(), &state, &client).await;
                if let Err(err) = audit::record_request(
                    &request.command,
                    &request.params,
                    &response,
                    started.elapsed(),
                    &client,
                ) {
                    tracing::error!(error = %err, "failed to write clawd audit record");
                }
                system_journal::record_clawd_request(
                    &request.command,
                    &request.params,
                    &response,
                    started.elapsed(),
                    &client,
                );
                response
            }
            Err(err) => {
                let response = Response::error(None, "invalid_json", err.to_string());
                if let Err(err) = audit::record_invalid(line, &response, started.elapsed(), &client)
                {
                    tracing::error!(error = %err, "failed to write clawd invalid-request audit record");
                }
                system_journal::record_invalid_request(line, &response, started.elapsed(), &client);
                response
            }
        };
        let encoded = encode_response(&response).map_err(|err| err.to_string())?;
        writer
            .write_all(encoded.as_bytes())
            .await
            .map_err(|err| format!("failed writing clawd response: {err}"))?;
        writer
            .flush()
            .await
            .map_err(|err| format!("failed flushing clawd response: {err}"))?;
    }

    Ok(())
}

async fn dispatch(request: Request, state: &DaemonState, client: &ClientIdentity) -> Response {
    let id = request.id.clone();
    match dispatch_result(request, state, client).await {
        Ok(result) => Response::ok(id, result),
        Err(message) => Response::error(id, "request_failed", message),
    }
}

async fn dispatch_result(
    request: Request,
    state: &DaemonState,
    client: &ClientIdentity,
) -> Result<Value, String> {
    match request.command.as_str() {
        "daemon.health" => Ok(json!({
            "status": "ok",
            "daemon": "clawd",
            "started_at": state.started_at(),
            "uptime_ms": state.uptime_millis(),
        })),
        "daemon.status" => Ok(json!({
            "status": "running",
            "daemon": "clawd",
            "started_at": state.started_at(),
            "uptime_ms": state.uptime_millis(),
            "tasks": tasks::counts()?,
            "context": context::snapshot(state)?,
            "transactions": transactions::list(state)?,
        })),
        "task.submit" => tasks::submit(request.params, client).await,
        "task.list" => tasks::list(request.params),
        "task.get" => tasks::get(request.params),
        "task.cancel" => tasks::cancel(request.params),
        "task.stream" | "task.result" => tasks::result(request.params).await,
        "context.snapshot" => context::snapshot(state),
        "context.sources" => context::sources(state),
        "context.update" => context::update(state, request.params),
        "context.event.append" => context_events::append(request.params, client),
        "context.event.query" => context_events::query(request.params),
        "permission.pending" => permissions::pending(request.params),
        "permission.recent" => permissions::recent(request.params),
        "permission.request" => permissions::request(request.params),
        "permission.decide" => permissions::decide(request.params),
        "system.operations" => system_journal::query(request.params),
        "memory.history" => memory::history(request.params),
        "memory.sessions" => memory::sessions(request.params),
        "transaction.begin" => transactions::begin(state, request.params),
        "transaction.list" => transactions::list(state),
        "transaction.commit" => transactions::commit(state, request.params),
        "transaction.rollback" => transactions::rollback(state, request.params),
        other => Err(format!("unknown clawd command: {other}")),
    }
}

async fn prepare_socket(socket_path: &PathBuf) -> Result<(), String> {
    if let Some(parent) = socket_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|err| format!("failed to create {}: {err}", parent.display()))?;
    }

    match UnixStream::connect(socket_path).await {
        Ok(_) => Err(format!(
            "another clawd instance is already listening on {}",
            socket_path.display()
        )),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => tokio::fs::remove_file(socket_path).await.map_err(|err| {
            format!(
                "failed to remove stale clawd socket {}: {err}",
                socket_path.display()
            )
        }),
    }
}

#[cfg(unix)]
fn set_socket_permissions(socket_path: &PathBuf, mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(mode))
        .map_err(|err| format!("failed to chmod {}: {err}", socket_path.display()))
}

#[cfg(not(unix))]
fn set_socket_permissions(_socket_path: &PathBuf, _mode: u32) -> Result<(), String> {
    Ok(())
}
