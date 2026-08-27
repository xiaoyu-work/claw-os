use super::*;

#[test]
fn assistant_row_splits_text_and_tool_use() {
    let body = "Let me check.\n[tool_use:cos_sysinfo] {\"command\":\"largest_files\",\"args\":[\"/\"]}";
    let p = parse_stored_content("assistant", body);
    assert_eq!(p.text, "Let me check.");
    assert_eq!(p.tool_calls.len(), 1);
    assert_eq!(p.tool_calls[0]["name"], "cos_sysinfo");
    assert_eq!(p.tool_calls[0]["input"]["command"], "largest_files");
}

#[test]
fn assistant_history_hides_legacy_evidence_markers() {
    let body = "Network is idle. [evidence:call_1 confidence=0.95]";
    let parsed = parse_stored_content("assistant", body);
    assert_eq!(parsed.text, "Network is idle.");
}

#[test]
fn assistant_row_with_only_tool_use_has_empty_text() {
    let body = "[tool_use:cos_sysinfo] {\"command\":\"info\"}";
    let p = parse_stored_content("assistant", body);
    assert!(p.text.is_empty());
    assert_eq!(p.tool_calls.len(), 1);
}

#[test]
fn user_row_tool_result_is_extracted() {
    let body = "[tool_result] {\"files\":[]}";
    let p = parse_stored_content("user", body);
    assert!(p.text.is_empty());
    assert_eq!(p.tool_results.len(), 1);
    assert_eq!(p.tool_results[0]["text"], "{\"files\":[]}");
    assert_eq!(p.tool_results[0]["is_error"], false);
}

#[test]
fn tool_result_error_marker_sets_is_error() {
    let body = "[tool_result:error] EACCES";
    let p = parse_stored_content("user", body);
    assert_eq!(p.tool_results[0]["is_error"], true);
    assert_eq!(p.tool_results[0]["text"], "EACCES");
}

#[test]
fn multiline_tool_result_body_is_captured() {
    let body = "[tool_result] {\n  \"a\": 1,\n  \"b\": 2\n}";
    let p = parse_stored_content("user", body);
    assert_eq!(p.tool_results.len(), 1);
    let txt = p.tool_results[0]["text"].as_str().unwrap();
    assert!(txt.contains("\"a\": 1"));
    assert!(txt.contains("\"b\": 2"));
}

#[test]
fn malformed_tool_use_stays_in_text() {
    let body = "Plain prose\n[tool_use:unterminated";
    let p = parse_stored_content("assistant", body);
    assert!(p.tool_calls.is_empty());
    assert!(p.text.contains("[tool_use:unterminated"));
}

#[test]
fn empty_body_yields_empty_parsed_row() {
    let body = "";
    let p = parse_stored_content("user", body);
    assert!(p.text.is_empty());
    assert!(p.tool_calls.is_empty());
    assert!(p.tool_results.is_empty());
}

#[test]
fn load_history_excludes_injected_rows_before_applying_limit() {
    let tmp = tempfile::tempdir().unwrap();
    let db = MemoryDb::open(tmp.path().join("memory.db")).unwrap();
    db.record_message("session", "user", "hi").unwrap();
    db.record_injected("session", "skills_catalog", "catalog")
        .unwrap();
    db.record_injected("session", "memory_notes", "notes")
        .unwrap();
    db.record_message("session", "assistant", "hello").unwrap();

    let messages = load_history(&db, "session", 2).unwrap();

    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, "user");
    assert_eq!(messages[0].text, "hi");
    assert_eq!(messages[1].role, "assistant");
    assert_eq!(messages[1].text, "hello");
}
