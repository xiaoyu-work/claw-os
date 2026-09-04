use super::*;

#[cfg(unix)]
#[test]
fn connection_failure_is_known_to_be_pre_dispatch() {
    let directory = tempfile::tempdir().expect("tempdir");
    let request = Request::build(
        super::super::routes::Command::DaemonHealth,
        serde_json::json!({}),
    );
    let error = request_blocking(directory.path().join("missing.sock"), request)
        .expect_err("missing socket must fail");

    assert!(!error.may_have_dispatched());
}

#[cfg(unix)]
#[test]
fn response_failure_is_indeterminate_for_a_mutation_caller() {
    use std::io::Read;
    use std::os::unix::net::UnixListener;

    let directory = tempfile::tempdir().expect("tempdir");
    let socket = directory.path().join("broker.sock");
    let listener = UnixListener::bind(&socket).expect("bind");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut header = [0_u8; super::super::wire::HEADER_BYTES];
        stream.read_exact(&mut header).expect("request header");
        let length = super::super::transport::frame::parse_header(
            &header,
            super::super::wire::KIND_REQUEST,
            super::super::wire::MAX_REQUEST_BYTES,
        )
        .expect("request frame");
        let mut body = vec![0_u8; length];
        stream.read_exact(&mut body).expect("request body");
    });
    let request = Request::build(
        super::super::routes::Command::DaemonHealth,
        serde_json::json!({}),
    );
    let error = request_blocking(&socket, request).expect_err("closed response must fail");
    server.join().expect("server");

    assert!(error.may_have_dispatched());
}
