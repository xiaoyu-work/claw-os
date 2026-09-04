//! The narrow per-launch broker endpoint.
//!
//! A sandboxed worker never sees the real `/run/cos/clawd.sock`. It
//! sees this endpoint, bind-mounted at the same path inside its mount
//! namespace, and it is the only route from hostile code to kernel
//! authority.
//!
//! ## What this endpoint is, and is not
//!
//! It is **plumbing**, not policy. A worker inside a pid and mount
//! namespace cannot present its own App session grant: that grant is
//! bound to its process tree, and the daemon sees a connection arriving
//! from the launcher instead. The launcher therefore holds an opaque
//! *relay grant* — issued by `clawd` when the session was bound, bound
//! `Process`-tight to the launcher, naming exactly one session,
//! carrying no capabilities of its own — and forwards each call through
//! the `app_session.relay` route.
//!
//! `clawd` remains the final authority on every relayed call. It
//! decodes the inner route's typed body, resolves the *live* App
//! session grant, takes its own decision, and makes the owning provider
//! spend the exact capability before any effect happens. The admission
//! checks below are a cheap early refusal so an obviously unauthorized
//! call never costs a round trip; passing them authorizes nothing.
//!
//! ## What it adds
//!
//! * **Peer binding.** The socket lives in a `0700` per-launch
//!   directory, is itself `0600`, and every connection's `SO_PEERCRED`
//!   uid must equal the account the worker runs as.
//! * **Route admission.** Session and identity control, the consent
//!   surface, the journal and the scheduler are refused outright; the
//!   relay route refuses anything that is not a `Session`-subject
//!   system-service route.
//! * **Live capability precheck.** Read from the routed registry row at
//!   call time, so a transient capability the kernel set for one MCP
//!   tool call is visible while it is set and gone the moment it is
//!   cleared. There is no frozen snapshot anywhere in this file.
//! * **Framing discipline.** One request per connection, a bounded
//!   header-then-body read, explicit read/write deadlines, and a
//!   bounded number of concurrent connections.
//! * **Local policy answers.** `worker.policy.check` is answered from
//!   the same live set without touching `clawd`, which is what
//!   `cos __policy check` — and therefore `cos_runtime.policy` — needs
//!   inside the sandbox.
//!
//! The relay handle never crosses into the sandbox, never appears in a
//! response body, and never reaches an audit record.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use super::provider::BrokerAuthority;
use crate::caps::{Cap, CapSet, Scope, Verb};
use crate::clawd::protocol::{BrokerError, BrokerErrorKind, ErrorBody};
use crate::clawd::routes::Command;
use crate::clawd::wire::{
    Fault, Response, HEADER_BYTES, KIND_REQUEST, KIND_RESPONSE, MAX_REQUEST_BYTES, PROTOCOL_VERSION,
};

/// Route name the endpoint answers itself.
pub const POLICY_CHECK_COMMAND: &str = "worker.policy.check";

/// Route the endpoint runs on the worker's behalf against the owner's
/// agent-memory store, which the sandbox deliberately does not mount.
pub const MEMORY_CALL_COMMAND: &str = "worker.memory.call";

/// Memory subcommands a worker may ask the launcher to run. Anything
/// else is refused before a single argument is parsed.
const MEMORY_SUBCOMMANDS: &[&str] = &["remember", "list", "show", "search", "forget"];

/// Bounds on a forwarded memory call, so a hostile worker cannot turn
/// the launcher into its own argument parser.
const MAX_MEMORY_ARGS: usize = 16;
const MAX_MEMORY_ARG_BYTES: usize = 64 * 1024;

/// Deadline for a worker to finish writing its request and read the
/// answer. A stuck worker must not pin a launcher thread.
const IO_DEADLINE: Duration = Duration::from_secs(30);

/// Concurrent in-flight broker connections per launch.
const MAX_INFLIGHT: u64 = 8;

/// Largest response the endpoint relays back into the sandbox.
const MAX_RELAY_RESPONSE: usize = 8 * 1024 * 1024;

/// A live endpoint. Dropping it stops the listener and removes the
/// socket, so worker authority cannot outlive the launch.
#[derive(Debug)]
pub struct BrokerEndpoint {
    socket: PathBuf,
    stop: Arc<AtomicBool>,
    stats: Arc<Stats>,
}

