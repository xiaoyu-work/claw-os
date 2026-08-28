use super::*;

#[test]
fn start_result_matches_exec_app_wire_response() {
    let result: StartResult = serde_json::from_value(serde_json::json!({
        "pid": 42,
        "command": ["cos-agent-ui", "--overlay"]
    }))
    .unwrap();

    assert_eq!(result.pid, 42);
    assert_eq!(result.command, ["cos-agent-ui", "--overlay"]);
}
