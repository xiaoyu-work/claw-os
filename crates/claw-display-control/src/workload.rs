//! Descriptor-pinned cgroup retirement shared by the Root supervisor and PAM.

use std::ffi::CString;
use std::io::{Read, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::{Error, ProcessIdentity};

const CGROUP2_SUPER_MAGIC: libc::c_long = 0x63677270;

/// The session cgroup as reported for a kernel-authenticated process.
#[derive(Clone, Debug)]
pub struct SessionGroup {
    path: PathBuf,
    identity: (u64, u64),
}

impl SessionGroup {
    pub fn of(process: &ProcessIdentity) -> Result<Self, Error> {
        process.assert_current()?;
        let record =
            crate::identity::read_bounded(&format!("/proc/{}/cgroup", process.pid()), 8192)?;
        let relative = record
            .lines()
            .find_map(|line| line.strip_prefix("0::"))
            .ok_or(Error::Protocol(
                "cgroup v2 is required for display activation",
            ))?;
        let relative = relative.trim_start_matches('/');
        if relative.is_empty()
            || relative
                .split('/')
                .any(|part| part.is_empty() || matches!(part, "." | ".."))
        {
            return Err(Error::Protocol(
                "display activation requires a non-root session cgroup",
            ));
        }
        let path = Path::new("/sys/fs/cgroup").join(relative);
        let descriptor = open_directory(&path)?;
        check_cgroup(descriptor.as_fd())?;
        let metadata = std::fs::File::from(descriptor).metadata()?;
        process.assert_current()?;
        Ok(Self {
            path,
            identity: (metadata.dev(), metadata.ino()),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn open(&self) -> Result<OwnedFd, Error> {
        let descriptor = open_directory(&self.path)?;
        check_cgroup(descriptor.as_fd())?;
        let metadata = std::fs::File::from(descriptor.try_clone()?).metadata()?;
        if (metadata.dev(), metadata.ino()) != self.identity {
            return Err(Error::Identity);
        }
        Ok(descriptor)
    }
}

#[derive(Debug)]
pub struct Workload {
    descriptor: OwnedFd,
}

#[derive(Debug)]
pub struct InstanceProcess {
    process: ProcessIdentity,
    pidfd: OwnedFd,
    group: SessionGroup,
    descriptor: OwnedFd,
}

impl InstanceProcess {
    pub fn receive(pidfd: OwnedFd, descriptor: OwnedFd) -> Result<Self, Error> {
        let process = ProcessIdentity::from_pidfd(pidfd.as_fd())?;
        if process.uid() == 0 {
            return Err(Error::Identity);
        }
        check_cgroup(descriptor.as_fd())?;
        let group = SessionGroup::of(&process)?;
        let expected = std::fs::File::from(group.open()?).metadata()?;
        let supplied = std::fs::File::from(descriptor.try_clone()?).metadata()?;
        if (expected.dev(), expected.ino()) != (supplied.dev(), supplied.ino()) {
            return Err(Error::Identity);
        }
        for field in ["loginuid", "sessionid"] {
            if crate::identity::read_bounded(&format!("/proc/{}/{field}", process.pid()), 32)?
                .trim()
                != "4294967295"
            {
                return Err(Error::Protocol(
                    "GUI workers must not inherit an ambient login identity",
                ));
            }
        }
        Ok(Self {
            process,
            pidfd,
            group,
            descriptor,
        })
    }

    pub fn active(&self) -> Result<bool, Error> {
        if crate::identity::pidfd_exited(self.pidfd.as_fd())? {
            return Ok(false);
        }
        self.process.assert_current()?;
        let current_group = SessionGroup::of(&self.process)?;
        if current_group.path() != self.group.path() {
            return Err(Error::Identity);
        }
        check_cgroup(self.descriptor.as_fd())?;
        Ok(true)
    }

    pub fn is_peer(&self, peer: &ProcessIdentity) -> Result<bool, Error> {
        if !self.active()? || peer.uid() != self.process.uid() {
            return Ok(false);
        }
        let group = SessionGroup::of(peer)?;
        Ok(group.path().starts_with(self.group.path()))
    }
}

impl Workload {
    pub fn receive(
        descriptor: OwnedFd,
        session: &SessionGroup,
        epoch: crate::Epoch,
    ) -> Result<Self, Error> {
        session.open()?;
        check_cgroup(descriptor.as_fd())?;
        let actual = std::fs::read_link(format!("/proc/self/fd/{}", descriptor.as_raw_fd()))?;
        let expected = session.path.join(format!("claw-display-{}", epoch.label()));
        if actual != expected {
            return Err(Error::Protocol(
                "workload is not the authenticated session's cgroup",
            ));
        }
        let workload = Self { descriptor };
        // A real, readable populated record is required, never a regular-file fixture.
        workload.is_empty()?;
        Ok(workload)
    }

    pub fn duplicate(&self) -> Result<OwnedFd, Error> {
        Ok(self.descriptor.try_clone()?)
    }

    pub fn is_empty(&self) -> Result<bool, Error> {
        cgroup_is_empty(self.as_fd())
    }

    pub fn retire(&self, deadline: Instant) -> Result<(), Error> {
        retire_cgroup(self.as_fd(), deadline)
    }
}

pub fn cgroup_is_empty(descriptor: BorrowedFd<'_>) -> Result<bool, Error> {
    let events = read_file(descriptor, "cgroup.events", 4096)?;
    let populated = events.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        (fields.next() == Some("populated"))
            .then(|| fields.next())
            .flatten()
    });
    match populated {
        Some("0") => Ok(read_file(descriptor, "cgroup.procs", 65536)?
            .trim()
            .is_empty()),
        Some("1") => Ok(false),
        _ => Err(Error::Protocol("invalid cgroup populated record")),
    }
}

pub fn retire_cgroup(descriptor: BorrowedFd<'_>, deadline: Instant) -> Result<(), Error> {
    write_file(descriptor, "cgroup.kill", b"1")?;
    loop {
        if cgroup_is_empty(descriptor)? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Error::Protocol(
                "workload remained populated after cgroup.kill",
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

impl AsFd for Workload {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.descriptor.as_fd()
    }
}

pub fn open_directory(path: &Path) -> Result<OwnedFd, Error> {
    let name = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| Error::Protocol("invalid cgroup directory"))?;
    let raw = unsafe {
        libc::open(
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if raw < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

pub fn check_cgroup(fd: BorrowedFd<'_>) -> Result<(), Error> {
    let mut filesystem: libc::statfs = unsafe { std::mem::zeroed() };
    let mut metadata: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstatfs(fd.as_raw_fd(), &mut filesystem) } != 0
        || unsafe { libc::fstat(fd.as_raw_fd(), &mut metadata) } != 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    if filesystem.f_type != CGROUP2_SUPER_MAGIC
        || metadata.st_mode & libc::S_IFMT != libc::S_IFDIR
        || metadata.st_uid != 0
        || metadata.st_mode & 0o022 != 0
    {
        return Err(Error::Protocol(
            "a protected Root-owned cgroup v2 descriptor is required",
        ));
    }
    Ok(())
}

pub fn read_file(parent: BorrowedFd<'_>, name: &str, maximum: usize) -> Result<String, Error> {
    let mut bytes = Vec::new();
    open_file(parent, name, libc::O_RDONLY)?
        .take(maximum as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return Err(Error::Protocol("cgroup record exceeds its bound"));
    }
    String::from_utf8(bytes).map_err(|_| Error::Protocol("invalid cgroup record"))
}

pub fn write_file(parent: BorrowedFd<'_>, name: &str, value: &[u8]) -> Result<(), Error> {
    open_file(parent, name, libc::O_WRONLY)?.write_all(value)?;
    Ok(())
}

fn open_file(parent: BorrowedFd<'_>, name: &str, flags: i32) -> Result<std::fs::File, Error> {
    if name.contains('/') {
        return Err(Error::Protocol("cgroup control must be a single name"));
    }
    let name = CString::new(name).map_err(|_| Error::Protocol("invalid cgroup control"))?;
    let raw = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if raw < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(unsafe { std::fs::File::from_raw_fd(raw) })
}
