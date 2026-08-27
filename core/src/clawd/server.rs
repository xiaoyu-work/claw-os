use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

use crate::audit_policy;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use super::client_identity::ClientIdentity;
use super::protocol::{encode_response, Request, Response};
use super::state::DaemonState;
use super::{
    accessibility, app_sessions, audio, audit, backup, bluetooth, camera, clipboard, config_editor,
    containers, context, context_events, crash, credentials, desktop, display, event_center,
    firewall, hardware, location, memory, network, packages, permissions, power, printer,
    scheduler, security, snapshots, storage, system_journal, systemd, tasks, transactions,
    usb_guard, users,
};

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
    let state = DaemonState::try_new()?;
    let _event_center = event_center::start();
    if let Err(error) = firewall::reconcile_on_start().await {
        tracing::error!(error = %error, "failed to reconcile managed firewall state");
    }
    if let Err(error) = usb_guard::reconcile_on_start().await {
        tracing::error!(error = %error, "failed to reconcile managed USB policy");
    }

    tracing::info!(socket = %options.socket_path.display(), "clawd listening");

    audit::install_runtime_hook();
    context::refresh_builtin_sources(&state);
    let agentd_shutdown = Arc::new(AtomicBool::new(false));
    // Agent work runs in unprivileged `claw-agentd` processes. The
    // supervisor handle is deliberately *not* part of the daemon's
    // fatal path: a worker exiting — normally or not — must never take
    // the broker down, and supervision stopping still leaves every
    // non-agent primitive served.
    let _agentd = crate::agentd::supervisor::spawn_supervisor(agentd_shutdown);
    spawn_heartbeat();
    let serve = async move {
        loop {
            let (stream, _addr) = listener
                .accept()
                .await
                .map_err(|err| format!("failed to accept clawd client: {err}"))?;
            let client = match ClientIdentity::from_stream(&stream) {
                Ok(client) => client,
                Err(err) => {
                    tracing::warn!(error = %err, "rejecting clawd client without peer credentials");
                    continue;
                }
            };
            let state = state.clone();
            tokio::spawn(async move {
                if let Err(err) = handle_client(stream, state, client).await {
                    tracing::warn!(error = %err, "clawd client handler failed");
                }
            });
        }
        #[allow(unreachable_code)]
        Ok::<(), String>(())
    };
    serve.await
}

/// Spawn the system-vitals heartbeat — the cheap, always-on reflex loop
/// that samples kernel vitals, emits `context.event`s on threshold
/// crossings (which the trigger engine may turn into agent jobs), and
/// drives the `cron` / `triggers` schedulers so the daemon is its own
/// clock. The heartbeat never calls the LLM itself. See
/// [`super::heartbeat`].
fn spawn_heartbeat() {
    if let Err(error) = crate::cron::cleanup_runtime_credentials() {
        tracing::error!(error = %error, "failed to clean stale cron credentials");
    }
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
                // Projected before dispatch and shared by both sinks:
                // the raw command and params never reach a record.
                let facts = audit_policy::request_facts(&request.command, &request.params);
                let response = dispatch(request, &state, &client).await;
                let outcome = response.audit_facts();
                let elapsed = started.elapsed();
                if let Err(err) = audit::record_request(&facts, &outcome, elapsed, &client) {
                    tracing::error!(error = %err, "failed to write clawd audit record");
                }
                system_journal::record_clawd_request(&facts, &outcome, elapsed, &client);
                response
            }
            Err(err) => {
                let facts = audit_policy::invalid_request_facts(line, &err);
                let response = Response::error_classified(
                    None,
                    "invalid_json",
                    "invalid_json",
                    err.to_string(),
                );
                let outcome = response.audit_facts();
                let elapsed = started.elapsed();
                if let Err(err) = audit::record_invalid(&facts, &outcome, elapsed, &client) {
                    tracing::error!(error = %err, "failed to write clawd invalid-request audit record");
                }
                system_journal::record_invalid_request(&facts, &outcome, elapsed, &client);
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
    if let Err(message) = authorize_command(&request.command, client) {
        return Response::error_classified(id, "request_failed", "command_not_authorized", message);
    }
    if !is_dispatchable(&request.command) {
        // Named separately from handler failures so the audit trail can
        // count probes for routes that do not exist without storing the
        // caller's string.
        return Response::error_classified(
            id,
            "request_failed",
            "unknown_command",
            format!("unknown clawd command: {}", request.command),
        );
    }
    // App-session and scheduler routes answer denials with structured
    // data (the approval request ids the caller must wait on), which
    // has to reach the peer without being flattened into the message
    // that audit records persist.
    if app_sessions::owns_command(&request.command) {
        return match app_sessions::dispatch(&request.command, request.params, client).await {
            Ok(result) => Response::ok(id, result),
            Err(error) => Response::error_with_data(id, "request_failed", error),
        };
    }
    if request.command == "scheduler.run" {
        return match scheduler::run(request.params, client).await {
            Ok(result) => Response::ok(id, result),
            Err(error) => Response::error_with_data(id, "request_failed", error),
        };
    }
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
            "tasks": tasks::counts(client)?,
            "context": context::snapshot_for_client(state, client)?,
            "transactions": transactions::list(state, client)?,
        })),
        "task.submit" => tasks::submit(request.params, client).await,
        "task.list" => tasks::list(request.params, client),
        "task.get" | "task.status" => tasks::get(request.params, client),
        "task.cancel" => tasks::cancel(request.params, client),
        "task.stream" | "task.result" => tasks::result(request.params, client).await,
        "task.count" => tasks::counts(client),
        "context.snapshot" => context::snapshot_for_client(state, client),
        "context.sources" => context::sources_for_client(state, client),
        "context.update" => context::update(state, request.params),
        "context.event.append" => context_events::append(request.params, client),
        "context.event.query" => context_events::query_for_client(request.params, client),
        "permission.pending" => permissions::pending(request.params, client),
        "permission.recent" => permissions::recent(request.params, client),
        "permission.status" => permissions::status(request.params, client),
        "permission.request" => permissions::request(request.params, client),
        "permission.decide" => permissions::decide(request.params, client),
        "system.operations" => system_journal::query_for_client(request.params, client),
        "memory.history" => memory::history(request.params, client),
        "memory.sessions" => memory::sessions(request.params, client),
        "credential.oauth-refresh" => credentials::oauth_refresh(request.params, client).await,
        "system.audio.control" => audio::control(request.params, client).await,
        "system.accessibility.control" => accessibility::control(request.params, client).await,
        "system.backup.control" => backup::control(request.params, client).await,
        "system.bluetooth.control" => bluetooth::control(request.params, client).await,
        "system.camera.control" => camera::control(request.params, client).await,
        "system.clipboard.control" => clipboard::control(request.params, client).await,
        "system.container.control" => containers::control(request.params, client).await,
        "system.config.control" => config_editor::control(request.params, client).await,
        "system.crash.inspect" => crash::inspect(request.params, client).await,
        "system.desktop.control" => desktop::control(request.params, client).await,
        "system.display.control" => display::control(request.params, client).await,
        "system.events.control" => event_center::control(request.params, client).await,
        "system.firewall.control" => firewall::control(request.params, client).await,
        "system.hardware.inspect" => hardware::inspect(request.params, client).await,
        "system.location.query" => location::query(request.params, client).await,
        "system.network.control" => network::control(request.params, client).await,
        "system.package.install" => packages::install(request.params, client).await,
        "system.package.control" => packages::control(request.params, client).await,
        "system.package.restore" => packages::restore(request.params, client).await,
        "system.power.control" => power::control(request.params, client).await,
        "system.printer.control" => printer::control(request.params, client).await,
        "system.security.inspect" => security::inspect(request.params, client).await,
        "system.service.control" => systemd::control(request.params, client).await,
        "system.service.restore" => systemd::restore(request.params, client).await,
        "system.snapshot.control" => snapshots::control(request.params, client).await,
        "system.storage.control" => storage::control(request.params, client).await,
        "system.usb.control" => usb_guard::control(request.params, client).await,
        "system.users.control" => users::control(request.params, client).await,
        "transaction.begin" => transactions::begin(state, request.params, client),
        "transaction.list" => transactions::list(state, client),
        "transaction.commit" => transactions::commit(state, request.params, client),
        "transaction.rollback" => transactions::rollback(state, request.params, client).await,
        other => Err(format!("unknown clawd command: {other}")),
    }
}

