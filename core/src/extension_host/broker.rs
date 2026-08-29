//! Broker-owned private socket used by one task's extension tree.

use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Semaphore;

use crate::clawd::client_identity::ClientIdentity;
use crate::clawd::protocol::{encode_response, Response};
use crate::clawd::routes::{Access, Command, Route};
use crate::clawd::state::DaemonState;
use crate::clawd::transport::frame::{PeerStream, ReadOutcome};
use crate::clawd::transport::limits::Admission;
use crate::clawd::transport::peer;
use crate::clawd::wire::{Fault, Request, RequestId, MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES};

const MAX_CONNECTIONS: usize = 16;
const READ_TIMEOUT: Duration = Duration::from_secs(10);
const WRITE_TIMEOUT: Duration = Duration::from_secs(60);

const HOST_LIFECYCLE_ROUTES: &[Command] = &[
    Command::AppSessionRegister,
    Command::McpSessionRegister,
    Command::AppSessionBind,
    Command::AppSessionSetTransient,
    Command::AppSessionDeregister,
    Command::PermissionStatus,
];

const CHILD_PROVIDER_ROUTES: &[Command] = &[
    Command::CredentialOauthRefresh,
    Command::SystemAudioControl,
    Command::SystemAccessibilityControl,
    Command::SystemBackupControl,
    Command::SystemBluetoothControl,
    Command::SystemCameraControl,
    Command::SystemClipboardControl,
    Command::SystemContainerControl,
    Command::SystemConfigControl,
    Command::SystemCrashInspect,
    Command::SystemDesktopControl,
    Command::SystemDisplayControl,
    Command::SystemEventsControl,
    Command::SystemFirewallControl,
    Command::SystemHardwareInspect,
    Command::SystemLocationQuery,
    Command::SystemNetworkControl,
    Command::SystemPackageInstall,
    Command::SystemPackageControl,
    Command::SystemPackageRestore,
    Command::SystemPowerControl,
    Command::SystemPrinterControl,
    Command::SystemSecurityInspect,
    Command::SystemServiceControl,
    Command::SystemServiceRestore,
    Command::SystemSnapshotControl,
    Command::SystemStorageControl,
    Command::SystemUsbControl,
    Command::SystemUsersControl,
];

#[derive(Debug)]
pub struct ExtensionLease {
    pub task_id: String,
    pub task_session_id: Option<String>,
    pub host_session_id: Option<String>,
    pub owner_uid: u32,
    pub owner_gid: u32,
    pub worker_pid: u32,
    pub worker_start_time_ticks: Option<u64>,
    pub host_pid: u32,
    pub host_start_time_ticks: Option<u64>,
    deadline_ms: AtomicU64,
    closed: AtomicBool,
}

impl ExtensionLease {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        task_id: String,
        task_session_id: Option<String>,
        host_session_id: Option<String>,
        owner_uid: u32,
        owner_gid: u32,
        worker_pid: u32,
        worker_start_time_ticks: Option<u64>,
        host_pid: u32,
        host_start_time_ticks: Option<u64>,
        deadline_ms: u64,
    ) -> Self {
        Self {
            task_id,
            task_session_id,
            host_session_id,
            owner_uid,
            owner_gid,
            worker_pid,
            worker_start_time_ticks,
            host_pid,
            host_start_time_ticks,
            deadline_ms: AtomicU64::new(deadline_ms),
            closed: AtomicBool::new(false),
        }
    }

    pub fn renew(&self, lease: Duration) {
        self.deadline_ms.store(
            crate::agentd::grant::now_ms().saturating_add(lease.as_millis() as u64),
            Ordering::SeqCst,
        );
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }

    fn verify_live(&self) -> Result<(), String> {
        if self.closed.load(Ordering::SeqCst) {
            return Err("extension task lease is closed".to_string());
        }
        if crate::agentd::grant::now_ms() > self.deadline_ms.load(Ordering::SeqCst) {
            return Err("extension task lease expired".to_string());
        }
        if !process_matches(self.worker_pid, self.worker_start_time_ticks) {
            return Err("extension task worker is no longer live".to_string());
        }
        if !process_matches(self.host_pid, self.host_start_time_ticks) {
            return Err("extension host is no longer live".to_string());
        }
        Ok(())
    }
}

pub fn bind_listener(path: &Path, owner_uid: u32, owner_gid: u32) -> Result<UnixListener, String> {
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)
        .map_err(|error| format!("bind extension broker socket: {error}"))?;
    peer::enable_credential_passing(listener.as_raw_fd())
        .map_err(|error| format!("enable extension broker credentials: {error}"))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("protect extension broker socket: {error}"))?;
    if unsafe { libc::geteuid() } == 0 {
        let raw = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
            .map_err(|_| "extension broker socket path contains NUL".to_string())?;
        if unsafe { libc::chown(raw.as_ptr(), owner_uid, owner_gid) } != 0 {
            return Err(format!(
                "chown extension broker socket: {}",
                std::io::Error::last_os_error()
            ));
        }
    }
    Ok(listener)
}

