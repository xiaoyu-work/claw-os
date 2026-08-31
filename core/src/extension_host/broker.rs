//! Broker-owned private socket used by one task's extension tree.

use std::ffi::CStr;
use std::os::unix::io::AsRawFd;
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
use crate::extension_host::spawn::HostPaths;

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
    pub extension_uid: u32,
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
        extension_uid: u32,
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
            extension_uid,
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

pub fn bind_listener(
    paths: &HostPaths,
    owner_uid: u32,
    owner_gid: u32,
) -> Result<UnixListener, String> {
    if unsafe { libc::geteuid() } != 0 {
        return Err("extension broker socket requires a root broker".to_string());
    }
    ensure_entry_absent(paths.task_dir_fd(), c"broker.sock")?;
    let listener = match UnixListener::bind(&paths.broker_socket) {
        Ok(listener) => listener,
        Err(error) => {
            let _ = paths.cleanup();
            return Err(format!("bind extension broker socket: {error}"));
        }
    };
    let configured = (|| {
        peer::enable_credential_passing(listener.as_raw_fd())
            .map_err(|error| format!("enable extension broker credentials: {error}"))?;
        let identity = verify_listener_path(&listener, paths, None)?;
        set_socket_identity(
            paths.task_dir_fd(),
            c"broker.sock",
            owner_uid,
            owner_gid,
            0o600,
        )?;
        verify_listener_path(&listener, paths, Some(identity))?;
        paths.activate(owner_uid, owner_gid)?;
        verify_listener_path(&listener, paths, Some(identity))
    })();
    match configured {
        Ok(_) => Ok(listener),
        Err(error) => {
            drop(listener);
            let _ = paths.cleanup();
            Err(error)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SocketPathIdentity {
    device: u64,
    inode: u64,
}

fn ensure_entry_absent(parent: i32, name: &CStr) -> Result<(), String> {
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe {
        libc::fstatat(
            parent,
            name.as_ptr(),
            std::ptr::addr_of_mut!(stat),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } == 0
    {
        return Err("extension broker socket path already exists".to_string());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ENOENT) {
        Ok(())
    } else {
        Err(format!("inspect extension broker socket path: {error}"))
    }
}

fn set_socket_identity(
    parent: i32,
    name: &CStr,
    uid: u32,
    gid: u32,
    mode: libc::mode_t,
) -> Result<(), String> {
    let before = socket_path_identity(parent, name)?;
    if unsafe { libc::fchownat(parent, name.as_ptr(), uid, gid, libc::AT_SYMLINK_NOFOLLOW) } != 0 {
        return Err(format!(
            "chown extension broker socket: {}",
            std::io::Error::last_os_error()
        ));
    }
    if unsafe { libc::fchmodat(parent, name.as_ptr(), mode, 0) } != 0 {
        return Err(format!(
            "chmod extension broker socket: {}",
            std::io::Error::last_os_error()
        ));
    }
    let after = socket_path_identity(parent, name)?;
    if before != after {
        return Err("extension broker socket changed while it was secured".to_string());
    }
    let stat = socket_stat(parent, name)?;
    if stat.st_uid != uid || stat.st_gid != gid || stat.st_mode & 0o7777 != mode {
        return Err("extension broker socket ownership or mode did not apply".to_string());
    }
    Ok(())
}

fn socket_path_identity(parent: i32, name: &CStr) -> Result<SocketPathIdentity, String> {
    let stat = socket_stat(parent, name)?;
    Ok(SocketPathIdentity {
        device: stat.st_dev,
        inode: stat.st_ino,
    })
}

fn socket_stat(parent: i32, name: &CStr) -> Result<libc::stat, String> {
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe {
        libc::fstatat(
            parent,
            name.as_ptr(),
            std::ptr::addr_of_mut!(stat),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(format!(
            "inspect extension broker socket: {}",
            std::io::Error::last_os_error()
        ));
    }
    if stat.st_mode & libc::S_IFMT != libc::S_IFSOCK || stat.st_nlink != 1 {
        return Err("extension broker endpoint is not a single-link Unix socket".to_string());
    }
    Ok(stat)
}

fn verify_listener_path(
    listener: &UnixListener,
    paths: &HostPaths,
    expected: Option<SocketPathIdentity>,
) -> Result<SocketPathIdentity, String> {
    let local = listener
        .local_addr()
        .map_err(|error| format!("inspect extension listener address: {error}"))?;
    if local.as_pathname() != Some(paths.broker_socket.as_path()) {
        return Err("extension listener is bound to an unexpected pathname".to_string());
    }
    let identity = socket_path_identity(paths.task_dir_fd(), c"broker.sock")?;
    if expected.is_some_and(|expected| expected != identity) {
        return Err("extension broker pathname changed after bind".to_string());
    }
    let mut endpoint: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(listener.as_raw_fd(), std::ptr::addr_of_mut!(endpoint)) } != 0 {
        return Err(format!(
            "inspect extension listener endpoint: {}",
            std::io::Error::last_os_error()
        ));
    }
    let endpoint_inode = endpoint.st_ino.to_string();
    let path = paths.broker_socket.to_string_lossy();
    let linked = std::fs::read_to_string("/proc/net/unix")
        .map_err(|error| format!("verify extension listener in /proc/net/unix: {error}"))?
        .lines()
        .skip(1)
        .any(|line| {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            fields.get(6) == Some(&endpoint_inode.as_str())
                && fields.get(7).is_some_and(|seen| *seen == path)
        });
    if !linked {
        return Err("extension broker pathname is not the listener endpoint".to_string());
    }
    Ok(identity)
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
    let client = ClientIdentity::from_verified_delegation(
        process.pid,
        lease.owner_uid,
        process.uid,
        process.gid,
        process.start_time_ticks,
    );
    if lease.verify_live().is_err()
        || process.uid != lease.extension_uid
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
