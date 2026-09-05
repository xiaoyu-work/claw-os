//! `claw-extension-host` process implementation.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Read;
use std::os::fd::FromRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};

use crate::agent::tools::mcp::integration::{McpServerHandle, McpServerSpec};
use crate::clawd::transport::frame::{PeerStream, ReadOutcome};
use crate::clawd::transport::peer;
use crate::clawd::wire::RequestId;

use super::protocol::{
    control_socket_for, ControlLane, ControlRequest, ControlResponse, ExtensionErrorCategory,
    HostAction, HostBootstrap, HostResult, MAX_AGENT_EVENT_ACTIONS, MAX_AGENT_EVENT_ADMISSIONS,
    MAX_CANONICAL_ADMISSIONS, MAX_CANONICAL_CONTROL_ACTIONS, MAX_CONTROL_FRAME_BYTES,
    MAX_PRIORITY_ADMISSIONS, MAX_PRIORITY_CONTROL_ACTIONS, MAX_REQUEST_TIMEOUT_MS,
    PROTOCOL_VERSION,
};

const RESPONSE_WRITE_TIMEOUT: Duration = Duration::from_secs(30);
const CONTROL_READ_TIMEOUT: Duration = Duration::from_millis(250);
const RECENT_REQUESTS: usize = 256;
const MAX_APP_ARGS: usize = 128;
const MAX_ARG_BYTES: usize = 64 * 1024;

struct HostedMcp {
    spec: McpServerSpec,
    handle: McpServerHandle,
}

struct HostState {
    binding: super::protocol::ExtensionBinding,
    isolation: super::child_isolation::IsolationAuthority,
    task_id: String,
    session_id: Option<String>,
    controller_uid: u32,
    controller_gid: u32,
    controller_pid: u32,
    controller_start_time_ticks: Option<u64>,
    lease_nonce: String,
    recent: Mutex<VecDeque<String>>,
    active: Mutex<HashMap<String, tokio::task::AbortHandle>>,
    mcp: tokio::sync::Mutex<HashMap<String, HostedMcp>>,
    agent_extensions: tokio::sync::Mutex<
        HashMap<String, Arc<tokio::sync::Mutex<super::agent_extension::HostedAgentExtension>>>,
    >,
    active_agent_events: Mutex<HashMap<String, HashMap<String, tokio::task::AbortHandle>>>,
    agent_extension_spawn: tokio::sync::Mutex<()>,
    shutting_down: AtomicBool,
    fatal_shutdown: AtomicBool,
    shutdown: Notify,
}

#[derive(Debug)]
struct HostDispatchError {
    category: ExtensionErrorCategory,
    message: String,
}

impl HostDispatchError {
    fn new(category: ExtensionErrorCategory, message: impl Into<String>) -> Self {
        Self {
            category,
            message: message.into(),
        }
    }

    fn busy(message: impl Into<String>) -> Self {
        Self::new(ExtensionErrorCategory::Busy, message)
    }

    #[cfg(test)]
    fn contains(&self, pattern: &str) -> bool {
        self.message.contains(pattern)
    }
}

impl std::fmt::Display for HostDispatchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl From<String> for HostDispatchError {
    fn from(message: String) -> Self {
        Self::new(ExtensionErrorCategory::RemoteCallFailure, message)
    }
}

impl From<crate::agent::tools::cos_apps_session::HostedAppError> for HostDispatchError {
    fn from(error: crate::agent::tools::cos_apps_session::HostedAppError) -> Self {
        Self::new(error.category(), error.to_string())
    }
}

pub fn main() -> ! {
    match run() {
        Ok(()) => std::process::exit(0),
        Err(error) => {
            eprintln!("claw-extension-host: {error}");
            std::process::exit(1);
        }
    }
}

