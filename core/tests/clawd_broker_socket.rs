//! Real-socket behaviour of the broker transport.
//!
//! The unit tests exercise the framing and credential logic over a
//! `socketpair(2)`. This file uses a bound `UnixListener` and a real
//! `connect(2)` instead, because that is the only way to prove the part
//! of the design that depends on `accept(2)`: `SO_PASSCRED` is set on
//! the listener, Linux copies the flag onto every accepted socket
//! (`unix_sock_inherit_flags`), and the kernel therefore stamps sender
//! credentials onto messages that arrive on connections the daemon has
//! not even accepted yet.

#![cfg(target_os = "linux")]

use std::os::unix::io::AsRawFd;
use std::path::PathBuf;

use cos::clawd::protocol::{encode_response, Request, RequestId, Response};
use cos::clawd::routes::Command;
use cos::clawd::transport::frame::PeerStream;
use cos::clawd::transport::{peer, ReadOutcome};
use cos::clawd::wire::{InboundRequest, MAX_REQUEST_BYTES, PROTOCOL_VERSION};
use serde_json::{json, Value};
use tokio::net::UnixListener;

struct Bound {
    _dir: tempfile::TempDir,
    path: PathBuf,
    listener: UnixListener,
}

fn bind() -> Bound {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("clawd.sock");
    let listener = UnixListener::bind(&path).expect("bind");
    // Exactly what `server::run` does, before the first accept.
    peer::enable_credential_passing(listener.as_raw_fd()).expect("SO_PASSCRED");
    Bound {
        _dir: dir,
        path,
        listener,
    }
}

