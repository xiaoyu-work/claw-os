use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

fn sample_tool_call() -> ToolCall {
    ToolCall {
        id: "call_1".to_string(),
        name: "echo".to_string(),
        input: serde_json::json!({"text": "hi"}),
    }
}

/// Counts every callback so tests can assert dispatch order +
/// frequency.
#[derive(Default)]
struct CountingHook {
    name: String,
    pre_turn: AtomicUsize,
    post_turn: AtomicUsize,
    pre_tool: AtomicUsize,
    post_tool: AtomicUsize,
}

impl Hook for CountingHook {
    fn name(&self) -> &str {
        &self.name
    }
    fn pre_turn(&self, _ctx: &HookContext) -> HookOutcome {
        self.pre_turn.fetch_add(1, Ordering::SeqCst);
        HookOutcome::Continue
    }
    fn post_turn(&self, _ctx: &HookContext, _summary: &TurnSummary) -> HookOutcome {
        self.post_turn.fetch_add(1, Ordering::SeqCst);
        HookOutcome::Continue
    }
    fn pre_tool(&self, _ctx: &HookContext, _t: &ToolCall) -> ToolDecision {
        self.pre_tool.fetch_add(1, Ordering::SeqCst);
        ToolDecision::Allow
    }
    fn post_tool(
        &self,
        _ctx: &HookContext,
        _t: &ToolCall,
        _r: &ToolResultSummary,
    ) -> HookOutcome {
        self.post_tool.fetch_add(1, Ordering::SeqCst);
        HookOutcome::Continue
    }
}

fn ctx() -> HookContext {
    HookContext::new("sess-1", "mock", "mock-model")
}

fn turn_summary_ok() -> TurnSummary {
    TurnSummary {
        success: true,
        latency_ms: 42,
        input_tokens: 10,
        output_tokens: 5,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        stop_reason: "Stop".into(),
        tool_calls_made: 0,
        error: None,
    }
}

fn tool_result_ok() -> ToolResultSummary {
    ToolResultSummary {
        tool_name: "echo".into(),
        success: true,
        latency_ms: 1,
        bytes_returned: 12,
        error: None,
    }
}

// ---- HookContext --------------------------------------------------

#[test]
fn hook_context_builder_sets_fields() {
    let c = HookContext::new("s", "p", "m")
        .with_turn_index(7)
        .with_started_at_ms(1_000)
        .with_delegated(true);
    assert_eq!(c.session_id, "s");
    assert_eq!(c.provider, "p");
    assert_eq!(c.model, "m");
    assert_eq!(c.turn_index, 7);
    assert_eq!(c.started_at_ms, 1_000);
    assert!(c.is_delegated);
}

#[test]
fn hook_context_default_started_at_is_recent() {
    let before = now_ms();
    let c = HookContext::new("s", "p", "m");
    let after = now_ms();
    assert!(c.started_at_ms >= before);
    assert!(c.started_at_ms <= after);
}

// ---- Outcomes -----------------------------------------------------

#[test]
fn hook_outcome_is_stop_predicate() {
    assert!(!HookOutcome::Continue.is_stop());
    assert!(HookOutcome::Stop("interrupt".into()).is_stop());
}

#[test]
fn tool_decision_predicates() {
    assert!(ToolDecision::Allow.is_allow());
    assert!(!ToolDecision::Allow.is_deny());
    assert!(ToolDecision::Deny("nope".into()).is_deny());
    assert!(!ToolDecision::Override(serde_json::json!({})).is_allow());
}

// ---- Default trait impls ------------------------------------------

/// A hook that overrides only `name()` should still get default
/// no-op behaviour from the rest.
struct NameOnly;
impl Hook for NameOnly {
    fn name(&self) -> &str {
        "name-only"
    }
}

#[test]
fn default_hook_methods_are_noop_continue() {
    let h = NameOnly;
    assert_eq!(h.pre_turn(&ctx()), HookOutcome::Continue);
    assert_eq!(
        h.post_turn(&ctx(), &turn_summary_ok()),
        HookOutcome::Continue
    );
    assert!(h.pre_tool(&ctx(), &sample_tool_call()).is_allow());
    assert_eq!(
        h.post_tool(&ctx(), &sample_tool_call(), &tool_result_ok()),
        HookOutcome::Continue
    );
}