fn run() -> Result<(), String> {
    crate::storage::set_private_umask();
    crate::agentd::spawn::set_process_undumpable()?;
    #[cfg(target_os = "linux")]
    unsafe {
        if libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) != 0 {
            return Err(format!(
                "set extension-host child subreaper: {}",
                std::io::Error::last_os_error()
            ));
        }
    }

    let bootstrap = read_bootstrap()?;
    let enforce_groups = bootstrap.enforce_groups;
    let binding = bootstrap.into_current_binding()?;
    require_hardened_identity(binding.extension_uid, binding.owner_gid, enforce_groups)?;
    let isolation = super::child_isolation::IsolationAuthority::from_binding(&binding)?;
    let control_socket = PathBuf::from(&binding.control_socket);

    if let Some(parent) = control_socket.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("create extension control directory: {error}"))?;
    }
    let _ = std::fs::remove_file(&control_socket);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .map_err(|error| format!("build extension-host runtime: {error}"))?;
    runtime.block_on(async move {
        let event_socket = PathBuf::from(control_socket_for(
            &binding.control_socket,
            ControlLane::AgentEvent,
        ));
        let priority_socket = PathBuf::from(control_socket_for(
            &binding.control_socket,
            ControlLane::Priority,
        ));
        let _ = std::fs::remove_file(&event_socket);
        let _ = std::fs::remove_file(&priority_socket);
        let control_mode = match binding.purpose {
            super::protocol::HostPurpose::Task => 0o660,
            super::protocol::HostPurpose::AppService => 0o600,
        };
        let canonical_listener = bind_control_listener(&control_socket, control_mode)?;
        let event_listener = bind_control_listener(&event_socket, control_mode)?;
        let priority_listener = bind_control_listener(&priority_socket, control_mode)?;

        let state = Arc::new(HostState {
            task_id: binding.task_id.clone(),
            session_id: binding.session_id.clone(),
            controller_uid: binding.controller_uid,
            controller_gid: binding.controller_gid,
            controller_pid: binding.controller_pid,
            controller_start_time_ticks: binding.controller_start_time_ticks,
            lease_nonce: binding.lease_nonce.clone(),
            binding,
            isolation,
            recent: Mutex::new(VecDeque::new()),
            active: Mutex::new(HashMap::new()),
            mcp: tokio::sync::Mutex::new(HashMap::new()),
            agent_extensions: tokio::sync::Mutex::new(HashMap::new()),
            active_agent_events: Mutex::new(HashMap::new()),
            agent_extension_spawn: tokio::sync::Mutex::new(()),
            shutting_down: AtomicBool::new(false),
            fatal_shutdown: AtomicBool::new(false),
            shutdown: Notify::new(),
        });
        let listeners = [
            tokio::spawn(accept_control(
                canonical_listener,
                ControlLane::Canonical,
                state.clone(),
                Arc::new(Semaphore::new(MAX_CANONICAL_ADMISSIONS)),
                Arc::new(Semaphore::new(MAX_CANONICAL_CONTROL_ACTIONS)),
            )),
            tokio::spawn(accept_control(
                event_listener,
                ControlLane::AgentEvent,
                state.clone(),
                Arc::new(Semaphore::new(MAX_AGENT_EVENT_ADMISSIONS)),
                Arc::new(Semaphore::new(MAX_AGENT_EVENT_ACTIONS)),
            )),
            tokio::spawn(accept_control(
                priority_listener,
                ControlLane::Priority,
                state.clone(),
                Arc::new(Semaphore::new(MAX_PRIORITY_ADMISSIONS)),
                Arc::new(Semaphore::new(MAX_PRIORITY_CONTROL_ACTIONS)),
            )),
        ];

        state.shutdown.notified().await;
        for listener in listeners {
            listener.abort();
        }

        state.shutting_down.store(true, Ordering::SeqCst);
        let active = state
            .active
            .lock()
            .map(|mut active| active.drain().map(|(_, handle)| handle).collect::<Vec<_>>())
            .unwrap_or_default();
        for handle in active {
            handle.abort();
        }
        crate::agent::tools::cos_apps_session::host_close_all_sessions().await;
        state.mcp.lock().await.clear();
        let interrupted_extensions = state
            .active_agent_events
            .lock()
            .map(|mut events| {
                let ids = events.keys().cloned().collect::<HashSet<_>>();
                for handle in events
                    .drain()
                    .flat_map(|(_, requests)| requests.into_values())
                {
                    handle.abort();
                }
                ids
            })
            .unwrap_or_default();
        let extensions = state
            .agent_extensions
            .lock()
            .await
            .drain()
            .collect::<Vec<_>>();
        for (id, extension) in extensions {
            let mut extension = extension.lock().await;
            let cleanup = if interrupted_extensions.contains(&id) {
                extension.abort().await
            } else {
                extension
                    .shutdown(super::abi::ShutdownReason::TaskComplete)
                    .await
            };
            if cleanup.is_err() {
                state.fatal_shutdown.store(true, Ordering::SeqCst);
            }
        }
        let _ = std::fs::remove_file(&control_socket);
        let _ = std::fs::remove_file(&event_socket);
        let _ = std::fs::remove_file(&priority_socket);
        if state.fatal_shutdown.load(Ordering::SeqCst) {
            unsafe {
                libc::_exit(124);
            }
        }
        Ok::<(), String>(())
    })
}

fn bind_control_listener(path: &Path, mode: u32) -> Result<UnixListener, String> {
    let listener = UnixListener::bind(path)
        .map_err(|error| format!("bind extension control socket {}: {error}", path.display()))?;
    peer::enable_credential_passing(listener.as_raw_fd())
        .map_err(|error| format!("enable extension peer credentials: {error}"))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).map_err(|error| {
        format!(
            "protect extension control socket {}: {error}",
            path.display()
        )
    })?;
    Ok(listener)
}

