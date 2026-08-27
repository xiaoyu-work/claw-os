use super::*;
use std::os::unix::io::AsRawFd;

use crate::clawd::wire::{KIND_REQUEST, MAX_REQUEST_BYTES};

fn header(kind: u8, flags: u8, len: u32) -> [u8; HEADER_BYTES] {
    let mut bytes = [0u8; HEADER_BYTES];
    bytes[..4].copy_from_slice(&MAGIC);
    bytes[4] = kind;
    bytes[5] = flags;
    bytes[6..].copy_from_slice(&len.to_be_bytes());
    bytes
}

async fn write_all(stream: &mut UnixStream, bytes: &[u8]) {
    use tokio::io::AsyncWriteExt;
    stream.write_all(bytes).await.expect("write");
    stream.flush().await.expect("flush");
}

fn peer_pair() -> (PeerStream, UnixStream) {
    let (server, client) = UnixStream::pair().expect("socketpair");
    (PeerStream::new(server).expect("SO_PASSCRED"), client)
}

#[test]
fn a_header_round_trips_and_declares_its_length_before_the_body() {
    let frame = encode_frame(KIND_REQUEST, b"{}");
    assert_eq!(frame.len(), HEADER_BYTES + 2);
    assert_eq!(&frame[..4], &MAGIC);
    let mut head = [0u8; HEADER_BYTES];
    head.copy_from_slice(&frame[..HEADER_BYTES]);
    assert_eq!(parse_header(&head, KIND_REQUEST, 64), Ok(2));
}

#[test]
fn an_oversized_declared_length_is_refused_before_any_allocation() {
    let head = header(KIND_REQUEST, 0, u32::MAX);
    assert_eq!(
        parse_header(&head, KIND_REQUEST, MAX_REQUEST_BYTES),
        Err(Fault::FrameTooLarge)
    );
}

#[test]
fn a_foreign_magic_kind_or_flag_is_refused() {
    let mut wrong_magic = header(KIND_REQUEST, 0, 0);
    wrong_magic[0] = b'X';
    assert_eq!(
        parse_header(&wrong_magic, KIND_REQUEST, 64),
        Err(Fault::UnsupportedFrame)
    );
    assert_eq!(
        parse_header(&header(KIND_RESPONSE, 0, 0), KIND_REQUEST, 64),
        Err(Fault::UnsupportedFrame)
    );
    assert_eq!(
        parse_header(&header(KIND_REQUEST, 1, 0), KIND_REQUEST, 64),
        Err(Fault::UnsupportedFrame)
    );
}

#[tokio::test]
async fn a_complete_frame_arrives_with_this_process_credentials() {
    let (mut server, mut client) = peer_pair();
    let body = br#"{"v":1,"id":"r1","command":"daemon.health"}"#;
    write_all(&mut client, &encode_frame(KIND_REQUEST, body)).await;

    let outcome = server
        .read_request(MAX_REQUEST_BYTES)
        .await
        .expect("frame must be readable");
    let ReadOutcome::Frame(frame) = outcome else {
        panic!("expected a frame");
    };
    assert_eq!(frame.body, body);
    assert_eq!(frame.credentials.pid, std::process::id());
    assert_eq!(frame.credentials.uid, unsafe { libc::getuid() });
}

#[tokio::test]
async fn a_frame_split_across_writes_is_reassembled() {
    let (mut server, mut client) = peer_pair();
    let body = br#"{"v":1,"id":"r1","command":"task.count"}"#;
    let frame = encode_frame(KIND_REQUEST, body);
    write_all(&mut client, &frame[..6]).await;
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    write_all(&mut client, &frame[6..]).await;

    let ReadOutcome::Frame(frame) = server.read_request(MAX_REQUEST_BYTES).await.unwrap() else {
        panic!("expected a frame");
    };
    assert_eq!(frame.body, body);
}

#[tokio::test]
async fn a_truncated_frame_is_refused_rather_than_waited_on() {
    let (mut server, mut client) = peer_pair();
    let frame = encode_frame(KIND_REQUEST, b"{\"v\":1}");
    write_all(&mut client, &frame[..frame.len() - 3]).await;
    drop(client);

    assert_eq!(
        server.read_request(MAX_REQUEST_BYTES).await.err(),
        Some(Fault::TruncatedFrame)
    );
}

#[tokio::test]
async fn an_oversized_frame_is_refused_at_the_header() {
    let (mut server, mut client) = peer_pair();
    write_all(&mut client, &header(KIND_REQUEST, 0, u32::MAX)).await;

    assert_eq!(
        server.read_request(MAX_REQUEST_BYTES).await.err(),
        Some(Fault::FrameTooLarge)
    );
}

