use std::io;
use std::mem::{size_of, zeroed};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::time::Instant;

pub(crate) const READY: [u8; 4] = *b"CG1R";
pub(crate) const INPUT: [u8; 4] = *b"CG1I";

pub(crate) struct Packet {
    pub descriptor: OwnedFd,
    pub credentials: libc::ucred,
}

pub(crate) fn pair() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [-1_i32; 2];
    if unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
            0,
            fds.as_mut_ptr(),
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    let first = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    let second = unsafe { OwnedFd::from_raw_fd(fds[1]) };
    validate_channel(first.as_fd())?;
    validate_channel(second.as_fd())?;
    Ok((first, second))
}

pub(crate) fn validate_channel(fd: BorrowedFd<'_>) -> io::Result<()> {
    for (option, expected) in [
        (libc::SO_TYPE, libc::SOCK_SEQPACKET),
        (libc::SO_DOMAIN, libc::AF_UNIX),
    ] {
        let mut value = 0_i32;
        let mut length = size_of::<i32>() as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                fd.as_raw_fd(),
                libc::SOL_SOCKET,
                option,
                std::ptr::addr_of_mut!(value).cast(),
                &mut length,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        if value != expected || length as usize != size_of::<i32>() {
            return Err(io::Error::other(
                "GUI bootstrap requires its private Unix packet socket",
            ));
        }
    }
    let enable = 1_i32;
    if unsafe {
        libc::setsockopt(
            fd.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PASSCRED,
            std::ptr::addr_of!(enable).cast(),
            size_of::<i32>() as libc::socklen_t,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(crate) fn peer(fd: BorrowedFd<'_>) -> io::Result<libc::ucred> {
    let mut credentials: libc::ucred = unsafe { zeroed() };
    let mut length = size_of::<libc::ucred>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            fd.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            std::ptr::addr_of_mut!(credentials).cast(),
            &mut length,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    if length as usize != size_of::<libc::ucred>() {
        return Err(io::Error::other(
            "GUI bootstrap peer credentials are incomplete",
        ));
    }
    Ok(credentials)
}

fn ready(fd: BorrowedFd<'_>, event: i16, deadline: Instant) -> io::Result<()> {
    loop {
        let duration = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::TimedOut, "GUI bootstrap deadline expired")
            })?;
        let mut poll = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: event,
            revents: 0,
        };
        let milliseconds = i32::try_from(duration.as_millis().max(1)).unwrap_or(i32::MAX);
        let result = unsafe { libc::poll(&mut poll, 1, milliseconds) };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if result > 0 && poll.revents & event != 0 {
            return Ok(());
        }
        if result > 0 {
            return Err(io::Error::other("GUI bootstrap endpoint closed"));
        }
    }
}

pub(crate) fn send(
    socket: BorrowedFd<'_>,
    tag: [u8; 4],
    descriptor: BorrowedFd<'_>,
    deadline: Instant,
) -> io::Result<()> {
    let mut control = [0_usize; 8];
    let mut vector = libc::iovec {
        iov_base: tag.as_ptr().cast_mut().cast(),
        iov_len: tag.len(),
    };
    let mut message: libc::msghdr = unsafe { zeroed() };
    message.msg_iov = &mut vector;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = unsafe { libc::CMSG_SPACE(size_of::<i32>() as u32) } as usize;
    unsafe {
        let header = libc::CMSG_FIRSTHDR(&message);
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len = libc::CMSG_LEN(size_of::<i32>() as u32) as usize;
        std::ptr::write_unaligned(
            libc::CMSG_DATA(header).cast::<i32>(),
            descriptor.as_raw_fd(),
        );
    }
    loop {
        ready(socket, libc::POLLOUT, deadline)?;
        let result = unsafe {
            libc::sendmsg(
                socket.as_raw_fd(),
                &message,
                libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
            )
        };
        if result == tag.len() as isize {
            return Ok(());
        }
        if result >= 0 {
            return Err(io::Error::other("GUI bootstrap packet was incomplete"));
        }
        let error = io::Error::last_os_error();
        if !matches!(
            error.kind(),
            io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
        ) {
            return Err(error);
        }
    }
}

pub(crate) fn receive(
    socket: BorrowedFd<'_>,
    tag: [u8; 4],
    deadline: Instant,
) -> io::Result<Packet> {
    let mut bytes = [0_u8; 5];
    let mut control = [0_usize; 16];
    loop {
        ready(socket, libc::POLLIN, deadline)?;
        let mut vector = libc::iovec {
            iov_base: bytes.as_mut_ptr().cast(),
            iov_len: bytes.len(),
        };
        let mut message: libc::msghdr = unsafe { zeroed() };
        message.msg_iov = &mut vector;
        message.msg_iovlen = 1;
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = size_of_val(&control);
        let count = unsafe {
            libc::recvmsg(
                socket.as_raw_fd(),
                &mut message,
                libc::MSG_DONTWAIT | libc::MSG_CMSG_CLOEXEC,
            )
        };
        if count < 0 {
            let error = io::Error::last_os_error();
            if matches!(
                error.kind(),
                io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
            ) {
                continue;
            }
            return Err(error);
        }
        let mut descriptors = Vec::new();
        let mut credentials = None;
        let mut invalid = message.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0;
        unsafe {
            let mut header = libc::CMSG_FIRSTHDR(&message);
            while !header.is_null() {
                let length = (*header)
                    .cmsg_len
                    .saturating_sub(libc::CMSG_LEN(0) as usize);
                let data = libc::CMSG_DATA(header);
                match ((*header).cmsg_level, (*header).cmsg_type) {
                    (libc::SOL_SOCKET, libc::SCM_RIGHTS) => {
                        invalid |= !length.is_multiple_of(size_of::<i32>());
                        for offset in
                            (0..length / size_of::<i32>()).map(|index| index * size_of::<i32>())
                        {
                            let raw = std::ptr::read_unaligned(data.add(offset).cast::<i32>());
                            descriptors.push(OwnedFd::from_raw_fd(raw));
                        }
                    }
                    (libc::SOL_SOCKET, libc::SCM_CREDENTIALS) => {
                        if length == size_of::<libc::ucred>() && credentials.is_none() {
                            credentials =
                                Some(std::ptr::read_unaligned(data.cast::<libc::ucred>()));
                        } else {
                            invalid = true;
                        }
                    }
                    _ => invalid = true,
                }
                header = libc::CMSG_NXTHDR(&message, header);
            }
        }
        if invalid || count != 4 || bytes[..4] != tag || descriptors.len() != 1 {
            return Err(io::Error::other(
                "invalid GUI bootstrap frame or descriptor count",
            ));
        }
        return Ok(Packet {
            descriptor: descriptors
                .pop()
                .ok_or_else(|| io::Error::other("missing GUI descriptor"))?,
            credentials: credentials
                .ok_or_else(|| io::Error::other("GUI bootstrap has no kernel sender"))?,
        });
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/worker/gui_transport/bootstrap.rs"
    ));
}