async fn accept_control(
    listener: UnixListener,
    lane: ControlLane,
    state: Arc<HostState>,
    admissions: Arc<Semaphore>,
    actions: Arc<Semaphore>,
) {
    loop {
        let admission = tokio::select! {
            _ = state.shutdown.notified() => return,
            permit = admissions.clone().acquire_owned() => match permit {
                Ok(permit) => permit,
                Err(_) => return,
            },
        };
        let accepted = tokio::select! {
            _ = state.shutdown.notified() => return,
            accepted = listener.accept() => accepted,
        };
        let (stream, _) = match accepted {
            Ok(accepted) => accepted,
            Err(_) => {
                state.shutting_down.store(true, Ordering::SeqCst);
                state.fatal_shutdown.store(true, Ordering::SeqCst);
                state.shutdown.notify_waiters();
                return;
            }
        };
        if !preauthenticate_peer(&stream, &state) {
            continue;
        }
        let state = state.clone();
        let actions = actions.clone();
        tokio::spawn(async move {
            serve_control(stream, lane, state, admission, actions).await;
        });
    }
}

fn preauthenticate_peer(stream: &UnixStream, state: &HostState) -> bool {
    let Ok(credentials) = stream.peer_cred() else {
        return false;
    };
    let Some(pid) = credentials.pid().and_then(|pid| u32::try_from(pid).ok()) else {
        return false;
    };
    credentials.uid() == state.controller_uid
        && credentials.gid() == state.controller_gid
        && pid == state.controller_pid
        && crate::proc::read_start_time_ticks_pub(pid) == state.controller_start_time_ticks
}

async fn serve_control(
    stream: UnixStream,
    lane: ControlLane,
    state: Arc<HostState>,
    admission: OwnedSemaphorePermit,
    actions: Arc<Semaphore>,
) {
    let mut stream = match PeerStream::new(stream) {
        Ok(stream) => stream,
        Err(_) => return,
    };
    let read = tokio::time::timeout(
        CONTROL_READ_TIMEOUT,
        stream.read_request(MAX_CONTROL_FRAME_BYTES),
    )
    .await;
    let (request, process) = match read {
        Ok(Ok(ReadOutcome::Frame(frame))) => {
            let Some(process) = peer::verify(frame.credentials) else {
                return;
            };
            let request = match serde_json::from_slice::<ControlRequest>(&frame.body) {
                Ok(request) => request,
                Err(_) => {
                    write_response(
                        &mut stream,
                        ControlResponse::error(
                            RequestId::unknown(),
                            ExtensionErrorCategory::Protocol,
                            "invalid extension-host request",
                        ),
                    )
                    .await;
                    return;
                }
            };
            (request, process)
        }
        _ => return,
    };

    let id = request.id.clone();
    if let Err(error) = validate_request(&request, process, &state) {
        write_response(
            &mut stream,
            ControlResponse::error(id, ExtensionErrorCategory::Protocol, error),
        )
        .await;
        return;
    }
    if request.action.control_lane() != lane {
        write_response(
            &mut stream,
            ControlResponse::error(
                id,
                ExtensionErrorCategory::Protocol,
                "extension-host action used the wrong control lane",
            ),
        )
        .await;
        return;
    }
    let action_permit = match actions.try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            write_response(
                &mut stream,
                ControlResponse::error(
                    id,
                    ExtensionErrorCategory::Busy,
                    "extension-host control lane is busy; retry with bounded backpressure",
                ),
            )
            .await;
            return;
        }
    };
    drop(admission);
    serve_authenticated(request, stream, state, action_permit).await;
}

