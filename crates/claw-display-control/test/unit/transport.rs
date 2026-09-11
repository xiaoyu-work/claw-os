use super::*;
use serde::{Deserialize, Serialize};
use std::os::unix::process::CommandExt;
use std::time::Duration;

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
enum Message {
    Ping {},
    OneFd {},
}

impl Packet for Message {
    fn descriptor_count(&self) -> usize {
        usize::from(matches!(self, Self::OneFd {}))
    }
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(5)
}

fn raw_send(connection: &Connection, bytes: &[u8], fds: &[BorrowedFd<'_>]) {
    let mut vector = libc::iovec {
        iov_base: bytes.as_ptr().cast_mut().cast(),
        iov_len: bytes.len(),
    };
    #[repr(C, align(16))]
    struct SendAncillary([u8; 1024]);
    let mut ancillary = SendAncillary([0; 1024]);
    let mut header: libc::msghdr = unsafe { zeroed() };
    header.msg_iov = &mut vector;
    header.msg_iovlen = 1;
    if !fds.is_empty() {
        let payload = fds.len() * size_of::<i32>();
        header.msg_control = ancillary.0.as_mut_ptr().cast();
        header.msg_controllen = unsafe { libc::CMSG_SPACE(payload as u32) } as usize;
        unsafe {
            let cmsg = libc::CMSG_FIRSTHDR(&header);
            (*cmsg).cmsg_level = libc::SOL_SOCKET;
            (*cmsg).cmsg_type = libc::SCM_RIGHTS;
            (*cmsg).cmsg_len = libc::CMSG_LEN(payload as u32) as usize;
            for (index, fd) in fds.iter().enumerate() {
                libc::CMSG_DATA(cmsg)
                    .cast::<i32>()
                    .add(index)
                    .write(fd.as_raw_fd());
            }
        }
    }
    let written =
        unsafe { libc::sendmsg(connection.as_fd().as_raw_fd(), &header, libc::MSG_NOSIGNAL) };
    assert_eq!(
        written,
        bytes.len() as isize,
        "{}",
        io::Error::last_os_error()
    );
}

#[test]
fn socket_and_received_descriptors_are_close_on_exec() {
    let (sender, receiver) = Connection::pair().unwrap();
    let owner = ProcessIdentity::current().unwrap();
    let file = tempfile::tempfile().unwrap();
    sender
        .send(&Message::OneFd {}, &[file.as_fd()], deadline())
        .unwrap();
    let received = receiver.receive::<Message>(&owner, deadline()).unwrap();
    assert_eq!(received.message, Message::OneFd {});
    assert_eq!(received.sender, owner);
    assert_eq!(received.descriptors.len(), 1);
    for fd in [receiver.as_fd(), received.descriptors[0].as_fd()] {
        assert_ne!(
            unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC,
            0
        );
    }
}

#[test]
fn unknown_fields_magic_and_fd_counts_fail_closed() {
    let owner = ProcessIdentity::current().unwrap();
    for (bytes, descriptor_count) in [
        (b"CDS1{\"action\":\"ping\",\"owner_uid\":0}".as_slice(), 0),
        (b"CDS2{\"action\":\"ping\"}".as_slice(), 0),
        (b"CDS1{\"action\":\"one-fd\"}".as_slice(), 0),
        (b"CDS1{\"action\":\"ping\"}".as_slice(), 1),
        (b"CDS1{\"action\":\"one-fd\"}".as_slice(), 2),
    ] {
        let (sender, receiver) = Connection::pair().unwrap();
        let file = tempfile::tempfile().unwrap();
        let fds = vec![file.as_fd(); descriptor_count];
        let before = std::fs::read_dir("/proc/self/fd").unwrap().count();
        raw_send(&sender, bytes, &fds);
        assert!(
            receiver.receive::<Message>(&owner, deadline()).is_err(),
            "{} with {descriptor_count} descriptors was accepted",
            String::from_utf8_lossy(bytes)
        );
        assert_eq!(std::fs::read_dir("/proc/self/fd").unwrap().count(), before);
    }
}

#[test]
fn login_activation_does_not_accept_claimed_owner_session_or_epoch() {
    let owner = ProcessIdentity::current().unwrap();
    for bytes in [
        b"CDS1{\"action\":\"register\",\"owner_uid\":0}".as_slice(),
        b"CDS1{\"action\":\"register\",\"audit_session\":1}".as_slice(),
        b"CDS1{\"action\":\"register\",\"epoch\":[1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1]}".as_slice(),
        b"CDS1{\"action\":\"register\",\"pid\":1}".as_slice(),
        b"CDS1{\"action\":\"register\",\"path\":\"/run/claimed\"}".as_slice(),
    ] {
        let (sender, receiver) = Connection::pair().unwrap();
        let file = tempfile::tempfile().unwrap();
        let before = std::fs::read_dir("/proc/self/fd").unwrap().count();
        raw_send(&sender, bytes, &[file.as_fd(), file.as_fd()]);
        assert!(receiver
            .receive::<crate::wire::LoginRequest>(&owner, deadline())
            .is_err());
        assert_eq!(std::fs::read_dir("/proc/self/fd").unwrap().count(), before);
    }
}

#[test]
fn oversized_and_ancillary_truncated_packets_do_not_leak_descriptors() {
    let owner = ProcessIdentity::current().unwrap();
    let (sender, receiver) = Connection::pair().unwrap();
    let bytes = vec![b'x'; MAX_FRAME_BYTES + 1];
    raw_send(&sender, &bytes, &[]);
    assert!(receiver.receive::<Message>(&owner, deadline()).is_err());
    let files: Vec<_> = (0..80).map(|_| tempfile::tempfile().unwrap()).collect();
    let before = std::fs::read_dir("/proc/self/fd").unwrap().count();
    raw_send(
        &sender,
        b"CDS1{\"action\":\"one-fd\"}",
        &files.iter().map(AsFd::as_fd).collect::<Vec<_>>(),
    );
    assert!(receiver.receive::<Message>(&owner, deadline()).is_err());
    assert_eq!(std::fs::read_dir("/proc/self/fd").unwrap().count(), before);
}

#[test]
fn no_packet_and_peer_loss_are_explicit() {
    let owner = ProcessIdentity::current().unwrap();
    let (sender, receiver) = Connection::pair().unwrap();
    assert!(matches!(
        receiver.receive::<Message>(&owner, Instant::now() + Duration::from_millis(2)),
        Err(Error::Timeout)
    ));
    drop(sender);
    assert!(matches!(
        receiver.receive::<Message>(&owner, deadline()),
        Err(Error::Closed)
    ));
}

#[test]
fn readiness_probe_does_not_consume_or_invent_messages() {
    let owner = ProcessIdentity::current().unwrap();
    let (sender, receiver) = Connection::pair().unwrap();
    assert!(!receiver.readable().unwrap());
    sender.send(&Message::Ping {}, &[], deadline()).unwrap();
    assert!(receiver.readable().unwrap());
    assert_eq!(
        receiver
            .receive::<Message>(&owner, deadline())
            .unwrap()
            .message,
        Message::Ping {}
    );
    assert!(!receiver.readable().unwrap());
    drop(sender);
    assert!(receiver.readable().unwrap());
    assert!(matches!(
        receiver.receive::<Message>(&owner, deadline()),
        Err(Error::Closed)
    ));
}

#[test]
fn missing_fixed_inherited_descriptor_is_an_error_not_an_owned_fd_abort() {
    assert!(matches!(
        unsafe { Connection::inherit(i32::MAX) },
        Err(Error::Protocol(_))
    ));
    assert!(matches!(
        unsafe { Connection::inherit(0) },
        Err(Error::Protocol(_))
    ));
}

#[test]
fn streams_and_regular_files_are_not_control_channels() {
    let file = tempfile::tempfile().unwrap();
    assert!(Connection::from_owned(file.into()).is_err());
    let (stream, _) = std::os::unix::net::UnixStream::pair().unwrap();
    assert!(Connection::from_owned(stream.into()).is_err());
}

fn sender_process(connection: Connection) -> std::process::Child {
    let source = connection.as_fd().as_raw_fd();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "transport::tests::private_sender_process",
            "--nocapture",
        ])
        .env("CLAW_DISPLAY_PRIVATE_SENDER", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null());
    unsafe {
        command.pre_exec(move || {
            if source != 3 && libc::dup2(source, 3) < 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::fcntl(3, libc::F_SETFD, 0) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command.spawn().unwrap()
}

#[test]
fn inherited_socket_credentials_do_not_authenticate_the_child_as_parent() {
    let owner = ProcessIdentity::current().unwrap();
    let (sender, receiver) = Connection::pair().unwrap();
    assert_eq!(receiver.peer().unwrap(), owner);
    let mut child = sender_process(sender);
    assert!(matches!(
        receiver.receive::<Message>(&owner, deadline()),
        Err(Error::Identity)
    ));
    receiver.send(&Message::Ping {}, &[], deadline()).unwrap();
    assert!(child.wait().unwrap().success());
}

#[test]
fn an_expected_child_is_authenticated_by_its_actual_message_credentials() {
    let (sender, receiver) = Connection::pair().unwrap();
    let mut child = sender_process(sender);
    let expected = ProcessIdentity::child(&child).unwrap();
    let received = receiver.receive::<Message>(&expected, deadline()).unwrap();
    assert_eq!(received.sender, expected);
    receiver.send(&Message::Ping {}, &[], deadline()).unwrap();
    assert!(child.wait().unwrap().success());
}

#[test]
fn private_sender_process() {
    if std::env::var("CLAW_DISPLAY_PRIVATE_SENDER").as_deref() != Ok("1") {
        return;
    }
    let connection = Connection::from_owned(unsafe { OwnedFd::from_raw_fd(3) }).unwrap();
    let parent = connection.peer().unwrap();
    connection.send(&Message::Ping {}, &[], deadline()).unwrap();
    connection.receive::<Message>(&parent, deadline()).unwrap();
}
