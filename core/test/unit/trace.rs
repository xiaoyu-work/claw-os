use super::*;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, Once};

static COUNTER: AtomicU32 = AtomicU32::new(0);
/// Mutex to serialize tests that manipulate process-global env vars.
static ENV_LOCK: Mutex<()> = Mutex::new(());
static TRACE_INIT: Once = Once::new();

fn unique_trace() -> String {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    TRACE_INIT.call_once(|| {
        let dir = std::env::temp_dir().join(format!("cos-test-shared-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        std::env::set_var("COS_DATA_DIR", &dir);
        // Tests don't set COS_SESSION; the new caps gate (strict by
        // default) would deny every trace op without an explicit
        // permissive override.
        std::env::set_var("COS_PERMS_MODE", "permissive");
    });
    // Clear trace env vars to prevent cross-test pollution
    std::env::remove_var("COS_TRACE_ID");
    std::env::remove_var("COS_SPAN_ID");
    format!("test-trace-{n}")
}

#[test]
fn start_creates_trace_file() {
    let _lock = ENV_LOCK.lock().unwrap();
    let id = unique_trace();
    let r = cmd_start(&[id.clone()]).unwrap();
    assert_eq!(r["trace_id"], id);
    assert_eq!(r["status"], "active");
    assert!(trace_path(&id).exists());
}

#[test]
fn end_updates_trace() {
    let _lock = ENV_LOCK.lock().unwrap();
    let id = unique_trace();
    cmd_start(&[id.clone()]).unwrap();
    let r = cmd_end(&[id.clone()]).unwrap();
    assert_eq!(r["status"], "completed");
    assert!(r["ended_at"].is_string());
    assert!(r["duration_ms"].is_number());
}

#[test]
fn end_with_failed_status() {
    let _lock = ENV_LOCK.lock().unwrap();
    let id = unique_trace();
    cmd_start(&[id.clone()]).unwrap();
    let r = cmd_end(&[id.clone(), "--status".into(), "failed".into()]).unwrap();
    assert_eq!(r["status"], "failed");
}

#[test]
fn span_adds_to_trace() {
    let _lock = ENV_LOCK.lock().unwrap();
    let id = unique_trace();
    cmd_start(&[id.clone()]).unwrap();
    std::env::set_var("COS_TRACE_ID", &id);

    let r = cmd_span(&["analyze".into()]).unwrap();
    assert_eq!(r["span"], "analyze");
    assert_eq!(r["span_path"], "analyze");

    // Verify span in trace file
    let data = fs::read_to_string(trace_path(&id)).unwrap();
    let trace: TraceInfo = serde_json::from_str(&data).unwrap();
    assert_eq!(trace.spans.len(), 1);
    assert_eq!(trace.spans[0].name, "analyze");

    std::env::remove_var("COS_TRACE_ID");
}

#[test]
fn nested_span() {
    let _lock = ENV_LOCK.lock().unwrap();
    let id = unique_trace();
    cmd_start(&[id.clone()]).unwrap();
    std::env::set_var("COS_TRACE_ID", &id);
    std::env::set_var("COS_SPAN_ID", "parent");

    let r = cmd_span(&["child".into()]).unwrap();
    assert_eq!(r["span_path"], "parent/child");

    std::env::remove_var("COS_TRACE_ID");
    std::env::remove_var("COS_SPAN_ID");
}

#[test]
fn span_end_closes_span() {
    let _lock = ENV_LOCK.lock().unwrap();
    let id = unique_trace();
    cmd_start(&[id.clone()]).unwrap();
    std::env::set_var("COS_TRACE_ID", &id);

    cmd_span(&["test-span".into()]).unwrap();
    std::env::set_var("COS_SPAN_ID", "test-span");

    let r = cmd_span_end(&[]).unwrap();
    assert_eq!(r["span"], "test-span");
    assert!(r["ended_at"].is_string());

    std::env::remove_var("COS_TRACE_ID");
    std::env::remove_var("COS_SPAN_ID");
}

#[test]
fn show_returns_tree() {
    let _lock = ENV_LOCK.lock().unwrap();
    let id = unique_trace();
    cmd_start(&[id.clone()]).unwrap();

    // Show should work even with no spans
    let r = cmd_show(&[id.clone()]).unwrap();
    assert_eq!(r["trace_id"], id);
    assert!(r["spans"].is_array());
    assert!(r["summary"].is_object());
}

#[test]
fn list_shows_traces() {
    let _lock = ENV_LOCK.lock().unwrap();
    let id = unique_trace();
    cmd_start(&[id.clone()]).unwrap();

    let r = cmd_list(&[]).unwrap();
    assert!(r["count"].as_u64().unwrap() >= 1);
    let traces = r["traces"].as_array().unwrap();
    assert!(traces.iter().any(|t| t["trace_id"] == id));
}

#[test]
fn list_filter_by_status() {
    let _lock = ENV_LOCK.lock().unwrap();
    let id = unique_trace();
    cmd_start(&[id.clone()]).unwrap();
    cmd_end(&[id.clone()]).unwrap();

    let r = cmd_list(&["--status".into(), "completed".into()]).unwrap();
    let traces = r["traces"].as_array().unwrap();
    assert!(traces.iter().all(|t| t["status"] == "completed"));
}

#[test]
fn start_missing_id() {
    let _lock = ENV_LOCK.lock().unwrap();
    let r = cmd_start(&[]);
    assert!(r.is_err());
}

#[test]
fn show_nonexistent() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _ = unique_trace(); // set up temp dir
    let r = cmd_show(&["nonexistent-trace".into()]);
    assert!(r.is_err());
}

#[test]
fn run_dispatch() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _ = unique_trace();
    let r = run("list", &[]).unwrap();
    assert!(r["traces"].is_array());

    let r = run("bogus", &[]);
    assert!(r.is_err());
}