// ---- Registry -----------------------------------------------------

#[test]
fn registry_starts_empty() {
    let r = HookRegistry::new();
    assert!(r.is_empty());
    assert_eq!(r.len(), 0);
    assert!(r.names().is_empty());
}

#[test]
fn registry_register_appends_and_returns_false_for_new() {
    let r = HookRegistry::new();
    let h = Arc::new(CountingHook {
        name: "a".into(),
        ..Default::default()
    });
    let replaced = r.register(h);
    assert!(!replaced);
    assert_eq!(r.len(), 1);
}

#[test]
fn registry_register_replaces_by_name_and_returns_true() {
    let r = HookRegistry::new();
    let h1 = Arc::new(CountingHook {
        name: "a".into(),
        ..Default::default()
    });
    let h2 = Arc::new(CountingHook {
        name: "a".into(),
        ..Default::default()
    });
    assert!(!r.register(h1));
    assert!(r.register(h2));
    assert_eq!(r.len(), 1);
}

#[test]
fn registry_unregister_returns_true_when_removed() {
    let r = HookRegistry::new();
    let h = Arc::new(CountingHook {
        name: "a".into(),
        ..Default::default()
    });
    r.register(h);
    assert!(r.unregister("a"));
    assert!(!r.unregister("a")); // already gone
    assert!(r.is_empty());
}

#[test]
fn registry_clear_drops_all() {
    let r = HookRegistry::new();
    for n in &["a", "b", "c"] {
        r.register(Arc::new(CountingHook {
            name: (*n).into(),
            ..Default::default()
        }));
    }
    assert_eq!(r.len(), 3);
    r.clear();
    assert!(r.is_empty());
}

#[test]
fn registry_names_preserves_order() {
    let r = HookRegistry::new();
    for n in &["c", "a", "b"] {
        r.register(Arc::new(CountingHook {
            name: (*n).into(),
            ..Default::default()
        }));
    }
    assert_eq!(r.names(), vec!["c", "a", "b"]);
}

// ---- Dispatch -----------------------------------------------------

#[test]
fn dispatch_pre_turn_hits_every_hook_when_all_continue() {
    let r = HookRegistry::new();
    let h1 = Arc::new(CountingHook {
        name: "a".into(),
        ..Default::default()
    });
    let h2 = Arc::new(CountingHook {
        name: "b".into(),
        ..Default::default()
    });
    r.register(h1.clone());
    r.register(h2.clone());

    let outcome = r.dispatch_pre_turn(&ctx());
    assert_eq!(outcome, HookOutcome::Continue);
    assert_eq!(h1.pre_turn.load(Ordering::SeqCst), 1);
    assert_eq!(h2.pre_turn.load(Ordering::SeqCst), 1);
}

/// First-stop-wins: when a hook returns Stop, later hooks
/// should NOT be called.
#[test]
fn dispatch_pre_turn_stops_on_first_stop_and_skips_later_hooks() {
    struct Stopper;
    impl Hook for Stopper {
        fn name(&self) -> &str {
            "stopper"
        }
        fn pre_turn(&self, _ctx: &HookContext) -> HookOutcome {
            HookOutcome::Stop("nope".into())
        }
    }

    let r = HookRegistry::new();
    r.register(Arc::new(Stopper));
    let later = Arc::new(CountingHook {
        name: "later".into(),
        ..Default::default()
    });
    r.register(later.clone());

    let outcome = r.dispatch_pre_turn(&ctx());
    match outcome {
        HookOutcome::Stop(reason) => assert_eq!(reason, "nope"),
        other => panic!("expected Stop, got {other:?}"),
    }
    assert_eq!(later.pre_turn.load(Ordering::SeqCst), 0);
}

#[test]
fn dispatch_post_turn_hits_every_hook() {
    let r = HookRegistry::new();
    let h = Arc::new(CountingHook {
        name: "a".into(),
        ..Default::default()
    });
    r.register(h.clone());
    let _ = r.dispatch_post_turn(&ctx(), &turn_summary_ok());
    assert_eq!(h.post_turn.load(Ordering::SeqCst), 1);
}

