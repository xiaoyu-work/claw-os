use super::*;
use crate::agent::tools::{cos_apps_session, registry::ToolRegistry, ToolResult};

pub(super) async fn execute(context: &ProcessContext) -> ToolResult {
    let mut registry = ToolRegistry::new();
    cos_apps_session::register_all(&mut registry);
    let tool = registry
        .get_unfiltered("app_host-process__host_replace")
        .unwrap();
    let mut calls = Vec::new();
    let mut raw_outputs = Vec::new();
    for (index, (path, other)) in [
        (context.input(), context.second_input()),
        (context.second_input(), context.input()),
    ]
    .into_iter()
    .enumerate()
    {
        let output = tool
            .exec(json!({
                "path":path,
                "other":other,
                "expected":context.expected[index].to_string(),
                "body":format!("session call {}\n", index + 1),
            }))
            .await;
        if output.is_error {
            return output;
        }
        let facts: Value = serde_json::from_str(&output.content).unwrap();
        assert_eq!(facts["invocations"], index + 1);
        calls.push(facts);
        raw_outputs.push(output.content);
        if index == 0 {
            std::fs::write(context.app_data().join("between-calls"), b"ready").unwrap();
            tokio::time::timeout(Duration::from_secs(35), async {
                while !context.app_data().join("continue").exists() {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("root never inspected the idle App");
        }
    }
    assert_eq!(calls[0]["session_id"], calls[1]["session_id"]);
    assert_eq!(calls[0]["app_pid"], calls[1]["app_pid"]);
    let at_rest: Value =
        serde_json::from_slice(&std::fs::read(context.app_data().join("at-rest.json")).unwrap())
            .unwrap();
    let closed = registry
        .get_unfiltered("cos_app_session_close")
        .unwrap()
        .exec(json!({"app":APP_ID}))
        .await;
    if closed.is_error {
        return closed;
    }
    ToolResult::ok(
        json!({
            "calls":calls,"raw_outputs":raw_outputs,"at_rest":at_rest,
        })
        .to_string(),
    )
}