#[derive(Debug, Default)]
struct Stats {
    served: AtomicU64,
    denied: AtomicU64,
    relayed: AtomicU64,
    inflight: AtomicU64,
}

impl BrokerEndpoint {
    /// Bind the endpoint and start serving.
    pub fn start(
        socket: PathBuf,
        authority: BrokerAuthority,
        owner_uid: u32,
    ) -> Result<Self, String> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            use std::os::unix::net::UnixListener;

            let _ = std::fs::remove_file(&socket);
            let listener = UnixListener::bind(&socket)
                .map_err(|error| format!("bind worker broker socket: {error}"))?;
            std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))
                .map_err(|error| format!("restrict worker broker socket: {error}"))?;
            listener
                .set_nonblocking(false)
                .map_err(|error| format!("configure worker broker socket: {error}"))?;

            let stop = Arc::new(AtomicBool::new(false));
            let stats = Arc::new(Stats::default());
            let authority = Arc::new(authority);
            {
                let stop = Arc::clone(&stop);
                let stats = Arc::clone(&stats);
                std::thread::Builder::new()
                    .name("cos-worker-broker".to_string())
                    .spawn(move || {
                        for stream in listener.incoming() {
                            if stop.load(Ordering::Relaxed) {
                                return;
                            }
                            let Ok(stream) = stream else { continue };
                            if stats.inflight.load(Ordering::Relaxed) >= MAX_INFLIGHT {
                                stats.denied.fetch_add(1, Ordering::Relaxed);
                                continue;
                            }
                            stats.inflight.fetch_add(1, Ordering::Relaxed);
                            let authority = Arc::clone(&authority);
                            let stats_for_thread = Arc::clone(&stats);
                            let spawned = std::thread::Builder::new()
                                .name("cos-worker-broker-conn".to_string())
                                .spawn(move || {
                                    match serve(stream, &authority, owner_uid) {
                                        Outcome::Served => {
                                            stats_for_thread.served.fetch_add(1, Ordering::Relaxed);
                                        }
                                        Outcome::Relayed => {
                                            stats_for_thread.served.fetch_add(1, Ordering::Relaxed);
                                            stats_for_thread
                                                .relayed
                                                .fetch_add(1, Ordering::Relaxed);
                                        }
                                        Outcome::Denied => {
                                            stats_for_thread.denied.fetch_add(1, Ordering::Relaxed);
                                        }
                                    }
                                    stats_for_thread.inflight.fetch_sub(1, Ordering::Relaxed);
                                });
                            if spawned.is_err() {
                                stats.inflight.fetch_sub(1, Ordering::Relaxed);
                            }
                        }
                    })
                    .map_err(|error| format!("start worker broker: {error}"))?;
            }
            Ok(Self {
                socket,
                stop,
                stats,
            })
        }
        #[cfg(not(unix))]
        {
            let _ = (socket, authority, owner_uid);
            Err("worker broker endpoints require Unix".to_string())
        }
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket
    }

    /// Counters for the launch audit record. No paths, no payloads, no
    /// handle.
    pub fn facts(&self) -> Value {
        serde_json::json!({
            "served": self.stats.served.load(Ordering::Relaxed),
            "denied": self.stats.denied.load(Ordering::Relaxed),
            "relayed": self.stats.relayed.load(Ordering::Relaxed),
        })
    }
}

impl Drop for BrokerEndpoint {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Wake the blocking `accept` so the listener thread observes
        // the stop flag and exits.
        #[cfg(unix)]
        {
            use std::os::unix::net::UnixStream;
            let _ = UnixStream::connect(&self.socket);
        }
        let _ = std::fs::remove_file(&self.socket);
    }
}

enum Outcome {
    Served,
    Relayed,
    Denied,
}

