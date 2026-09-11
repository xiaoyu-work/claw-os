use super::*;
use std::time::Duration;

#[test]
fn packet_preserves_descriptor_and_actual_kernel_sender() {
    let (parent, child) = pair().unwrap();
    let payload = std::fs::File::open("/dev/null").unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    send(child.as_fd(), READY, payload.as_fd(), deadline).unwrap();
    let packet = receive(parent.as_fd(), READY, deadline).unwrap();
    assert_eq!(packet.credentials.pid, unsafe { libc::getpid() });
    assert_eq!(packet.credentials.uid, unsafe { libc::geteuid() });
    assert_eq!(packet.credentials.gid, unsafe { libc::getegid() });
    assert_ne!(
        unsafe { libc::fcntl(packet.descriptor.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC,
        0
    );
    let expected = payload.metadata().unwrap();
    let actual = std::fs::File::from(packet.descriptor).metadata().unwrap();
    use std::os::unix::fs::MetadataExt;
    assert_eq!(
        (expected.dev(), expected.ino()),
        (actual.dev(), actual.ino())
    );
}

#[test]
fn wrong_frame_and_missing_descriptor_fail_closed() {
    let (parent, child) = pair().unwrap();
    let payload = std::fs::File::open("/dev/null").unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    send(child.as_fd(), INPUT, payload.as_fd(), deadline).unwrap();
    assert!(receive(parent.as_fd(), READY, deadline).is_err());
    assert_eq!(
        unsafe {
            libc::send(
                child.as_raw_fd(),
                READY.as_ptr().cast(),
                READY.len(),
                libc::MSG_NOSIGNAL,
            )
        },
        4
    );
    assert!(receive(parent.as_fd(), READY, deadline).is_err());
}

#[test]
fn ordinary_stream_and_expired_bootstrap_are_refused() {
    let (stream, _) = std::os::unix::net::UnixStream::pair().unwrap();
    assert!(validate_channel(stream.as_fd()).is_err());
    let (parent, _child) = pair().unwrap();
    assert_eq!(
        receive(parent.as_fd(), READY, Instant::now())
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::TimedOut
    );
}