#[test]
fn dispatch_pre_tool_first_non_allow_wins() {
    struct Denier;
    impl Hook for Denier {
        fn name(&self) -> &str {
            "denier"
        }
        fn pre_tool(&self, _ctx: &HookContext, _t: &ToolCall) -> ToolDecision {
            ToolDecision::Deny("nope".into())
        }
    }

    let r = HookRegistry::new();
    let allow_first = Arc::new(CountingHook {
        name: "allow".into(),
        ..Default::default()
    });
    r.register(allow_first.clone());
    r.register(Arc::new(Denier));
    let later = Arc::new(CountingHook {
        name: "later".into(),
        ..Default::default()
    });
    r.register(later.clone());

    let decision = r.dispatch_pre_tool(&ctx(), &sample_tool_call());
    assert!(decision.is_deny());

    // First hook ran (Allow); denier ran; later did NOT.
    assert_eq!(allow_first.pre_tool.load(Ordering::SeqCst), 1);
    assert_eq!(later.pre_tool.load(Ordering::SeqCst), 0);
}

#[test]
fn dispatch_pre_tool_override_short_circuits_chain() {
    struct Overrider;
    impl Hook for Overrider {
        fn name(&self) -> &str {
            "overrider"
        }
        fn pre_tool(&self, _ctx: &HookContext, _t: &ToolCall) -> ToolDecision {
            ToolDecision::Override(serde_json::json!({"replaced": true}))
        }
    }

    let r = HookRegistry::new();
    r.register(Arc::new(Overrider));
    let later = Arc::new(CountingHook {
        name: "later".into(),
        ..Default::default()
    });
    r.register(later.clone());

    let decision = r.dispatch_pre_tool(&ctx(), &sample_tool_call());
    match decision {
        ToolDecision::Override(v) => {
            assert_eq!(v["replaced"], serde_json::Value::Bool(true));
        }
        other => panic!("expected Override, got {other:?}"),
    }
    assert_eq!(later.pre_tool.load(Ordering::SeqCst), 0);
}

#[test]
fn dispatch_pre_tool_all_allow_returns_allow() {
    let r = HookRegistry::new();
    for n in &["a", "b", "c"] {
        r.register(Arc::new(CountingHook {
            name: (*n).into(),
            ..Default::default()
        }));
    }
    let decision = r.dispatch_pre_tool(&ctx(), &sample_tool_call());
    assert!(decision.is_allow());
}

#[test]
fn dispatch_post_tool_stop_short_circuits_later_hooks() {
    struct Stopper;
    impl Hook for Stopper {
        fn name(&self) -> &str {
            "stopper"
        }
        fn post_tool(
            &self,
            _ctx: &HookContext,
            _t: &ToolCall,
            _r: &ToolResultSummary,
        ) -> HookOutcome {
            HookOutcome::Stop("done".into())
        }
    }

    let r = HookRegistry::new();
    r.register(Arc::new(Stopper));
    let later = Arc::new(CountingHook {
        name: "later".into(),
        ..Default::default()
    });
    r.register(later.clone());

    let outcome = r.dispatch_post_tool(&ctx(), &sample_tool_call(), &tool_result_ok());
    assert!(outcome.is_stop());
    assert_eq!(later.post_tool.load(Ordering::SeqCst), 0);
}

// ---- Global registry ----------------------------------------------

#[test]
fn global_registry_returns_same_instance() {
    let a = global_registry();
    let b = global_registry();
    // Same Arc<RwLock>; mutating through one is visible through the other.
    let h = Arc::new(CountingHook {
        name: "global-test-hook".into(),
        ..Default::default()
    });
    a.register(h);
    let names = b.names();
    assert!(names.contains(&"global-test-hook".to_string()));
    // Cleanup so we don't leak registrations into other tests.
    b.unregister("global-test-hook");
}

// ---- LoggingHook (smoke test — just confirms it doesn't panic) ----

#[test]
fn logging_hook_callbacks_smoke() {
    let h = LoggingHook;
    assert_eq!(h.name(), "logging");
    assert_eq!(h.pre_turn(&ctx()), HookOutcome::Continue);
    assert_eq!(
        h.post_turn(&ctx(), &turn_summary_ok()),
        HookOutcome::Continue
    );
    assert!(h.pre_tool(&ctx(), &sample_tool_call()).is_allow());
    assert_eq!(
        h.post_tool(&ctx(), &sample_tool_call(), &tool_result_ok()),
        HookOutcome::Continue
    );
}