#[cfg(unix)]
fn serve(
    mut stream: std::os::unix::net::UnixStream,
    authority: &BrokerAuthority,
    owner_uid: u32,
) -> Outcome {
    if peer_uid(&stream) != Some(owner_uid) {
        return Outcome::Denied;
    }
    let _ = stream.set_read_timeout(Some(IO_DEADLINE));
    let _ = stream.set_write_timeout(Some(IO_DEADLINE));

    let body = match read_request(&mut stream) {
        Ok(Some(body)) => body,
        Ok(None) => return Outcome::Denied,
        Err(fault) => {
            respond_fault(&mut stream, "unknown", fault);
            return Outcome::Denied;
        }
    };
    let envelope: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => {
            respond_fault(&mut stream, "unknown", Fault::MalformedBody);
            return Outcome::Denied;
        }
    };
    let id = envelope
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    if envelope.get("v").and_then(Value::as_u64) != Some(u64::from(PROTOCOL_VERSION)) {
        respond_fault(&mut stream, &id, Fault::UnsupportedVersion);
        return Outcome::Denied;
    }
    let Some(name) = envelope.get("command").and_then(Value::as_str) else {
        respond_fault(&mut stream, &id, Fault::InvalidEnvelope);
        return Outcome::Denied;
    };

    // Provenance first, before any command is even classified. The
    // sandbox has no session registry of its own, so *this* endpoint is
    // where a sandboxed worker's capability decisions come from: a
    // liveness check anywhere later would let a revoked package still
    // receive an "allow" for the request already in flight. The
    // identity checked is the launch's, held on this side of the
    // socket; nothing in the request influences it.
    if let Err(reason) = authority.assert_live() {
        respond_denied(
            &mut stream,
            &id,
            &format!("the package backing this launch is no longer trusted: {reason}"),
        );
        return Outcome::Denied;
    }

    if name == POLICY_CHECK_COMMAND {
        let result = policy_check(authority, envelope.get("params").unwrap_or(&Value::Null));
        respond_ok(&mut stream, &id, result);
        return Outcome::Served;
    }

    if name == MEMORY_CALL_COMMAND {
        return match memory_call(authority, envelope.get("params").unwrap_or(&Value::Null)) {
            Ok(result) => {
                respond_ok(&mut stream, &id, serde_json::json!({ "result": result }));
                Outcome::Served
            }
            Err(message) => {
                respond_denied(&mut stream, &id, &message);
                Outcome::Denied
            }
        };
    }

    let Some(command) = Command::parse(name) else {
        respond_fault(&mut stream, &id, Fault::UnknownCommand);
        return Outcome::Denied;
    };
    if let Err(message) = admit(command, authority) {
        respond_denied(&mut stream, &id, &message);
        return Outcome::Denied;
    }
    let params = envelope.get("params").cloned().unwrap_or(Value::Null);
    match relay(authority, command, params) {
        Ok(result) => {
            respond_ok(&mut stream, &id, result);
            Outcome::Relayed
        }
        Err(error) => {
            respond_error(&mut stream, &id, &error);
            Outcome::Denied
        }
    }
}

#[cfg(unix)]
fn peer_uid(stream: &std::os::unix::net::UnixStream) -> Option<u32> {
    super::peer_uid_of(stream)
}

#[cfg(unix)]
fn read_request(stream: &mut std::os::unix::net::UnixStream) -> Result<Option<Vec<u8>>, Fault> {
    let mut header = [0_u8; HEADER_BYTES];
    match stream.read_exact(&mut header) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(_) => return Err(Fault::TruncatedFrame),
    }
    let len =
        crate::clawd::transport::frame::parse_header(&header, KIND_REQUEST, MAX_REQUEST_BYTES)?;
    let mut body = vec![0_u8; len];
    stream
        .read_exact(&mut body)
        .map_err(|_| Fault::TruncatedFrame)?;
    // One request per connection. The peer is waiting for its answer
    // and will not close, so this has to be a *non-blocking* look: a
    // blocking read would sit here until the deadline and starve the
    // exchange it is guarding.
    if has_pending_input(stream) {
        return Err(Fault::ExtraFrame);
    }
    Ok(Some(body))
}

/// Is there another byte already queued on this connection?
///
/// `MSG_PEEK | MSG_DONTWAIT` answers without consuming and without
/// blocking; `EAGAIN` means "nothing more yet", which is the normal
/// case for a peer waiting on its response.
#[cfg(unix)]
fn has_pending_input(stream: &std::os::unix::net::UnixStream) -> bool {
    use std::os::unix::io::AsRawFd;

    let mut byte = [0_u8; 1];
    let seen = unsafe {
        libc::recv(
            stream.as_raw_fd(),
            byte.as_mut_ptr().cast::<libc::c_void>(),
            1,
            libc::MSG_PEEK | libc::MSG_DONTWAIT,
        )
    };
    seen > 0
}