#[tokio::test]
async fn a_pre_v1_newline_request_is_recognised_and_never_parsed() {
    let (mut server, mut client) = peer_pair();
    write_all(
        &mut client,
        b"{\"command\":\"daemon.health\",\"params\":{}}\n",
    )
    .await;

    assert!(matches!(
        server.read_request(MAX_REQUEST_BYTES).await.unwrap(),
        ReadOutcome::Legacy
    ));
}

#[tokio::test]
async fn a_connection_that_closes_without_a_frame_is_not_a_fault() {
    let (mut server, client) = peer_pair();
    drop(client);
    assert!(matches!(
        server.read_request(MAX_REQUEST_BYTES).await.unwrap(),
        ReadOutcome::Closed
    ));
}

#[tokio::test]
async fn attached_descriptors_are_refused_and_closed() {
    let (mut server, client) = peer_pair();
    let body = br#"{"v":1,"id":"r1","command":"daemon.health"}"#;
    let frame = encode_frame(KIND_REQUEST, body);

    // A second socketpair stands in for the descriptor an attacker
    // would try to smuggle in; `probe` proves it was closed.
    let mut passed = [0i32; 2];
    assert_eq!(
        unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, passed.as_mut_ptr()) },
        0
    );
    let mut iov = libc::iovec {
        iov_base: frame.as_ptr() as *mut libc::c_void,
        iov_len: frame.len(),
    };
    let mut control = [0u8; 64];
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = std::ptr::addr_of_mut!(iov);
    message.msg_iovlen = 1;
    unsafe {
        message.msg_control = control.as_mut_ptr().cast::<libc::c_void>();
        message.msg_controllen = libc::CMSG_SPACE(size_of::<i32>() as u32) as _;
        let cmsg = libc::CMSG_FIRSTHDR(&message);
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len = libc::CMSG_LEN(size_of::<i32>() as u32) as _;
        std::ptr::write_unaligned(libc::CMSG_DATA(cmsg).cast::<i32>(), passed[1]);
        let sent = libc::sendmsg(client.as_raw_fd(), &message, libc::MSG_NOSIGNAL);
        assert!(sent > 0);
        libc::close(passed[1]);
    }

    assert_eq!(
        server.read_request(MAX_REQUEST_BYTES).await.err(),
        Some(Fault::DescriptorPassing),
        "a request carrying a descriptor must be refused outright"
    );

    let byte = [0u8; 1];
    let written = unsafe {
        libc::send(
            passed[0],
            byte.as_ptr().cast::<libc::c_void>(),
            1,
            libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT,
        )
    };
    assert!(
        written < 0,
        "the smuggled descriptor must already be closed"
    );
    unsafe { libc::close(passed[0]) };
}

#[tokio::test]
async fn a_second_pipelined_frame_is_visible_before_dispatch() {
    let (mut server, mut client) = peer_pair();
    let body = br#"{"v":1,"id":"r1","command":"daemon.health"}"#;
    let mut both = encode_frame(KIND_REQUEST, body);
    both.extend_from_slice(&encode_frame(KIND_REQUEST, body));
    write_all(&mut client, &both).await;

    let ReadOutcome::Frame(_) = server.read_request(MAX_REQUEST_BYTES).await.unwrap() else {
        panic!("expected a frame");
    };
    assert!(
        server.has_pending_input(),
        "the reader must not have consumed the second frame, and must see it"
    );
}

#[tokio::test]
async fn a_single_frame_leaves_nothing_pending() {
    let (mut server, mut client) = peer_pair();
    let body = br#"{"v":1,"id":"r1","command":"daemon.health"}"#;
    write_all(&mut client, &encode_frame(KIND_REQUEST, body)).await;

    let ReadOutcome::Frame(_) = server.read_request(MAX_REQUEST_BYTES).await.unwrap() else {
        panic!("expected a frame");
    };
    assert!(!server.has_pending_input());
}

#[tokio::test]
async fn a_slow_client_is_bounded_by_the_caller_deadline() {
    let (mut server, _client) = peer_pair();
    let outcome = tokio::time::timeout(
        std::time::Duration::from_millis(120),
        server.read_request(MAX_REQUEST_BYTES),
    )
    .await;
    assert!(
        outcome.is_err(),
        "a connection that never sends must be abandoned, not held"
    );
}

#[tokio::test]
async fn a_response_frame_round_trips_to_a_client() {
    let (mut server, mut client) = peer_pair();
    server
        .write_response(b"{\"ok\":true}")
        .await
        .expect("write");
    let body = read_response_async(&mut client, 1024).await.expect("read");
    assert_eq!(body, b"{\"ok\":true}");
}