/// Commands any authenticated peer may issue. The daemon still derives
/// identity and capability from the connection; this list only decides
/// what a non-root uid is allowed to reach.
pub(crate) const USER_COMMANDS: &[&str] = &[
    "daemon.health",
    "daemon.status",
    "task.submit",
    "task.list",
    "task.get",
    "task.status",
    "task.cancel",
    "task.stream",
    "task.result",
    "task.count",
    "memory.history",
    "memory.sessions",
    "credential.oauth-refresh",
    "system.audio.control",
    "system.accessibility.control",
    "system.backup.control",
    "system.bluetooth.control",
    "system.camera.control",
    "system.clipboard.control",
    "system.container.control",
    "system.config.control",
    "system.crash.inspect",
    "system.desktop.control",
    "system.display.control",
    "system.events.control",
    "system.firewall.control",
    "system.hardware.inspect",
    "system.location.query",
    "system.network.control",
    "system.package.install",
    "system.package.control",
    "system.package.restore",
    "system.power.control",
    "system.printer.control",
    "system.security.inspect",
    "system.service.control",
    "system.service.restore",
    "system.snapshot.control",
    "system.storage.control",
    "system.usb.control",
    "system.users.control",
    "scheduler.run",
    "app_session.register",
    "app_session.register_native",
    "mcp_session.register",
    "app_session.bind",
    "app_session.set_transient",
    "app_session.deregister",
    "permission.pending",
    "permission.recent",
    "permission.status",
    "permission.request",
    "permission.decide",
    "context.snapshot",
    "context.sources",
    "context.event.append",
    "context.event.query",
    "system.operations",
    "transaction.begin",
    "transaction.list",
    "transaction.commit",
    "transaction.rollback",
];

/// Commands only root may issue.
pub(crate) const ROOT_COMMANDS: &[&str] = &["context.update"];

/// Whether the broker routes this command at all.
///
/// Together with [`USER_COMMANDS`] and [`ROOT_COMMANDS`] this is the
/// canonical set of broker routes, so
/// [`crate::audit_policy::known_commands`] can be checked against it and
/// a new route cannot reach a sink without an audit policy.
fn is_dispatchable(command: &str) -> bool {
    USER_COMMANDS.contains(&command) || ROOT_COMMANDS.contains(&command)
}

fn authorize_command(command: &str, client: &ClientIdentity) -> Result<(), String> {
    let uid = client.require_uid()?;
    if uid == 0 {
        return Ok(());
    }

    if USER_COMMANDS.contains(&command) {
        Ok(())
    } else {
        Err(format!("clawd command requires root: {command}"))
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

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/server.rs"
    ));
}