/// Routes a sandboxed worker may never reach, whatever it holds.
///
/// Identity control mints, binds, widens or retires a session: a worker
/// that could call it would describe itself to the authority instead of
/// being described by it. The consent surface is refused for the same
/// reason — a worker must not answer, file or revoke the prompts that
/// govern it. Journal and scheduler routes are kernel bookkeeping with
/// no App-facing use at all.
///
/// The relay route enforces the same shape again inside the daemon;
/// this copy only saves the round trip.
fn is_forbidden(command: Command) -> bool {
    let name = command.as_str();
    name.starts_with("app_session.")
        || name.starts_with("mcp_session.")
        || name.starts_with("permission.")
        || name.starts_with("journal.")
        || name.starts_with("scheduler.")
}

/// Verbs that make a route worth relaying at all.
///
/// An *admission* filter, never authorization: `clawd` re-derives the
/// exact capability the inner route needs from its typed body and makes
/// the owning provider spend it against the live session grant. The
/// only job here is to refuse an obviously pointless round trip — a
/// launch holding just `fs.read` cannot open a conversation about the
/// firewall. An unmapped route is refused, so a newly added `clawd`
/// route is closed to workers until somebody decides otherwise.
fn required_verbs(command: Command) -> &'static [Verb] {
    match command.as_str() {
        "system.network.control" => &[Verb::NET_MANAGE],
        "system.firewall.control" => &[Verb::NET_FIREWALL],
        "system.audio.control" => &[Verb::DEVICE_AUDIO, Verb::DEVICE_MEDIA_ROUTE],
        "system.bluetooth.control" => &[Verb::DEVICE_BLUETOOTH],
        "system.camera.control" => &[Verb::DEVICE_CAMERA],
        "system.clipboard.control" => &[Verb::CLIPBOARD_READ, Verb::CLIPBOARD_WRITE],
        "system.config.control" => &[Verb::SYS_CONFIG],
        "system.container.control" => &[Verb::SYS_CONTAINER],
        "system.crash.inspect" => &[Verb::SYS_CRASH],
        "system.desktop.control" => &[Verb::DESKTOP_WINDOW, Verb::DESKTOP_LAUNCH],
        "system.browser.control" => &[
            Verb::BROWSER_TABS_READ,
            Verb::BROWSER_NAV,
            Verb::BROWSER_DOM_READ,
            Verb::BROWSER_DOM_WRITE,
            Verb::BROWSER_INPUT_SECRET,
            Verb::BROWSER_EVAL,
        ],
        "system.display.control" => &[Verb::DEVICE_DISPLAY],
        "system.events.control" => &[Verb::SYS_EVENTS],
        "system.hardware.inspect" => &[Verb::SYS_OBSERVE],
        "system.location.query" => &[Verb::DEVICE_LOCATION],
        "system.accessibility.control" => &[Verb::UI_ACCESSIBILITY],
        "system.backup.control" => &[Verb::DATA_BACKUP],
        "system.package.control" | "system.package.install" | "system.package.restore" => {
            &[Verb::SYS_PACKAGE]
        }
        "system.power.control" => &[Verb::SYS_POWER],
        "system.printer.control" => &[Verb::DEVICE_PRINTER],
        "system.security.inspect" => &[Verb::SYS_SECURITY],
        "system.service.control" | "system.service.restore" => &[Verb::SYS_SERVICE],
        "system.snapshot.control" => &[Verb::SYS_SNAPSHOT],
        "system.storage.control" => &[Verb::SYS_STORAGE, Verb::SYS_MOUNT],
        "system.usb.control" => &[Verb::DEVICE_USB],
        "system.users.control" => &[Verb::SYS_IDENTITY],
        "system.operations" => &[Verb::SYS_OBSERVE],
        _ => &[],
    }
}

fn admit(command: Command, authority: &BrokerAuthority) -> Result<(), String> {
    if is_forbidden(command) {
        return Err(format!(
            "worker broker refuses control route `{}`",
            command.as_str()
        ));
    }
    if authority.relay_handle().is_none() {
        return Err(format!(
            "this worker holds no relay authority for `{}`",
            command.as_str()
        ));
    }
    let verbs = required_verbs(command);
    if verbs.is_empty() {
        return Err(format!(
            "worker broker has no admission rule for route `{}`",
            command.as_str()
        ));
    }
    let caps = authority.live_caps();
    let justified = verbs.iter().any(|verb| {
        caps.iter()
            .any(|cap| cap.verb == *verb || cap.covers(&Cap::new(*verb, Scope::Wild)))
    });
    if !justified {
        return Err(format!(
            "worker is not authorized for broker route `{}`",
            command.as_str()
        ));
    }
    Ok(())
}

