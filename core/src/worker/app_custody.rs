//! Private OS Host cleanup acknowledgement; no App code or approval protocol.

use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::LaunchResources;

pub(crate) struct Gate {
    input: OwnedFd,
    output: Mutex<Option<std::fs::File>>,
    token: String,
}

impl Gate {
    pub(crate) fn new() -> Result<Self, String> {
        let mut descriptors = [-1; 2];
        if unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(Self {
            input: unsafe { OwnedFd::from_raw_fd(descriptors[0]) },
            output: Mutex::new(Some(unsafe { std::fs::File::from_raw_fd(descriptors[1]) })),
            token: uuid::Uuid::new_v4().simple().to_string(),
        })
    }

    pub(crate) fn fd(&self) -> i32 {
        self.input.as_raw_fd()
    }

    pub(crate) fn arguments(&self) -> [String; 4] {
        ["--custody-gate".into(), self.fd().to_string(), self.token.clone(), "--".into()]
    }

    pub(crate) fn release(&self) -> Result<(), String> {
        let mut output = self.output.lock().map_err(|_| "App custody gate lock poisoned")?;
        let Some(mut output) = output.take() else { return Ok(()) };
        output.write_all(self.token.as_bytes()).map_err(|error| error.to_string())
    }
}

pub(crate) struct Custody {
    socket: PathBuf,
    resources: Mutex<Option<LaunchResources>>,
    pub(crate) gate: Gate,
    ending: AtomicBool,
    process: Mutex<Option<OwnedFd>>,
}

impl Custody {
    pub(crate) fn start(resources: LaunchResources, gate: Gate) -> Result<Arc<Self>, String> {
        let directory = resources.runtime_dir.as_ref().ok_or("App custody has no private runtime")?;
        let socket = directory.child("custody.sock");
        let listener = UnixListener::bind(&socket).map_err(|error| error.to_string())?;
        listener.set_nonblocking(true).map_err(|error| error.to_string())?;
        let custody = Arc::new(Self { socket, resources: Mutex::new(Some(resources)), gate, ending: AtomicBool::new(false), process: Mutex::new(None) });
        let retained = custody.clone();
        std::thread::Builder::new().name("app-resource-custody".into()).spawn(move || {
            // Root keeps this exact authenticated connection, not a reconnectable pathname.
            loop {
                if retained.ending.load(Ordering::Acquire)
                    && retained.retire(Instant::now() + Duration::from_secs(5)).is_ok()
                {
                    return;
                }
                let mut connection = match listener.accept() {
                    Ok((connection, _)) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    Err(error) => {
                        tracing::error!(%error, "App cleanup listener failed; custody retained");
                        std::thread::sleep(Duration::from_millis(100));
                        continue;
                    }
                };
                if super::peer_uid_of(&connection) != Some(0) {
                    continue;
                }
                let _ = connection.set_read_timeout(Some(Duration::from_secs(30)));
                loop {
                    let mut request = [0; 1];
                    match connection.read_exact(&mut request) {
                        Err(error) if matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => continue,
                        Err(_) => {
                            // Losing Root is not proof of cleanup. Retain the resources and refuse
                            // replacement in the new broker via its stale App cgroup check.
                            while let Err(error) = retained.retire(Instant::now() + Duration::from_secs(5)) {
                                tracing::error!(%error, "App Host retains incomplete resource cleanup after Root disconnect");
                                std::thread::sleep(Duration::from_millis(100));
                            }
                            return;
                        }
                        Ok(()) if request == [1] => {
                            let result = retained.retire(Instant::now() + Duration::from_secs(5));
                            let reply = match &result { Ok(()) => [1], Err(_) => [0] };
                            let _ = connection.write_all(&reply);
                            if result.is_ok() {
                                return;
                            }
                        }
                        Ok(()) => return,
                    }
                }
            }
        }).map_err(|error| error.to_string())?;
        Ok(custody)
    }

    pub(crate) fn socket(&self) -> &Path {
        &self.socket
    }

    pub(crate) fn ending(&self) {
        self.ending.store(true, Ordering::Release);
    }