// ---- AuditHook ----------------------------------------------------

fn read_jsonl(p: &std::path::Path) -> Vec<serde_json::Value> {
    let body = std::fs::read_to_string(p).unwrap_or_default();
    body.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("valid JSON line"))
        .collect()
}

#[test]
fn audit_hook_name_is_audit() {
    let dir = tempfile::tempdir().unwrap();
    let h = AuditHook::at(dir.path().join("audit.jsonl"));
    assert_eq!(h.name(), "audit");
}

#[test]
fn audit_hook_pre_turn_writes_jsonl_event() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("audit.jsonl");
    let h = AuditHook::at(&p);
    assert_eq!(h.pre_turn(&ctx()), HookOutcome::Continue);
    let events = read_jsonl(&p);
    assert_eq!(events.len(), 1);
    let e = &events[0];
    assert_eq!(e["kind"], serde_json::json!("pre_turn"));
    assert_eq!(e["session_id"], serde_json::json!("sess-1"));
    assert_eq!(e["provider"], serde_json::json!("mock"));
    assert_eq!(e["model"], serde_json::json!("mock-model"));
    assert!(e["timestamp"].is_string());
}

#[test]
fn audit_hook_post_turn_records_token_usage() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("audit.jsonl");
    let h = AuditHook::at(&p);
    let mut s = turn_summary_ok();
    s.cache_read_tokens = 7;
    s.cache_write_tokens = 3;
    let _ = h.post_turn(&ctx(), &s);
    let events = read_jsonl(&p);
    assert_eq!(events.len(), 1);
    let e = &events[0];
    assert_eq!(e["kind"], serde_json::json!("post_turn"));
    assert_eq!(e["success"], serde_json::json!(true));
    assert_eq!(e["latency_ms"], serde_json::json!(42));
    assert_eq!(e["input_tokens"], serde_json::json!(10));
    assert_eq!(e["output_tokens"], serde_json::json!(5));
    assert_eq!(e["cache_read_tokens"], serde_json::json!(7));
    assert_eq!(e["cache_write_tokens"], serde_json::json!(3));
    assert_eq!(e["stop_reason"], serde_json::json!("Stop"));
}

#[test]
fn audit_hook_pre_tool_records_call_id_and_name() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("audit.jsonl");
    let h = AuditHook::at(&p);
    let dec = h.pre_tool(&ctx(), &sample_tool_call());
    assert!(dec.is_allow());
    let events = read_jsonl(&p);
    assert_eq!(events.len(), 1);
    let e = &events[0];
    assert_eq!(e["kind"], serde_json::json!("pre_tool"));
    assert_eq!(e["tool_call_id"], serde_json::json!("call_1"));
    assert_eq!(e["tool_name"], serde_json::json!("echo"));
}

#[test]
fn audit_hook_post_tool_records_bytes_and_latency() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("audit.jsonl");
    let h = AuditHook::at(&p);
    let _ = h.post_tool(&ctx(), &sample_tool_call(), &tool_result_ok());
    let events = read_jsonl(&p);
    let e = &events[0];
    assert_eq!(e["kind"], serde_json::json!("post_tool"));
    assert_eq!(e["success"], serde_json::json!(true));
    assert_eq!(e["latency_ms"], serde_json::json!(1));
    assert_eq!(e["bytes_returned"], serde_json::json!(12));
}

#[test]
fn audit_hook_records_error_field_on_failure() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("audit.jsonl");
    let h = AuditHook::at(&p);
    let mut bad = tool_result_ok();
    bad.success = false;
    bad.error = Some("boom".into());
    let _ = h.post_tool(&ctx(), &sample_tool_call(), &bad);
    let events = read_jsonl(&p);
    let e = &events[0];
    assert_eq!(e["success"], serde_json::json!(false));
    assert_eq!(e["error"], serde_json::json!("boom"));
}