/// Answer one capability question from the launch's *live* authority.
///
/// Advisory by design: it tells an App what its own kernel checks will
/// say, read from the same routed registry row `clawd` and
/// `caps::require` read, so a transient capability appears and
/// disappears with the call it was granted for.
fn policy_check(authority: &BrokerAuthority, params: &Value) -> Value {
    let verb = params.get("verb").and_then(Value::as_str).unwrap_or("");
    let Some(verb) = Verb::parse(verb) else {
        return serde_json::json!({
            "decision": "deny",
            "reason": "unknown-verb",
        });
    };
    let scope: Scope = params
        .get("scope")
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or(Scope::Wild);
    let cap = Cap::new(verb, scope.clone());
    let allowed = authority.live_caps().covers(&cap);
    serde_json::json!({
        "decision": if allowed { "allow" } else { "deny" },
        "verb": verb.as_str(),
        "scope": scope,
        "session": authority.session_id,
        "app": authority.app_id,
        "reason": if allowed { Value::Null } else { Value::String("not-granted".into()) },
    })
}

/// Run one App→agent-memory call on the worker's behalf.
///
/// The App's summaries belong in the owner's cross-App memory, but the
/// database is not mounted into the sandbox and must not be: a bind
/// would hand hostile code every other source's rows and the `cos`
/// process inside the sandbox cannot resolve a session row anyway.
/// Instead the launcher — which already runs as the owner — re-parses
/// the call through the bridge's own typed argument handling and takes
/// every authorization decision from this launch's *live* capability
/// set, so `memory.write` still has to be scoped `self:<source>` to the
/// exact source being written.
#[cfg(unix)]
fn memory_call(authority: &BrokerAuthority, params: &Value) -> Result<Value, String> {
    let command = params
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !MEMORY_SUBCOMMANDS.contains(&command) {
        return Err(format!("worker broker refuses memory call `{command}`"));
    }
    let raw = params
        .get("args")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if raw.len() > MAX_MEMORY_ARGS {
        return Err("worker broker refuses an oversized memory call".to_string());
    }
    let mut args = Vec::with_capacity(raw.len());
    for value in raw {
        let Some(text) = value.as_str() else {
            return Err("worker broker refuses a non-string memory argument".to_string());
        };
        if text.len() > MAX_MEMORY_ARG_BYTES {
            return Err("worker broker refuses an oversized memory argument".to_string());
        }
        args.push(text.to_string());
    }
    crate::mem_bridge::run_with(
        &crate::mem_bridge::LaunchAuthority::new(authority.live_caps()),
        command,
        &args,
    )
}

/// Forward one admitted call through the daemon's relay route.
///
/// The relay handle is added here and only here; it never enters the
/// sandbox and never leaves this function. What the worker receives is
/// the inner route's own result, so no relay-layer field can be
/// mistaken for something the provider returned.
#[cfg(unix)]
fn relay(
    authority: &BrokerAuthority,
    command: Command,
    params: Value,
) -> Result<Value, BrokerError> {
    let handle = authority
        .relay_handle()
        .ok_or_else(|| BrokerError::authorization("this worker holds no relay authority"))?;
    let request = crate::clawd::protocol::Request::build(
        Command::AppSessionRelay,
        serde_json::json!({
            "session_id": authority.session_id,
            "handle": handle,
            "command": command.as_str(),
            "params": params,
        }),
    );
    let response =
        crate::clawd::client::request_blocking(crate::paths::clawd_socket_path(), request)
            .map_err(|error| {
                if error.may_have_dispatched() {
                    BrokerError::indeterminate(
                        "worker broker lost the system broker response after dispatch",
                    )
                } else {
                    BrokerError::unavailable("worker broker could not reach the system broker")
                }
            })?;
    if !response.ok {
        return Err(response
            .error
            .map(broker_error_from_body)
            .unwrap_or_else(|| {
                BrokerError::indeterminate(format!(
                    "relayed route `{}` failed without an error",
                    command.as_str()
                ))
            }));
    }
    response
        .result
        .and_then(|value| value.get("result").cloned())
        .ok_or_else(|| {
            BrokerError::indeterminate(format!(
                "relayed route `{}` returned no result",
                command.as_str()
            ))
        })
}