async fn serve_authenticated(
    request: ControlRequest,
    mut stream: PeerStream,
    state: Arc<HostState>,
    _action_permit: OwnedSemaphorePermit,
) {
    let id = request.id.clone();
    if let HostAction::Cancel { request_id } = &request.action {
        if cancel_active(&state, request_id) {
            // A blocking App operation cannot be safely unwound inside this
            // process. Exiting lets the broker kill the cgroup/process tree.
            state.shutting_down.store(true, Ordering::SeqCst);
            state.fatal_shutdown.store(true, Ordering::SeqCst);
        }

        write_response(&mut stream, ControlResponse::ok(id, HostResult::Cancelled)).await;
        if state.fatal_shutdown.load(Ordering::SeqCst) {
            state.shutdown.notify_waiters();
        }
        return;
    }
    if matches!(request.action, HostAction::Shutdown) {
        state.shutting_down.store(true, Ordering::SeqCst);
        state.shutdown.notify_waiters();
        write_response(&mut stream, ControlResponse::ok(id, HostResult::Shutdown)).await;
        return;
    }

    let requested_timeout =
        Duration::from_millis(request.timeout_ms.clamp(1, MAX_REQUEST_TIMEOUT_MS));
    let event_deadline = match &request.action {
        HostAction::AgentExtensionEvent {
            deadline_monotonic_ns,
            ..
        } => Some(*deadline_monotonic_ns),
        _ => None,
    };
    let timeout = match event_deadline {
        Some(deadline) => deadline
            .remaining()
            .unwrap_or(Duration::ZERO)
            .min(requested_timeout),
        None => requested_timeout,
    };
    let event_extension_id = match &request.action {
        HostAction::AgentExtensionEvent { extension_id, .. } => Some(extension_id.clone()),
        _ => None,
    };
    let action = request.action;
    let state_for_action = state.clone();
    let (start_tx, start_rx) = tokio::sync::oneshot::channel();
    let mut task = tokio::spawn(async move {
        let _ = start_rx.await;
        dispatch(action, state_for_action).await
    });
    let abort = task.abort_handle();
    if let Ok(mut active) = state.active.lock() {
        active.insert(id.as_str().to_string(), abort.clone());
    }
    if let Some(extension_id) = event_extension_id.as_ref() {
        if let Ok(mut events) = state.active_agent_events.lock() {
            events
                .entry(extension_id.clone())
                .or_default()
                .insert(id.as_str().to_string(), abort.clone());
        }
    }
    let _ = start_tx.send(());
    let result = tokio::time::timeout(timeout, &mut task).await;
    if let Ok(mut active) = state.active.lock() {
        active.remove(id.as_str());
    }
    let lifecycle_aborted = event_extension_id.as_ref().is_some_and(|extension_id| {
        state
            .active_agent_events
            .lock()
            .map(|events| {
                !events
                    .get(extension_id)
                    .is_some_and(|requests| requests.contains_key(id.as_str()))
            })
            .unwrap_or(false)
    });
    if let Some(extension_id) = event_extension_id.as_ref() {
        if let Ok(mut events) = state.active_agent_events.lock() {
            if let Some(requests) = events.get_mut(extension_id) {
                requests.remove(id.as_str());
                if requests.is_empty() {
                    events.remove(extension_id);
                }
            }
        }
    }
    let response = match result {
        Ok(Ok(Ok(result))) => ControlResponse::ok(id, result),
        Ok(Ok(Err(error))) => ControlResponse::error(id, error.category, error.message),
        Ok(Err(_)) if lifecycle_aborted => ControlResponse::error(
            id,
            ExtensionErrorCategory::Busy,
            "Agent extension event was interrupted by priority lifecycle control",
        ),
        Ok(Err(join)) => {
            state.shutting_down.store(true, Ordering::SeqCst);
            state.fatal_shutdown.store(true, Ordering::SeqCst);
            ControlResponse::error(
                id,
                ExtensionErrorCategory::Crash,
                format!("extension host action crashed: {join}"),
            )
        }
        Err(_) if event_extension_id.is_some() => {
            abort.abort();
            if let Some(extension_id) = event_extension_id.as_deref() {
                abort_hosted_agent_extension(state.clone(), extension_id).await;
            }
            ControlResponse::error(
                id,
                ExtensionErrorCategory::Timeout,
                format!(
                    "Agent extension event exceeded its absolute deadline after {}ms",
                    timeout.as_millis()
                ),
            )
        }
        Err(_) => {
            abort.abort();
            state.shutting_down.store(true, Ordering::SeqCst);
            state.fatal_shutdown.store(true, Ordering::SeqCst);
            ControlResponse::error(
                id,
                ExtensionErrorCategory::Timeout,
                format!(
                    "extension host action timed out after {}ms",
                    timeout.as_millis()
                ),
            )
        }
    };
    if let Some(deadline) = event_deadline {
        if let Ok(remaining) = deadline.remaining() {
            write_response_with_timeout(&mut stream, response, remaining).await;
        }
    } else {
        write_response(&mut stream, response).await;
    }
    if state.fatal_shutdown.load(Ordering::SeqCst) {
        state.shutdown.notify_waiters();
    }
}

fn cancel_active(state: &HostState, request_id: &RequestId) -> bool {
    let cancelled = state
        .active
        .lock()
        .ok()
        .and_then(|mut active| active.remove(request_id.as_str()));
    if let Some(handle) = cancelled {
        handle.abort();
        true
    } else {
        false
    }
}

fn validate_request(
    request: &ControlRequest,
    process: peer::PeerProcess,
    state: &HostState,
) -> Result<(), String> {
    if state.shutting_down.load(Ordering::SeqCst) {
        return Err("extension host is shutting down".to_string());
    }
    if request.protocol != PROTOCOL_VERSION {
        return Err(format!(
            "extension-host protocol mismatch: worker speaks v{}, host speaks v{}",
            request.protocol, PROTOCOL_VERSION
        ));
    }
    if process.uid != state.controller_uid
        || process.gid != state.controller_gid
        || process.pid != state.controller_pid
        || Some(process.start_time_ticks) != state.controller_start_time_ticks
    {
        return Err("extension-host request came from a different controller".to_string());
    }
    if request.task_id != state.task_id
        || request.session_id != state.session_id
        || request.lease_nonce != state.lease_nonce
        || request.binding_digest != state.binding.digest()?
    {
        return Err("extension-host request does not match this host lease".to_string());
    }
    if request.timeout_ms == 0 || request.timeout_ms > MAX_REQUEST_TIMEOUT_MS {
        return Err("extension-host request timeout is outside the allowed range".to_string());
    }
    let mut recent = state
        .recent
        .lock()
        .map_err(|_| "extension-host replay cache is unavailable".to_string())?;
    if recent.iter().any(|seen| seen == request.id.as_str()) {
        return Err("extension-host request id was already used".to_string());
    }
    if recent.len() == RECENT_REQUESTS {
        recent.pop_front();
    }
    recent.push_back(request.id.as_str().to_string());
    Ok(())
}

