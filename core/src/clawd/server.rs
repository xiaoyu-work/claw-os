//! Socket lifecycle and request admission for the broker.
//!
//! Framing, per-message credentials and the ceilings live in
//! [`super::transport`]; the route surface lives in [`super::routes`].
//! What is left here is the order in which a request is admitted, and
//! that order is the security property:
//!
//! 1. accept, and take a connection slot for the peer's accounting
//!    bucket;
//! 2. read exactly one frame, inside the read deadline, refusing any
//!    descriptor the peer attached;
//! 3. refuse a second frame on the same connection;
//! 4. take the credentials the kernel stamped on that frame and
//!    re-verify the sending process through `/proc`;
//! 5. parse the versioned envelope — no legacy fallback;
//! 6. resolve the route, then decode its typed body;
//! 7. authorize the peer's access class;
//! 8. take a global, per-principal and per-route slot, and refuse a
//!    replayed mutation;
//! 9. resolve the route's capability grant from the authority and take
//!    its authorization decision;
//! 10. dispatch under the route's budget;
//! 11. refuse to release the answer if the route owed the authority a
//!     capability check and never took one;
//! 12. write one bounded response and close.
//!
//! Every refusal before step 10 is audited by its stable class and by
//! how many bytes had been read. Nothing the caller sent — not the
//! frame, not the ancillary data, not a `serde` message — is recorded.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

use serde_json::Value;
use tokio::net::{UnixListener, UnixStream};

use crate::audit_policy;

use super::client_identity::ClientIdentity;
use super::protocol::{encode_response, Response};
use super::routes::{Command, Deadline, Route, RouteCall};
use super::state::DaemonState;
use super::transport::frame::{PeerStream, ReadOutcome};
use super::transport::{mutation_key, peer, Admission, Limits};
use super::wire::{
    legacy_upgrade_notice, Fault, InboundRequest, RequestId, MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES,
    PROTOCOL_VERSION,
};
use super::{audit, context, event_center, firewall, system_journal, usb_guard};

#[derive(Debug, Clone)]
pub struct ServerOptions {
    pub socket_path: PathBuf,
    pub socket_mode: u32,
}

pub async fn run(options: ServerOptions) -> Result<(), String> {
    prepare_socket(&options.socket_path).await?;
    let listener = UnixListener::bind(&options.socket_path)
        .map_err(|err| format!("failed to bind {}: {err}", options.socket_path.display()))?;
    // Before the first `accept`, so no connection is ever served
    // without the kernel stamping credentials onto its messages.
    enable_credential_passing(&listener)?;
    set_socket_permissions(&options.socket_path, options.socket_mode)?;
    let state = DaemonState::try_new()?;
    let _event_center = event_center::start();
    if let Err(error) = firewall::reconcile_on_start().await {
        tracing::error!(error = %error, "failed to reconcile managed firewall state");
    }
    if let Err(error) = usb_guard::reconcile_on_start().await {
        tracing::error!(error = %error, "failed to reconcile managed USB policy");
    }

    tracing::info!(
        socket = %options.socket_path.display(),
        protocol = PROTOCOL_VERSION,
        "clawd listening"
    );

    audit::install_runtime_hook();
    context::refresh_builtin_sources(&state);
    spawn_authority_sweep();
    let admission = Admission::new(Limits::default());
    let agentd_shutdown = Arc::new(AtomicBool::new(false));
    // Agent work runs in unprivileged `claw-agentd` processes. The
    // supervisor handle is deliberately *not* part of the daemon's
    // fatal path: a worker exiting — normally or not — must never take
    // the broker down, and supervision stopping still leaves every
    // non-agent primitive served.
    let broker_context = crate::agentd::supervisor::BrokerContext::new(
        state.clone(),
        admission.clone(),
        options.socket_path.clone(),
    )?;
    let _agentd = crate::agentd::supervisor::spawn_supervisor(agentd_shutdown, broker_context);
    spawn_heartbeat();
    let serve = async move {
        loop {
            let (stream, _addr) = listener
                .accept()
                .await
                .map_err(|err| format!("failed to accept clawd client: {err}"))?;
            let bucket = accounting_bucket(&stream);
            let Some(permit) = admission.accept_connection(bucket) else {
                tracing::warn!(
                    class = Fault::TooManyConnections.class(),
                    "refusing clawd connection at the open-connection ceiling"
                );
                continue;
            };
            let state = state.clone();
            let admission = Arc::clone(&admission);
            tokio::spawn(async move {
                serve_connection(stream, state, admission).await;
                drop(permit);
            });
        }
        #[allow(unreachable_code)]
        Ok::<(), String>(())
    };
    serve.await
}