#[cfg(not(unix))]
fn relay(
    _authority: &BrokerAuthority,
    _command: Command,
    _params: Value,
) -> Result<Value, BrokerError> {
    Err(BrokerError::unavailable(
        "worker broker relays require Unix",
    ))
}

fn broker_error_from_body(error: ErrorBody) -> BrokerError {
    let kind = match error.code.as_str() {
        "not_authorized" => BrokerErrorKind::Unauthorized,
        "unavailable" => BrokerErrorKind::Unavailable,
        "indeterminate" => BrokerErrorKind::Indeterminate,
        _ => BrokerErrorKind::Execution,
    };
    BrokerError {
        kind,
        message: error.message,
        data: error.data,
        audit_class: None,
    }
}

#[cfg(unix)]
fn respond_ok(stream: &mut std::os::unix::net::UnixStream, id: &str, result: Value) {
    let body = serde_json::json!({
        "v": PROTOCOL_VERSION,
        "id": id,
        "ok": true,
        "result": result,
    });
    write_response(stream, &body);
}

#[cfg(unix)]
fn respond_denied(stream: &mut std::os::unix::net::UnixStream, id: &str, message: &str) {
    let body = serde_json::json!({
        "v": PROTOCOL_VERSION,
        "id": id,
        "ok": false,
        "error": { "code": "not_authorized", "message": message },
    });
    write_response(stream, &body);
}

#[cfg(unix)]
fn respond_error(stream: &mut std::os::unix::net::UnixStream, id: &str, error: &BrokerError) {
    let body = serde_json::json!({
        "v": PROTOCOL_VERSION,
        "id": id,
        "ok": false,
        "error": {
            "code": error.kind.code(),
            "message": error.message,
            "data": error.data,
        },
    });
    write_response(stream, &body);
}

#[cfg(unix)]
fn respond_fault(stream: &mut std::os::unix::net::UnixStream, id: &str, fault: Fault) {
    let body = serde_json::json!({
        "v": PROTOCOL_VERSION,
        "id": id,
        "ok": false,
        "error": { "code": fault.class(), "message": fault.message() },
    });
    write_response(stream, &body);
}

#[cfg(unix)]
fn write_response(stream: &mut std::os::unix::net::UnixStream, body: &Value) {
    let encoded = serde_json::to_vec(body).unwrap_or_default();
    let _ = stream.write_all(&crate::clawd::transport::frame::encode_frame(
        KIND_RESPONSE,
        &encoded,
    ));
    let _ = stream.flush();
}

/// Ask the launch's broker endpoint for a capability decision.
///
/// Returns `None` outside a sandbox, which is what keeps the normal
/// registry-backed enforcement path in place for every process that is
/// not a sandboxed worker. Inside a sandbox it never returns `None`: an
/// endpoint that cannot be reached is a denial, not a fall-through to a
/// session registry the sandbox deliberately does not have.
pub fn sandbox_policy_check(verb: Verb, scope: &Scope) -> Option<Result<(), String>> {
    #[cfg(unix)]
    {
        // Present only inside a sandbox; absent everywhere else, which
        // is what keeps the registry-backed path in place for the
        // launcher, the daemon and the CLI.
        std::env::var_os(super::SANDBOX_MARKER_ENV)?;
        Some(ask_sandbox_broker(verb, scope).unwrap_or_else(Err))
    }
    #[cfg(not(unix))]
    {
        let _ = (verb, scope);
        None
    }
}

/// One request/response exchange with the launch endpoint. The outer
/// `Err` is a transport failure, which the caller turns into a denial.
#[cfg(unix)]
fn ask_sandbox_broker(verb: Verb, scope: &Scope) -> Result<Result<(), String>, String> {
    let value = sandbox_exchange(
        POLICY_CHECK_COMMAND,
        serde_json::json!({ "verb": verb.as_str(), "scope": scope }),
    )?;
    // A refusal carries the endpoint's own message — including "this
    // package is no longer trusted", which is a different fact from
    // "you were not granted this" and is worth the App seeing. Absence
    // of an explicit `allow` is a denial, so a malformed or truncated
    // answer fails closed rather than falling through to anything else.
    if let Some(message) = value.pointer("/error/message").and_then(Value::as_str) {
        return Ok(Err(message.to_string()));
    }
    let decision = value
        .pointer("/result/decision")
        .and_then(Value::as_str)
        .unwrap_or("deny");
    Ok(if decision == "allow" {
        Ok(())
    } else {
        Err("worker sandbox authority did not grant this capability".to_string())
    })
}

