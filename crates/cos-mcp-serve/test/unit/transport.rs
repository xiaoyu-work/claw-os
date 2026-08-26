use super::*;

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
    let mut cli_in_w = cli_r_to_srv;
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

/// A peer that streams more than [`MAX_FRAME_BYTES`] without a
/// newline must trip the size guard rather than letting us
/// buffer endlessly. Regression test for the prior unbounded
/// `read_line`.
#[tokio::test(flavor = "current_thread")]
async fn stdio_transport_rejects_oversize_frame() {
    let (mut cli_in_w, srv_in) = tokio::io::duplex(64 * 1024);
    let (srv_out, _cli_out_r) = tokio::io::duplex(4096);
    let server = StdioTransport::from_pair(Box::new(srv_in), Box::new(srv_out));

    use tokio::io::AsyncWriteExt;
    // Use a small chunk repeatedly so we don't block on the
    // duplex's own buffer; recv() reads byte-by-byte so the
    // bytes drain as we feed them.
    let chunk = vec![b'a'; 64 * 1024];
    let writer = tokio::spawn(async move {
        let need = MAX_FRAME_BYTES + 16;
        let mut written = 0;
        while written < need {
            let to_write = std::cmp::min(chunk.len(), need - written);
            if cli_in_w.write_all(&chunk[..to_write]).await.is_err() {
                return;
            }
            written += to_write;
        }
        let _ = cli_in_w.flush().await;
        // Hold the write half open until the test reads the
        // error so tokio doesn't surface a premature EOF.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    });

    let err = server.recv().await.unwrap_err();
    assert!(
        matches!(err, TransportError::TooLarge { limit } if limit == MAX_FRAME_BYTES),
        "expected TooLarge, got {err:?}",
    );
    writer.abort();
}
