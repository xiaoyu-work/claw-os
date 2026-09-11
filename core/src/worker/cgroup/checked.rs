use std::ffi::CString;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use claw_display_control::workload::{
    cgroup_is_empty, check_cgroup, open_directory, read_file, retire_cgroup, write_file, Workload,
};
use claw_display_control::InstanceId;

use super::Limits;

#[derive(Debug)]
pub(super) struct CheckedScope {
    parent: OwnedFd,
    directory: OwnedFd,
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
        if unsafe { libc::geteuid() } != 0 {
            return Err("checked GUI containment is Root-owned".to_string());
        }
        let parent = parent.duplicate().map_err(|error| error.to_string())?;
        check_cgroup(parent.as_fd()).map_err(|error| error.to_string())?;
        let label = format!("gui-{}", claw_display_control::Epoch(instance.0).label());
        let name = CString::new(label.clone()).map_err(|error| error.to_string())?;
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
        let directory = unsafe { OwnedFd::from_raw_fd(raw) };
        let scope = Self {
            parent,
            directory,
            name,
            path,
            removed: false,
        };
        if unsafe { libc::fchmod(scope.directory.as_raw_fd(), 0o755) } != 0 {
            return Err(format!(
                "permit compositor cgroup identity inspection: {}",
                std::io::Error::last_os_error()
            ));
        }
        check_cgroup(scope.directory.as_fd()).map_err(|error| error.to_string())?;
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
        scope.retire(Instant::now() + Duration::from_secs(1))?;
        Ok(scope)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn duplicate(&self) -> Result<OwnedFd, String> {
        self.directory
            .try_clone()
            .map_err(|error| format!("duplicate GUI cgroup: {error}"))
    }

    pub fn retire(&self, deadline: Instant) -> Result<(), String> {
        if self.removed {
            return Ok(());
        }
        retire_cgroup(self.directory.as_fd(), deadline)
            .map_err(|error| format!("checked GUI retirement failed: {error}"))
    }

    pub fn remove(&mut self) -> Result<(), String> {
        if self.removed {
            return Ok(());
        }
        if !cgroup_is_empty(self.directory.as_fd()).map_err(|error| error.to_string())? {
            return Err("GUI cgroup remains populated".to_string());
        }
        use std::os::unix::fs::MetadataExt;
        let expected = std::fs::File::from(self.duplicate()?)
            .metadata()
            .map_err(|e| e.to_string())?;
        let current = std::fs::File::from(open_directory(&self.path).map_err(|e| e.to_string())?)
            .metadata()
            .map_err(|e| e.to_string())?;
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
            return Err(format!(
                "remove retired GUI cgroup: {}",
                std::io::Error::last_os_error()
            ));
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