#[test]
fn audit_hook_full_lifecycle_writes_four_events_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("audit.jsonl");
    let h = AuditHook::at(&p);
    let _ = h.pre_turn(&ctx());
    let _ = h.pre_tool(&ctx(), &sample_tool_call());
    let _ = h.post_tool(&ctx(), &sample_tool_call(), &tool_result_ok());
    let _ = h.post_turn(&ctx(), &turn_summary_ok());
    let events = read_jsonl(&p);
    let kinds: Vec<&str> = events.iter().map(|e| e["kind"].as_str().unwrap()).collect();
    assert_eq!(
        kinds,
        vec!["pre_turn", "pre_tool", "post_tool", "post_turn"]
    );
}

// ---- CheckpointHook ----------------------------------------------

/// Test creator that just records every `create()` call so tests
/// can assert dispatch behaviour without touching real overlay
/// state. Returns a synthetic id of `cp-N` where N is the call
/// counter; if `fail_with` is `Some(s)` returns `Err(s)` instead.
#[derive(Debug)]
struct RecordingCreator {
    calls: std::sync::Mutex<Vec<String>>,
    fail_with: Option<String>,
}

impl RecordingCreator {
    fn ok() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            calls: std::sync::Mutex::new(Vec::new()),
            fail_with: None,
        })
    }

    fn err(msg: &str) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            calls: std::sync::Mutex::new(Vec::new()),
            fail_with: Some(msg.to_string()),
        })
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl CheckpointCreator for RecordingCreator {
    fn create(&self, description: &str) -> Result<String, String> {
        let mut calls = self.calls.lock().unwrap();
        let id = format!("cp-{}", calls.len() + 1);
        calls.push(description.to_string());
        match &self.fail_with {
            Some(e) => Err(e.clone()),
            None => Ok(id),
        }
    }
}

fn checkpoint_hook_with(
    creator: std::sync::Arc<dyn CheckpointCreator>,
    audit: std::path::PathBuf,
    dangerous: &[&str],
) -> CheckpointHook {
    let set: std::collections::HashSet<String> =
        dangerous.iter().map(|s| s.to_string()).collect();
    CheckpointHook::with_overrides(creator, audit, set)
}

#[test]
fn checkpoint_hook_name_is_canonical() {
    let h = CheckpointHook::with_overrides(
        RecordingCreator::ok(),
        std::env::temp_dir().join("noop.jsonl"),
        std::collections::HashSet::new(),
    );
    assert_eq!(h.name(), "checkpoint");
}

#[test]
fn default_dangerous_tools_includes_expected_set() {
    let s = default_dangerous_tools();
    assert!(s.contains("cos_sandbox"));
    assert!(s.contains("cos_proc"));
    assert!(s.contains("cos_credential"));
    assert!(s.contains("cos_oauth_login"));
    assert!(s.contains("cos_cron"));
    assert!(s.contains("cos_netfilter"));
}

#[test]
fn checkpoint_hook_skips_safe_tool_calls() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("audit.jsonl");
    let creator = RecordingCreator::ok();
    let h = checkpoint_hook_with(
        creator.clone() as std::sync::Arc<dyn CheckpointCreator>,
        p.clone(),
        &["cos_sandbox"],
    );

    let safe = ToolCall {
        id: "call_safe".to_string(),
        name: "cos_sysinfo".to_string(),
        input: serde_json::json!({}),
    };
    let decision = h.pre_tool(&ctx(), &safe);
    assert!(matches!(decision, ToolDecision::Allow));

    // Creator must not have been called.
    assert!(creator.calls().is_empty());
    // No audit entry written either.
    assert!(!p.exists() || std::fs::read_to_string(&p).unwrap().is_empty());
}

