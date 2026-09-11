use std::io;
use std::mem::{size_of, zeroed};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::time::Instant;

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::ProcessIdentity;

pub const MAX_FRAME_BYTES: usize = 128 * 1024;
pub const MAX_DESCRIPTORS: usize = 3;
const MAGIC: &[u8; 4] = b"CDS1";

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Protocol(&'static str),
    Identity,
    UnauthenticatedLogin,
    Timeout,
    Closed,
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "display control I/O: {error}"),
            Self::Protocol(reason) => write!(formatter, "display control protocol: {reason}"),
            Self::Identity => {
                formatter.write_str("display control peer identity is invalid or stale")
            }
            Self::UnauthenticatedLogin => {
                formatter.write_str("display activation requires an authenticated kernel login")
            }
            Self::Timeout => formatter.write_str("display control deadline exceeded"),
            Self::Closed => formatter.write_str("display control connection closed"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub trait Packet: Serialize + DeserializeOwned {
    fn descriptor_count(&self) -> usize;
    fn validate(&self) -> Result<(), Error> {
        Ok(())
    }
}

pub struct Received<T> {
    pub message: T,
    pub descriptors: Vec<OwnedFd>,
    pub sender: ProcessIdentity,
}

#[derive(Debug)]
pub struct Connection {
    fd: OwnedFd,
}

#[repr(C, align(16))]
struct Ancillary([u8; 256]);

impl Connection {
    /// Take a fixed descriptor supplied by the trusted parent before starting threads.
    ///
    /// # Safety
    /// The caller must own this descriptor exclusively and must not close it again.
    pub unsafe fn inherit(raw: i32) -> Result<Self, Error> {
        if raw < 3 || unsafe { libc::fcntl(raw, libc::F_GETFD) } < 0 {
            return Err(Error::Protocol(
                "required inherited control descriptor is absent",
            ));
        }
        Self::from_owned(unsafe { OwnedFd::from_raw_fd(raw) })
    }

    pub fn readable(&self) -> Result<bool, Error> {
        let mut descriptor = libc::pollfd {
            fd: self.fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let result = unsafe { libc::poll(&mut descriptor, 1, 0) };
        if result < 0 {
            return Err(io::Error::last_os_error().into());
        }
        if descriptor.revents & libc::POLLNVAL != 0 {
            return Err(Error::Protocol("closed control descriptor"));
        }
        Ok(result != 0)
    }

    pub fn pair() -> Result<(Self, Self), Error> {
        let mut descriptors = [-1; 2];
        if unsafe {
            libc::socketpair(
                libc::AF_UNIX,
                libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
                0,
                descriptors.as_mut_ptr(),
            )
        } != 0
        {
            return Err(io::Error::last_os_error().into());
        }
        let first = unsafe { OwnedFd::from_raw_fd(descriptors[0]) };
        let second = unsafe { OwnedFd::from_raw_fd(descriptors[1]) };
        Ok((Self::from_owned(first)?, Self::from_owned(second)?))
    }

    pub fn from_owned(fd: OwnedFd) -> Result<Self, Error> {
        let mut kind: libc::c_int = 0;
        let mut length = size_of::<libc::c_int>() as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                fd.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_TYPE,
                (&mut kind as *mut libc::c_int).cast(),
                &mut length,
            )
        } != 0
        {
            return Err(io::Error::last_os_error().into());
        }
        if kind != libc::SOCK_SEQPACKET {
            return Err(Error::Protocol("control descriptor is not SOCK_SEQPACKET"));
        }
        let mut address: libc::sockaddr_storage = unsafe { zeroed() };
        let mut length = size_of::<libc::sockaddr_storage>() as libc::socklen_t;
        if unsafe {
            libc::getsockname(
                fd.as_raw_fd(),
                (&mut address as *mut libc::sockaddr_storage).cast(),
                &mut length,
            )
        } != 0
        {
            return Err(io::Error::last_os_error().into());
        }
        if address.ss_family as i32 != libc::AF_UNIX {
            return Err(Error::Protocol("control descriptor is not AF_UNIX"));
        }
        let enabled: libc::c_int = 1;
        if unsafe {
            libc::setsockopt(
                fd.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PASSCRED,
                (&enabled as *const libc::c_int).cast(),
                size_of::<libc::c_int>() as libc::socklen_t,
            )
        } != 0
        {
            return Err(io::Error::last_os_error().into());
        }
        if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(Self { fd })
    }

    pub fn peer(&self) -> Result<ProcessIdentity, Error> {
        let mut credentials: libc::ucred = unsafe { zeroed() };
        let mut length = size_of::<libc::ucred>() as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                self.fd.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut credentials as *mut libc::ucred).cast(),
                &mut length,
            )
        } != 0
        {
            return Err(io::Error::last_os_error().into());
        }
        ProcessIdentity::from_credentials(credentials)
    }

    pub fn into_fd(self) -> OwnedFd {
        self.fd
    }

    pub fn send<T: Packet>(
        &self,
        message: &T,
        descriptors: &[BorrowedFd<'_>],
        deadline: Instant,
    ) -> Result<(), Error> {
        message.validate()?;
        if descriptors.len() != message.descriptor_count() || descriptors.len() > MAX_DESCRIPTORS {
            return Err(Error::Protocol("incorrect descriptor count"));
        }
        let mut bytes = MAGIC.to_vec();
        serde_json::to_writer(&mut bytes, message)
            .map_err(|_| Error::Protocol("cannot encode control packet"))?;
        if bytes.len() > MAX_FRAME_BYTES {
            return Err(Error::Protocol("control packet exceeds its bound"));
        }
        let mut ancillary = Ancillary([0; 256]);
        let mut vector = libc::iovec {
            iov_base: bytes.as_mut_ptr().cast(),
            iov_len: bytes.len(),
        };
        let mut header: libc::msghdr = unsafe { zeroed() };
        header.msg_iov = &mut vector;
        header.msg_iovlen = 1;
        if !descriptors.is_empty() {
            let payload = descriptors.len() * size_of::<i32>();
            header.msg_control = ancillary.0.as_mut_ptr().cast();
            header.msg_controllen = unsafe { libc::CMSG_SPACE(payload as u32) } as usize;
            unsafe {
                let cmsg = libc::CMSG_FIRSTHDR(&header);
                (*cmsg).cmsg_level = libc::SOL_SOCKET;
                (*cmsg).cmsg_type = libc::SCM_RIGHTS;
                (*cmsg).cmsg_len = libc::CMSG_LEN(payload as u32) as usize;
                let output = libc::CMSG_DATA(cmsg).cast::<i32>();
                for (index, fd) in descriptors.iter().enumerate() {
                    output.add(index).write(fd.as_raw_fd());
                }
            }
        }
        loop {
            wait(self.as_fd(), libc::POLLOUT, deadline)?;
            let written = unsafe {
                libc::sendmsg(
                    self.fd.as_raw_fd(),
                    &header,
                    libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT,
                )
            };
            if written == bytes.len() as isize {
                return Ok(());
            }
            if written >= 0 {
                return Err(Error::Protocol("partial SOCK_SEQPACKET write"));
            }
            let error = io::Error::last_os_error();
            if !matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) {
                return Err(error.into());
            }
        }
    }

    pub fn receive<T: Packet>(
        &self,
        expected: &ProcessIdentity,
        deadline: Instant,
    ) -> Result<Received<T>, Error> {
        expected.assert_current()?;
        let received = self.receive_first::<T>(expected.uid(), deadline)?;
        if received.sender != *expected {
            return Err(Error::Identity);
        }
        expected.assert_current()?;
        Ok(received)
    }

    pub fn receive_first<T: Packet>(
        &self,
        required_uid: u32,
        deadline: Instant,
    ) -> Result<Received<T>, Error> {
        let mut bytes = vec![0; MAX_FRAME_BYTES];
        loop {
            wait(self.as_fd(), libc::POLLIN, deadline)?;
            let mut ancillary = Ancillary([0; 256]);
            let mut vector = libc::iovec {
                iov_base: bytes.as_mut_ptr().cast(),
                iov_len: bytes.len(),
            };
            let mut header: libc::msghdr = unsafe { zeroed() };
            header.msg_iov = &mut vector;
            header.msg_iovlen = 1;
            header.msg_control = ancillary.0.as_mut_ptr().cast();
            header.msg_controllen = ancillary.0.len();
            let read = unsafe {
                libc::recvmsg(
                    self.fd.as_raw_fd(),
                    &mut header,
                    libc::MSG_CMSG_CLOEXEC | libc::MSG_DONTWAIT,
                )
            };
            if read < 0 {
                let error = io::Error::last_os_error();
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) {
                    continue;
                }
                return Err(error.into());
            }
            let (credentials, descriptors) = collect_ancillary(&header)?;
            if header.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0 {
                return Err(Error::Protocol(
                    "truncated control packet or ancillary data",
                ));
            }
            if read == 0 {
                return Err(Error::Closed);
            }
            let credentials = credentials.ok_or(Error::Identity)?;
            if credentials.uid != required_uid {
                return Err(Error::Identity);
            }
            let sender = ProcessIdentity::from_credentials(credentials)?;
            let bytes = &bytes[..read as usize];
            if !bytes.starts_with(MAGIC) {
                return Err(Error::Protocol("invalid control magic/version"));
            }
            let message: T = serde_json::from_slice(&bytes[MAGIC.len()..])
                .map_err(|_| Error::Protocol("invalid or undeclared control fields"))?;
            message.validate()?;
            if descriptors.len() != message.descriptor_count() {
                return Err(Error::Protocol("incorrect descriptor count"));
            }
            return Ok(Received {
                message,
                descriptors,
                sender,
            });
        }
    }
}

