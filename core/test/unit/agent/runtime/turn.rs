use super::*;
use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
use crate::agent::llm::ToolCall;
use crate::agent::runtime::hooks::{
    global_registry, Hook, HookContext, HookOutcome, ToolDecision, ToolResultSummary,
};
use crate::agent::tools::registry::builtin_only_registry;
use crate::config::AgentConfig;
use std::sync::atomic::{AtomicU32, Ordering};

fn cfg() -> AgentConfig {
    AgentConfig {
        provider: "mock".into(),
        model: "mock-model".into(),
        max_turns: 5,
        max_tokens: 1024,
        temperature: 0.0,
        system_prompt_path: None,
        ..Default::default()
    }
}

fn ctx() -> HookContext {
    HookContext::new("turn-tests".to_string(), "mock", "mock-model".to_string())
}

fn mixed_capability_primitive(command: &str, args: &[String]) -> Result<serde_json::Value, String> {
    let path = args
        .first()
        .ok_or_else(|| "mixed proxy requires a path".to_string())?;
    let verb = match command {
        "read" => crate::caps::Verb::FS_READ,
        "write" => crate::caps::Verb::FS_DELETE,
        other => return Err(format!("unknown mixed command: {other}")),
    };
    crate::caps::require_or_json(verb, crate::caps::Scope::path(path))
        .map_err(|value| value.to_string())?;
    Ok(serde_json::json!({"command": command, "path": path}))
}

/// pre_tool returning Allow runs the tool unmodified and the
/// hook's post_tool sees a successful result summary.
#[tokio::test]
async fn pre_tool_allow_passes_through() {
    struct Spy {
        pre_calls: Arc<AtomicU32>,
        post_success: Arc<AtomicU32>,
    }

    impl Hook for Spy {
        fn name(&self) -> &str {
            "turn-allow-spy"
        }
        fn pre_tool(&self, _c: &HookContext, _t: &ToolCall) -> ToolDecision {
            self.pre_calls.fetch_add(1, Ordering::SeqCst);
            ToolDecision::Allow
        }
        fn post_tool(
            &self,
            _c: &HookContext,
            _t: &ToolCall,
            s: &ToolResultSummary,
        ) -> HookOutcome {
            if s.success {
                self.post_success.fetch_add(1, Ordering::SeqCst);
            }
            HookOutcome::Continue
        }
    }
    let pre = Arc::new(AtomicU32::new(0));
    let post = Arc::new(AtomicU32::new(0));
    global_registry().register(Arc::new(Spy {
        pre_calls: pre.clone(),
        post_success: post.clone(),
    }));

    let cfg = cfg();
    let mock = MockProvider::new(&cfg.model, &cfg);
    mock.push_response(MockResponse::ToolUse(vec![ToolCall {
        id: "c1".into(),
        name: "echo".into(),
        input: serde_json::json!({"text": "hi"}),
    }]));
    mock.push_response(MockResponse::Text("done".into()));
    let provider: Arc<dyn crate::agent::llm::Provider> = Arc::new(mock);
    let tools = builtin_only_registry();
    let llm_tools = tools.as_llm_tools();
    let mut messages = vec![Message {
        role: Role::User,
        content: vec![ContentBlock::Text { text: "go".into() }],
    }];
    let hctx = ctx();

    let _ = run_turn(
        provider.clone(),
        &cfg.model,
        "sys",
        &mut messages,
        &tools,
        &llm_tools,
        cfg.max_tokens,
        cfg.temperature,
        None,
        None,
        Some(&hctx),
        progress::null_progress(),
    )
    .await
    .unwrap();

    global_registry().unregister("turn-allow-spy");

    assert_eq!(pre.load(Ordering::SeqCst), 1);
    assert_eq!(post.load(Ordering::SeqCst), 1);
}