async fn write_response(stream: &mut PeerStream, response: ControlResponse) {
    write_response_with_timeout(stream, response, RESPONSE_WRITE_TIMEOUT).await;
}

async fn write_response_with_timeout(
    stream: &mut PeerStream,
    response: ControlResponse,
    timeout: Duration,
) {
    let Ok(body) = serde_json::to_vec(&response) else {
        return;
    };
    if body.len() > MAX_CONTROL_FRAME_BYTES {
        let fallback = ControlResponse::error(
            response.id,
            ExtensionErrorCategory::Protocol,
            "extension-host response is too large",
        );
        if let Ok(body) = serde_json::to_vec(&fallback) {
            let _ = tokio::time::timeout(timeout, stream.write_response(&body)).await;
        }
        return;
    }
    let _ = tokio::time::timeout(timeout, stream.write_response(&body)).await;
}

async fn dispatch(
    action: HostAction,
    state: Arc<HostState>,
) -> Result<HostResult, HostDispatchError> {
    match action {
        HostAction::Ping => Ok(HostResult::Ready {
            pid: std::process::id(),
            start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
            dumpable: unsafe { libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0) } == 1,
            seccomp_mode: unsafe { libc::prctl(libc::PR_GET_SECCOMP, 0, 0, 0, 0) }
                .try_into()
                .unwrap_or_default(),
        }),
        HostAction::RunApp {
            app_id,
            command,
            args,
        } => {
            require_purpose(&state, super::protocol::HostPurpose::Task, "run App")?;
            if state.binding.app_id.is_some() {
                return Err("App-owned agents cannot launch Apps".to_string().into());
            }
            validate_name(&app_id, "App id")?;
            validate_text(&command, "App command", 256)?;
            validate_args(&args)?;
            let apps_root = apps_root();
            let app = crate::apps::find_verified(&apps_root, &app_id)?;
            let launch = crate::bridge::AppLaunch::new(app.require_verified()?.clone())?;
            let data = crate::paths::user_data_dir().to_string_lossy().into_owned();
            let apps = apps_root.to_string_lossy().into_owned();
            let output = tokio::task::spawn_blocking(move || {
                crate::bridge::run_app(&launch, &command, &args, &data, &apps)
            })
            .await
            .map_err(|error| format!("App host task failed: {error}"))??;
            if output
                .as_ref()
                .is_some_and(|value| value.len() > MAX_CONTROL_FRAME_BYTES / 2)
            {
                return Err("App output exceeds the extension-host limit"
                    .to_string()
                    .into());
            }
            Ok(HostResult::AppOutput { output })
        }
        HostAction::AppCall {
            app_id,
            tool,
            arguments,
            audit,
        } => {
            require_purpose(&state, super::protocol::HostPurpose::Task, "relay App call")?;
            validate_name(&app_id, "App id")?;
            validate_text(&tool, "App tool", 256)?;
            if !arguments.is_object() && !arguments.is_null() {
                return Err("App tool arguments must be an object".to_string().into());
            }
            if audit.app_id != app_id || audit.tool != tool {
                return Err("App invocation audit does not match its target"
                    .to_string()
                    .into());
            }
            audit.validate_live_binding(&state.binding)?;
            let value = relay_app_call(
                &state.binding,
                &app_id,
                &tool,
                arguments,
                audit,
                Duration::from_millis(MAX_REQUEST_TIMEOUT_MS),
            )
            .await?;
            Ok(HostResult::AppCall { value })
        }
        HostAction::AuthorizedAppCall {
            app_id,
            tool,
            arguments,
            authorized_mounts,
            authorization,
            context,
        } => {
            require_purpose(
                &state,
                super::protocol::HostPurpose::AppService,
                "execute authorized App call",
            )?;
            validate_name(&app_id, "App id")?;
            validate_text(&tool, "App tool", 256)?;
            validate_text(&authorization, "App authorization", 64)?;
            if state.binding.app_id.as_deref() != Some(app_id.as_str()) {
                return Err("App service host is bound to a different App"
                    .to_string()
                    .into());
            }
            let package = state
                .binding
                .package
                .as_ref()
                .ok_or_else(|| "App service host omitted its package binding".to_string())?;
            let current = crate::apps::find_verified_fresh(&apps_root(), &app_id)?;
            if crate::provenance::runtime::PackageRef::of(current.require_verified()?) != *package {
                return Err("App service package changed after host startup"
                    .to_string()
                    .into());
            }
            if !arguments.is_object() {
                return Err("App tool arguments must be an object".to_string().into());
            }
            let value = crate::agent::tools::cos_apps_session::host_call_session(
                &app_id,
                &tool,
                arguments,
                authorized_mounts,
                context,
                authorization,
                Duration::from_millis(MAX_REQUEST_TIMEOUT_MS),
            )
            .await?;
            Ok(HostResult::AppCall { value })
        }
        HostAction::WarmApp { app_id } => {
            require_purpose(
                &state,
                super::protocol::HostPurpose::AppService,
                "warm App service",
            )?;
            validate_name(&app_id, "App id")?;
            if state.binding.app_id.as_deref() != Some(app_id.as_str()) {
                return Err("App service host is bound to a different App"
                    .to_string()
                    .into());
            }
            let package = state
                .binding
                .package
                .as_ref()
                .ok_or_else(|| "App service host omitted its package binding".to_string())?;
            let current = crate::apps::find_verified_fresh(&apps_root(), &app_id)?;
            if crate::provenance::runtime::PackageRef::of(current.require_verified()?) != *package {
                return Err("App service package changed after host startup"
                    .to_string()
                    .into());
            }
            crate::agent::tools::cos_apps_session::host_warm_session(&app_id).await?;
            Ok(HostResult::AppWarmed)
        }
        HostAction::McpAttach { spec } => {
            require_purpose(&state, super::protocol::HostPurpose::Task, "attach MCP")?;
            attach_mcp(spec, &state).await.map_err(Into::into)
        }
        HostAction::McpCall {
            server,
            tool,
            descriptor_digest,
            audit,
            arguments,
        } => {
            require_purpose(&state, super::protocol::HostPurpose::Task, "call MCP")?;
            call_mcp(
                &server,
                &tool,
                &descriptor_digest,
                &audit,
                arguments,
                &state,
            )
            .await
            .map_err(Into::into)
        }
        HostAction::McpDetach { server } => {
            require_purpose(&state, super::protocol::HostPurpose::Task, "detach MCP")?;
            validate_name(&server, "MCP server")?;
            let detached = state.mcp.lock().await.remove(&server).is_some();
            Ok(HostResult::McpDetached { detached })
        }
        HostAction::AgentExtensionAttach { registration } => {
            Ok(attach_agent_extension(registration, &state).await?)
        }
        HostAction::AgentExtensionEvent {
            extension_id,
            binding,
            event_id,
            deadline_monotonic_ns,
            payload,
            capability_refs,
        } => {
            validate_name(&extension_id, "Agent extension")?;
            let extension = state
                .agent_extensions
                .lock()
                .await
                .get(&extension_id)
                .cloned()
                .ok_or_else(|| format!("Agent extension `{extension_id}` is not attached"))?;
            let extension_handle = extension.clone();
            let mut extension = extension.try_lock_owned().map_err(|_| {
                HostDispatchError::busy(format!(
                    "Agent extension `{extension_id}` already has an event in flight"
                ))
            })?;
            let result = extension
                .event(
                    &binding,
                    event_id,
                    deadline_monotonic_ns,
                    payload,
                    capability_refs,
                )
                .await;
            match result {
                Ok(value) => Ok(HostResult::AgentExtensionEvent { value }),
                Err(error) => {
                    let mut extensions = state.agent_extensions.lock().await;
                    if extensions
                        .get(&extension_id)
                        .is_some_and(|current| Arc::ptr_eq(current, &extension_handle))
                    {
                        extensions.remove(&extension_id);
                    }
                    drop(extensions);
                    let cleanup_state = state.clone();
                    tokio::spawn(async move {
                        if extension.abort().await.is_err() {
                            cleanup_state.shutting_down.store(true, Ordering::SeqCst);
                            cleanup_state.fatal_shutdown.store(true, Ordering::SeqCst);
                            cleanup_state.shutdown.notify_waiters();
                        }
                    });
                    Err(error.into())
                }
            }
        }
        HostAction::AgentExtensionDetach {
            extension_id,
            binding,
            reason,
        } => {
            validate_name(&extension_id, "Agent extension")?;
            let interrupted = abort_agent_extension_events(&state, &extension_id);
            let Some(extension) = state.agent_extensions.lock().await.remove(&extension_id) else {
                return Ok(HostResult::AgentExtensionDetached { detached: false });
            };
            let mut extension = extension.lock().await;
            if extension.binding() != &binding {
                let cleanup = extension.abort().await;
                let error = match cleanup {
                    Ok(()) => "Agent extension detach binding does not match".to_string(),
                    Err(cleanup) => {
                        format!("Agent extension detach binding does not match; {cleanup}")
                    }
                };
                return Err(error.into());
            }
            if interrupted {
                return detached_after_cleanup(extension.abort().await);
            } else {
                extension.shutdown(reason).await?;
            }
            Ok(HostResult::AgentExtensionDetached { detached: true })
        }
        HostAction::Cancel { .. } | HostAction::Shutdown => {
            Err("control action was handled before dispatch"
                .to_string()
                .into())
        }
    }
}

