use super::*;

#[test]
fn chat_opts_chaining() {
    let opts = ChatOpts::default()
        .origin("external-content")
        .max_units(2000)
        .app("test-app")
        .tools(["fs.read_text", "kv.get"]);
    assert_eq!(opts.origin.as_deref(), Some("external-content"));
    assert_eq!(opts.max_units, Some(2000));
    assert_eq!(opts.app_id.as_deref(), Some("test-app"));
    assert_eq!(opts.tools.as_ref().unwrap().len(), 2);
}

#[test]
fn chat_rejects_blank_prompt() {
    let err = chat("", ChatOpts::default()).unwrap_err();
    assert!(matches!(err, AiError::InvalidArg(_)));
}

#[test]
fn classify_budget_error_by_code() {
    let payload = serde_json::json!({"error": "out of units", "code": "BUDGET_EXCEEDED"});
    let err = classify_ai_error("out of units", &payload);
    assert!(matches!(err, AiError::BudgetExceeded(_)));
}

#[test]
fn classify_safety_error_by_keyword() {
    let payload = serde_json::json!({"error": "safety blocked"});
    let err = classify_ai_error("safety blocked", &payload);
    assert!(matches!(err, AiError::SafetyViolation(_)));
}

// Silence unused-import warnings for HashMap when feature combos
// exclude every consumer (current code-shape doesn't use it, but
// we leave the import to keep the module ready for future per-call
// metadata).
#[allow(dead_code)]
fn _placate() {}