#[test]
fn one_proxy_uses_capability_consent_for_read_and_write_commands() {
    let _lock = crate::test_env::lock_env();
    let tmp = tempfile::tempdir().unwrap();
    let read_path = tmp.path().join("read.txt").to_string_lossy().into_owned();
    let write_path = tmp.path().join("write.txt").to_string_lossy().into_owned();
    let proc_dir = tmp.path().join("proc");
    std::fs::create_dir_all(&proc_dir).unwrap();
    std::fs::write(
        proc_dir.join("registry.json"),
        serde_json::to_vec(&serde_json::json!({
            "sessions": [{
                "session_id": "mixed-session",
                "pid": 0,
                "caps": [{
                    "verb": "fs.read",
                    "scope": {"kind": "path", "value": read_path.clone()}
                }]
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", tmp.path());
    let _log = crate::test_env::TestEnvVarGuard::set("COS_LOG_DIR", tmp.path());
    let _session = crate::test_env::TestEnvVarGuard::set("COS_SESSION", "mixed-session");
    let _mode = crate::test_env::TestEnvVarGuard::set("COS_PERMS_MODE", "strict");

    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(
        crate::agent::tools::cos_proxy::CosPrimitiveTool::new(
            "cos_mixed",
            "mixed capability proxy",
            mixed_capability_primitive,
            &["read", "write"],
        )
        .with_capability_approval(),
    ));
    registry.set_approval(
        crate::agent::runtime::approval::ApprovalGate::new(
            crate::agent::runtime::approval::ApprovalConfig::new()
                .dangerous("cos_mixed")
                .auto_approve("cos_mixed"),
        ),
    );

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let invocation =
        crate::approvals::LocalApprovalInvocation::new("test:mixed-proxy:turn:1").unwrap();
    let exposure = ToolExposureContext::isolated(
        crate::agent::tools::guardrails::Guardrails::permissive(),
    );
    runtime.block_on(invocation.scope(async {
        let read = dispatch_tool(
            &registry,
            &exposure,
            &ToolCall {
                id: "read".to_string(),
                name: "cos_mixed".to_string(),
                input: serde_json::json!({"command": "read", "args": [read_path.clone()]}),
            },
            Some("mixed-session"),
        )
        .await;
        assert!(!read.is_error, "read command should use its held cap: {read:?}");

        let write = dispatch_tool(
            &registry,
            &exposure,
            &ToolCall {
                id: "write".to_string(),
                name: "cos_mixed".to_string(),
                input: serde_json::json!({"command": "write", "args": [write_path.clone()]}),
            },
            Some("mixed-session"),
        )
        .await;
        assert!(write.is_error);
        assert!(
            write.content.contains("\"verb\":\"fs.delete\""),
            "write must reach the exact capability gate, not the coarse tool-name prompt: {}",
            write.content
        );
        let pending = crate::approvals::list_pending();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].scope, crate::caps::Scope::path(write_path));
        assert_eq!(pending[0].risk, Some(crate::caps::Risk::High));
    }));
}

/// pre_tool returning Deny short-circuits the dispatch and feeds
/// the deny reason back as a `tool_result` error block. The real
/// tool is never invoked.
#[tokio::test]
async fn pre_tool_deny_short_circuits_dispatch() {
    struct Denier;
    impl Hook for Denier {
        fn name(&self) -> &str {
            "turn-denier"
        }
        fn pre_tool(&self, _c: &HookContext, _t: &ToolCall) -> ToolDecision {
            ToolDecision::Deny("policy: blocked-in-test".into())
        }
    }
    global_registry().register(Arc::new(Denier));

    let cfg = cfg();
    let mock = MockProvider::new(&cfg.model, &cfg);
    mock.push_response(MockResponse::ToolUse(vec![ToolCall {
        id: "c1".into(),
        name: "echo".into(),
        input: serde_json::json!({"text": "hi"}),
    }]));
    mock.push_response(MockResponse::Text("done".into()));
    let provider: Arc<dyn crate::agent::llm::Provider> = Arc::new(mock);
    let tools = builtin_only_registry();
    let llm_tools = tools.as_llm_tools();
    let mut messages = vec![Message {
        role: Role::User,
        content: vec![ContentBlock::Text { text: "go".into() }],
    }];
    let hctx = ctx();

    let _ = run_turn(
        provider.clone(),
        &cfg.model,
        "sys",
        &mut messages,
        &tools,
        &llm_tools,
        cfg.max_tokens,
        cfg.temperature,
        None,
        None,
        Some(&hctx),
        progress::null_progress(),
    )
    .await
    .unwrap();

    global_registry().unregister("turn-denier");

    // The tool_result message is the last one (User role with ToolResult blocks).
    let last = messages.last().unwrap();
    assert_eq!(last.role, Role::User);
    let block = last.content.first().unwrap();
    match block {
        ContentBlock::ToolResult {
            is_error, content, ..
        } => {
            assert!(*is_error, "tool_result should be an error");
            assert!(content.contains("hook deny"), "got {content}");
            assert!(content.contains("policy: blocked-in-test"), "got {content}");
        }
        other => panic!("expected ToolResult, got {other:?}"),
    }
}

/// pre_tool returning Override substitutes the tool input. Echo
/// tool returns the substituted text, proving the original input
/// was replaced.
#[tokio::test]
async fn pre_tool_override_substitutes_input() {
    struct Overrider;
    impl Hook for Overrider {
        fn name(&self) -> &str {
            "turn-overrider"
        }
        fn pre_tool(&self, _c: &HookContext, _t: &ToolCall) -> ToolDecision {
            ToolDecision::Override(serde_json::json!({"text": "REPLACED"}))
        }
    }
    global_registry().register(Arc::new(Overrider));

    let cfg = cfg();
    let mock = MockProvider::new(&cfg.model, &cfg);
    mock.push_response(MockResponse::ToolUse(vec![ToolCall {
        id: "c1".into(),
        name: "echo".into(),
        input: serde_json::json!({"text": "ORIGINAL"}),
    }]));
    mock.push_response(MockResponse::Text("done".into()));
    let provider: Arc<dyn crate::agent::llm::Provider> = Arc::new(mock);
    let tools = builtin_only_registry();
    let llm_tools = tools.as_llm_tools();
    let mut messages = vec![Message {
        role: Role::User,
        content: vec![ContentBlock::Text { text: "go".into() }],
    }];
    let hctx = ctx();

    let _ = run_turn(
        provider.clone(),
        &cfg.model,
        "sys",
        &mut messages,
        &tools,
        &llm_tools,
        cfg.max_tokens,
        cfg.temperature,
        None,
        None,
        Some(&hctx),
        progress::null_progress(),
    )
    .await
    .unwrap();

    global_registry().unregister("turn-overrider");

    let last = messages.last().unwrap();
    let block = last.content.first().unwrap();
    match block {
        ContentBlock::ToolResult {
            is_error, content, ..
        } => {
            assert!(!*is_error);
            assert!(content.contains("REPLACED"), "got {content}");
            assert!(!content.contains("ORIGINAL"), "got {content}");
        }
        other => panic!("expected ToolResult, got {other:?}"),
    }
}

/// post_tool returning Stop captures a Stop reason but lets the
/// loop finish appending all tool_results before propagating
/// AgentError::Interrupted. This keeps assistant tool_use ↔ user
/// tool_result history balanced.
#[tokio::test]
async fn post_tool_stop_propagates_after_results_appended() {
    struct Stopper;
    impl Hook for Stopper {
        fn name(&self) -> &str {
            "turn-post-stopper"
        }
        fn post_tool(
            &self,
            _c: &HookContext,
            _t: &ToolCall,
            _s: &ToolResultSummary,
        ) -> HookOutcome {
            HookOutcome::Stop("audit-veto".into())
        }
    }
    global_registry().register(Arc::new(Stopper));

    let cfg = cfg();
    let mock = MockProvider::new(&cfg.model, &cfg);
    mock.push_response(MockResponse::ToolUse(vec![
        ToolCall {
            id: "c1".into(),
            name: "echo".into(),
            input: serde_json::json!({"text": "first"}),
        },
        ToolCall {
            id: "c2".into(),
            name: "echo".into(),
            input: serde_json::json!({"text": "second"}),
        },
    ]));
    let provider: Arc<dyn crate::agent::llm::Provider> = Arc::new(mock);
    let tools = builtin_only_registry();
    let llm_tools = tools.as_llm_tools();
    let mut messages = vec![Message {
        role: Role::User,
        content: vec![ContentBlock::Text { text: "go".into() }],
    }];
    let hctx = ctx();

    let result = run_turn(
        provider.clone(),
        &cfg.model,
        "sys",
        &mut messages,
        &tools,
        &llm_tools,
        cfg.max_tokens,
        cfg.temperature,
        None,
        None,
        Some(&hctx),
        progress::null_progress(),
    )
    .await;

    global_registry().unregister("turn-post-stopper");

    match result {
        Err(super::super::loop_::AgentError::Interrupted(reason)) => {
            assert!(reason.contains("audit-veto"), "got {reason}");
            assert!(reason.contains("post_tool"), "got {reason}");
        }
        other => panic!("expected Interrupted, got {other:?}"),
    }
    // History is balanced: assistant message (with two tool_use)
    // and a user message with two tool_result blocks both got
    // appended before the Interrupted bubbled up.
    assert_eq!(messages.len(), 3); // initial user + assistant + tool-results user
    let last = messages.last().unwrap();
    assert_eq!(last.content.len(), 2, "both tool_results appended");
}

/// hook_ctx = None disables all hook dispatch — proves the
/// zero-cost path for callers that don't care about hooks. We
/// register a hook that would Deny if it ran, then verify the
/// real tool ran (not denied) because dispatch was skipped.
#[tokio::test]
async fn hook_ctx_none_skips_dispatch_entirely() {
    struct WouldDeny;
    impl Hook for WouldDeny {
        fn name(&self) -> &str {
            "turn-would-deny"
        }
        fn pre_tool(&self, _c: &HookContext, _t: &ToolCall) -> ToolDecision {
            ToolDecision::Deny("should not run".into())
        }
    }
    global_registry().register(Arc::new(WouldDeny));

    let cfg = cfg();
    let mock = MockProvider::new(&cfg.model, &cfg);
    mock.push_response(MockResponse::ToolUse(vec![ToolCall {
        id: "c1".into(),
        name: "echo".into(),
        input: serde_json::json!({"text": "ORIGINAL"}),
    }]));
    mock.push_response(MockResponse::Text("done".into()));
    let provider: Arc<dyn crate::agent::llm::Provider> = Arc::new(mock);
    let tools = builtin_only_registry();
    let llm_tools = tools.as_llm_tools();
    let mut messages = vec![Message {
        role: Role::User,
        content: vec![ContentBlock::Text { text: "go".into() }],
    }];

    let _ = run_turn(
        provider.clone(),
        &cfg.model,
        "sys",
        &mut messages,
        &tools,
        &llm_tools,
        cfg.max_tokens,
        cfg.temperature,
        None,
        None,
        None, // <— no hook context: dispatch skipped
        progress::null_progress(),
    )
    .await
    .unwrap();

    global_registry().unregister("turn-would-deny");

    let last = messages.last().unwrap();
    let block = last.content.first().unwrap();
    match block {
        ContentBlock::ToolResult {
            is_error, content, ..
        } => {
            assert!(!*is_error, "deny should not have fired");
            assert!(content.contains("ORIGINAL"), "got {content}");
            assert!(!content.contains("should not run"), "got {content}");
        }
        other => panic!("expected ToolResult, got {other:?}"),
    }
}

// ----------------------------------------------------------------
// ProgressSink + parallel-dispatch unit tests
// ----------------------------------------------------------------

use crate::agent::tools::{Tool, ToolResult as TR};

/// Recording sink: every callback captures (id, name, ok?, latency).
/// Used to assert the runtime called us at the right moments with
/// the right arguments.
#[derive(Default)]
struct RecordingProgress {
    starts: std::sync::Mutex<Vec<(String, String)>>,
    results: std::sync::Mutex<Vec<(String, String, bool, usize)>>,
}
impl progress::ProgressSink for RecordingProgress {
    fn on_tool_start(&self, id: &str, name: &str, _input: &serde_json::Value) {
        self.starts
            .lock()
            .unwrap()
            .push((id.to_string(), name.to_string()));
    }
    fn on_tool_result(
        &self,
        id: &str,
        name: &str,
        ok: bool,
        _latency_ms: u64,
        bytes_returned: usize,
        _preview: &str,
    ) {
        self.results.lock().unwrap().push((
            id.to_string(),
            name.to_string(),
            ok,
            bytes_returned,
        ));
    }
}

/// Slow read-only tool. Sleeps for `delay` then returns. Marked
/// `parallel_safe = true` so the dispatch loop runs siblings
/// concurrently. Used to verify the parallel batch actually
/// overlaps work in wall time.
struct SlowReader {
    name: &'static str,
    delay: std::time::Duration,
}
#[async_trait::async_trait]
impl Tool for SlowReader {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "slow read"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({"type":"object"})
    }
    async fn exec(&self, _input: serde_json::Value) -> TR {
        tokio::time::sleep(self.delay).await;
        TR::ok(format!("done {}", self.name))
    }
    fn parallel_safe(&self) -> bool {
        true
    }
}

/// Side-effecting tool. Default `parallel_safe = false`.
struct SerialWriter {
    name: &'static str,
    delay: std::time::Duration,
}
#[async_trait::async_trait]
impl Tool for SerialWriter {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "serial write"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({"type":"object"})
    }
    async fn exec(&self, _input: serde_json::Value) -> TR {
        tokio::time::sleep(self.delay).await;
        TR::ok(format!("wrote {}", self.name))
    }
    // parallel_safe = false (default)
}

fn registry_with(tools_vec: Vec<Arc<dyn Tool>>) -> ToolRegistry {
    let mut r = ToolRegistry::new();
    for t in tools_vec {
        r.register(t);
    }
    r
}

fn exposure() -> ToolExposureContext {
    ToolExposureContext::isolated(crate::agent::tools::guardrails::Guardrails::permissive())
}

fn calls(specs: &[(&str, &str)]) -> Vec<ToolCall> {
    specs
        .iter()
        .map(|(id, name)| ToolCall {
            id: (*id).to_string(),
            name: (*name).to_string(),
            input: serde_json::json!({}),
        })
        .collect()
}

/// Progress sink receives exactly one start + one result per
/// dispatched tool call, in declaration order for the serial
/// path.
#[tokio::test]
async fn progress_sink_fires_for_every_dispatch_in_order() {
    let registry = registry_with(vec![Arc::new(SerialWriter {
        name: "w1",
        delay: std::time::Duration::from_millis(1),
    })]);
    let tool_calls = calls(&[("id-1", "w1"), ("id-2", "w1")]);
    let p = Arc::new(RecordingProgress::default());
    let exposure = exposure();
    let (blocks, stop) =
        dispatch_calls(
            &registry,
            &exposure,
            None,
            &tool_calls,
            None,
            p.as_ref() as &dyn progress::ProgressSink,
            None,
        )
        .await
        .unwrap();
    assert!(stop.is_none());
    assert_eq!(blocks.len(), 2);
    let starts = p.starts.lock().unwrap();
    let results = p.results.lock().unwrap();
    assert_eq!(
        starts.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
        vec!["id-1", "id-2"]
    );
    assert_eq!(
        results
            .iter()
            .map(|(id, _, ok, _)| (id.as_str(), *ok))
            .collect::<Vec<_>>(),
        vec![("id-1", true), ("id-2", true)]
    );
}

/// Parallel-safe tools dispatched concurrently complete in
/// max(durations) rather than sum(durations). Three 100ms
/// sleeps must finish in well under 300ms.
#[tokio::test]
async fn parallel_safe_tools_dispatch_concurrently() {
    let delay = std::time::Duration::from_millis(100);
    let registry = registry_with(vec![
        Arc::new(SlowReader { name: "r1", delay }),
        Arc::new(SlowReader { name: "r2", delay }),
        Arc::new(SlowReader { name: "r3", delay }),
    ]);
    let tool_calls = calls(&[("a", "r1"), ("b", "r2"), ("c", "r3")]);
    let p = progress::null_progress();
    let started = std::time::Instant::now();
    let exposure = exposure();
    let (blocks, _) =
        dispatch_calls(
            &registry,
            &exposure,
            None,
            &tool_calls,
            None,
            p.as_ref() as &dyn progress::ProgressSink,
            None,
        )
        .await
        .unwrap();
    let elapsed = started.elapsed();
    assert_eq!(blocks.len(), 3);
    // Sequential dispatch would take ~300ms. Concurrent
    // dispatch finishes inside one delay plus scheduling
    // slack; 250ms is a generous upper bound that still proves
    // overlap occurred.
    assert!(
        elapsed < std::time::Duration::from_millis(250),
        "expected concurrent dispatch, got {elapsed:?}"
    );
}

/// `parallel_safe = false` tools serialise even when batched
/// together. Three 80ms serial writers must take at least
/// 240ms total.
#[tokio::test]
async fn serial_tools_remain_sequential() {
    let delay = std::time::Duration::from_millis(80);
    let registry = registry_with(vec![
        Arc::new(SerialWriter { name: "w1", delay }),
        Arc::new(SerialWriter { name: "w2", delay }),
        Arc::new(SerialWriter { name: "w3", delay }),
    ]);
    let tool_calls = calls(&[("a", "w1"), ("b", "w2"), ("c", "w3")]);
    let p = progress::null_progress();
    let started = std::time::Instant::now();
    let exposure = exposure();
    let _ =
        dispatch_calls(
            &registry,
            &exposure,
            None,
            &tool_calls,
            None,
            p.as_ref() as &dyn progress::ProgressSink,
            None,
        )
        .await
        .unwrap();
    let elapsed = started.elapsed();
    assert!(
        elapsed >= std::time::Duration::from_millis(220),
        "expected serial dispatch (~240ms), got {elapsed:?}"
    );
}

/// Mixed batch: result_blocks are returned in original
/// declaration order even when readers (parallel) finish
/// before writers (serial).
#[tokio::test]
async fn mixed_batch_preserves_declaration_order() {
    let fast = std::time::Duration::from_millis(10);
    let slow = std::time::Duration::from_millis(50);
    let registry = registry_with(vec![
        Arc::new(SerialWriter {
            name: "w1",
            delay: slow,
        }),
        Arc::new(SlowReader {
            name: "r1",
            delay: fast,
        }),
        Arc::new(SerialWriter {
            name: "w2",
            delay: slow,
        }),
    ]);
    // Declaration order: w1 (serial), r1 (parallel), w2 (serial).
    let tool_calls = calls(&[("id-w1", "w1"), ("id-r1", "r1"), ("id-w2", "w2")]);
    let p = progress::null_progress();
    let exposure = exposure();
    let (blocks, _) =
        dispatch_calls(
            &registry,
            &exposure,
            None,
            &tool_calls,
            None,
            p.as_ref() as &dyn progress::ProgressSink,
            None,
        )
        .await
        .unwrap();
    let ids: Vec<&str> = blocks
        .iter()
        .map(|b| match b {
            ContentBlock::ToolResult { tool_use_id, .. } => tool_use_id.as_str(),
            _ => panic!("expected ToolResult"),
        })
        .collect();
    assert_eq!(ids, vec!["id-w1", "id-r1", "id-w2"]);
}

struct FixedStreamProvider {
    events:
        std::sync::Mutex<Option<Vec<crate::agent::llm::Result<crate::agent::llm::StreamEvent>>>>,
}

impl FixedStreamProvider {
    fn new(events: Vec<crate::agent::llm::Result<crate::agent::llm::StreamEvent>>) -> Self {
        Self {
            events: std::sync::Mutex::new(Some(events)),
        }
    }
}

#[async_trait::async_trait]
impl crate::agent::llm::Provider for FixedStreamProvider {
    fn name(&self) -> &str {
        "fixed-stream"
    }

    fn supported_models(&self) -> Vec<String> {
        vec!["fixed-stream-model".into()]
    }

    fn is_configured(&self) -> bool {
        true
    }

    async fn chat(&self, _request: ChatRequest) -> crate::agent::llm::Result<ChatResponse> {
        Err(crate::agent::llm::LlmError::Internal(
            "buffered chat is not used by this test provider".into(),
        ))
    }

    async fn chat_stream(
        &self,
        _request: ChatRequest,
    ) -> crate::agent::llm::Result<
        futures_util::stream::BoxStream<
            'static,
            crate::agent::llm::Result<crate::agent::llm::StreamEvent>,
        >,
    > {
        use futures_util::StreamExt;

        let events = self
            .events
            .lock()
            .unwrap()
            .take()
            .expect("chat_stream called once");
        Ok(futures_util::stream::iter(events).boxed())
    }
}

struct CountingSideEffectTool {
    calls: Arc<AtomicU32>,
}

#[async_trait::async_trait]
impl Tool for CountingSideEffectTool {
    fn name(&self) -> &'static str {
        "side_effect"
    }

    fn description(&self) -> &'static str {
        "count executions"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    async fn exec(&self, _input: serde_json::Value) -> TR {
        self.calls.fetch_add(1, Ordering::SeqCst);
        TR::ok("executed")
    }
}

