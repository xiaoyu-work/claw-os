//! `claw-extension-host` process implementation.

use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::os::fd::FromRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Notify, Semaphore};

use crate::agent::tools::mcp::integration::{McpServerHandle, McpServerSpec};
use crate::clawd::transport::frame::{PeerStream, ReadOutcome};
use crate::clawd::transport::peer;
use crate::clawd::wire::RequestId;

use super::protocol::{
    ControlRequest, ControlResponse, ExtensionErrorCategory, HostAction, HostBootstrap, HostResult,
    MAX_CONTROL_CONNECTIONS, MAX_CONTROL_FRAME_BYTES, MAX_REQUEST_TIMEOUT_MS, PROTOCOL_VERSION,
};

const RESPONSE_WRITE_TIMEOUT: Duration = Duration::from_secs(30);
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
    worker_uid: u32,
    owner_gid: u32,
    worker_pid: u32,
    worker_start_time_ticks: Option<u64>,
    lease_nonce: String,
    recent: Mutex<VecDeque<String>>,
    active: Mutex<HashMap<String, tokio::task::AbortHandle>>,
    mcp: tokio::sync::Mutex<HashMap<String, HostedMcp>>,
    shutting_down: AtomicBool,
    fatal_shutdown: AtomicBool,
    shutdown: Notify,
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
    require_hardened_identity(
        binding.extension_uid,
        binding.owner_gid,
        enforce_groups,
    )?;
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
        let listener = UnixListener::bind(&control_socket)
            .map_err(|error| format!("bind extension control socket: {error}"))?;
        peer::enable_credential_passing(listener.as_raw_fd())
            .map_err(|error| format!("enable extension peer credentials: {error}"))?;
        std::fs::set_permissions(&control_socket, std::fs::Permissions::from_mode(0o660))
            .map_err(|error| format!("protect extension control socket: {error}"))?;

        let state = Arc::new(HostState {
            task_id: binding.task_id.clone(),
            session_id: binding.session_id.clone(),
            worker_uid: binding.owner_uid,
            owner_gid: binding.owner_gid,
            worker_pid: binding.worker_pid,
            worker_start_time_ticks: binding.worker_start_time_ticks,
            lease_nonce: binding.lease_nonce.clone(),
            binding,
            isolation,
            recent: Mutex::new(VecDeque::new()),
            active: Mutex::new(HashMap::new()),
            mcp: tokio::sync::Mutex::new(HashMap::new()),
            shutting_down: AtomicBool::new(false),
            fatal_shutdown: AtomicBool::new(false),
            shutdown: Notify::new(),
        });
        let slots = Arc::new(Semaphore::new(MAX_CONTROL_CONNECTIONS));

        loop {
            tokio::select! {
                _ = state.shutdown.notified() => break,
                accepted = listener.accept() => {
                    let (stream, _) = accepted
                        .map_err(|error| format!("accept extension control request: {error}"))?;
                    let Ok(permit) = slots.clone().try_acquire_owned() else {
                        continue;
                    };
                    let state = state.clone();
                    tokio::spawn(async move {
                        let _permit = permit;
                        serve_control(stream, state).await;
                    });
                }
            }
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
        let _ = std::fs::remove_file(&control_socket);
        if state.fatal_shutdown.load(Ordering::SeqCst) {
            unsafe {
                libc::_exit(124);
            }
        }
        Ok::<(), String>(())
    })
}

