use super::*;
use std::os::unix::io::RawFd;

/// Send `payload` on `fd`, optionally attaching `passed` as
/// `SCM_RIGHTS` — the exact shape of an attempted descriptor injection.
fn send_with_descriptor(fd: RawFd, payload: &[u8], passed: Option<RawFd>) -> isize {
    let mut iov = libc::iovec {
        iov_base: payload.as_ptr() as *mut libc::c_void,
        iov_len: payload.len(),
    };
    let mut control = [0u8; 64];
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = std::ptr::addr_of_mut!(iov);
    message.msg_iovlen = 1;
    if let Some(passed) = passed {
        unsafe {
            message.msg_control = control.as_mut_ptr().cast::<libc::c_void>();
            message.msg_controllen = libc::CMSG_SPACE(size_of::<RawFd>() as u32) as _;
            let cmsg = libc::CMSG_FIRSTHDR(&message);
            (*cmsg).cmsg_level = libc::SOL_SOCKET;
            (*cmsg).cmsg_type = libc::SCM_RIGHTS;
            (*cmsg).cmsg_len = libc::CMSG_LEN(size_of::<RawFd>() as u32) as _;
            std::ptr::write_unaligned(libc::CMSG_DATA(cmsg).cast::<RawFd>(), passed);
        }
    }
    unsafe { libc::sendmsg(fd, &message, libc::MSG_NOSIGNAL) }
}

fn socket_pair() -> (RawFd, RawFd) {
    let mut fds = [0 as RawFd; 2];
    let rc = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
    assert_eq!(rc, 0, "socketpair failed");
    (fds[0], fds[1])
}

fn is_closed(fd: RawFd) -> bool {
    // Everything else holding the peer end is gone, so a write returns
    // EPIPE (or ECONNRESET) instead of succeeding.
    let byte = [0u8; 1];
    let written = unsafe {
        libc::send(
            fd,
            byte.as_ptr().cast::<libc::c_void>(),
            1,
            libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT,
        )
    };
    if written >= 0 {
        return false;
    }
    matches!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EPIPE) | Some(libc::ECONNRESET)
    )
}

#[test]
fn credentials_arrive_with_the_bytes_of_each_message() {
    let (reader, writer) = socket_pair();
    enable_credential_passing(reader).expect("SO_PASSCRED");

    assert_eq!(send_with_descriptor(writer, b"hello", None), 5);
    let mut buf = [0u8; 16];
    let segment = recv_segment(reader, &mut buf).expect("recvmsg");

    assert_eq!(segment.bytes, 5);
    assert_eq!(segment.descriptors, 0);
    assert!(!segment.control_truncated);
    let credentials = segment.credentials.expect("kernel credentials");
    assert_eq!(credentials.pid, std::process::id());
    assert_eq!(credentials.uid, unsafe { libc::getuid() });

    unsafe {
        libc::close(reader);
        libc::close(writer);
    }
}

#[test]
fn a_passed_descriptor_is_counted_and_closed() {
    let (reader, writer) = socket_pair();
    enable_credential_passing(reader).expect("SO_PASSCRED");

    // The object whose liveness proves the received descriptor was
    // closed: `probe` stays open here, `victim` is what we hand over.
    let (probe, victim) = socket_pair();
    assert_eq!(send_with_descriptor(writer, b"x", Some(victim)), 1);
    unsafe { libc::close(victim) };

    let mut buf = [0u8; 8];
    let segment = recv_segment(reader, &mut buf).expect("recvmsg");
    assert_eq!(segment.bytes, 1);
    assert_eq!(
        segment.descriptors, 1,
        "an attached descriptor must be reported so the request is refused"
    );
    assert!(segment.credentials.is_some());
    assert!(
        is_closed(probe),
        "the received descriptor must be closed before the caller sees the segment"
    );

    unsafe {
        libc::close(probe);
        libc::close(reader);
        libc::close(writer);
    }
}

#[test]
fn the_current_process_verifies_against_proc() {
    let credentials = Credentials {
        pid: std::process::id(),
        uid: unsafe { libc::getuid() },
        gid: unsafe { libc::getgid() },
    };
    let process = verify(credentials).expect("self must verify");
    assert_eq!(process.pid, credentials.pid);
    assert_eq!(process.uid, credentials.uid);
    assert!(process.start_time_ticks > 0);
}

#[test]
fn credentials_that_do_not_match_the_live_process_are_refused() {
    let mine = Credentials {
        pid: std::process::id(),
        uid: unsafe { libc::getuid() },
        gid: unsafe { libc::getgid() },
    };
    // A pid the kernel never reported for us, and a uid this pid does
    // not actually run as: both fail closed.
    assert!(verify(Credentials { pid: 0, ..mine }).is_none());
    assert!(verify(Credentials {
        uid: mine.uid.wrapping_add(1),
        ..mine
    })
    .is_none());
}

#[test]
fn pending_bytes_sees_queued_input_without_consuming_it() {
    let (reader, writer) = socket_pair();
    enable_credential_passing(reader).expect("SO_PASSCRED");
    assert_eq!(pending_bytes(reader).unwrap_or(0), 0);
    assert_eq!(send_with_descriptor(writer, b"ab", None), 2);
    assert_eq!(pending_bytes(reader).expect("peek"), 1);

    let mut buf = [0u8; 8];
    let segment = recv_segment(reader, &mut buf).expect("recvmsg");
    assert_eq!(segment.bytes, 2, "the peek must not consume the message");

    unsafe {
        libc::close(reader);
        libc::close(writer);
    }
}

#[test]
fn proc_stat_parsing_survives_a_command_name_with_spaces_and_parentheses() {
    // Fields 3..=52, each carrying its own field number, after a comm
    // that contains both a space and a closing parenthesis.
    let fields: Vec<String> = (3..=52).map(|index| index.to_string()).collect();
    let stat = format!("77 (weird ) name) {}\n", fields.join(" "));
    assert_eq!(linux::start_time(&stat), Some(22));
    assert_eq!(linux::start_time("no parenthesis here"), None);
}

#[test]
fn proc_status_ids_are_read_as_real_then_effective() {
    let status = "Name:\tcos\nUid:\t1000\t1000\t1000\t1000\nGid:\t1000\t1000\t1000\t1000\n";
    let uid = linux::status_id(status, "Uid:").expect("uid line");
    assert_eq!(uid.real, 1000);
    assert_eq!(uid.effective, 1000);
    assert!(linux::status_id(status, "Suid:").is_none());
}

#[test]
fn a_setuid_style_mismatch_between_real_and_effective_uid_is_refused() {
    let status = "Uid:\t1000\t0\t0\t0\nGid:\t1000\t1000\t1000\t1000\n";
    let uid = linux::status_id(status, "Uid:").expect("uid line");
    assert_ne!(
        uid.real, uid.effective,
        "the parser must expose the difference `verify` refuses on"
    );
}
