use super::*;

impl StdioTransport {
    fn with_max_frame_bytes(mut self, n: usize) -> Self {
        self.max_frame_bytes = n;
        self
    }
}

#[tokio::test]
async fn pair_round_trips() {
    let (a, b) = in_memory_pair();
    a.send("{\"hi\":1}".into()).await.unwrap();
    let recv = b.recv().await.unwrap();
    assert_eq!(recv.as_deref(), Some("{\"hi\":1}"));
}

#[tokio::test]
async fn closing_one_end_signals_other() {
    let (a, b) = in_memory_pair();
    drop(a);
    let recv = b.recv().await.unwrap();
    assert!(recv.is_none(), "peer drop must surface as Ok(None)");
}

#[tokio::test]
async fn send_after_close_errors() {
    let (a, b) = in_memory_pair();
    drop(b);
    let err = a.send("{}".into()).await.unwrap_err();
    assert!(matches!(err, TransportError::Closed));
}

/// StdioTransport over a `tokio::io::duplex` pair. Reader sees
/// what the writer wrote, framed by `\n`. Smoke test: round-trip
/// one frame both directions.
#[tokio::test]
async fn stdio_transport_round_trips_frame() {
    // Two duplexes give us a full bidirectional pair: the
    // server-under-test reads from `srv_r` and writes to
    // `srv_w`; the test driver writes to `cli_w` and reads
    // from `cli_r`.
    let (cli_r_to_srv, srv_in) = tokio::io::duplex(4096);
    let (srv_out, cli_w_from_srv) = tokio::io::duplex(4096);
    let server = StdioTransport::from_pair(Box::new(srv_in), Box::new(srv_out));

    // Drive: write a request from client side; server reads it.
    let (mut cli_in_w, _hold) = (cli_r_to_srv, ());
    let _ = _hold;
    use tokio::io::AsyncWriteExt;
    cli_in_w.write_all(b"{\"hello\":1}\n").await.unwrap();
    cli_in_w.flush().await.unwrap();
    let frame = server.recv().await.unwrap().unwrap();
    assert_eq!(frame, "{\"hello\":1}");

    // Server writes a response; client side reads it.
    server.send("{\"ok\":true}".into()).await.unwrap();
    use tokio::io::AsyncReadExt;
    let mut buf = vec![0u8; 64];
    let n = cli_w_from_srv.take(64).read(&mut buf).await.unwrap();
    let s = std::str::from_utf8(&buf[..n]).unwrap();
    assert!(
        s.starts_with("{\"ok\":true}"),
        "frame should be JSON line, got {s:?}"
    );
    assert!(s.contains('\n'), "frame must end with newline");
}

/// Blank lines between frames are tolerated as keepalives.
#[tokio::test]
async fn stdio_transport_skips_blank_lines() {
    let (mut cli_in_w, srv_in) = tokio::io::duplex(4096);
    let (srv_out, _cli_out_r) = tokio::io::duplex(4096);
    let server = StdioTransport::from_pair(Box::new(srv_in), Box::new(srv_out));

    use tokio::io::AsyncWriteExt;
    cli_in_w.write_all(b"\n\n   \n{\"go\":1}\n").await.unwrap();
    cli_in_w.flush().await.unwrap();

    let frame = server.recv().await.unwrap().unwrap();
    assert_eq!(frame, "{\"go\":1}");
}

/// EOF on the read half surfaces as `Ok(None)`, matching the
/// transport-closed contract documented on `Transport::recv`.
#[tokio::test]
async fn stdio_transport_eof_returns_none() {
    let (cli_in_w, srv_in) = tokio::io::duplex(4096);
    let (srv_out, _cli_out_r) = tokio::io::duplex(4096);
    let server = StdioTransport::from_pair(Box::new(srv_in), Box::new(srv_out));
    drop(cli_in_w);

    let frame = server.recv().await.unwrap();
    assert!(frame.is_none(), "EOF must surface as Ok(None)");
}

/// A peer that streams more bytes than the configured per-frame
/// cap without ever sending `\n` must be cut off with a `Decode`
/// error rather than allowed to drive us to OOM. Use a small
/// override of the cap to keep the test fast.
#[tokio::test]
async fn stdio_transport_rejects_oversize_frame() {
    use tokio::io::AsyncWriteExt;
    let (mut cli_in_w, srv_in) = tokio::io::duplex(8192);
    let (srv_out, _cli_out_r) = tokio::io::duplex(4096);
    let server =
        StdioTransport::from_pair(Box::new(srv_in), Box::new(srv_out)).with_max_frame_bytes(1024);

    let recv_task = tokio::spawn(async move { server.recv().await });

    // 2 KiB of bytes, no newline. Cap is 1 KiB → cap+1 = 1025
    // bytes get pulled, then `\n` not found → Decode error.
    let payload = vec![b'x'; 2048];
    let _ = cli_in_w.write_all(&payload).await;
    let _ = cli_in_w.shutdown().await;
    drop(cli_in_w);

    let result = recv_task.await.unwrap();
    match result {
        Err(TransportError::Decode(msg)) => {
            assert!(msg.contains("exceeded"), "got {msg}");
        }
        other => panic!("expected Decode(exceeded), got {other:?}"),
    }
}