fn detached_after_cleanup(cleanup: Result<(), String>) -> Result<HostResult, HostDispatchError> {
    cleanup?;
    Ok(HostResult::AgentExtensionDetached { detached: true })
}

fn abort_agent_extension_events(state: &HostState, extension_id: &str) -> bool {
    let handles = state
        .active_agent_events
        .lock()
        .ok()
        .and_then(|mut events| events.remove(extension_id))
        .unwrap_or_default();
    let interrupted = !handles.is_empty();
    for handle in handles.into_values() {
        handle.abort();
    }
    interrupted
}

async fn abort_hosted_agent_extension(state: Arc<HostState>, extension_id: &str) {
    let extension = state.agent_extensions.lock().await.remove(extension_id);
    if let Some(extension) = extension {
        tokio::spawn(async move {
            let mut extension = extension.lock_owned().await;
            if extension.abort().await.is_err() {
                state.shutting_down.store(true, Ordering::SeqCst);
                state.fatal_shutdown.store(true, Ordering::SeqCst);
                state.shutdown.notify_waiters();
            }
        });
    }
}

async fn attach_agent_extension(
    registration: super::protocol::AgentExtensionRegistration,
    state: &HostState,
) -> Result<HostResult, String> {
    validate_name(&registration.extension_id, "Agent extension")?;
    registration.validate()?;
    let existing = state
        .agent_extensions
        .lock()
        .await
        .get(&registration.extension_id)
        .cloned();
    if let Some(existing) = existing {
        let existing = existing.lock().await;
        let binding = existing.binding();
        if binding.extension_id != registration.extension_id
            || binding.extension_version != registration.extension_version
            || binding.package_digest != registration.package_digest
            || binding.manifest_digest != registration.manifest_digest
        {
            return Err(
                "Agent extension is already attached with a different verified registration"
                    .to_string(),
            );
        }
        return Ok(HostResult::AgentExtensionReady {
            binding: Box::new(binding.clone()),
        });
    }
    let _spawn = state.agent_extension_spawn.lock().await;
    let hosted = super::agent_extension::HostedAgentExtension::attach(
        &registration,
        &state.binding,
        &state.isolation,
    )
    .await?;
    let binding = hosted.binding().clone();
    let hosted = Arc::new(tokio::sync::Mutex::new(hosted));
    let mut extensions = state.agent_extensions.lock().await;
    if extensions.contains_key(&registration.extension_id) {
        drop(extensions);
        let _ = hosted
            .lock()
            .await
            .shutdown(super::abi::ShutdownReason::Disabled)
            .await;
        return Err("Agent extension was concurrently attached".to_string());
    }
    extensions.insert(registration.extension_id, hosted);
    Ok(HostResult::AgentExtensionReady {
        binding: Box::new(binding),
    })
}

