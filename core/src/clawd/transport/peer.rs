//! Per-message peer credentials, and the deliberate refusal of Unix
//! descriptor passing.
//!
//! # Why not `SO_PEERCRED`
//!
//! `SO_PEERCRED` answers "who called `connect(2)`", once, at connection
//! establishment, and for a `socketpair(2)` it answers "who called
//! `socketpair`" — which for a pre-fork channel is the parent, not the
//! process now writing. It cannot say who sent *this* message. A peer
//! that hands its connected descriptor to another process keeps the
//! original answer.
//!
//! # What this module uses instead
//!
//! `SO_PASSCRED` on the listening socket. On Linux the flag is
//! inherited by every socket `accept(2)` returns
//! (`unix_sock_inherit_flags` in `net/unix/af_unix.c`), and the sending
//! side stamps `struct ucred` onto each `sk_buff` at `sendmsg(2)` time
//! from the *sending task's* real uid/gid and thread-group id
//! (`maybe_add_creds`). The stamp is also applied while the connection
//! is still on the accept queue, so bytes written before `accept`
//! returns are covered too. A sender may name credentials explicitly,
//! but the kernel only accepts its own pid unless it holds
//! `CAP_SYS_ADMIN`, and a uid it already owns unless it holds
//! `CAP_SETUID` — so the values cannot be forged downward into another
//! user.
//!
//! A stream reader never merges bytes carrying different credentials
//! (`unix_skb_scm_eq`), so the `SCM_CREDENTIALS` control message that
//! arrives with a `recvmsg(2)` describes exactly the bytes that call
//! returned. The frame reader requires every segment of a frame to
//! carry the same credentials, so a descriptor handed to a second
//! process mid-frame is a protocol fault rather than an identity
//! upgrade.
//!
//! Credentials are translated into the receiver's namespaces. A sender
//! the daemon cannot see — another pid namespace — arrives as pid 0,
//! which this module treats as "no usable identity" and refuses.
//!
//! # Descriptors
//!
//! `clawd` accepts no descriptor from any peer. Ancillary data is
//! received deliberately (never with a null `msg_control`, which would
//! silently drop and leak passed descriptors into the daemon), every
//! `SCM_RIGHTS` descriptor is closed immediately, and the request is
//! refused. `MSG_CMSG_CLOEXEC` means a descriptor cannot survive into a
//! child spawned in the window before it is closed.

/// Credentials the kernel attached to one received segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Credentials {
    pub pid: u32,
    pub uid: u32,
    pub gid: u32,
}

/// What one `recvmsg(2)` produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    pub bytes: usize,
    pub credentials: Option<Credentials>,
    /// Descriptors the peer attached. Already closed; non-zero means
    /// the request is refused.
    pub descriptors: usize,
    /// The kernel could not fit the ancillary data. Undelivered
    /// descriptors are released by the kernel, but the peer is still
    /// refused: an ancillary payload we could not fully account for is
    /// not something to serve a privileged request on.
    pub control_truncated: bool,
}

