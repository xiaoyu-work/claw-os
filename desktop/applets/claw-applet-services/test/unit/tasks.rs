use super::*;

/// The `cos agent ls` JSON envelope shape we consume.
#[test]
fn parses_real_kernel_ls_response() {
    let raw = r#"{
        "n": 2,
        "tasks": [
            {
                "id": "ses_0019e2566eb1f_e71a8d6a8ca4",
                "purpose": "smoke test",
                "status": "running",
                "creator_runtime": "smoke",
                "created_at": "2025-01-01T00:00:00Z",
                "ended_at": null,
                "lease": {
                    "pid": 12345,
                    "runtime": "cos-agent",
                    "started_at": "2025-01-01T00:00:01Z",
                    "heartbeat_at": "2025-01-01T00:00:30Z"
                }
            },
            {
                "id": "ses_0019e25670000_aaaaaaaaaaaa",
                "purpose": "",
                "status": "paused",
                "creator_runtime": null,
                "created_at": "2025-01-01T00:01:00Z",
                "ended_at": null,
                "lease": null
            }
        ]
    }"#;
    let env: LsEnvelope = serde_json::from_str(raw).unwrap();
    assert_eq!(env.n, 2);
    assert_eq!(env.tasks[0].status, "running");
    assert!(env.tasks[0].lease.is_some());
    assert_eq!(env.tasks[0].lease.as_ref().unwrap().pid, 12345);
    assert_eq!(env.tasks[1].status, "paused");
    assert!(env.tasks[1].lease.is_none());
    assert!(env.tasks[1].creator_runtime.is_none());
}

#[test]
fn parses_empty_ls_response() {
    let raw = r#"{"n": 0, "tasks": []}"#;
    let env: LsEnvelope = serde_json::from_str(raw).unwrap();
    assert_eq!(env.n, 0);
    assert!(env.tasks.is_empty());
}
