use std::io::Read;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};

use crate::transport::Error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessIdentity {
    pid: i32,
    uid: u32,
    gid: u32,
    start_ticks: u64,
}

impl ProcessIdentity {
    pub fn current() -> Result<Self, Error> {
        Self::read(unsafe { libc::getpid() })
    }

    pub fn parent() -> Result<Self, Error> {
        Self::read(unsafe { libc::getppid() })
    }

    pub fn unix_peer(fd: BorrowedFd<'_>) -> Result<Self, Error> {
        let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
        let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                fd.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut credentials as *mut libc::ucred).cast(),
                &mut length,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        Self::from_credentials(credentials)
    }

    pub fn child(child: &std::process::Child) -> Result<Self, Error> {
        let pid = i32::try_from(child.id()).map_err(|_| Error::Identity)?;
        Self::read(pid)
    }

    pub fn from_pidfd(fd: BorrowedFd<'_>) -> Result<Self, Error> {
        let text = read_bounded(&format!("/proc/self/fdinfo/{}", fd.as_raw_fd()), 4096)?;
        let pid = text
            .lines()
            .find_map(|line| line.strip_prefix("Pid:"))
            .and_then(|value| value.trim().parse::<i32>().ok())
            .filter(|pid| *pid > 0)
            .ok_or(Error::Identity)?;
        let identity = Self::read(pid)?;
        if pidfd_exited(fd)? {
            return Err(Error::Identity);
        }
        Ok(identity)
    }

    pub fn pidfd(&self) -> Result<OwnedFd, Error> {
        self.assert_current()?;
        let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, self.pid, 0) };
        if raw < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let fd = unsafe { OwnedFd::from_raw_fd(raw as i32) };
        if Self::from_pidfd(fd.as_fd())? != *self {
            return Err(Error::Identity);
        }
        Ok(fd)
    }

    pub fn pid(&self) -> i32 {
        self.pid
    }

    pub fn uid(&self) -> u32 {
        self.uid
    }

    pub fn gid(&self) -> u32 {
        self.gid
    }

    pub fn start_ticks(&self) -> u64 {
        self.start_ticks
    }

    pub fn assert_current(&self) -> Result<(), Error> {
        if Self::read(self.pid)? == *self {
            Ok(())
        } else {
            Err(Error::Identity)
        }
    }

    pub fn in_host_pid_namespace(&self) -> Result<bool, Error> {
        self.assert_current()?;
        let status = read_bounded(&format!("/proc/{}/status", self.pid), 16384)?;
        let ids = status
            .lines()
            .find_map(|line| line.strip_prefix("NSpid:"))
            .ok_or(Error::Protocol(
                "kernel PID namespace identity is unavailable",
            ))?
            .split_whitespace()
            .map(|value| value.parse::<i32>().map_err(|_| Error::Identity))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ids.as_slice() == [self.pid])
    }

    pub(crate) fn from_credentials(credentials: libc::ucred) -> Result<Self, Error> {
        let identity = Self::read(credentials.pid)?;
        if identity.uid != credentials.uid || identity.gid != credentials.gid {
            return Err(Error::Identity);
        }
        Ok(identity)
    }

    fn read(pid: i32) -> Result<Self, Error> {
        if pid <= 0 {
            return Err(Error::Identity);
        }
        let stat = read_bounded(&format!("/proc/{pid}/stat"), 8192)?;
        let tail = stat.rsplit_once(')').ok_or(Error::Identity)?.1;
        if matches!(tail.split_whitespace().next(), Some("Z" | "X")) {
            return Err(Error::Identity);
        }
        let start_ticks = tail
            .split_whitespace()
            .nth(19)
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or(Error::Identity)?;
        let status = read_bounded(&format!("/proc/{pid}/status"), 16384)?;
        let ids = |prefix: &str| -> Result<u32, Error> {
            let values: Vec<u32> = status
                .lines()
                .find_map(|line| line.strip_prefix(prefix))
                .ok_or(Error::Identity)?
                .split_whitespace()
                .map(|value| value.parse::<u32>().map_err(|_| Error::Identity))
                .collect::<Result<_, _>>()?;
            if values.len() != 4 || values.iter().any(|value| *value != values[0]) {
                return Err(Error::Identity);
            }
            Ok(values[0])
        };
        Ok(Self {
            pid,
            uid: ids("Uid:")?,
            gid: ids("Gid:")?,
            start_ticks,
        })
    }
}

pub fn pidfd_exited(fd: BorrowedFd<'_>) -> Result<bool, Error> {
    let mut poll = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let result = unsafe { libc::poll(&mut poll, 1, 0) };
    if result < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    if poll.revents & libc::POLLNVAL != 0 {
        return Err(Error::Identity);
    }
    Ok(poll.revents & (libc::POLLIN | libc::POLLHUP) != 0)
}

pub fn unix_stream(descriptor: OwnedFd) -> Result<std::os::unix::net::UnixStream, Error> {
    for (option, expected) in [
        (libc::SO_TYPE, libc::SOCK_STREAM),
        (libc::SO_DOMAIN, libc::AF_UNIX),
    ] {
        let mut value = 0_i32;
        let mut length = std::mem::size_of::<i32>() as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                descriptor.as_raw_fd(),
                libc::SOL_SOCKET,
                option,
                std::ptr::addr_of_mut!(value).cast(),
                &mut length,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        if value != expected || length as usize != std::mem::size_of::<i32>() {
            return Err(Error::Protocol(
                "GUI creator descriptor is not a Unix stream",
            ));
        }
    }
    Ok(std::os::unix::net::UnixStream::from(descriptor))
}

/// Kernel audit identity established by the login PAM stack, not PAM environment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KernelLogin {
    pub owner_uid: u32,
    pub session_id: u32,
}

impl KernelLogin {
    pub fn of(process: &ProcessIdentity) -> Result<Self, Error> {
        process.assert_current()?;
        let read = |name: &str| -> Result<u32, Error> {
            let value = read_bounded(&format!("/proc/{}/{name}", process.pid), 32)?
                .trim()
                .parse::<u32>()
                .map_err(|_| Error::Identity)?;
            if value == u32::MAX {
                return Err(Error::UnauthenticatedLogin);
            }
            Ok(value)
        };
        let identity = Self {
            owner_uid: read("loginuid")?,
            session_id: read("sessionid")?,
        };
        if identity.owner_uid == 0 {
            return Err(Error::UnauthenticatedLogin);
        }
        process.assert_current()?;
        Ok(identity)
    }
}

pub fn read_bounded(path: &str, maximum: usize) -> Result<String, Error> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(maximum as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return Err(Error::Protocol("kernel record exceeds its bound"));
    }
    String::from_utf8(bytes).map_err(|_| Error::Identity)
}
