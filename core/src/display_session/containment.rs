use std::ffi::CString;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use claw_display_control::workload::{
    open_directory, read_file, write_file, SessionGroup, Workload,
};
use claw_display_control::{Epoch, Error, ProcessIdentity};

pub struct Containment {
    anchor: SessionGroup,
    leader: PathBuf,
    workload_path: PathBuf,
    compositor_path: PathBuf,
    pub workload: Workload,
    compositor_membership: OwnedFd,
    retired: bool,
}

impl Containment {
    pub fn prepare(parent: &ProcessIdentity, epoch: Epoch) -> Result<Self, Error> {
        let current = ProcessIdentity::current()?;
        if parent.uid() != 0 || current.uid() != 0 || ProcessIdentity::parent()? != *parent {
            return Err(Error::Identity);
        }
        let anchor = SessionGroup::of(parent)?;
        if SessionGroup::of(&current)?.path() != anchor.path() {
            return Err(Error::Identity);
        }
        let directory = anchor.open()?;
        let mut expected = vec![parent.pid(), current.pid()];
        expected.sort_unstable();
        if members(directory.as_fd())? != expected {
            return Err(Error::Protocol(
                "login cgroup contains processes outside the authenticated activation pair",
            ));
        }
        if !read_file(directory.as_fd(), "cgroup.subtree_control", 4096)?
            .trim()
            .is_empty()
        {
            return Err(Error::Protocol(
                "login cgroup already has distributed controllers",
            ));
        }
        let available = read_file(directory.as_fd(), "cgroup.controllers", 4096)?;
        for controller in ["cpu", "memory", "pids"] {
            if !available
                .split_whitespace()
                .any(|value| value == controller)
            {
                return Err(Error::Protocol(
                    "display activation requires delegated cpu, memory and pids controllers",
                ));
            }
        }
        let leader = anchor.path().join(format!("claw-login-{}", epoch.label()));
        let workload_path = anchor
            .path()
            .join(format!("claw-display-{}", epoch.label()));
        let compositor_path = workload_path.join("compositor");
        let mut preparation = Preparation {
            anchor: &anchor,
            parent,
            current: &current,
            leader: &leader,
            workload: &workload_path,
            compositor: &compositor_path,
            leader_created: false,
            parent_moved: false,
            current_moved: false,
            controllers_attempted: false,
            workload_created: false,
            compositor_created: false,
        };
        let result = (|| {
            std::fs::create_dir(&leader)?;
            preparation.leader_created = true;
            let leader_directory = open_directory(&leader)?;
            if unsafe { libc::fchmod(leader_directory.as_raw_fd(), 0o755) } != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            parent.assert_current()?;
            current.assert_current()?;
            write_file(
                leader_directory.as_fd(),
                "cgroup.procs",
                parent.pid().to_string().as_bytes(),
            )?;
            preparation.parent_moved = true;
            write_file(
                leader_directory.as_fd(),
                "cgroup.procs",
                current.pid().to_string().as_bytes(),
            )?;
            preparation.current_moved = true;
            if !members(directory.as_fd())?.is_empty()
                || members(leader_directory.as_fd())? != expected
            {
                return Err(Error::Protocol("login cgroup placement was not exclusive"));
            }
            preparation.controllers_attempted = true;
            write_file(
                directory.as_fd(),
                "cgroup.subtree_control",
                b"+cpu +memory +pids",
            )?;
            std::fs::create_dir(&workload_path)?;
            preparation.workload_created = true;
            let workload = Workload::receive(open_directory(&workload_path)?, &anchor, epoch)?;
            if unsafe { libc::fchmod(workload.as_fd().as_raw_fd(), 0o755) } != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            write_file(
                workload.as_fd(),
                "cgroup.subtree_control",
                b"+cpu +memory +pids",
            )?;
            write_file(workload.as_fd(), "memory.oom.group", b"1")?;
            std::fs::create_dir(&compositor_path)?;
            preparation.compositor_created = true;
            let compositor = open_directory(&compositor_path)?;
            if unsafe { libc::fchmod(compositor.as_raw_fd(), 0o755) } != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            let descriptor = unsafe {
                libc::openat(
                    compositor.as_raw_fd(),
                    c"cgroup.procs".as_ptr(),
                    libc::O_WRONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                )
            };
            if descriptor < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            use std::os::fd::FromRawFd;
            let compositor_membership = unsafe { OwnedFd::from_raw_fd(descriptor) };
            workload.retire(Instant::now() + Duration::from_secs(1))?;
            Ok((workload, compositor_membership))
        })();
        let (workload, compositor_membership) = match result {
            Ok(prepared) => prepared,
            Err(error) => {
                if let Err(rollback_error) = preparation.rollback() {
                    eprintln!("display cgroup preparation failed: {error}; rollback failed: {rollback_error}");
                    return Err(rollback_error);
                }
                return Err(error);
            }
        };
        Ok(Self {
            anchor,
            leader,
            workload_path,
            compositor_path,
            workload,
            compositor_membership,
            retired: false,
        })
    }