/// Retire capability grants whose process, deadline or use budget is
/// gone.
///
/// The store already sweeps on every entry point, but a daemon that
/// goes quiet after a burst of launches would otherwise hold rows for
/// processes that exited. This is what makes "cleaned up on process
/// exit" true without waiting for the next request.
fn spawn_authority_sweep() {
    tokio::spawn(async {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(30));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            super::authority::sweep();
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
    if let Err(error) = crate::cron::cleanup_runtime_credentials() {
        tracing::error!(error = %error, "failed to clean stale cron credentials");
    }
    let cfg = super::heartbeat::HeartbeatConfig::from_env();
    let shutdown = Arc::new(AtomicBool::new(false));
    tokio::spawn(super::heartbeat::run_loop(cfg, shutdown));
}

/// Which bucket a new connection is counted against.
///
/// `SO_PEERCRED` is fine for this and only this: it spreads a fixed
/// connection budget across principals before any message has arrived.
/// It is never consulted for authority — that comes from the
/// credentials the kernel attaches to the request message itself.
fn accounting_bucket(stream: &UnixStream) -> u32 {
    stream
        .peer_cred()
        .map(|cred| cred.uid())
        .unwrap_or(u32::MAX)
}

async fn serve_connection(stream: UnixStream, state: DaemonState, admission: Arc<Admission>) {
    let started = Instant::now();
    let mut peer_stream = match PeerStream::new(stream) {
        Ok(peer_stream) => peer_stream,
        Err(err) => {
            tracing::warn!(error = %err, "failed to prepare clawd connection");
            return;
        }
    };

    let limits = admission.limits();
    let read = tokio::time::timeout(
        limits.read_deadline,
        peer_stream.read_request(MAX_REQUEST_BYTES),
    )
    .await;

    let frame = match read {
        Err(_elapsed) => {
            return refuse(&mut peer_stream, Fault::ReadTimeout, 0, started).await;
        }
        Ok(Err(fault)) => {
            return refuse(&mut peer_stream, fault, 0, started).await;
        }
        Ok(Ok(ReadOutcome::Closed)) => return,
        Ok(Ok(ReadOutcome::Legacy)) => {
            record_protocol_failure(Fault::UnsupportedFrame, 0, None, started, &unknown_peer());
            let notice = legacy_upgrade_notice();
            let _ =
                tokio::time::timeout(limits.write_deadline, peer_stream.write_raw(&notice)).await;
            let _ = tokio::time::timeout(DRAIN_DEADLINE, peer_stream.drain_pending()).await;
            return;
        }
        Ok(Ok(ReadOutcome::Frame(frame))) => frame,
    };

    let bytes = frame.body.len();

    // A pipelined second frame is refused before anything in the first
    // one is authorized, so a peer cannot get one request served while
    // smuggling another behind it.
    if peer_stream.has_pending_input() {
        return refuse(&mut peer_stream, Fault::ExtraFrame, bytes, started).await;
    }

    let Some(process) = peer::verify(frame.credentials) else {
        return refuse(&mut peer_stream, Fault::PeerUnverified, bytes, started).await;
    };
    let client = ClientIdentity::from_peer(process);

    let response = match admit(&frame.body, &client, &admission).await {
        Ok(admitted) => {
            let facts = audit_policy::request_facts_for_route(
                admitted.route.name,
                admitted.route.audit_fields,
                &admitted.params,
            );
            let response = dispatch(
                admitted.route,
                admitted.id,
                admitted.params,
                admitted.decision.as_ref(),
                &state,
                &client,
            )
            .await;
            let outcome = response.audit_facts();
            let elapsed = started.elapsed();
            if let Err(err) = audit::record_request(&facts, &outcome, elapsed, &client) {
                tracing::error!(error = %err, "failed to write clawd audit record");
            }
            system_journal::record_clawd_request(&facts, &outcome, elapsed, &client);
            response
        }
        Err(refusal) => {
            record_protocol_failure(refusal.fault, bytes, refusal.command, started, &client);
            Response::fault(refusal.id, refusal.fault)
        }
    };

    write_response(&mut peer_stream, response, limits.write_deadline, &client).await;
    let _ = tokio::time::timeout(DRAIN_DEADLINE, peer_stream.drain_pending()).await;
}

/// How long the daemon will spend discarding a refused peer's leftover
/// bytes so its close is a clean end-of-file rather than a reset.
const DRAIN_DEADLINE: std::time::Duration = std::time::Duration::from_millis(200);

/// A request that cleared every check before dispatch.
struct Admitted {
    route: &'static Route,
    id: RequestId,
    params: Value,
    /// The authority decision the middleware took. `None` only for
    /// peer-scoped routes, which resolve no grant.
    decision: Option<super::authority::Decision>,
    /// Held for the lifetime of the request; dropping them returns the
    /// global, per-principal and per-route slots.
    _request_permit: super::transport::limits::RequestPermit,
    _route_permit: super::transport::limits::RoutePermit,
}

struct Refusal {
    fault: Fault,
    id: RequestId,
    /// The registry's own name for the route, when one was resolved.
    /// Never the caller's string.
    command: Option<&'static str>,
}

impl std::fmt::Debug for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Deliberately omits the correlation id: this is what a
        // `tracing` line or a test failure prints, and the id is the
        // one field here a caller chose.
        f.debug_struct("Refusal")
            .field("fault", &self.fault)
            .field("command", &self.command)
            .finish()
    }
}

