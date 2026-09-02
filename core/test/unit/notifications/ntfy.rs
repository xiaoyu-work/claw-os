use super::*;
use crate::notifications::{
    DeliveryPolicy, Notification, NotificationState, SCHEMA_VERSION,
};

fn notification() -> Notification {
    Notification {
        schema: SCHEMA_VERSION,
        sequence: 1,
        id: "notif-test".to_string(),
        owner_uid: 1000,
        source: "agent".to_string(),
        kind: "agent.failed".to_string(),
        severity: Severity::Error,
        title: "Agent task failed".to_string(),
        body: "Open the Agent for details.".to_string(),
        delivery_policy: DeliveryPolicy::Immediate,
        dedupe_key: None,
        task_id: Some("task-1".to_string()),
        session_id: None,
        job_id: None,
        state: NotificationState::Unread,
        occurrences: 1,
        created_at_ms: 1,
        updated_at_ms: 1,
        expires_at_ms: None,
        read_at_ms: None,
        acknowledged_at_ms: None,
        dismissed_at_ms: None,
        actions: Vec::new(),
        deliveries: Vec::new(),
    }
}

#[test]
fn endpoint_encodes_topic_as_one_path_segment() {
    let target = NtfyTarget {
        server: "https://ntfy.sh/base".to_string(),
        topic: "alerts+prod".to_string(),
        bearer_token: None,
    };
    assert_eq!(
        NtfyAdapter::endpoint(&target).unwrap().as_str(),
        "https://ntfy.sh/base/alerts+prod"
    );
}

#[test]
fn endpoint_rejects_invalid_topic() {
    let target = NtfyTarget {
        server: "https://ntfy.sh".to_string(),
        topic: "../admin".to_string(),
        bearer_token: None,
    };
    assert!(matches!(
        NtfyAdapter::endpoint(&target),
        Err(DeliveryError::InvalidTarget(_))
    ));
}

#[tokio::test]
async fn adapter_posts_notification_to_ntfy_endpoint() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = vec![0_u8; 8 * 1024];
        let read = socket.read(&mut request).await.unwrap();
        let request = String::from_utf8_lossy(&request[..read]).to_string();
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}")
            .await
            .unwrap();
        request
    });

    let adapter = NtfyAdapter::default();
    adapter
        .deliver(
            &notification(),
            &NtfyTarget {
                server: format!("http://{address}"),
                topic: "alerts".to_string(),
                bearer_token: Some("test-token".to_string()),
            },
        )
        .await
        .unwrap();
    let request = server.await.unwrap();
    assert!(request.starts_with("POST /alerts HTTP/1.1"));
    assert!(request.contains("authorization: Bearer test-token"));
    assert!(request.contains("title: Agent task failed"));
    assert!(request.ends_with("Open the Agent for details."));
}
