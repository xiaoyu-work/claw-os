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