async fn admit(
    body: &[u8],
    client: &ClientIdentity,
    admission: &Arc<Admission>,
) -> Result<Admitted, Refusal> {
    let refuse =
        |fault: Fault, id: RequestId, command: Option<&'static str>| Refusal { fault, id, command };

    let envelope = serde_json::from_slice::<InboundRequest>(body).map_err(|error| {
        let fault = match error.classify() {
            serde_json::error::Category::Syntax | serde_json::error::Category::Eof => {
                Fault::MalformedBody
            }
            _ => Fault::InvalidEnvelope,
        };
        refuse(fault, RequestId::unknown(), None)
    })?;

    if envelope.v != PROTOCOL_VERSION {
        return Err(refuse(Fault::UnsupportedVersion, envelope.id, None));
    }

    let Some(command) = Command::parse(envelope.command.as_str()) else {
        return Err(refuse(Fault::UnknownCommand, envelope.id, None));
    };
    let route = command.route();

    // Typed decode first: an unknown field, an oversized string or a
    // wrong JSON type is refused before the access class is even
    // consulted, so no unvalidated value reaches an authorization
    // decision or a handler.
    let params = (route.decode)(envelope.params)
        .map_err(|fault| refuse(fault, envelope.id.clone(), Some(route.name)))?;

    route
        .authorize(client)
        .map_err(|fault| refuse(fault, envelope.id.clone(), Some(route.name)))?;

    let uid = client.uid.ok_or_else(|| {
        refuse(
            Fault::MissingCredentials,
            envelope.id.clone(),
            Some(route.name),
        )
    })?;

    let request_permit = admission
        .accept_request(uid)
        .map_err(|fault| refuse(fault, envelope.id.clone(), Some(route.name)))?;
    let route_permit = admission
        .accept_route(route)
        .map_err(|fault| refuse(fault, envelope.id.clone(), Some(route.name)))?;

    if route.kind == super::routes::Kind::Mutation {
        let key = mutation_key(
            uid,
            client.pid.unwrap_or_default(),
            client.start_time_ticks.unwrap_or_default(),
            command,
            envelope.id.as_str(),
        );
        admission
            .admit_mutation(key)
            .map_err(|fault| refuse(fault, envelope.id.clone(), Some(route.name)))?;
    }

    // The single mandatory authorization step. It runs after the typed
    // decode, so nothing unvalidated reaches a policy decision, and
    // before dispatch, so no handler is entered without one.
    let decision = super::authority::authorize(route.name, &route.authority, &params, client)
        .await
        .map_err(|fault| refuse(fault, envelope.id.clone(), Some(route.name)))?;

    Ok(Admitted {
        route,
        id: envelope.id,
        params,
        decision,
        _request_permit: request_permit,
        _route_permit: route_permit,
    })
}

async fn dispatch(
    route: &'static Route,
    id: RequestId,
    params: Value,
    decision: Option<&super::authority::Decision>,
    state: &DaemonState,
    client: &ClientIdentity,
) -> Response {
    let call = RouteCall {
        state,
        client,
        params,
        authority: decision,
    };
    let running = (route.handler)(call);
    let result = match route.budget.deadline {
        Deadline::Interruptible(limit) => match tokio::time::timeout(limit, running).await {
            Ok(result) => result,
            Err(_elapsed) => {
                return Response::fault(id, Fault::RouteTimeout);
            }
        },
        // Dropping a privileged mutation part-way could leave a package
        // half-installed or a unit half-restored, so these routes are
        // bounded by their own tool and lock timeouts plus the route's
        // in-flight ceiling, never by cancellation.
        Deadline::Uninterruptible => running.await,
    };
    // A route whose descriptor says it derives its own capability owes
    // the authority one spend. If it answered without taking one, the
    // authority has no record that anything was authorized, so there is
    // nothing to release.
    if !super::authority::obligation_met(decision) {
        tracing::error!(
            route = route.name,
            "route answered without exercising its capability requirement"
        );
        return Response::fault(id, Fault::NotAuthorized);
    }
    match result {
        Ok(value) => Response::ok(id, value),
        Err(error) => route.errors.response(id, error),
    }
}