/// Accept one connection and answer it with `reply`, returning what the
/// kernel said about the peer that sent the request.
async fn serve_once(
    listener: UnixListener,
    reply: impl FnOnce(InboundRequest) -> Response + Send + 'static,
) -> (peer::Credentials, InboundRequest) {
    let (stream, _addr) = listener.accept().await.expect("accept");
    let mut peer_stream = PeerStream::new(stream).expect("prepare");
    let outcome = peer_stream
        .read_request(MAX_REQUEST_BYTES)
        .await
        .expect("request frame");
    let ReadOutcome::Frame(frame) = outcome else {
        panic!("expected a framed request");
    };
    let envelope: InboundRequest = serde_json::from_slice(&frame.body).expect("v1 envelope");
    let response = reply(envelope.clone());
    let body = encode_response(&response).expect("encode");
    peer_stream.write_response(&body).await.expect("write");
    (frame.credentials, envelope)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_connected_client_is_identified_by_kernel_credentials_on_its_message() {
    let bound = bind();
    let path = bound.path.clone();
    let server = tokio::spawn(serve_once(bound.listener, |envelope| {
        Response::ok(envelope.id, json!({"served": true}))
    }));

    let request = Request::build(Command::DaemonHealth, Value::Null);
    let expected_id = request.id.clone();
    let client =
        tokio::task::spawn_blocking(move || cos::clawd::client::request_blocking(&path, request));

    let (credentials, envelope) = server.await.expect("server task");
    let response = client.await.expect("client task").expect("response");

    assert_eq!(
        credentials.pid,
        std::process::id(),
        "the daemon must learn the sender from the kernel, not from the request"
    );
    assert_eq!(credentials.uid, unsafe { libc::getuid() });
    assert_eq!(envelope.v, PROTOCOL_VERSION);
    assert_eq!(envelope.command.as_str(), "daemon.health");
    assert_eq!(
        response.id, expected_id,
        "the response must echo the id the client minted"
    );
    assert!(response.ok);

    // The credentials the kernel reported resolve to this live process.
    let process = peer::verify(credentials).expect("verify");
    assert_eq!(process.pid, std::process::id());
    assert!(process.start_time_ticks > 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_response_for_another_request_is_refused_by_the_client() {
    let bound = bind();
    let path = bound.path.clone();
    let server = tokio::spawn(serve_once(bound.listener, |_envelope| {
        // A correlation id the caller never chose.
        Response::ok(RequestId::parse("someone-else").unwrap(), json!({}))
    }));

    let request = Request::build(Command::DaemonHealth, Value::Null);
    let client =
        tokio::task::spawn_blocking(move || cos::clawd::client::request_blocking(&path, request));

    let _ = server.await.expect("server task");
    let error = client
        .await
        .expect("client task")
        .expect_err("a mismatched correlation id must not be accepted");
    assert!(error.contains("correlate"), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_response_from_another_protocol_version_is_refused_by_the_client() {
    let bound = bind();
    let path = bound.path.clone();
    let server = tokio::spawn(serve_once(bound.listener, |envelope| {
        let mut response = Response::ok(envelope.id, json!({}));
        response.v = PROTOCOL_VERSION + 1;
        response
    }));

    let request = Request::build(Command::DaemonHealth, Value::Null);
    let client =
        tokio::task::spawn_blocking(move || cos::clawd::client::request_blocking(&path, request));

    let _ = server.await.expect("server task");
    let error = client
        .await
        .expect("client task")
        .expect_err("a foreign protocol version must not be accepted");
    assert!(error.contains("protocol"), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn descriptors_attached_to_a_real_connection_are_refused_and_closed() {
    let bound = bind();
    let path = bound.path.clone();

    let sender = std::thread::spawn(move || {
        use std::os::unix::net::UnixStream as StdUnixStream;

        let stream = StdUnixStream::connect(&path).expect("connect");
        let request = Request::build(Command::DaemonHealth, Value::Null);
        let body = serde_json::to_vec(&request).expect("encode");
        let mut frame = Vec::new();
        frame.extend_from_slice(b"CBK1");
        frame.push(0x01);
        frame.push(0);
        frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
        frame.extend_from_slice(&body);

        // The descriptor an attacker would try to hand the broker, plus
        // the end that proves it was closed.
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
            message.msg_controllen = libc::CMSG_SPACE(4) as _;
            let cmsg = libc::CMSG_FIRSTHDR(&message);
            (*cmsg).cmsg_level = libc::SOL_SOCKET;
            (*cmsg).cmsg_type = libc::SCM_RIGHTS;
            (*cmsg).cmsg_len = libc::CMSG_LEN(4) as _;
            std::ptr::write_unaligned(libc::CMSG_DATA(cmsg).cast::<i32>(), passed[1]);
            let sent = libc::sendmsg(stream.as_raw_fd(), &message, libc::MSG_NOSIGNAL);
            assert!(sent > 0, "sendmsg with SCM_RIGHTS");
            libc::close(passed[1]);
        }
        // Keep the connection alive until the daemon has read.
        std::thread::sleep(std::time::Duration::from_millis(200));
        passed[0]
    });

    let (stream, _addr) = bound.listener.accept().await.expect("accept");
    let mut peer_stream = PeerStream::new(stream).expect("prepare");
    let fault = peer_stream
        .read_request(MAX_REQUEST_BYTES)
        .await
        .err()
        .expect("a request carrying descriptors must be refused");
    assert_eq!(fault, cos::clawd::wire::Fault::DescriptorPassing);

    let probe = sender.join().expect("sender thread");
    let byte = [0u8; 1];
    let written = unsafe {
        libc::send(
            probe,
            byte.as_ptr().cast::<libc::c_void>(),
            1,
            libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT,
        )
    };
    assert!(
        written < 0,
        "the smuggled descriptor must have been closed by the broker"
    );
    unsafe { libc::close(probe) };
}

#[tokio::test(flavor = "multi_thread")]
async fn bytes_written_before_the_daemon_accepts_still_carry_credentials() {
    // The window this closes: a client that connects and writes
    // immediately, before `accept(2)` returns. Linux stamps credentials
    // in `maybe_add_creds` when the peer socket has not been accepted
    // yet, so the daemon still authenticates the message.
    let bound = bind();
    let path = bound.path.clone();

    let sender = std::thread::spawn(move || {
        use std::io::Write;
        use std::os::unix::net::UnixStream as StdUnixStream;

        let mut stream = StdUnixStream::connect(&path).expect("connect");
        let request = Request::build(Command::TaskCount, Value::Null);
        let body = serde_json::to_vec(&request).expect("encode");
        let mut frame = Vec::new();
        frame.extend_from_slice(b"CBK1");
        frame.push(0x01);
        frame.push(0);
        frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
        frame.extend_from_slice(&body);
        stream.write_all(&frame).expect("write");
        stream.flush().expect("flush");
        std::thread::sleep(std::time::Duration::from_millis(200));
    });

    // Give the client time to connect *and* write before accepting.
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    let (stream, _addr) = bound.listener.accept().await.expect("accept");
    let mut peer_stream = PeerStream::new(stream).expect("prepare");
    let ReadOutcome::Frame(frame) = peer_stream
        .read_request(MAX_REQUEST_BYTES)
        .await
        .expect("request frame")
    else {
        panic!("expected a framed request");
    };
    assert_eq!(frame.credentials.pid, std::process::id());
    assert_eq!(frame.credentials.uid, unsafe { libc::getuid() });
    sender.join().expect("sender thread");
}