async fn attach_mcp(spec: McpServerSpec, state: &HostState) -> Result<HostResult, String> {
    validate_name(&spec.name, "MCP server")?;
    validate_text(&spec.command, "MCP command", 4096)?;
    validate_args(&spec.args)?;
    if spec.env.len() > 64 {
        return Err("MCP environment exceeds 64 entries".to_string());
    }
    {
        let mcp = state.mcp.lock().await;
        if let Some(existing) = mcp.get(&spec.name) {
            if existing.spec != spec {
                return Err(format!(
                    "MCP server `{}` is already attached with a different specification",
                    spec.name
                ));
            }
            return Ok(HostResult::McpAttached {
                tools: existing.handle.descriptors().to_vec(),
            });
        }
    }

    let handle = crate::agent::tools::mcp::integration::attach_server_local(
        &spec,
        None,
        Some(&state.isolation),
    )
    .await?;
    let tools = handle.descriptors().to_vec();
    state
        .mcp
        .lock()
        .await
        .insert(spec.name.clone(), HostedMcp { spec, handle });
    Ok(HostResult::McpAttached { tools })
}

async fn call_mcp(
    server: &str,
    tool: &str,
    descriptor_digest: &str,
    audit: &super::protocol::McpInvocationAudit,
    arguments: Option<Value>,
    state: &HostState,
) -> Result<HostResult, String> {
    validate_name(server, "MCP server")?;
    validate_text(tool, "MCP tool", 256)?;
    if descriptor_digest.len() != 64
        || !descriptor_digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err("MCP descriptor binding is invalid".to_string());
    }
    audit.validate()?;
    let expected_policy_identity =
        crate::agent::tools::mcp::descriptor::model_tool_name(server, tool)?;
    if audit.server_identity != server
        || audit.policy_identity != expected_policy_identity
        || audit.descriptor_digest != descriptor_digest
        || audit.capability_generation != state.binding.capability_generation
    {
        return Err("MCP invocation audit identity does not match the host lease".to_string());
    }
    let (client, timeout, expected_digest) = {
        let mcp = state.mcp.lock().await;
        let hosted = mcp
            .get(server)
            .ok_or_else(|| format!("MCP server `{server}` is not attached"))?;
        (
            hosted.handle.client(),
            hosted.handle.timeout(),
            hosted.handle.descriptor_digest().to_string(),
        )
    };
    if descriptor_digest != expected_digest {
        return Err(
            "MCP descriptor binding does not match the attached server session".to_string(),
        );
    }
    crate::agent::tools::mcp::integration::verify_descriptor_stability(
        server,
        &client,
        timeout,
        &expected_digest,
    )
    .await?;
    let value = tokio::time::timeout(timeout, client.call_tool(tool.to_string(), arguments))
        .await
        .map_err(|_| {
            format!(
                "MCP server `{server}` timed out after {}s",
                timeout.as_secs()
            )
        })?
        .map_err(|error| format!("MCP server `{server}` failed: {error}"))?;
    Ok(HostResult::McpCall { value })
}

