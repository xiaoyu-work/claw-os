use super::*;

#[test]
fn ask_response_hides_internal_evidence_markers() {
    let response = task_result_to_ask_response(json!({
        "id": "task-1",
        "status": "ok",
        "response": "Network is idle. [evidence:call_1 confidence=0.95]",
        "evidence": {"status": "verified"},
    }))
    .unwrap();
    assert_eq!(response["answer"], "Network is idle.");
    assert_eq!(response["evidence"]["status"], "verified");
    assert!(response.get("stream_requested").is_none());
}

#[test]
fn activity_submit_flag_preserves_legacy_options_and_broker_parameters() {
    let args = [
        "one step",
        "--session",
        "session-id",
        "--activity",
        "activity-id",
        "--max-turns",
        "4",
    ]
    .map(str::to_string);
    assert_eq!(
        service_submit_params(&args).unwrap(),
        json!({
            "prompt": "one step",
            "session_id": "session-id",
            "activity_id": "activity-id",
            "max_turns": 4,
        })
    );
    assert_eq!(
        service_submit_params(&["legacy".to_string()]).unwrap(),
        json!({"prompt": "legacy"})
    );
}

#[test]
fn activity_list_flag_preserves_status_limit_and_all_options() {
    let args = ["--activity", "activity-id", "--status", "error", "--limit", "2"]
        .map(str::to_string);
    assert_eq!(
        service_list_params(&args).unwrap(),
        json!({"activity_id": "activity-id", "status": "error", "limit": 2})
    );
    let args = ["--activity", "activity-id", "--all"].map(str::to_string);
    assert_eq!(
        service_list_params(&args).unwrap(),
        json!({"activity_id": "activity-id", "limit": u64::MAX})
    );
    assert_eq!(service_list_params(&[]).unwrap(), json!({}));
}

#[test]
fn activity_flags_require_a_value() {
    for args in [
        vec!["--activity".to_string()],
        vec!["--activity".to_string(), " ".to_string()],
    ] {
        assert!(service_list_params(&args).unwrap_err().contains("--activity"));
        let mut submit = vec!["work".to_string()];
        submit.extend(args);
        assert!(service_submit_params(&submit).unwrap_err().contains("--activity"));
    }
}

#[test]
fn ask_response_preserves_activity_association() {
    let response = task_result_to_ask_response(json!({
        "id": "task-1",
        "status": "ok",
        "response": "one step done",
        "session_id": "session-1",
        "activity_id": "activity-1",
    }))
    .unwrap();
    assert_eq!(response["activity_id"], "activity-1");
    assert_eq!(response["session_id"], "session-1");
}
