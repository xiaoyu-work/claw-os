//! Root-owned, per-App data views admitted after a task's App registration.
//! No caller path or environment value selects the backing storage.

use std::collections::BTreeMap;
use std::sync::Mutex;

use super::*;
use crate::clawd::client_identity::ClientIdentity;

const MAX_APP_VIEWS: usize = 64;
const MOUNT_HELPER_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug)]
struct View {
    binding: app_data::Binding,
    destination: CString,
}

pub(crate) struct TaskAppData {
    owner: WorkerIdentity,
    execution: ExtensionIdentity,
    binding: ExtensionBinding,
    paths: HostPaths,
    namespace: Arc<OwnedFd>,
    stopped: AtomicBool,
    views: Mutex<BTreeMap<String, View>>,
}

impl std::fmt::Debug for TaskAppData {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("TaskAppData")
            .field("owner_uid", &self.owner.uid)
            .field("host_pid", &self.binding.host_pid)
            .field("stopped", &self.stopped.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl TaskAppData {
    pub(super) fn new(
        owner: WorkerIdentity,
        execution: ExtensionIdentity,
        binding: ExtensionBinding,
        paths: HostPaths,
        namespace: Arc<OwnedFd>,
    ) -> Self {
        Self {
            owner,
            execution,
            binding,
            paths,
            namespace,
            stopped: AtomicBool::new(false),
            views: Mutex::new(BTreeMap::new()),
        }
    }

    fn require_client(&self, client: &ClientIdentity) -> Result<(), String> {
        let host = client
            .extension_host
            .as_ref()
            .ok_or("App data requires a private Task Host")?;
        if self.stopped.load(Ordering::Acquire)
            || self.binding.purpose != protocol::HostPurpose::Task
            || self.binding.app_id.is_some()
            || host.purpose != protocol::HostPurpose::Task
            || host.lease_id != self.binding.task_id
            || host.owner_uid != self.owner.uid
            || host.extension_uid != self.execution.uid
            || host.host_pid != self.binding.host_pid
            || host.host_start_time_ticks != self.binding.host_start_time_ticks
            || client.pid != Some(self.binding.host_pid)
            || client.start_time_ticks != self.binding.host_start_time_ticks
            || client.uid != Some(self.owner.uid)
            || client.execution_uid != Some(self.execution.uid)
            || client.gid != Some(self.execution.gid)
            || self.binding.host_start_time_ticks.is_none()
            || crate::proc::read_start_time_ticks_pub(self.binding.host_pid)
                != self.binding.host_start_time_ticks
            || crate::proc::read_start_time_ticks_pub(self.binding.controller_pid)
                != self.binding.controller_start_time_ticks
        {
            return Err("App data binding does not belong to this live Task Host".to_string());
        }
        Ok(())
    }

    pub(crate) fn bind(
        &self,
        client: &ClientIdentity,
        package: &crate::provenance::VerifiedPackage,
    ) -> Result<PathBuf, String> {
        let package = PackageRef::of(package);
        if package.kind != crate::provenance::PackageKind::App {
            return Err("Task App data requires a verified App package".to_string());
        }
        self.bind_app(client, &package.id)
    }

    fn bind_app(&self, client: &ClientIdentity, app: &str) -> Result<PathBuf, String> {
        self.require_client(client)?;
        crate::worker::derive::validate_app_id(app)?;
        let mut views = self
            .views
            .lock()
            .map_err(|_| "Task App data lock poisoned")?;
        self.require_client(client)?;
        if let Some(view) = views.get(app) {
            if let Err(error) = view.binding.require_current() {
                self.close();
                return Err(error);
            }
            return Ok(self.paths.dir.join("app-data"));
        }
        Self::require_capacity(views.len())?;
        let prepared = app_data::prepare_app(&self.owner, &self.execution, app, &self.paths)?;
        self.require_client(client)?;
        // Retain the target before installing: an interrupted helper may
        // already have attached it, so cleanup must still visit that target.
        views.insert(
            app.to_string(),
            View {
                binding: prepared.binding.clone(),
                destination: prepared.destination.clone(),
            },
        );
        if let Err(error) = in_namespace(&self.namespace, || prepared.install()) {
            self.close();
            return Err(error);
        }
        self.require_client(client)?;
        if let Err(error) = prepared.binding.require_current() {
            self.close();
            return Err(error);
        }
        Ok(prepared.view_root)
    }

    fn require_capacity(held: usize) -> Result<(), String> {
        if held >= MAX_APP_VIEWS {
            return Err("Task App data view limit reached".to_string());
        }
        Ok(())
    }

    pub(crate) fn close(&self) {
        self.stopped.store(true, Ordering::Release);
    }

    pub(super) fn cleanup(&self) -> Result<(), String> {
        self.close();
        let mut views = self
            .views
            .lock()
            .map_err(|_| "Task App data lock poisoned")?;
        if views.is_empty() {
            return Ok(());
        }
        in_namespace(&self.namespace, || {
            for view in views.values() {
                if unsafe { libc::umount2(view.destination.as_ptr(), 0) } != 0
                    && std::io::Error::last_os_error().raw_os_error() != Some(libc::EINVAL)
                {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        })?;
        views.clear();
        Ok(())
    }
}

fn in_namespace(
    namespace: &OwnedFd,
    operation: impl FnOnce() -> std::io::Result<()>,
) -> Result<(), String> {
    let parent = unsafe { libc::getpid() };
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(format!(
            "fork Task App mount helper: {}",
            std::io::Error::last_os_error()
        ));
    }
    if pid == 0 {
        let result = if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) } != 0
        {
            Err(std::io::Error::last_os_error())
        } else if unsafe { libc::getppid() } != parent {
            Err(std::io::Error::from_raw_os_error(libc::EPIPE))
        } else if unsafe { libc::setns(namespace.as_raw_fd(), libc::CLONE_NEWNS) } != 0 {
            Err(std::io::Error::last_os_error())
        } else {
            operation()
        };
        let code = result
            .err()
            .map(|error| error.raw_os_error().unwrap_or(libc::EIO))
            .unwrap_or(0);
        unsafe { libc::_exit(code) };
    }
    let deadline = Instant::now() + MOUNT_HELPER_TIMEOUT;
    let mut status = 0;
    loop {
        let waited = unsafe { libc::waitpid(pid, std::ptr::addr_of_mut!(status), libc::WNOHANG) };
        if waited == pid {
            return if libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0 {
                Ok(())
            } else if libc::WIFEXITED(status) {
                Err(format!(
                    "Task App mount helper: {}",
                    std::io::Error::from_raw_os_error(libc::WEXITSTATUS(status))
                ))
            } else {
                Err(format!("Task App mount helper exited abnormally: {status}"))
            };
        }
        if waited < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return Err(format!(
                "wait for Task App mount helper: {}",
                std::io::Error::last_os_error()
            ));
        }
        if Instant::now() >= deadline {
            let killed = unsafe { libc::kill(pid, libc::SIGKILL) };
            let stopped = if killed == 0 {
                None
            } else {
                Some(std::io::Error::last_os_error())
            };
            loop {
                let waited = unsafe { libc::waitpid(pid, std::ptr::addr_of_mut!(status), 0) };
                if waited == pid {
                    return Err(match stopped {
                        Some(error) => {
                            format!("Task App mount helper timed out; kill failed: {error}")
                        }
                        None => "Task App mount helper timed out".to_string(),
                    });
                }
                if waited < 0
                    && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
                {
                    continue;
                }
                return Err(format!(
                    "Task App mount helper timed out; reap failed: {}",
                    std::io::Error::last_os_error()
                ));
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/extension_host/spawn/task_data.rs"
    ));
}