/// Ask the launcher to run one App→agent-memory call.
///
/// `None` outside a sandbox, so the ordinary in-process bridge keeps
/// serving the CLI and the agent unchanged. Inside a sandbox this never
/// falls through: an unreachable endpoint is an error, never a silent
/// write into the App's own partition where the agent would never find
/// it.
pub fn sandbox_memory_call(command: &str, args: &[String]) -> Option<Result<Value, String>> {
    #[cfg(unix)]
    {
        std::env::var_os(super::SANDBOX_MARKER_ENV)?;
        Some(ask_sandbox_memory(command, args).unwrap_or_else(Err))
    }
    #[cfg(not(unix))]
    {
        let _ = (command, args);
        None
    }
}

#[cfg(unix)]
fn ask_sandbox_memory(command: &str, args: &[String]) -> Result<Result<Value, String>, String> {
    let value = sandbox_exchange(
        MEMORY_CALL_COMMAND,
        serde_json::json!({ "command": command, "args": args }),
    )?;
    if let Some(result) = value.pointer("/result/result") {
        return Ok(Ok(result.clone()));
    }
    let message = value
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("worker sandbox authority refused this memory call");
    Ok(Err(format!("memory {command} denied: {message}")))
}

/// One bounded request/response exchange with the launch endpoint.
#[cfg(unix)]
fn sandbox_exchange(command: &str, params: Value) -> Result<Value, String> {
    use std::os::unix::net::UnixStream;

    let socket = crate::paths::clawd_socket_path();
    let mut stream = UnixStream::connect(&socket)
        .map_err(|error| format!("worker sandbox authority is unreachable: {error}"))?;
    let _ = stream.set_read_timeout(Some(IO_DEADLINE));
    let _ = stream.set_write_timeout(Some(IO_DEADLINE));
    let request_id = uuid::Uuid::new_v4().simple().to_string();
    let body = serde_json::json!({
        "v": PROTOCOL_VERSION,
        "id": request_id,
        "command": command,
        "params": params,
    });
    let encoded = serde_json::to_vec(&body).map_err(|error| error.to_string())?;
    stream
        .write_all(&crate::clawd::transport::frame::encode_frame(
            KIND_REQUEST,
            &encoded,
        ))
        .map_err(|error| format!("worker sandbox authority write failed: {error}"))?;
    let mut header = [0_u8; HEADER_BYTES];
    stream
        .read_exact(&mut header)
        .map_err(|error| format!("worker sandbox authority read failed: {error}"))?;
    let len =
        crate::clawd::transport::frame::parse_header(&header, KIND_RESPONSE, MAX_RELAY_RESPONSE)
            .map_err(|fault| format!("worker sandbox authority refused: {}", fault.message()))?;
    let mut response = vec![0_u8; len];
    stream
        .read_exact(&mut response)
        .map_err(|error| format!("worker sandbox authority truncated: {error}"))?;
    let response: Response = serde_json::from_slice(&response)
        .map_err(|_| "worker sandbox authority answered with a malformed body".to_string())?;
    if response.v != PROTOCOL_VERSION {
        return Err(format!(
            "worker sandbox authority answered protocol v{}; expected v{PROTOCOL_VERSION}",
            response.v
        ));
    }
    if response.id.as_str() != request_id {
        return Err("worker sandbox authority response did not correlate".to_string());
    }
    serde_json::to_value(response)
        .map_err(|_| "worker sandbox authority response could not be encoded".to_string())
}

/// The launch's current capability set, read from the routed registry
/// row the kernel keeps for this session.
///
/// Base capabilities plus whatever transient set a session tool call
/// installed — exactly what `caps::require` and the daemon's own
/// authority read. Falls back to the set the launcher derived when the
/// row cannot be read, so a transient registry failure narrows rather
/// than widens.
pub(crate) fn live_session_caps(session_id: &str, base: &CapSet) -> CapSet {
    let Some(session) = crate::proc::session_row_for_launcher(session_id) else {
        return base.clone();
    };
    let mut caps = session.caps.clone().unwrap_or_else(CapSet::new);
    if let Some(transient) = session.transient_caps.clone() {
        caps.extend(transient.iter().cloned());
    }
    caps
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/worker/broker.rs"
    ));
}