    pub(crate) fn bind_process(&self, pid: u32) -> Result<(), String> {
        let mut process = self.process.lock().map_err(|_| "App process custody lock poisoned")?;
        if process.is_some() { return Ok(()); }
        let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        if raw < 0 { return Err(std::io::Error::last_os_error().to_string()); }
        let descriptor = unsafe { OwnedFd::from_raw_fd(raw as i32) };
        *process = Some(descriptor);
        Ok(())
    }

    pub(crate) fn finish(&self) -> Result<(), String> {
        self.retire(Instant::now() + Duration::from_secs(5))
    }

    pub(crate) fn stop_process(&self) {
        if let Ok(process) = self.process.lock() {
            if let Some(process) = process.as_ref() {
                unsafe { libc::syscall(libc::SYS_pidfd_send_signal, process.as_raw_fd(),
                    libc::SIGKILL, std::ptr::null::<libc::siginfo_t>(), 0); }
            }
        }
    }

    fn retire(&self, deadline: Instant) -> Result<(), String> {
        let mut resources = self.resources.lock().map_err(|_| "App resource custody lock poisoned")?;
        let Some(held) = resources.as_mut() else { return Ok(()) };
        if let Some(broker) = held.broker.as_mut() {
            broker.retire(deadline)?;
        }
        if let Some(egress) = held.egress.as_mut() {
            egress.retire(deadline)?;
        }
        if let Some(directory) = held.runtime_dir.as_ref() {
            directory.remove_checked()?;
        }
        *resources = None;
        Ok(())
    }
}

pub(crate) async fn connect(path: &Path, pid: u32, uid: u32) -> Result<UnixStream, String> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};

    if !path.is_absolute() || path.as_os_str().len() > 4096 {
        return Err("invalid App cleanup endpoint".into());
    }
    // The launcher may own a private /tmp mount. Pin its endpoint, not a
    // same-named path in Root's namespace or a pathname that can be replaced.
    let target = PathBuf::from(format!("/proc/{pid}/root"))
        .join(path.strip_prefix("/").map_err(|error| error.to_string())?);
    let pin = std::fs::OpenOptions::new().read(true)
        .custom_flags(libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(target).map_err(|error| format!("pin App cleanup custody: {error}"))?;
    let metadata = pin.metadata().map_err(|error| error.to_string())?;
    if !metadata.file_type().is_socket() || metadata.uid() != uid {
        return Err("App cleanup endpoint is not its launcher's socket".into());
    }
    let pinned = format!("/proc/self/fd/{}", pin.as_raw_fd());
    let stream = tokio::time::timeout(Duration::from_secs(2), tokio::net::UnixStream::connect(pinned))
        .await.map_err(|_| "App cleanup custody connection deadline exceeded")?
        .map_err(|error| format!("connect App cleanup custody: {error}"))?
        .into_std().map_err(|error| error.to_string())?;
    stream.set_nonblocking(false).map_err(|error| error.to_string())?;
    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    if unsafe { libc::getsockopt(stream.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERCRED,
        std::ptr::addr_of_mut!(credentials).cast(), &mut length) } != 0
        || credentials.pid as u32 != pid || credentials.uid != uid
    {
        return Err("App cleanup endpoint is not held by its authenticated launcher".into());
    }
    Ok(stream)
}

pub(crate) fn retire(stream: &mut UnixStream, sent: &mut bool, deadline: Instant) -> Result<(), String> {
    let remaining = deadline.checked_duration_since(Instant::now()).ok_or("App cleanup deadline exceeded")?;
    stream.set_read_timeout(Some(remaining)).map_err(|error| error.to_string())?;
    stream.set_write_timeout(Some(remaining)).map_err(|error| error.to_string())?;
    if !*sent {
        stream.write_all(&[1]).map_err(|error| format!("request App cleanup: {error}"))?;
        *sent = true;
    }
    let mut reply = [0];
    stream.read_exact(&mut reply).map_err(|error| format!("App cleanup acknowledgement remains pending: {error}"))?;
    if reply != [1] {
        *sent = false;
        return Err("App Host reports incomplete provider/resource cleanup".into());
    }
    Ok(())
}