/// The process the credentials point at, re-read from `/proc`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerProcess {
    pub pid: u32,
    pub uid: u32,
    pub gid: u32,
    /// Field 22 of `/proc/<pid>/stat`. Together with the pid this
    /// identifies one process across pid reuse.
    pub start_time_ticks: u64,
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{Credentials, PeerProcess, Segment};
    use std::mem::size_of;
    use std::os::unix::io::RawFd;

    /// Room for one `ucred` plus a run of descriptors. A peer that
    /// attaches more than fits gets `MSG_CTRUNC` and is refused; the
    /// kernel releases the descriptors it could not deliver.
    const CONTROL_BYTES: usize = 256;

    /// Ask the kernel to stamp sender credentials onto every message
    /// this socket receives.
    ///
    /// Set on the listener before the first `accept`, so there is no
    /// window in which a connection is served without them.
    pub fn enable_credential_passing(fd: RawFd) -> std::io::Result<()> {
        let enable: libc::c_int = 1;
        // SAFETY: `fd` is a socket owned by the caller for the duration
        // of this call, and the option value is one `c_int` whose
        // length is passed exactly.
        let rc = unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_PASSCRED,
                std::ptr::addr_of!(enable).cast::<libc::c_void>(),
                size_of::<libc::c_int>() as libc::socklen_t,
            )
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    /// Receive one segment together with its ancillary data.
    ///
    /// Never blocks: the caller drives readiness through the async
    /// runtime and retries on `WouldBlock`.
    pub fn recv_segment(fd: RawFd, buf: &mut [u8]) -> std::io::Result<Segment> {
        let mut control = [0u8; CONTROL_BYTES];
        let mut iov = libc::iovec {
            iov_base: buf.as_mut_ptr().cast::<libc::c_void>(),
            iov_len: buf.len(),
        };
        // SAFETY: `msghdr` is a C struct of scalars and pointers with
        // no invalid bit patterns; every field `recvmsg` reads is
        // assigned below.
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_iov = std::ptr::addr_of_mut!(iov);
        message.msg_iovlen = 1;
        message.msg_control = control.as_mut_ptr().cast::<libc::c_void>();
        message.msg_controllen = control.len() as _;

        // SAFETY: `fd` is owned by the caller, `iov` describes `buf`
        // for exactly `buf.len()` bytes, and `msg_control` describes
        // `control` for exactly `control.len()` bytes. Both live until
        // the call returns.
        let received = unsafe {
            libc::recvmsg(
                fd,
                std::ptr::addr_of_mut!(message),
                libc::MSG_CMSG_CLOEXEC | libc::MSG_DONTWAIT,
            )
        };
        if received < 0 {
            return Err(std::io::Error::last_os_error());
        }

        let mut segment = Segment {
            bytes: received as usize,
            credentials: None,
            descriptors: 0,
            control_truncated: message.msg_flags & libc::MSG_CTRUNC != 0,
        };
        // SAFETY: `message` was just filled by the kernel and its
        // control buffer is `control`, which is still alive and
        // unmodified. `collect_ancillary` only walks it with the
        // kernel's own CMSG accessors.
        unsafe { collect_ancillary(&message, &mut segment) };
        Ok(segment)
    }

    /// Walk the control buffer once: close every passed descriptor and
    /// record the sender credentials.
    ///
    /// # Safety
    ///
    /// `message` must be a `msghdr` a successful `recvmsg` just filled,
    /// with its control buffer still allocated and untouched.
    unsafe fn collect_ancillary(message: &libc::msghdr, segment: &mut Segment) {
        let header_bytes = libc::CMSG_LEN(0) as usize;
        let mut cmsg = libc::CMSG_FIRSTHDR(message);
        while !cmsg.is_null() {
            let entry = &*cmsg;
            let payload = (entry.cmsg_len as usize).saturating_sub(header_bytes);
            if entry.cmsg_level == libc::SOL_SOCKET {
                match entry.cmsg_type {
                    libc::SCM_RIGHTS => {
                        let data = libc::CMSG_DATA(cmsg).cast::<RawFd>();
                        let count = payload / size_of::<RawFd>();
                        for index in 0..count {
                            let descriptor = std::ptr::read_unaligned(data.add(index));
                            if descriptor >= 0 {
                                libc::close(descriptor);
                            }
                            segment.descriptors += 1;
                        }
                    }
                    libc::SCM_CREDENTIALS if payload >= size_of::<libc::ucred>() => {
                        let ucred =
                            std::ptr::read_unaligned(libc::CMSG_DATA(cmsg).cast::<libc::ucred>());
                        segment.credentials = Some(Credentials {
                            pid: u32::try_from(ucred.pid).unwrap_or(0),
                            uid: ucred.uid,
                            gid: ucred.gid,
                        });
                    }
                    _ => {}
                }
            }
            cmsg = libc::CMSG_NXTHDR(message, cmsg);
        }
    }

    /// Whether the peer has already queued more bytes.
    ///
    /// `MSG_PEEK` leaves them where they are: the connection is closed
    /// straight after the response, so nothing is ever read from it
    /// again.
    pub fn pending_bytes(fd: RawFd) -> std::io::Result<usize> {
        let mut probe = [0u8; 1];
        // SAFETY: `fd` is owned by the caller and `probe` is one byte
        // of writable stack.
        let seen = unsafe {
            libc::recv(
                fd,
                probe.as_mut_ptr().cast::<libc::c_void>(),
                probe.len(),
                libc::MSG_PEEK | libc::MSG_DONTWAIT,
            )
        };
        if seen < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(seen as usize)
    }

    /// Re-read the sending process through `/proc` and confirm it is
    /// still the process the kernel named.
    ///
    /// The credentials are already authoritative for *who sent the
    /// bytes*; this answers *which live process that was*, so routes
    /// that walk process ancestry or bind a start time are working from
    /// an identity the broker confirmed rather than one a request
    /// described. A pid the daemon cannot resolve, or one whose real
    /// and effective uid no longer match what the kernel stamped, is
    /// refused.
    pub fn verify(credentials: Credentials) -> Option<PeerProcess> {
        if credentials.pid == 0 {
            return None;
        }
        let status = std::fs::read_to_string(format!("/proc/{}/status", credentials.pid)).ok()?;
        let real_uid = status_id(&status, "Uid:")?;
        let real_gid = status_id(&status, "Gid:")?;
        if real_uid.real != credentials.uid
            || real_uid.effective != credentials.uid
            || real_gid.real != credentials.gid
        {
            return None;
        }
        let stat = std::fs::read_to_string(format!("/proc/{}/stat", credentials.pid)).ok()?;
        let start_time_ticks = start_time(&stat)?;
        Some(PeerProcess {
            pid: credentials.pid,
            uid: credentials.uid,
            gid: credentials.gid,
            start_time_ticks,
        })
    }

    pub(super) struct IdLine {
        pub real: u32,
        pub effective: u32,
    }

    pub(super) fn status_id(status: &str, key: &str) -> Option<IdLine> {
        let line = status.lines().find(|line| line.starts_with(key))?;
        let mut fields = line[key.len()..].split_whitespace();
        let real = fields.next()?.parse().ok()?;
        let effective = fields.next()?.parse().ok()?;
        Some(IdLine { real, effective })
    }

    /// Field 22 of `/proc/<pid>/stat`.
    ///
    /// The command name is field 2 and may contain spaces and
    /// parentheses, so parsing starts after its closing parenthesis.
    pub(super) fn start_time(stat: &str) -> Option<u64> {
        let tail = &stat[stat.rfind(')')? + 1..];
        tail.split_whitespace().nth(19)?.parse().ok()
    }
}

#[cfg(target_os = "linux")]
pub use linux::{enable_credential_passing, pending_bytes, recv_segment, verify};

#[cfg(not(target_os = "linux"))]
mod fallback {
    use super::{Credentials, PeerProcess, Segment};
    use std::io::{Error, ErrorKind};

    pub fn enable_credential_passing(_fd: i32) -> std::io::Result<()> {
        Err(Error::new(
            ErrorKind::Unsupported,
            "clawd credential passing requires Linux",
        ))
    }

    pub fn recv_segment(_fd: i32, _buf: &mut [u8]) -> std::io::Result<Segment> {
        Err(Error::new(
            ErrorKind::Unsupported,
            "clawd credential passing requires Linux",
        ))
    }

    pub fn pending_bytes(_fd: i32) -> std::io::Result<usize> {
        Ok(0)
    }

    pub fn verify(_credentials: Credentials) -> Option<PeerProcess> {
        None
    }
}

#[cfg(not(target_os = "linux"))]
pub use fallback::{enable_credential_passing, pending_bytes, recv_segment, verify};

#[cfg(all(test, target_os = "linux"))]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/transport/peer.rs"
    ));
}