/// Dispatch a request whose process identity was verified by another
/// broker-owned Unix listener.
///
/// The extension-host proxy uses this after applying its stricter route and
/// task/session checks. Typed decoding, admission, duplicate detection,
/// capability resolution, final provider enforcement, deadlines, and audit
/// remain identical to the primary broker socket.
pub(crate) async fn dispatch_verified_request(
    request: crate::clawd::wire::Request,
    client: &ClientIdentity,
    state: &DaemonState,
    admission: &Arc<Admission>,
) -> Response {
    let started = Instant::now();
    let body = match serde_json::to_vec(&request) {
        Ok(body) => body,
        Err(_) => return Response::fault(request.id, Fault::InvalidEnvelope),
    };
    let bytes = body.len();
    match admit(&body, client, admission).await {
        Ok(admitted) => {
            let facts = audit_policy::request_facts_for_route(
                admitted.route.name,
                admitted.route.audit_fields,
                &admitted.params,
            );
            let response = dispatch(
                admitted.route,
                admitted.id,
                admitted.params,
                admitted.decision.as_ref(),
                state,
                client,
            )
            .await;
            let outcome = response.audit_facts();
            let elapsed = started.elapsed();
            if let Err(error) = audit::record_request(&facts, &outcome, elapsed, client) {
                tracing::error!(%error, "failed to write proxied clawd audit record");
            }
            system_journal::record_clawd_request(&facts, &outcome, elapsed, client);
            response
        }
        Err(refusal) => {
            record_protocol_failure(refusal.fault, bytes, refusal.command, started, client);
            Response::fault(refusal.id, refusal.fault)
        }
    }
}

async fn refuse(peer_stream: &mut PeerStream, fault: Fault, bytes: usize, started: Instant) {
    let client = unknown_peer();
    record_protocol_failure(fault, bytes, None, started, &client);
    if !fault.is_reportable() {
        return;
    }
    let response = Response::fault(RequestId::unknown(), fault);
    write_response(
        peer_stream,
        response,
        Limits::default().write_deadline,
        &client,
    )
    .await;
    let _ = tokio::time::timeout(DRAIN_DEADLINE, peer_stream.drain_pending()).await;
}

fn unknown_peer() -> ClientIdentity {
    ClientIdentity::unknown()
}

fn record_protocol_failure(
    fault: Fault,
    bytes: usize,
    command: Option<&'static str>,
    started: Instant,
    client: &ClientIdentity,
) {
    let facts = audit_policy::protocol_failure_facts(fault.class(), bytes, command);
    let outcome = audit_policy::ResponseFacts {
        ok: false,
        error: Some(audit_policy::error_facts(
            fault.code(),
            Some(fault.class()),
            fault.message(),
        )),
    };
    let elapsed = started.elapsed();
    if let Err(err) = audit::record_protocol_failure(&facts, &outcome, elapsed, client) {
        tracing::error!(error = %err, "failed to write clawd protocol-failure audit record");
    }
    system_journal::record_protocol_failure(&facts, &outcome, elapsed, client);
}

async fn write_response(
    peer_stream: &mut PeerStream,
    response: Response,
    deadline: std::time::Duration,
    client: &ClientIdentity,
) {
    let id = response.id.clone();
    let encoded = match encode_response(&response) {
        Ok(encoded) if encoded.len() <= MAX_RESPONSE_BYTES => encoded,
        Ok(oversized) => {
            record_protocol_failure(
                Fault::ResponseTooLarge,
                oversized.len(),
                None,
                Instant::now(),
                client,
            );
            match encode_response(&Response::fault(id, Fault::ResponseTooLarge)) {
                Ok(encoded) => encoded,
                Err(_) => return,
            }
        }
        Err(err) => {
            tracing::error!(error = %err, "failed to encode clawd response");
            return;
        }
    };
    match tokio::time::timeout(deadline, peer_stream.write_response(&encoded)).await {
        Ok(Ok(())) => {}
        Ok(Err(err)) => {
            tracing::debug!(error = %err, "clawd client closed before its response");
        }
        Err(_elapsed) => {
            record_protocol_failure(
                Fault::WriteTimeout,
                encoded.len(),
                None,
                Instant::now(),
                client,
            );
        }
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
fn enable_credential_passing(listener: &UnixListener) -> Result<(), String> {
    use std::os::unix::io::AsRawFd;

    peer::enable_credential_passing(listener.as_raw_fd())
        .map_err(|err| format!("failed to enable clawd peer credential passing: {err}"))
}

#[cfg(not(unix))]
fn enable_credential_passing(_listener: &UnixListener) -> Result<(), String> {
    Err("clawd requires Unix domain sockets".to_string())
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
