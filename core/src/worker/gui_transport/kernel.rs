use std::io;
use std::mem::{size_of, zeroed};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};

use claw_display_control::ProcessIdentity;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct Data {
    pub number: i32,
    pub architecture: u32,
    instruction_pointer: u64,
    pub args: [u64; 6],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct Notice {
    pub id: u64,
    pub tid: u32,
    flags: u32,
    pub data: Data,
}

#[repr(C)]
#[derive(Default)]
struct Response {
    id: u64,
    value: i64,
    error: i32,
    flags: u32,
}

#[repr(C)]
#[derive(Default)]
struct Sizes {
    notice: u16,
    response: u16,
    data: u16,
}

const RECV: libc::c_ulong = 0xc000_2100 | (size_of::<Notice>() as libc::c_ulong) << 16;
const SEND: libc::c_ulong = 0xc000_2101 | (size_of::<Response>() as libc::c_ulong) << 16;
const VALID: libc::c_ulong = 0x4008_2102;

pub(crate) struct Listener {
    descriptor: OwnedFd,
}

pub(crate) enum Notification {
    Idle,
    Request(Notice),
    Ended,
}

pub(crate) struct Subject {
    pub identity: ProcessIdentity,
    pidfd: OwnedFd,
    tid: u32,
}

impl Listener {
    pub fn receive(descriptor: OwnedFd) -> io::Result<Self> {
        let mut sizes = Sizes::default();
        if unsafe { libc::syscall(libc::SYS_seccomp, 3_u32, 0_u32, &mut sizes) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if usize::from(sizes.notice) != size_of::<Notice>()
            || usize::from(sizes.response) != size_of::<Response>()
            || usize::from(sizes.data) != size_of::<Data>()
        {
            return Err(io::Error::other("unsupported GUI syscall notification ABI"));
        }
        let listener = Self { descriptor };
        match listener.valid(0) {
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => Ok(listener),
            Err(error) => Err(error),
            Ok(()) => Err(io::Error::other(
                "GUI syscall listener has an invalid notification identity",
            )),
        }
    }

    pub fn next(&self) -> io::Result<Notification> {
        let mut poll = libc::pollfd {
            fd: self.descriptor.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let result = unsafe { libc::poll(&mut poll, 1, 50) };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        if result == 0 {
            return Ok(Notification::Idle);
        }
        // Kernel HUP means no task retains the filter, not a client-declared exit.
        if poll.revents & libc::POLLHUP != 0 {
            return Ok(Notification::Ended);
        }
        if poll.revents & libc::POLLIN == 0 {
            return Err(io::Error::other("GUI syscall listener poll failed"));
        }
        let mut notice = Notice::default();
        if unsafe { libc::ioctl(self.descriptor.as_raw_fd(), RECV, &mut notice) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if notice.tid == 0 || notice.flags != 0 {
            return Err(io::Error::other("invalid kernel GUI syscall notification"));
        }
        Ok(Notification::Request(notice))
    }

    pub fn valid(&self, id: u64) -> io::Result<()> {
        if unsafe { libc::ioctl(self.descriptor.as_raw_fd(), VALID, &id) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn reply(&self, id: u64, result: Result<(), i32>) -> io::Result<()> {
        let response = Response {
            id,
            value: 0,
            error: result.err().map_or(0, |errno| -errno),
            flags: 0,
        };
        if unsafe { libc::ioctl(self.descriptor.as_raw_fd(), SEND, &response) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn subject(&self, notice: &Notice) -> io::Result<Subject> {
        self.valid(notice.id)?;
        let status = claw_display_control::identity::read_bounded(
            &format!("/proc/{}/status", notice.tid),
            16384,
        )
        .map_err(io::Error::other)?;
        let tgid = status
            .lines()
            .find_map(|line| line.strip_prefix("Tgid:"))
            .and_then(|value| value.trim().parse::<i32>().ok())
            .filter(|pid| *pid > 0)
            .ok_or_else(|| {
                io::Error::other("GUI notifying thread has no kernel process identity")
            })?;
        let pidfd = open_pidfd(tgid)?;
        let identity = ProcessIdentity::from_pidfd(pidfd.as_fd()).map_err(io::Error::other)?;
        if notice.tid != tgid as u32 {
            // pidfd_getfd addresses the process leader on older supported kernels.
            // Refuse a different file table rather than duplicating the wrong FD.
            let comparison =
                unsafe { libc::syscall(libc::SYS_kcmp, notice.tid, tgid, 2_u32, 0_u64, 0_u64) };
            if comparison != 0 {
                return Err(if comparison < 0 {
                    io::Error::last_os_error()
                } else {
                    io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "GUI thread has a separate descriptor table",
                    )
                });
            }
        }
        self.valid(notice.id)?;
        Ok(Subject {
            identity,
            pidfd,
            tid: notice.tid,
        })
    }
}

impl Subject {
    pub fn descriptor(&self, number: u64) -> io::Result<OwnedFd> {
        let number =
            i32::try_from(number).map_err(|_| io::Error::from_raw_os_error(libc::EBADF))?;
        let result =
            unsafe { libc::syscall(libc::SYS_pidfd_getfd, self.pidfd.as_raw_fd(), number, 0_u32) };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { OwnedFd::from_raw_fd(result as i32) })
    }

    pub fn address(&self, pointer: u64, length: u64) -> io::Result<Vec<u8>> {
        let length = usize::try_from(length)
            .ok()
            .filter(|length| (3..=size_of::<libc::sockaddr_un>()).contains(length))
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?;
        let pointer = usize::try_from(pointer)
            .ok()
            .filter(|pointer| *pointer > 0)
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EFAULT))?;
        let mut bytes = vec![0_u8; length];
        let local = libc::iovec {
            iov_base: bytes.as_mut_ptr().cast(),
            iov_len: length,
        };
        let remote = libc::iovec {
            iov_base: pointer as *mut libc::c_void,
            iov_len: length,
        };
        let count = unsafe { libc::process_vm_readv(self.tid as i32, &local, 1, &remote, 1, 0) };
        if count < 0 {
            return Err(io::Error::last_os_error());
        }
        if count as usize != length {
            return Err(io::Error::from_raw_os_error(libc::EFAULT));
        }
        if u16::from_ne_bytes([bytes[0], bytes[1]]) != libc::AF_UNIX as u16 {
            return Err(io::Error::from_raw_os_error(libc::EAFNOSUPPORT));
        }
        Ok(bytes)
    }
}

pub(crate) fn sender(credentials: libc::ucred) -> io::Result<ProcessIdentity> {
    let pidfd = open_pidfd(credentials.pid)?;
    let identity = ProcessIdentity::from_pidfd(pidfd.as_fd()).map_err(io::Error::other)?;
    if identity.uid() != credentials.uid || identity.gid() != credentials.gid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "GUI sender identity changed",
        ));
    }
    Ok(identity)
}

pub(crate) fn descriptor_of(identity: &ProcessIdentity, number: i32) -> io::Result<OwnedFd> {
    let pidfd = identity.pidfd().map_err(io::Error::other)?;
    let raw = unsafe { libc::syscall(libc::SYS_pidfd_getfd, pidfd.as_raw_fd(), number, 0_u32) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    let descriptor = unsafe { OwnedFd::from_raw_fd(raw as i32) };
    identity.assert_current().map_err(io::Error::other)?;
    Ok(descriptor)
}

pub(crate) fn socket_cookie(descriptor: BorrowedFd<'_>) -> io::Result<u64> {
    let mut cookie = 0_u64;
    let mut length = size_of::<u64>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            descriptor.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_COOKIE,
            std::ptr::addr_of_mut!(cookie).cast(),
            &mut length,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    if length as usize != size_of::<u64>() || cookie == 0 {
        return Err(io::Error::other("GUI socket has no kernel cookie"));
    }
    Ok(cookie)
}

fn open_pidfd(pid: i32) -> io::Result<OwnedFd> {
    if pid <= 0 {
        return Err(io::Error::from_raw_os_error(libc::ESRCH));
    }
    let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0_u32) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(raw as i32) })
}

pub(crate) fn connect(descriptor: BorrowedFd<'_>, path: &std::path::Path) -> io::Result<()> {
    let bytes = path.as_os_str().as_encoded_bytes();
    let mut address: libc::sockaddr_un = unsafe { zeroed() };
    if bytes.is_empty() || bytes.len() >= address.sun_path.len() || bytes.contains(&0) {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (target, source) in address.sun_path.iter_mut().zip(bytes) {
        *target = *source as libc::c_char;
    }
    if unsafe {
        libc::connect(
            descriptor.as_raw_fd(),
            std::ptr::addr_of!(address).cast(),
            (size_of::<libc::sa_family_t>() + bytes.len() + 1) as libc::socklen_t,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
