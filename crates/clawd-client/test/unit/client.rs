#[cfg(unix)]
mod unix {
    use super::super::*;
    use crate::{ErrorCode, RemoteError, RequestId, PROTOCOL_VERSION};
    use serde_json::json;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixStream;

    fn config(read_timeout: Duration) -> ClientConfig {
        ClientConfig {
            connect_timeout: Duration::from_secs(1),
            write_timeout: Duration::from_secs(1),
            read_timeout,
            max_request_bytes: MAX_REQUEST_BYTES,
            max_response_bytes: MAX_RESPONSE_BYTES,
        }
    }

    async fn read_request(stream: &mut UnixStream) -> Request {
        let mut header = [0u8; HEADER_BYTES];
        stream.read_exact(&mut header).await.unwrap();
        assert_eq!(&header[..4], &MAGIC);
        assert_eq!(header[4], KIND_REQUEST);
        assert_eq!(header[5], 0);
        let len = u32::from_be_bytes(header[6..10].try_into().unwrap()) as usize;
        let mut body = vec![0; len];
        stream.read_exact(&mut body).await.unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    async fn write_response(stream: &mut UnixStream, response: &Response) {
        let body = serde_json::to_vec(response).unwrap();
        stream
            .write_all(&encode_frame(KIND_RESPONSE, &body))
            .await
            .unwrap();
        stream.flush().await.unwrap();
    }

    #[tokio::test]
    async fn framed_exchange_correlates_and_decodes_success() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let request = Request::with_id(
            Command::PermissionPending,
            json!({"limit": 100}),
            RequestId::parse("approval-1").unwrap(),
        );
        let expected_request = request.clone();
        let server_task = async {
            let received = read_request(&mut server).await;
            assert_eq!(received, expected_request);
            write_response(
                &mut server,
                &Response {
                    v: PROTOCOL_VERSION,
                    id: received.id,
                    ok: true,
                    result: Some(json!({"requests": []})),
                    error: None,
                },
            )
            .await;
        };
        let client_task = exchange_on_stream(client, request, config(Duration::from_secs(1)));
        let ((), response) = tokio::join!(server_task, client_task);
        assert_eq!(
            response.unwrap().into_result().unwrap(),
            json!({"requests": []})
        );
    }

    #[tokio::test]
    async fn remote_error_keeps_stable_code_and_data() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let request = Request::new(Command::TaskSubmit, json!({"prompt": "hello"}));
        let expected_id = request.id.clone();
        let server_task = async move {
            let received = read_request(&mut server).await;
            write_response(
                &mut server,
                &Response {
                    v: PROTOCOL_VERSION,
                    id: received.id,
                    ok: false,
                    result: None,
                    error: Some(RemoteError {
                        code: ErrorCode::NotAuthorized,
                        message: "approval required".to_string(),
                        data: Some(json!({"approval_requests": ["request-1"]})),
                    }),
                },
            )
            .await;
        };
        let client_task = exchange_on_stream(client, request, config(Duration::from_secs(1)));
        let ((), response) = tokio::join!(server_task, client_task);
        let error = response
            .unwrap()
            .into_result()
            .expect_err("remote failure must not become a successful result");
        let Error::Remote(error) = error else {
            panic!("expected typed remote error");
        };
        assert_eq!(error.code, ErrorCode::NotAuthorized);
        assert_eq!(
            error.data,
            Some(json!({"approval_requests": ["request-1"]}))
        );
        assert_eq!(expected_id.as_str().len(), 32);
    }

    #[tokio::test]
    async fn malformed_mismatched_oversized_and_truncated_responses_fail_closed() {
        async fn run(body: Vec<u8>, max: usize) -> Result<Response, ClientError> {
            let (client, mut server) = UnixStream::pair().unwrap();
            let request = Request::with_id(
                Command::PermissionPending,
                json!({}),
                RequestId::parse("expected").unwrap(),
            );
            let server_task = async move {
                let _ = read_request(&mut server).await;
                server.write_all(&body).await.unwrap();
                drop(server);
            };
            let mut settings = config(Duration::from_secs(1));
            settings.max_response_bytes = max;
            let client_task = exchange_on_stream(client, request, settings);
            let ((), result) = tokio::join!(server_task, client_task);
            result
        }

        let malformed = encode_frame(KIND_RESPONSE, b"not-json");
        assert!(matches!(
            run(malformed, MAX_RESPONSE_BYTES).await,
            Err(ClientError::MalformedResponse(_))
        ));

        let mismatched = Response {
            v: PROTOCOL_VERSION,
            id: RequestId::parse("other").unwrap(),
            ok: true,
            result: Some(Value::Null),
            error: None,
        };
        assert!(matches!(
            run(
                encode_frame(KIND_RESPONSE, &serde_json::to_vec(&mismatched).unwrap()),
                MAX_RESPONSE_BYTES
            )
            .await,
            Err(ClientError::MismatchedRequestId { .. })
        ));

        let mut oversized = Vec::from(MAGIC);
        oversized.extend_from_slice(&[KIND_RESPONSE, 0]);
        oversized.extend_from_slice(&1024u32.to_be_bytes());
        assert!(matches!(
            run(oversized, 128).await,
            Err(ClientError::ResponseTooLarge {
                actual: 1024,
                maximum: 128
            })
        ));

        let mut truncated = encode_frame(KIND_RESPONSE, b"123456");
        truncated.truncate(truncated.len() - 2);
        assert!(matches!(
            run(truncated, MAX_RESPONSE_BYTES).await,
            Err(ClientError::TruncatedResponse)
        ));
    }

    #[tokio::test]
    async fn nonresponsive_response_is_bounded_and_closes_the_socket() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let request = Request::new(Command::PermissionPending, json!({}));
        let server_task = async {
            let _ = read_request(&mut server).await;
            tokio::time::sleep(Duration::from_millis(200)).await;
            let mut byte = [0];
            assert_eq!(server.read(&mut byte).await.unwrap(), 0);
        };
        let client_task = exchange_on_stream(client, request, config(Duration::from_millis(50)));
        let ((), result) = tokio::join!(server_task, client_task);
        assert!(matches!(result, Err(ClientError::ReadTimeout)));
    }

    #[tokio::test]
    async fn request_size_is_checked_before_writing() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let request = Request::new(Command::TaskSubmit, json!({"prompt": "too large"}));
        let mut settings = config(Duration::from_secs(1));
        settings.max_request_bytes = 8;
        let result = exchange_on_stream(client, request, settings).await;
        assert!(matches!(result, Err(ClientError::RequestTooLarge { .. })));
        let mut byte = [0];
        assert_eq!(server.read(&mut byte).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn nonresponsive_writer_is_bounded() {
        let (client, _server) = UnixStream::pair().unwrap();
        let request = Request::new(
            Command::TaskSubmit,
            json!({"prompt": "x".repeat(2 * 1024 * 1024)}),
        );
        let mut settings = config(Duration::from_secs(1));
        settings.max_request_bytes = 3 * 1024 * 1024;
        settings.write_timeout = Duration::from_millis(1);
        let result = exchange_on_stream(client, request, settings).await;
        assert!(matches!(result, Err(ClientError::WriteTimeout)));
    }

    #[tokio::test]
    async fn nonresponsive_connector_is_bounded() {
        let result = connect_with_timeout(
            "/run/cos/clawd.sock".to_string(),
            Duration::from_millis(1),
            std::future::pending::<std::io::Result<()>>(),
        )
        .await;
        assert!(matches!(
            result,
            Err(ClientError::ConnectTimeout { path })
                if path == "/run/cos/clawd.sock"
        ));
    }
}