struct CountingDispatchHook {
    calls: Arc<AtomicU32>,
}

impl Hook for CountingDispatchHook {
    fn name(&self) -> &str {
        "malformed-stream-dispatch-spy"
    }

    fn pre_tool(&self, _c: &HookContext, _t: &ToolCall) -> ToolDecision {
        self.calls.fetch_add(1, Ordering::SeqCst);
        ToolDecision::Allow
    }

    fn post_tool(&self, _c: &HookContext, _t: &ToolCall, _s: &ToolResultSummary) -> HookOutcome {
        self.calls.fetch_add(1, Ordering::SeqCst);
        HookOutcome::Continue
    }
}

#[tokio::test]
async fn unterminated_stream_never_appends_or_dispatches_completed_tool() {
    use crate::agent::llm::StreamEvent;

    let provider: Arc<dyn crate::agent::llm::Provider> = Arc::new(FixedStreamProvider::new(vec![
        Ok(StreamEvent::ToolUseStart {
            id: "call_1".into(),
            name: "side_effect".into(),
        }),
        Ok(StreamEvent::ToolInputDelta {
            id: "call_1".into(),
            partial_json: "{\"value\":1}".into(),
        }),
        Ok(StreamEvent::ToolUse(ToolCall {
            id: "call_1".into(),
            name: "side_effect".into(),
            input: serde_json::json!({"value": 1}),
        })),
    ]));
    let calls = Arc::new(AtomicU32::new(0));
    let tools = registry_with(vec![Arc::new(CountingSideEffectTool {
        calls: calls.clone(),
    })]);
    let llm_tools = tools.as_llm_tools();
    let mut messages = vec![Message::user_text("run it")];

    let result = run_turn_streaming(
        provider,
        "fixed-stream-model",
        "sys",
        &mut messages,
        &tools,
        &llm_tools,
        64,
        0.0,
        None,
        crate::agent::llm::accumulate::null_sink(),
        None,
        progress::null_progress(),
    )
    .await;

    assert!(matches!(
        result,
        Err(super::super::loop_::AgentError::Llm(
            crate::agent::llm::LlmError::UpstreamMalformed(message)
        )) if message.contains("terminal Done")
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(messages.len(), 1, "partial assistant output was persisted");
}

#[tokio::test]
async fn malformed_streamed_tool_input_never_runs_hooks_or_dispatch() {
    use crate::agent::llm::StreamEvent;

    let provider: Arc<dyn crate::agent::llm::Provider> = Arc::new(FixedStreamProvider::new(vec![
        Ok(StreamEvent::ToolUseStart {
            id: "call_1".into(),
            name: "side_effect".into(),
        }),
        Ok(StreamEvent::ToolInputDelta {
            id: "call_1".into(),
            partial_json: "{\"value\":".into(),
        }),
        Err(crate::agent::llm::LlmError::UpstreamMalformed(
            "anthropic tool_use input at content block 0".into(),
        )),
    ]));
    let tool_calls = Arc::new(AtomicU32::new(0));
    let hook_calls = Arc::new(AtomicU32::new(0));
    let tools = registry_with(vec![Arc::new(CountingSideEffectTool {
        calls: tool_calls.clone(),
    })]);
    let llm_tools = tools.as_llm_tools();
    let mut messages = vec![Message::user_text("run it")];
    let hook_ctx = ctx();
    global_registry().register(Arc::new(CountingDispatchHook {
        calls: hook_calls.clone(),
    }));

    let result = run_turn_streaming(
        provider,
        "fixed-stream-model",
        "sys",
        &mut messages,
        &tools,
        &llm_tools,
        64,
        0.0,
        None,
        crate::agent::llm::accumulate::null_sink(),
        Some(&hook_ctx),
        progress::null_progress(),
    )
    .await;

    global_registry().unregister("malformed-stream-dispatch-spy");

    assert!(matches!(
        result,
        Err(super::super::loop_::AgentError::Llm(
            crate::agent::llm::LlmError::UpstreamMalformed(message)
        )) if message.contains("tool_use input")
    ));
    assert_eq!(hook_calls.load(Ordering::SeqCst), 0);
    assert_eq!(tool_calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        messages.len(),
        1,
        "malformed assistant output was persisted"
    );
}

#[tokio::test]
async fn unterminated_text_stream_never_appends_partial_answer() {
    use crate::agent::llm::StreamEvent;

    let provider: Arc<dyn crate::agent::llm::Provider> =
        Arc::new(FixedStreamProvider::new(vec![Ok(StreamEvent::TextDelta {
            text: "partial".into(),
        })]));
    let tools = ToolRegistry::new();
    let mut messages = vec![Message::user_text("answer")];

    let result = run_turn_streaming(
        provider,
        "fixed-stream-model",
        "sys",
        &mut messages,
        &tools,
        &[],
        64,
        0.0,
        None,
        crate::agent::llm::accumulate::null_sink(),
        None,
        progress::null_progress(),
    )
    .await;

    assert!(matches!(
        result,
        Err(super::super::loop_::AgentError::Llm(
            crate::agent::llm::LlmError::UpstreamMalformed(_)
        ))
    ));
    assert_eq!(messages.len(), 1, "partial assistant output was persisted");
}

#[tokio::test]
async fn clean_stream_eof_is_a_typed_runtime_failure() {
    let provider: Arc<dyn crate::agent::llm::Provider> =
        Arc::new(FixedStreamProvider::new(Vec::new()));
    let tools = ToolRegistry::new();
    let mut messages = vec![Message::user_text("answer")];

    let result = run_turn_streaming(
        provider,
        "fixed-stream-model",
        "sys",
        &mut messages,
        &tools,
        &[],
        64,
        0.0,
        None,
        crate::agent::llm::accumulate::null_sink(),
        None,
        progress::null_progress(),
    )
    .await;

    assert!(matches!(
        result,
        Err(super::super::loop_::AgentError::Llm(
            crate::agent::llm::LlmError::UpstreamMalformed(_)
        ))
    ));
    assert_eq!(messages.len(), 1);
}
