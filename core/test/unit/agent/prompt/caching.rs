use super::*;
use crate::agent::llm::Message;

fn req() -> ChatRequest {
    ChatRequest {
        model: "claude-sonnet-4.6".into(),
        messages: vec![
            Message::user_text("first"),
            Message::assistant_text("answer"),
            Message::user_text("follow-up"),
        ],
        system: Some("be helpful".into()),
        tools: Vec::new(),
        tool_choice: Default::default(),
        max_tokens: None,
        temperature: None,
        top_p: None,
        stop_sequences: Vec::new(),
        extra: serde_json::Value::Null,
    }
}

#[test]
fn no_breakpoints_by_default() {
    let r = req();
    assert!(get_breakpoints(&r).is_empty());
    assert!(!is_system_cached(&r));
    assert!(!is_tools_cached(&r));
}

#[test]
fn mark_one_breakpoint_round_trips() {
    let mut r = req();
    mark_breakpoint(&mut r, 0).unwrap();
    assert_eq!(get_breakpoints(&r), vec![0]);
}

#[test]
fn mark_multiple_breakpoints_returns_sorted() {
    let mut r = req();
    mark_breakpoint(&mut r, 2).unwrap();
    mark_breakpoint(&mut r, 0).unwrap();
    mark_breakpoint(&mut r, 1).unwrap();
    assert_eq!(get_breakpoints(&r), vec![0, 1, 2]);
}

#[test]
fn mark_duplicate_returns_error() {
    let mut r = req();
    mark_breakpoint(&mut r, 0).unwrap();
    let err = mark_breakpoint(&mut r, 0).unwrap_err();
    assert_eq!(err, CacheMarkError::Duplicate(0));
}

#[test]
fn mark_at_limit_returns_error() {
    let mut r = req();
    for i in 0..MAX_CACHE_BREAKPOINTS as u32 {
        mark_breakpoint(&mut r, i).unwrap();
    }
    let err = mark_breakpoint(&mut r, MAX_CACHE_BREAKPOINTS as u32).unwrap_err();
    assert_eq!(err, CacheMarkError::AtLimit);
}

#[test]
fn set_breakpoints_truncates_and_dedups() {
    let mut r = req();
    set_breakpoints(&mut r, vec![5, 5, 4, 3, 2, 1, 0]);
    let bp = get_breakpoints(&r);
    assert_eq!(bp.len(), MAX_CACHE_BREAKPOINTS);
    assert_eq!(bp, vec![0, 1, 2, 3]);
}

#[test]
fn set_breakpoints_empty_clears() {
    let mut r = req();
    mark_breakpoint(&mut r, 0).unwrap();
    set_breakpoints(&mut r, Vec::new());
    assert!(get_breakpoints(&r).is_empty());
}

#[test]
fn mark_system_and_tools_round_trip() {
    let mut r = req();
    mark_system_cached(&mut r);
    mark_tools_cached(&mut r);
    assert!(is_system_cached(&r));
    assert!(is_tools_cached(&r));
}

#[test]
fn consume_markers_clears_them() {
    let mut r = req();
    mark_breakpoint(&mut r, 1).unwrap();
    mark_system_cached(&mut r);
    mark_tools_cached(&mut r);
    let consumed = consume_markers(&mut r);
    assert_eq!(consumed.breakpoints, vec![1]);
    assert!(consumed.cache_system);
    assert!(consumed.cache_tools);
    assert!(get_breakpoints(&r).is_empty());
    assert!(!is_system_cached(&r));
    assert!(!is_tools_cached(&r));
    // extra normalised to null when empty.
    assert!(r.extra.is_null());
}

#[test]
fn consume_markers_preserves_other_extras() {
    let mut r = req();
    r.extra = serde_json::json!({"metadata": {"user": "u-1"}});
    mark_breakpoint(&mut r, 0).unwrap();
    let consumed = consume_markers(&mut r);
    assert_eq!(consumed.breakpoints, vec![0]);
    // metadata preserved
    assert_eq!(
        r.extra.as_object().and_then(|o| o.get("metadata")),
        Some(&serde_json::json!({"user": "u-1"}))
    );
}

#[test]
fn extra_starts_as_non_object_marker_works() {
    let mut r = req();
    r.extra = serde_json::Value::Null;
    mark_breakpoint(&mut r, 1).unwrap();
    assert_eq!(get_breakpoints(&r), vec![1]);
}

#[test]
fn extra_starts_as_string_replaced_by_object() {
    // Non-object extras are clobbered; this is intentional —
    // pass-through of raw strings has no provider-side meaning.
    let mut r = req();
    r.extra = serde_json::Value::String("garbage".into());
    mark_system_cached(&mut r);
    assert!(is_system_cached(&r));
    assert!(r.extra.is_object());
}

#[test]
fn clear_markers_alias_works() {
    let mut r = req();
    mark_breakpoint(&mut r, 0).unwrap();
    mark_system_cached(&mut r);
    clear_markers(&mut r);
    assert!(get_breakpoints(&r).is_empty());
    assert!(!is_system_cached(&r));
}

#[test]
fn get_breakpoints_handles_garbage_array_entries() {
    let mut r = req();
    r.extra = serde_json::json!({KEY_BREAKPOINTS: [0, "bad", -1, 2.5, 3]});
    // Only valid u32 entries survive.
    let bp = get_breakpoints(&r);
    assert_eq!(bp, vec![0, 3]);
}

#[test]
fn get_breakpoints_handles_extra_not_object() {
    let mut r = req();
    r.extra = serde_json::Value::Bool(true);
    assert!(get_breakpoints(&r).is_empty());
}
