use super::{ClawdTimeouts, clawd_request_on_stream};
use serde_json::json;
use std::time::Duration;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};

fn test_timeouts(read: Duration) -> ClawdTimeouts {
    ClawdTimeouts {
        connect: Duration::from_secs(1),
        write: Duration::from_secs(1),
        flush: Duration::from_secs(1),
        read,
    }
}

async fn read_request(stream: &mut UnixStream) {
    let mut line = String::new();
    let mut reader = BufReader::new(stream);
    let bytes = reader
        .read_line(&mut line)
        .await
        .expect("read clawd request");
    assert!(bytes > 0, "client closed before sending a request");
}

#[tokio::test]
async fn nonresponsive_clawd_read_times_out() {
    let (client_stream, mut server_stream) = UnixStream::pair().expect("create socket pair");

    let server = async {
        read_request(&mut server_stream).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let mut byte = [0];
        assert_eq!(
            server_stream
                .read(&mut byte)
                .await
                .expect("read after timeout"),
            0,
            "a timed-out request must close its socket"
        );
    };
    let client = clawd_request_on_stream(
        client_stream,
        "permission.pending",
        json!({ "limit": 100 }),
        test_timeouts(Duration::from_millis(100)),
    );

    let ((), result) = tokio::join!(server, client);
    let error = result.expect_err("unresponsive clawd must time out");
    assert!(
        error
            .0
            .contains("read clawd response permission.pending timed out"),
        "unexpected error: {}",
        error.0
    );
}

#[tokio::test]
async fn request_recovers_after_a_timed_out_response() {
    let (stalled_client, mut stalled_server) = UnixStream::pair().expect("create stalled pair");
    let (recovered_client, mut recovered_server) =
        UnixStream::pair().expect("create recovered pair");

    let server = async {
        read_request(&mut stalled_server).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        drop(stalled_server);

        read_request(&mut recovered_server).await;
        recovered_server
            .write_all(b"{\"id\":1,\"ok\":true,\"result\":{\"requests\":[]}}\n")
            .await
            .expect("write recovered response");
        recovered_server
            .flush()
            .await
            .expect("flush recovered response");
    };
    let client = async {
        let first = clawd_request_on_stream(
            stalled_client,
            "permission.pending",
            json!({ "limit": 100 }),
            test_timeouts(Duration::from_millis(100)),
        )
        .await;
        assert!(first.is_err(), "first request should time out");

        clawd_request_on_stream(
            recovered_client,
            "permission.pending",
            json!({ "limit": 100 }),
            test_timeouts(Duration::from_secs(1)),
        )
        .await
    };

    let ((), result) = tokio::join!(server, client);
    assert_eq!(
        result.expect("retry should succeed"),
        json!({ "requests": [] })
    );
}