impl AsFd for Connection {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

fn collect_ancillary(header: &libc::msghdr) -> Result<(Option<libc::ucred>, Vec<OwnedFd>), Error> {
    let mut credentials = None;
    let mut descriptors = Vec::new();
    let mut invalid = false;
    unsafe {
        let mut cmsg = libc::CMSG_FIRSTHDR(header);
        while !cmsg.is_null() {
            let minimum = libc::CMSG_LEN(0) as usize;
            let end = (header.msg_control as usize).saturating_add(header.msg_controllen);
            if (cmsg as usize).saturating_add(minimum) > end
                || (*cmsg).cmsg_len < minimum
                || (cmsg as usize).saturating_add((*cmsg).cmsg_len) > end
            {
                invalid = true;
                break;
            }
            let payload = (*cmsg).cmsg_len - minimum;
            match ((*cmsg).cmsg_level, (*cmsg).cmsg_type) {
                (libc::SOL_SOCKET, libc::SCM_RIGHTS) => {
                    if !payload.is_multiple_of(size_of::<i32>()) {
                        invalid = true;
                    } else {
                        let input = libc::CMSG_DATA(cmsg).cast::<i32>();
                        for index in 0..payload / size_of::<i32>() {
                            descriptors
                                .push(OwnedFd::from_raw_fd(input.add(index).read_unaligned()));
                        }
                    }
                }
                (libc::SOL_SOCKET, libc::SCM_CREDENTIALS) => {
                    if payload != size_of::<libc::ucred>() || credentials.is_some() {
                        invalid = true;
                    } else {
                        credentials =
                            Some(libc::CMSG_DATA(cmsg).cast::<libc::ucred>().read_unaligned());
                    }
                }
                _ => invalid = true,
            }
            cmsg = libc::CMSG_NXTHDR(header, cmsg);
        }
    }
    if invalid || descriptors.len() > MAX_DESCRIPTORS {
        return Err(Error::Protocol("invalid ancillary data"));
    }
    Ok((credentials, descriptors))
}

fn wait(fd: BorrowedFd<'_>, events: i16, deadline: Instant) -> Result<(), Error> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(Error::Timeout);
        }
        let timeout = remaining
            .as_millis()
            .saturating_add(1)
            .min(i32::MAX as u128) as i32;
        let mut descriptor = libc::pollfd {
            fd: fd.as_raw_fd(),
            events,
            revents: 0,
        };
        let result = unsafe { libc::poll(&mut descriptor, 1, timeout) };
        if result > 0 {
            if descriptor.revents & libc::POLLNVAL != 0 {
                return Err(Error::Protocol("closed control descriptor"));
            }
            return Ok(());
        }
        if result == 0 {
            continue;
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error.into());
        }
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/transport.rs"
    ));
}
