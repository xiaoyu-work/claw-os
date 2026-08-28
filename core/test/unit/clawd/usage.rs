use super::*;

fn write_usage(root: &std::path::Path, uid: u32, input_tokens: u32) {
    let path = root
        .join("users")
        .join(uid.to_string())
        .join("logs")
        .join("ai.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        path,
        format!(
            "{}\n",
            serde_json::json!({
                "timestamp": "2026-08-27T12:00:00Z",
                "provider": "anthropic",
                "model": "claude-sonnet",
                "duration_ms": 10,
                "input_tokens": input_tokens,
                "output_tokens": 5,
                "finish_reason": "stop",
                "status": "ok"
            })
        ),
    )
    .unwrap();
}

fn client(uid: u32) -> ClientIdentity {
    ClientIdentity {
        pid: Some(42),
        uid: Some(uid),
        gid: Some(uid),
        start_time_ticks: Some(1),
    }
}

#[test]
fn usage_query_is_partitioned_by_authenticated_peer_uid() {
    let _lock = crate::test_env::lock_env();
    let data = tempfile::tempdir().unwrap();
    let _data_dir = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", data.path());
    write_usage(data.path(), 1001, 120);
    write_usage(data.path(), 1002, 990);

    let first = query_blocking(serde_json::json!({"args": ["overall"]}), &client(1001)).unwrap();
    let second = query_blocking(serde_json::json!({"args": ["overall"]}), &client(1002)).unwrap();

    assert_eq!(first["total"]["input_tokens"], 120);
    assert_eq!(second["total"]["input_tokens"], 990);
    assert_ne!(first["log"], second["log"]);
}

#[test]
fn usage_query_requires_authenticated_uid() {
    let error = query_blocking(
        serde_json::json!({"args": ["overall"]}),
        &ClientIdentity::unknown(),
    )
    .unwrap_err();
    assert!(error.contains("uid"));
}

#[cfg(unix)]
#[test]
fn usage_query_rejects_symlinked_owner_log_directory() {
    use std::os::unix::fs::symlink;

    let _lock = crate::test_env::lock_env();
    let data = tempfile::tempdir().unwrap();
    let _data_dir = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", data.path());
    write_usage(data.path(), 1002, 990);
    let owner = data.path().join("users").join("1001");
    std::fs::create_dir_all(&owner).unwrap();
    symlink(
        data.path().join("users").join("1002").join("logs"),
        owner.join("logs"),
    )
    .unwrap();

    let error =
        query_blocking(serde_json::json!({"args": ["overall"]}), &client(1001)).unwrap_err();
    assert!(error.contains("open AI usage log"));
}

#[test]
fn usage_query_rejects_oversized_log_before_aggregation() {
    let _lock = crate::test_env::lock_env();
    let data = tempfile::tempdir().unwrap();
    let _data_dir = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", data.path());
    write_usage(data.path(), 1001, 120);
    let path = data
        .path()
        .join("users")
        .join("1001")
        .join("logs")
        .join("ai.jsonl");
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_len(crate::agent::llm::usage::MAX_QUERY_BYTES + 1)
        .unwrap();

    let error =
        query_blocking(serde_json::json!({"args": ["overall"]}), &client(1001)).unwrap_err();
    assert!(error.contains("query limit"));
}
