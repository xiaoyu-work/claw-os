use super::*;

#[test]
fn call_rejects_blank_name() {
    let err = call("", &serde_json::json!({})).unwrap_err();
    assert!(matches!(err, ToolError::InvalidArg(_)));
}

#[test]
fn for_chat_passes_through() {
    let names = for_chat(["fs.read_text", "kv.get"]);
    assert_eq!(names, vec!["fs.read_text", "kv.get"]);
}