    pub fn compositor_membership(&self) -> &OwnedFd {
        &self.compositor_membership
    }

    pub fn retire(&mut self, deadline: Instant) -> Result<(), Error> {
        if self.retired {
            return Ok(());
        }
        self.workload.retire(deadline)?;
        self.retired = true;
        Ok(())
    }

    pub fn remove_empty_workload(&self) -> Result<(), Error> {
        if !self.retired || !self.workload.is_empty()? {
            return Err(Error::Protocol(
                "display workload has not completed retirement",
            ));
        }
        std::fs::remove_dir(&self.compositor_path)?;
        std::fs::remove_dir(&self.workload_path)?;
        Ok(())
    }

    pub fn restore_empty_login(&self) -> Result<(), Error> {
        let leader = open_directory(&self.leader)?;
        let parent = ProcessIdentity::parent()?;
        let current = ProcessIdentity::current()?;
        let mut expected = vec![parent.pid(), current.pid()];
        expected.sort_unstable();
        if members(leader.as_fd())? != expected {
            // Session children still belong to logind; never move or kill them by inference.
            return Err(Error::Protocol(
                "login children remain; logind must retain ownership of the login leaf",
            ));
        }
        let anchor = self.anchor.open()?;
        write_file(
            anchor.as_fd(),
            "cgroup.subtree_control",
            b"-cpu -memory -pids",
        )?;
        for process in [&parent, &current] {
            process.assert_current()?;
            write_file(
                anchor.as_fd(),
                "cgroup.procs",
                process.pid().to_string().as_bytes(),
            )?;
        }
        std::fs::remove_dir(&self.leader)?;
        Ok(())
    }
}

struct Preparation<'a> {
    anchor: &'a SessionGroup,
    parent: &'a ProcessIdentity,
    current: &'a ProcessIdentity,
    leader: &'a std::path::Path,
    workload: &'a std::path::Path,
    compositor: &'a std::path::Path,
    leader_created: bool,
    parent_moved: bool,
    current_moved: bool,
    controllers_attempted: bool,
    workload_created: bool,
    compositor_created: bool,
}

impl Preparation<'_> {
    fn rollback(&self) -> Result<(), Error> {
        // No child is released until both controllers acknowledge the completed preparation.
        if self.compositor_created {
            std::fs::remove_dir(self.compositor)?;
        }
        if self.workload_created {
            std::fs::remove_dir(self.workload)?;
        }
        let anchor = self.anchor.open()?;
        if self.controllers_attempted {
            write_file(
                anchor.as_fd(),
                "cgroup.subtree_control",
                b"-cpu -memory -pids",
            )?;
        }
        for (process, moved) in [
            (self.parent, self.parent_moved),
            (self.current, self.current_moved),
        ] {
            if moved {
                process.assert_current()?;
                write_file(
                    anchor.as_fd(),
                    "cgroup.procs",
                    process.pid().to_string().as_bytes(),
                )?;
            }
        }
        if self.leader_created {
            std::fs::remove_dir(self.leader)?;
        }
        Ok(())
    }
}

impl Drop for Containment {
    fn drop(&mut self) {
        if !self.retired {
            if let Err(error) = self
                .workload
                .retire(Instant::now() + Duration::from_secs(5))
            {
                eprintln!("display workload emergency retirement failed: {error}");
            }
        }
    }
}

fn members(directory: std::os::fd::BorrowedFd<'_>) -> Result<Vec<i32>, Error> {
    let mut members = read_file(directory, "cgroup.procs", 65536)?
        .split_whitespace()
        .map(|value| value.parse::<i32>().map_err(|_| Error::Identity))
        .collect::<Result<Vec<_>, _>>()?;
    members.sort_unstable();
    Ok(members)
}

pub fn enter(descriptor: i32) -> Result<(), std::io::Error> {
    // Writing zero moves only the calling pre-exec child, never a supplied PID.
    if unsafe { libc::write(descriptor, c"0".as_ptr().cast(), 1) } != 1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

pub use claw_display_control::install::protected_executable;

pub fn account(uid: u32) -> Result<(u32, CString, PathBuf), Error> {
    let mut record: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found = std::ptr::null_mut();
    let mut buffer = vec![0; 64 * 1024];
    let result = unsafe {
        libc::getpwuid_r(
            uid,
            &mut record,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut found,
        )
    };
    if result != 0 || found.is_null() || record.pw_uid != uid {
        return Err(Error::Protocol(
            "authenticated login account is unavailable",
        ));
    }
    use std::ffi::CStr;
    use std::os::unix::ffi::OsStrExt;
    let name = unsafe { CStr::from_ptr(record.pw_name) }.to_owned();
    let home = PathBuf::from(std::ffi::OsStr::from_bytes(
        unsafe { CStr::from_ptr(record.pw_dir) }.to_bytes(),
    ));
    if !home.is_absolute() {
        return Err(Error::Protocol("login account home is not absolute"));
    }
    Ok((record.pw_gid, name, home))
}
