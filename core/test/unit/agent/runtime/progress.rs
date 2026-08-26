use super::*;

#[test]
fn null_sink_is_silent() {
    let sink = NullProgressSink;
    // No state is mutated; the test asserts via "compiles + no
    // panic". The real value is documenting the contract.
    sink.on_tool_start("id", "name", &Value::Null);
    sink.on_tool_result("id", "name", true, 0, 0, "");
}

#[test]
fn render_preview_passes_through_short_text() {
    assert_eq!(render_preview("hello", true), "hello");
}

#[test]
fn render_preview_pretty_prints_json_objects() {
    let raw = r#"{"a":1,"b":[2,3]}"#;
    let out = render_preview(raw, true);
    assert!(out.contains("\"a\": 1"), "{out}");
    assert!(out.contains("\"b\": ["), "{out}");
}

#[test]
fn render_preview_truncates_long_success_bodies() {
    let body = "x".repeat(DEFAULT_PREVIEW_BYTES * 4);
    let out = render_preview(&body, true);
    assert!(out.len() < body.len());
    assert!(out.contains("truncated"));
}

#[test]
fn render_preview_never_truncates_errors() {
    let body = "x".repeat(DEFAULT_PREVIEW_BYTES * 4);
    let out = render_preview(&body, false);
    assert_eq!(out.len(), body.len());
    assert!(!out.contains("truncated"));
}

#[test]
fn truncate_respects_utf8_boundary() {
    // "中" is 3 bytes in UTF-8. Build a string whose char
    // boundary lands awkwardly relative to a byte limit.
    let s = "abc中文中文";
    // Cap right in the middle of the first multi-byte char.
    let out = truncate_utf8(s, 4);
    // Must end on a char boundary — std::str slicing would have
    // panicked otherwise. Verify the prefix decodes cleanly.
    assert!(out.starts_with("abc"));
    assert!(out.is_char_boundary(out.find('\n').unwrap_or(out.len())));
}

#[test]
fn format_header_includes_all_fields() {
    let h = format_result_header("id-1", "cos_sysinfo", true, 1234, 56);
    assert!(h.contains("id=id-1"));
    assert!(h.contains("name=cos_sysinfo"));
    assert!(h.contains("ok"));
    assert!(!h.contains("ERROR"));
    assert!(h.contains("ms=1234"));
    assert!(h.contains("bytes=56"));
}

#[test]
fn format_header_renders_error_status() {
    let h = format_result_header("id-1", "cos_sysinfo", false, 0, 0);
    assert!(h.contains("ERROR"));
    assert!(!h.contains(" ok "));
}

#[tokio::test]
async fn heartbeat_stop_cancels_outstanding_tick() {
    // Smoke test: start a heartbeat, stop it before the first
    // interval tick (2s) elapses. We can't easily observe stderr
    // without TTY plumbing, so the test verifies the registry
    // bookkeeping: after stop(), the inflight map is empty.
    let hb = Heartbeat::new();
    hb.start("tool-x", "");
    assert!(hb.inflight.lock().unwrap().contains_key("tool-x"));
    hb.stop("tool-x");
    assert!(!hb.inflight.lock().unwrap().contains_key("tool-x"));
}

#[tokio::test]
async fn heartbeat_stop_is_idempotent_on_unknown_id() {
    let hb = Heartbeat::new();
    // Stop with no matching start should be a silent no-op.
    hb.stop("never-started");
    assert!(hb.inflight.lock().unwrap().is_empty());
}