fn require_purpose(
    state: &HostState,
    purpose: super::protocol::HostPurpose,
    action: &str,
) -> Result<(), String> {
    if state.binding.purpose != purpose {
        return Err(format!(
            "{} extension host cannot {action}",
            match state.binding.purpose {
                super::protocol::HostPurpose::Task => "task",
                super::protocol::HostPurpose::AppService => "App service",
            }
        ));
    }
    Ok(())
}

async fn relay_app_call(
    binding: &super::protocol::ExtensionBinding,
    app_id: &str,
    tool: &str,
    arguments: Value,
    audit: super::protocol::AppInvocationAudit,
    timeout: Duration,
) -> Result<crate::agent::tools::mcp::protocol::CallToolResult, String> {
    let request = crate::clawd::wire::Request::new(
        crate::clawd::routes::Command::AppServiceCall,
        serde_json::json!({
            "app_id": app_id,
            "tool": tool,
            "arguments": arguments,
            "audit": audit,
        }),
    );
    let response = tokio::time::timeout(
        timeout,
        crate::clawd::client::request(&binding.broker_socket, request),
    )
    .await
    .map_err(|_| "App service relay timed out".to_string())??;
    if !response.ok {
        return Err(response
            .error
            .map(|error| error.message)
            .unwrap_or_else(|| "App service relay was refused".to_string()));
    }
    serde_json::from_value(
        response
            .result
            .ok_or_else(|| "App service relay omitted its result".to_string())?,
    )
    .map_err(|_| "App service relay returned an invalid result".to_string())
}

fn apps_root() -> PathBuf {
    PathBuf::from(std::env::var("COS_APPS_DIR").unwrap_or_else(|_| "/usr/lib/cos/apps".to_string()))
}

fn validate_name(value: &str, label: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(format!("{label} is invalid"));
    }
    Ok(())
}

fn validate_text(value: &str, label: &str, max: usize) -> Result<(), String> {
    if value.is_empty() || value.len() > max || value.bytes().any(|byte| byte == 0) {
        return Err(format!("{label} is invalid"));
    }
    Ok(())
}

fn validate_args(args: &[String]) -> Result<(), String> {
    if args.len() > MAX_APP_ARGS || args.iter().any(|arg| arg.len() > MAX_ARG_BYTES) {
        return Err("extension arguments exceed their limits".to_string());
    }
    Ok(())
}

fn read_bootstrap() -> Result<HostBootstrap, String> {
    let mut args = std::env::args();
    let mut bootstrap_fd = None;
    while let Some(arg) = args.next() {
        if arg == "--bootstrap-fd" {
            bootstrap_fd = args.next().and_then(|value| value.parse::<i32>().ok());
            break;
        }
    }
    let bootstrap_fd = bootstrap_fd
        .filter(|fd| *fd >= 3)
        .ok_or_else(|| "extension-host bootstrap descriptor argument is missing".to_string())?;
    let mut file = unsafe { std::fs::File::from_raw_fd(bootstrap_fd) };
    let mut bytes = Vec::new();
    file.by_ref()
        .take(64 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read extension-host bootstrap: {error}"))?;
    if bytes.is_empty() || bytes.len() > 64 * 1024 {
        return Err("extension-host bootstrap is missing or oversized".to_string());
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| "extension-host bootstrap is not a valid typed record".to_string())
}

fn require_hardened_identity(uid: u32, gid: u32, enforce_groups: bool) -> Result<(), String> {
    if unsafe { libc::getuid() } as u32 != uid || unsafe { libc::geteuid() } as u32 != uid {
        return Err("extension host uid drop did not take effect".to_string());
    }
    if unsafe { libc::getgid() } as u32 != gid || unsafe { libc::getegid() } as u32 != gid {
        return Err("extension host isolated gid drop did not take effect".to_string());
    }
    #[cfg(target_os = "linux")]
    {
        if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } != 0 {
            return Err(format!(
                "disable extension-host dumpability: {}",
                std::io::Error::last_os_error()
            ));
        }
        if unsafe { libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) } == 0 {
            return Err("extension host is missing PR_SET_NO_NEW_PRIVS".to_string());
        }
    }
    let mut groups = [0 as libc::gid_t; 64];
    let count = unsafe { libc::getgroups(groups.len() as libc::c_int, groups.as_mut_ptr()) };
    if count < 0 {
        return Err(format!(
            "read extension-host supplementary groups: {}",
            std::io::Error::last_os_error()
        ));
    }
    let gid = unsafe { libc::getgid() };
    if enforce_groups
        && groups
            .iter()
            .take(count as usize)
            .any(|group| *group != gid)
    {
        return Err("extension host retained supplementary groups".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/extension_host/host.rs"
    ));
}