#[test]
fn checkpoint_hook_creates_for_dangerous_tool_calls() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("audit.jsonl");
    let creator = RecordingCreator::ok();
    let h = checkpoint_hook_with(
        creator.clone() as std::sync::Arc<dyn CheckpointCreator>,
        p.clone(),
        &["cos_sandbox"],
    );

    let dangerous = ToolCall {
        id: "call_danger".to_string(),
        name: "cos_sandbox".to_string(),
        input: serde_json::json!({"command": "run"}),
    };
    let decision = h.pre_tool(&ctx().with_turn_index(7), &dangerous);
    assert!(
        matches!(decision, ToolDecision::Allow),
        "checkpoint hook is best-effort, never blocks tool dispatch"
    );

    let calls = creator.calls();
    assert_eq!(calls.len(), 1);
    assert!(
        calls[0].contains("cos_sandbox") && calls[0].contains("turn=7"),
        "description should embed tool name and turn: {:?}",
        calls[0]
    );

    let events = read_jsonl(&p);
    assert_eq!(events.len(), 1);
    let e = &events[0];
    assert_eq!(e["kind"], serde_json::json!("pre_tool_checkpoint"));
    assert_eq!(e["status"], serde_json::json!("ok"));
    assert_eq!(e["tool_name"], serde_json::json!("cos_sandbox"));
    assert_eq!(e["tool_call_id"], serde_json::json!("call_danger"));
    assert_eq!(e["checkpoint_id"], serde_json::json!("cp-1"));
    assert!(e["error"].is_null());
}

#[test]
fn checkpoint_hook_logs_failure_but_still_allows_tool() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("audit.jsonl");
    let creator = RecordingCreator::err("overlayfs unavailable");
    let h = checkpoint_hook_with(
        creator.clone() as std::sync::Arc<dyn CheckpointCreator>,
        p.clone(),
        &["cos_sandbox"],
    );

    let dangerous = ToolCall {
        id: "call_danger".to_string(),
        name: "cos_sandbox".to_string(),
        input: serde_json::json!({}),
    };
    let decision = h.pre_tool(&ctx(), &dangerous);
    assert!(
        matches!(decision, ToolDecision::Allow),
        "checkpoint failure must NOT block the tool — best-effort safety"
    );

    // Creator was attempted exactly once.
    assert_eq!(creator.calls().len(), 1);

    let events = read_jsonl(&p);
    assert_eq!(events.len(), 1);
    let e = &events[0];
    assert_eq!(e["kind"], serde_json::json!("pre_tool_checkpoint"));
    assert_eq!(e["status"], serde_json::json!("error"));
    assert_eq!(e["error"], serde_json::json!("overlayfs unavailable"));
    assert!(e["checkpoint_id"].is_null());
}

#[test]
fn checkpoint_hook_only_fires_on_pre_tool_not_other_callbacks() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("audit.jsonl");
    let creator = RecordingCreator::ok();
    let h = checkpoint_hook_with(
        creator.clone() as std::sync::Arc<dyn CheckpointCreator>,
        p.clone(),
        &["cos_sandbox"],
    );
    // pre_turn / post_turn / post_tool must all default to no-op.
    let _ = h.pre_turn(&ctx());
    let _ = h.post_turn(&ctx(), &turn_summary_ok());
    let dangerous = ToolCall {
        id: "id".into(),
        name: "cos_sandbox".into(),
        input: serde_json::json!({}),
    };
    let _ = h.post_tool(&ctx(), &dangerous, &tool_result_ok());
    // No checkpoint was created.
    assert!(creator.calls().is_empty());
    // No audit events written.
    assert!(!p.exists() || std::fs::read_to_string(&p).unwrap().is_empty());
}

#[test]
fn checkpoint_hook_is_dangerous_query_reflects_set() {
    let h = checkpoint_hook_with(
        RecordingCreator::ok() as std::sync::Arc<dyn CheckpointCreator>,
        std::env::temp_dir().join("no.jsonl"),
        &["cos_sandbox", "cos_proc"],
    );
    assert!(h.is_dangerous("cos_sandbox"));
    assert!(h.is_dangerous("cos_proc"));
    assert!(!h.is_dangerous("cos_sysinfo"));
    assert!(!h.is_dangerous("echo"));
}

#[test]
fn checkpoint_hook_default_constructors_use_default_set() {
    let h = CheckpointHook::new();
    for t in default_dangerous_tools() {
        assert!(
            h.is_dangerous(&t),
            "{t} should be in the default dangerous set"
        );
    }
    let h2 = CheckpointHook::with_dangerous(["custom_tool".to_string()].into_iter().collect());
    assert!(h2.is_dangerous("custom_tool"));
    assert!(!h2.is_dangerous("cos_sandbox"));
}