pub async fn serve(
    listener: UnixListener,
    lease: Arc<ExtensionLease>,
    state: DaemonState,
    admission: Arc<Admission>,
) {
    let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    loop {
        if lease.closed.load(Ordering::SeqCst) {
            return;
        }
        let accepted = tokio::select! {
            accepted = listener.accept() => accepted,
            _ = tokio::time::sleep(Duration::from_millis(100)) => continue,
        };
        let Ok((stream, _)) = accepted else {
            return;
        };
        let Ok(permit) = slots.clone().try_acquire_owned() else {
            continue;
        };
        let lease = lease.clone();
        let state = state.clone();
        let admission = admission.clone();
        tokio::spawn(async move {
            let _permit = permit;
            serve_connection(stream, lease, state, admission).await;
        });
    }
}

async fn serve_connection(
    stream: UnixStream,
    lease: Arc<ExtensionLease>,
    state: DaemonState,
    admission: Arc<Admission>,
) {
    let mut peer_stream = match PeerStream::new(stream) {
        Ok(stream) => stream,
        Err(_) => return,
    };
    let frame =
        match tokio::time::timeout(READ_TIMEOUT, peer_stream.read_request(MAX_REQUEST_BYTES)).await
        {
            Ok(Ok(ReadOutcome::Frame(frame))) => frame,
            _ => return,
        };
    if peer_stream.has_pending_input() {
        write_fault(&mut peer_stream, RequestId::unknown(), Fault::ExtraFrame).await;
        return;
    }
    let request = match serde_json::from_slice::<Request>(&frame.body) {
        Ok(request) => request,
        Err(_) => {
            write_fault(
                &mut peer_stream,
                RequestId::unknown(),
                Fault::InvalidEnvelope,
            )
            .await;
            return;
        }
    };
    let id = request.id.clone();
    let Some(process) = peer::verify(frame.credentials) else {
        write_fault(&mut peer_stream, id, Fault::PeerUnverified).await;
        return;
    };
    let client = ClientIdentity::from_verified_parts(
        process.pid,
        process.uid,
        process.gid,
        process.start_time_ticks,
    );
    if lease.verify_live().is_err()
        || process.uid != lease.owner_uid
        || process.gid != lease.owner_gid
        || !request_allowed(&request, process, &lease)
    {
        write_fault(&mut peer_stream, id, Fault::NotAuthorized).await;
        return;
    }

    let response =
        crate::clawd::server::dispatch_verified_request(request, &client, &state, &admission).await;
    write_response(&mut peer_stream, response).await;
}

fn request_allowed(request: &Request, process: peer::PeerProcess, lease: &ExtensionLease) -> bool {
    let route = request.command.route();
    if process.pid == lease.host_pid
        && Some(process.start_time_ticks) == lease.host_start_time_ticks
    {
        return host_lifecycle_route(request.command);
    }
    if !crate::proc::process_descends_from(process.pid, lease.host_pid) {
        return false;
    }
    if !child_route(route) {
        return false;
    }
    let Some(host_session_id) = lease.host_session_id.as_deref() else {
        return false;
    };
    let Some(session) = crate::proc::nearest_session_for_owner(lease.owner_uid, process.pid) else {
        return false;
    };
    session_matches_request(&session, host_session_id, request)
}

fn session_matches_request(
    session: &crate::proc::SessionInfo,
    host_session_id: &str,
    request: &Request,
) -> bool {
    session.session_id != host_session_id
        && session.parent.as_deref() == Some(host_session_id)
        && matches!(session.group.as_deref(), Some("app" | "mcp"))
        && request
            .params
            .get("session")
            .and_then(serde_json::Value::as_str)
            == Some(session.session_id.as_str())
}

fn host_lifecycle_route(command: Command) -> bool {
    HOST_LIFECYCLE_ROUTES.contains(&command)
}

fn child_route(route: &Route) -> bool {
    route.access == Access::User && CHILD_PROVIDER_ROUTES.contains(&route.command)
}

fn process_matches(pid: u32, start_time_ticks: Option<u64>) -> bool {
    pid > 1
        && start_time_ticks.is_some()
        && crate::proc::read_start_time_ticks_pub(pid) == start_time_ticks
}

async fn write_fault(stream: &mut PeerStream, id: RequestId, fault: Fault) {
    write_response(stream, Response::fault(id, fault)).await;
}

async fn write_response(stream: &mut PeerStream, response: Response) {
    let body = match encode_response(&response) {
        Ok(body) if body.len() <= MAX_RESPONSE_BYTES => body,
        _ => encode_response(&Response::fault(response.id, Fault::ResponseTooLarge))
            .unwrap_or_default(),
    };
    let _ = tokio::time::timeout(WRITE_TIMEOUT, stream.write_response(&body)).await;
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/extension_host/broker.rs"
    ));
}