async fn serve_control(stream: UnixStream, state: Arc<HostState>) {
    let mut stream = match PeerStream::new(stream) {
        Ok(stream) => stream,
        Err(_) => return,
    };
    let read = tokio::time::timeout(
        Duration::from_secs(10),
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

    let timeout = Duration::from_millis(request.timeout_ms.clamp(1, MAX_REQUEST_TIMEOUT_MS));
    let action = request.action;
    let state_for_action = state.clone();
    let mut task = tokio::spawn(async move { dispatch(action, state_for_action).await });
    let abort = task.abort_handle();
    if let Ok(mut active) = state.active.lock() {
        active.insert(id.as_str().to_string(), abort.clone());
    }
    let result = tokio::time::timeout(timeout, &mut task).await;
    if let Ok(mut active) = state.active.lock() {
        active.remove(id.as_str());
    }
    let response = match result {
        Ok(Ok(Ok(result))) => ControlResponse::ok(id, result),
        Ok(Ok(Err(error))) => {
            ControlResponse::error(id, ExtensionErrorCategory::RemoteCallFailure, error)
        }
        Ok(Err(join)) => {
            state.shutting_down.store(true, Ordering::SeqCst);
            state.fatal_shutdown.store(true, Ordering::SeqCst);
            ControlResponse::error(
                id,
                ExtensionErrorCategory::Crash,
                format!("extension host action crashed: {join}"),
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
    write_response(&mut stream, response).await;
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
    if process.uid != state.worker_uid
        || process.gid != state.owner_gid
        || process.pid != state.worker_pid
        || Some(process.start_time_ticks) != state.worker_start_time_ticks
    {
        return Err("extension-host request came from a different worker".to_string());
    }
    if request.task_id != state.task_id
        || request.session_id != state.session_id
        || request.lease_nonce != state.lease_nonce
        || request.binding_digest != state.binding.digest()?
    {
        return Err("extension-host request does not match this task lease".to_string());
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
            let _ =
                tokio::time::timeout(RESPONSE_WRITE_TIMEOUT, stream.write_response(&body)).await;
        }
        return;
    }
    let _ = tokio::time::timeout(RESPONSE_WRITE_TIMEOUT, stream.write_response(&body)).await;
}

async fn dispatch(action: HostAction, state: Arc<HostState>) -> Result<HostResult, String> {
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
            validate_name(&app_id, "App id")?;
            validate_text(&command, "App command", 256)?;
            validate_args(&args)?;
            let apps_root = apps_root();
            let app_dir = crate::apps::find(&apps_root, &app_id)
                .map(|app| app.dir)
                .ok_or_else(|| format!("App `{app_id}` is not installed"))?;
            let data = crate::paths::user_data_dir().to_string_lossy().into_owned();
            let apps = apps_root.to_string_lossy().into_owned();
            let isolation = state.isolation.clone();
            let output = tokio::task::spawn_blocking(move || {
                crate::bridge::run_app_with_isolation(
                    &app_dir,
                    &command,
                    &args,
                    &data,
                    &apps,
                    isolation,
                )
            })
            .await
            .map_err(|error| format!("App host task failed: {error}"))??;
            if output
                .as_ref()
                .is_some_and(|value| value.len() > MAX_CONTROL_FRAME_BYTES / 2)
            {
                return Err("App output exceeds the extension-host limit".to_string());
            }
            Ok(HostResult::AppOutput { output })
        }
        HostAction::AppOpen { app_id } => {
            validate_name(&app_id, "App id")?;
            let tool_count = crate::agent::tools::cos_apps_session::host_open_session(
                &app_id,
                &state.isolation,
            )
            .await?;
            Ok(HostResult::AppOpened { tool_count })
        }
        HostAction::AppCall {
            app_id,
            tool,
            arguments,
        } => {
            validate_name(&app_id, "App id")?;
            validate_text(&tool, "App tool", 256)?;
            if !arguments.is_object() && !arguments.is_null() {
                return Err("App tool arguments must be an object".to_string());
            }
            let value = crate::agent::tools::cos_apps_session::host_call_session(
                &app_id,
                &tool,
                arguments,
                Duration::from_millis(MAX_REQUEST_TIMEOUT_MS),
            )
            .await?;
            Ok(HostResult::AppCall { value })
        }
        HostAction::AppClose { app_id } => {
            validate_name(&app_id, "App id")?;
            let closed = crate::agent::tools::cos_apps_session::host_close_session(&app_id).await;
            Ok(HostResult::AppClosed { closed })
        }
        HostAction::McpAttach { spec } => attach_mcp(spec, &state).await,
        HostAction::McpCall {
            server,
            tool,
            descriptor_digest,
            audit,
            arguments,
        } => call_mcp(
            &server,
            &tool,
            &descriptor_digest,
            &audit,
            arguments,
            &state,
        )
        .await,
        HostAction::McpDetach { server } => {
            validate_name(&server, "MCP server")?;
            let detached = state.mcp.lock().await.remove(&server).is_some();
            Ok(HostResult::McpDetached { detached })
        }
        HostAction::Cancel { .. } | HostAction::Shutdown => {
            Err("control action was handled before dispatch".to_string())
        }
    }
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
