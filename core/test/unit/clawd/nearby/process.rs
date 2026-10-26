use super::*;
use tokio::io::{AsyncWriteExt, BufReader};

#[test]
#[ignore = "requires root and bubblewrap; never acquires any wireless radio"]
fn nearby_sandbox_runs_with_the_owner_identity_and_without_a_home_or_system_bus() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    for name in ["incoming", "outgoing"] {
        std::fs::create_dir(root.path().join(name)).unwrap();
        std::fs::set_permissions(
            root.path().join(name),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    let result = sandbox_command(root.path(), 65534, "/usr/bin/id")
        .unwrap()
        .arg("-u")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(String::from_utf8(result.stdout).unwrap().trim(), "65534");
    let result = sandbox_command(root.path(), 65534, "/usr/bin/test")
        .unwrap()
        .args(["!", "-e", "/run/dbus/system_bus_socket"])
        .output()
        .unwrap();
    assert!(result.status.success());
    let result = sandbox_command(root.path(), 65534, "/usr/bin/test")
        .unwrap()
        .args(["!", "-e", "/home"])
        .output()
        .unwrap();
    assert!(result.status.success());
}

#[tokio::test]
async fn nearby_event_frames_survive_a_cancelled_read() {
    let (mut writer, reader) = tokio::io::duplex(1024);
    let mut reader = BufReader::new(reader);
    let mut bytes = Vec::new();
    writer.write_all(b"{\"event\":\"rea").await.unwrap();
    assert!(tokio::time::timeout(
        std::time::Duration::from_millis(10),
        event(&mut reader, &mut bytes)
    )
    .await
    .is_err());
    assert!(!bytes.is_empty());
    writer.write_all(b"dy\"}\n").await.unwrap();
    assert!(matches!(
        event(&mut reader, &mut bytes).await.unwrap(),
        Some(WorkerEvent::Ready {})
    ));
    assert!(bytes.is_empty());
}

#[tokio::test]
async fn nearby_event_frames_refuse_truncation_unknown_fields_and_oversize() {
    for input in [
        b"{\"event\":\"ready\"}".to_vec(),
        b"{\"event\":\"ready\",\"caps\":true}\n".to_vec(),
        vec![b'x'; MAX_FRAME_BYTES + 1],
    ] {
        let mut reader = BufReader::new(input.as_slice());
        let mut bytes = Vec::new();
        assert!(event(&mut reader, &mut bytes).await.is_err());
        assert!(bytes.len() <= MAX_FRAME_BYTES);
    }
}
