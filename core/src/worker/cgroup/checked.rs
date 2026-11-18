use std::ffi::CString;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use claw_display_control::workload::{check_cgroup, open_directory, read_file, write_file, Workload};
use claw_display_control::InstanceId;

use super::Limits;

#[derive(Debug)]
pub(super) struct CheckedScope {
    parent: OwnedFd,
    directory: Workload,
    name: CString,
    path: PathBuf,
    removed: bool,
}

impl CheckedScope {
    pub fn create(
        parent: &Workload,
        instance: InstanceId,
        limits: &Limits,
    ) -> Result<Self, String> {
        Self::create_at(
            parent.duplicate().map_err(|error| error.to_string())?,
            &format!("gui-{}", claw_display_control::Epoch(instance.0).label()),
            Some(limits),
        )
    }

    pub(super) fn create_at(
        parent: OwnedFd,
        label: &str,
        limits: Option<&Limits>,
    ) -> Result<Self, String> {
        if unsafe { libc::geteuid() } != 0 {
            return Err("checked App containment is Root-owned".to_string());
        }
        check_cgroup(parent.as_fd()).map_err(|error| error.to_string())?;
        if label.is_empty() || !label.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-') {
            return Err("invalid checked App cgroup name".into());
        }
        let name = CString::new(label).map_err(|error| error.to_string())?;
        let path = std::fs::read_link(format!("/proc/self/fd/{}", parent.as_raw_fd()))
            .map_err(|error| format!("locate pinned GUI cgroup parent: {error}"))?
            .join(label);
        if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o755) } != 0 {
            return Err(format!(
                "create GUI cgroup: {}",
                std::io::Error::last_os_error()
            ));
        }
        let raw = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if raw < 0 {
            let error = std::io::Error::last_os_error();
            if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) } != 0
            {
                return Err(format!(
                    "open GUI cgroup: {error}; rollback: {}",
                    std::io::Error::last_os_error()
                ));
            }
            return Err(format!("open GUI cgroup: {error}"));
        }
        let directory = match Workload::pin(unsafe { OwnedFd::from_raw_fd(raw) }) {
            Ok(directory) => directory,
            Err(error) => {
                if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) }
                    != 0
                {
                    return Err(format!("pin GUI cgroup: {error}; rollback: {}", std::io::Error::last_os_error()));
                }
                return Err(error.to_string());
            }
        };
        let scope = Self {
            parent,
            directory,
            name,
            path,
            removed: false,
        };
        if unsafe { libc::fchmod(scope.directory.as_fd().as_raw_fd(), 0o755) } != 0 {
            return Err(format!(
                "permit compositor cgroup identity inspection: {}",
                std::io::Error::last_os_error()
            ));
        }
        if let Some(limits) = limits {
            for (control, value) in [
                ("memory.max", limits.memory_bytes.to_string()),
                ("memory.swap.max", "0".to_string()),
                ("memory.oom.group", "1".to_string()),
                ("pids.max", limits.pids_max.to_string()),
                (
                    "cpu.max",
                    format!("{} 100000", u64::from(limits.cpu_percent) * 1000),
                ),
            ] {
                write_file(scope.directory.as_fd(), control, value.as_bytes())
                    .map_err(|error| format!("apply checked GUI {control}: {error}"))?;
                let actual = read_file(scope.directory.as_fd(), control, 4096)
                    .map_err(|error| format!("read back GUI {control}: {error}"))?;
                if actual.trim() != value {
                    return Err(format!("GUI {control} readback did not match its limit"));
                }
            }
        }
        scope.retire(Instant::now() + Duration::from_secs(1))?;
        Ok(scope)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn duplicate(&self) -> Result<OwnedFd, String> {
        self.directory
            .duplicate()
            .map_err(|error| format!("duplicate GUI cgroup: {error}"))
    }

    pub(super) fn workload(&self) -> &Workload {
        &self.directory
    }

    pub(super) fn attach(&self, pid: u32) -> Result<(), String> {
        write_file(self.directory.as_fd(), "cgroup.procs", pid.to_string().as_bytes())
            .map_err(|error| format!("attach gated App to checked cgroup: {error}"))
    }

    pub fn retire(&self, deadline: Instant) -> Result<(), String> {
        if self.removed {
            return Ok(());
        }
        self.directory.retire(deadline)
            .map_err(|error| format!("checked GUI retirement failed: {error}"))
    }

    pub fn remove(&mut self) -> Result<(), String> {
        if self.removed {
            return Ok(());
        }
        if self.directory.is_removed().map_err(|error| error.to_string())? {
            self.removed = true;
            return Ok(());
        }
        if !self.directory.is_empty().map_err(|error| error.to_string())? {
            return Err("GUI cgroup remains populated".to_string());
        }
        use std::os::unix::fs::MetadataExt;
        let expected = std::fs::File::from(self.duplicate()?)
            .metadata()
            .map_err(|e| e.to_string())?;
        let current = self.directory.confirm_removed(open_directory(&self.path).map(Some), None)
            .map_err(|e| e.to_string())?;
        let Some(current) = current else {
            self.removed = true;
            return Ok(());
        };
        let current = std::fs::File::from(current).metadata().map_err(|e| e.to_string())?;
        if (expected.dev(), expected.ino()) != (current.dev(), current.ino()) {
            return Err("GUI cgroup pathname was replaced".to_string());
        }
        if unsafe {
            libc::unlinkat(
                self.parent.as_raw_fd(),
                self.name.as_ptr(),
                libc::AT_REMOVEDIR,
            )
        } != 0
        {
            self.directory
                .confirm_removed(Err(std::io::Error::last_os_error().into()), ())
                .map_err(|error| format!("remove retired GUI cgroup: {error}"))?;
        }
        self.removed = true;
        Ok(())
    }
}

impl Drop for CheckedScope {
    fn drop(&mut self) {
        if !self.removed {
            if let Err(error) = self
                .retire(Instant::now() + Duration::from_secs(5))
                .and_then(|()| self.remove())
            {
                tracing::error!(%error, "checked GUI cgroup cleanup failed");
            }
        }
    }
}
