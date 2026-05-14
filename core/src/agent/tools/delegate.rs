//! `cos_delegate` — spawn a sub-agent with a scoped tool subset.
//!
//! Lets a parent agent fan a focused sub-task out to a child agent that has
//! a different (usually narrower) set of tools, and optionally a different
//! model/provider. The child runs a complete, fresh `ask_with` loop and
//! returns its final assistant text to the parent as a tool result.
//!
//! Why a scoped sub-agent matters:
//!
//!   * **Blast radius**: parent picks the subset of tools the child can use
//!     (e.g. only `echo` + `cos_sysinfo`), so an over-eager child can't
//!     touch credentials or the sandbox unless explicitly allowed.
//!   * **Context isolation**: the child has a fresh trajectory, so its
//!     turns don't pollute the parent's prompt window. Useful for
//!     long-running research / extraction tasks the parent only needs the
//!     final summary of.
//!   * **Model choice**: the parent might be a 200B-class planner and the
//!     child a small fast worker. `provider` / `model` overrides let the
//!     parent route by cost.
//!
//! ## Recursion bounding
//!
//! Each call increments a [`DELEGATE_DEPTH`] tokio task-local. If a
//! delegated child also calls `cos_delegate`, depth = 2; once depth would
//! exceed `max_depth` (default 3, cap 5) the call is refused with a tool
//! error. This prevents runaway delegation trees.
//!
//! ## Memory
//!
//! The child runs via `ask_with` (no DB), so its turns do **not** land in
//! the parent's SQLite-FTS5 history. The parent's tool_use / tool_result
//! pair *does* get recorded under the parent's session, which is
//! sufficient to recover what was delegated and what came back.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

use super::registry::{default_registry, ToolRegistry};
use super::{Tool, ToolResult};
use crate::agent::llm::{self, Provider};
use crate::agent::runtime::loop_::{self, AskResult};
use crate::config::AgentConfig;

/// Default ceiling for nested delegate calls. A single-level fan-out is the
/// common case; we allow a couple of nested layers but cap at 3 to keep
/// total cost predictable.
pub const DEFAULT_MAX_DEPTH: u32 = 3;

/// Hard ceiling regardless of caller-provided override. Keeps a buggy
/// agent from setting `max_depth` to `u32::MAX`.
pub const HARD_MAX_DEPTH: u32 = 5;

/// Hard ceiling for the child's `max_turns`. The default `AgentConfig`
/// uses 10 turns; we let callers go higher for research-heavy tasks but
/// cap at 50 to bound spend.
pub const HARD_MAX_TURNS: u32 = 50;

/// Default per-call timeout. A delegated task that hasn't produced a final
/// answer in 10 minutes is almost certainly stuck.
pub const DEFAULT_TIMEOUT_SECS: u64 = 600;

tokio::task_local! {
    /// Depth of the current delegate call stack. 0 = top-level agent (no
    /// delegate active). Each `cos_delegate.exec` reads this, refuses if
    /// `>= max_depth`, then runs the child inside `scope(depth + 1, ...)`.
    static DELEGATE_DEPTH: u32;
}

/// Read the current delegate depth. Returns 0 outside any delegate scope.
pub fn current_depth() -> u32 {
    DELEGATE_DEPTH.try_with(|d| *d).ok().unwrap_or(0)
}

/// `cos_delegate` tool — spawn a child agent with a scoped tool subset.
pub struct Delegate;

#[derive(Debug, Deserialize)]
struct DelegateInput {
    /// Task instructions handed to the child as its user message.
    task: String,

    /// Names of tools the child is permitted to call. Required (may be
    /// empty for pure-LLM tasks). `cos_delegate` is silently filtered out
    /// even if the parent listed it — the parent must reduce depth via
    /// `max_depth` rather than re-listing the tool.
    #[serde(default)]
    allowed_tools: Vec<String>,

    /// Optional provider override (e.g. `"openai"`, `"anthropic"`,
    /// `"gemini"`). Defaults to the parent agent's provider.
    #[serde(default)]
    provider: Option<String>,

    /// Optional model override. Defaults to the parent agent's model.
    #[serde(default)]
    model: Option<String>,

    /// Optional `max_turns` for the child. Capped at [`HARD_MAX_TURNS`].
    #[serde(default)]
    max_turns: Option<u32>,

    /// Optional `max_depth` ceiling. Capped at [`HARD_MAX_DEPTH`]. A
    /// caller-supplied `max_depth` lower than the current depth + 1
    /// causes the tool to refuse before launching the child.
    #[serde(default)]
    max_depth: Option<u32>,

    /// Optional per-call timeout in seconds.
    #[serde(default)]
    timeout_secs: Option<u64>,
}

#[async_trait]
impl Tool for Delegate {
    fn name(&self) -> &'static str {
        "cos_delegate"
    }

    fn description(&self) -> &'static str {
        "Spawn a child sub-agent with a scoped subset of tools and an \
         optional model override. Returns the child's final answer. Use \
         this for focused sub-tasks (research, extraction, summarisation) \
         that you only need the final result of."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "Instructions for the child agent. Becomes the child's user message."
                },
                "allowed_tools": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Tool names the child may call. Empty = LLM-only sub-task. cos_delegate is always filtered out to prevent recursion."
                },
                "provider": {
                    "type": "string",
                    "description": "Optional provider override (e.g. 'openai', 'anthropic', 'gemini'). Defaults to parent's."
                },
                "model": {
                    "type": "string",
                    "description": "Optional model override. Defaults to parent's."
                },
                "max_turns": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": HARD_MAX_TURNS,
                    "description": "Maximum turns the child may take. Capped at 50."
                },
                "max_depth": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": HARD_MAX_DEPTH,
                    "description": "Maximum nesting depth for further delegation. Capped at 5."
                },
                "timeout_secs": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Per-call timeout in seconds. Defaults to 600."
                }
            },
            "required": ["task", "allowed_tools"]
        })
    }

    async fn exec(&self, input: serde_json::Value) -> ToolResult {
        let parsed: DelegateInput = match serde_json::from_value(input) {
            Ok(p) => p,
            Err(e) => return ToolResult::err(format!("invalid delegate input: {e}")),
        };

        let parent_cfg = crate::config::get().agent.clone();
        run_delegate(parsed, &parent_cfg, registry_factory).await
    }
}

/// Allow tests to inject a custom registry instead of opening the real
/// default_registry (which does Disk I/O for the FTS5 DB).
type RegistryFactory = fn() -> ToolRegistry;

fn registry_factory() -> ToolRegistry {
    default_registry()
}

/// Core delegate logic. Split out from `Tool::exec` so tests can drive it
/// without going through `serde_json::Value`.
async fn run_delegate(
    input: DelegateInput,
    parent_cfg: &AgentConfig,
    factory: RegistryFactory,
) -> ToolResult {
    let cur = current_depth();
    let requested_max_depth = input
        .max_depth
        .unwrap_or(DEFAULT_MAX_DEPTH)
        .min(HARD_MAX_DEPTH);

    if cur + 1 > requested_max_depth {
        return ToolResult::err(format!(
            "delegate depth limit reached: current={cur} max={requested_max_depth}; \
             refusing further delegation"
        ));
    }

    let provider_name = input
        .provider
        .clone()
        .unwrap_or_else(|| parent_cfg.provider.clone());
    let model = input
        .model
        .clone()
        .unwrap_or_else(|| parent_cfg.model.clone());

    let child_cfg = AgentConfig {
        provider: provider_name.clone(),
        model: model.clone(),
        max_turns: input
            .max_turns
            .unwrap_or(parent_cfg.max_turns)
            .clamp(1, HARD_MAX_TURNS),
        max_tokens: parent_cfg.max_tokens,
        temperature: parent_cfg.temperature,
        // Child uses a clean system prompt — no MEMORY.md / USER.md
        // injection, since the child has a fresh, isolated trajectory and
        // those are properties of the parent's session.
        system_prompt_path: None,
        ..parent_cfg.clone()
    };

    let provider: Arc<dyn Provider> = match llm::registry::build(&provider_name, &model, &child_cfg)
    {
        Ok(p) => p,
        Err(e) => {
            return ToolResult::err(format!(
                "failed to build provider '{provider_name}' for delegate: {e}"
            ));
        }
    };
    let provider = crate::ai::gate::wrap_for_system(provider);

    let tools = build_child_registry(factory(), &input.allowed_tools);

    let timeout = Duration::from_secs(input.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS));
    let task = input.task.clone();

    let child_future = DELEGATE_DEPTH.scope(cur + 1, async move {
        loop_::ask_with(provider, &child_cfg, &task, &tools).await
    });

    let outcome = tokio::time::timeout(timeout, child_future).await;
    match outcome {
        Ok(Ok(result)) => ToolResult::ok(format_result(&result)),
        Ok(Err(e)) => ToolResult::err(format!("delegate child failed: {e}")),
        Err(_) => ToolResult::err(format!("delegate timed out after {}s", timeout.as_secs())),
    }
}

/// Build a registry that contains only the tools the parent allow-listed.
/// `cos_delegate` is always filtered out (depth-counter handles recursion;
/// we additionally never pass the tool to children to keep the surface
/// minimal). Unknown names are silently dropped — the child sees only the
/// tools that actually exist.
fn build_child_registry(source: ToolRegistry, allowed: &[String]) -> ToolRegistry {
    let mut child = ToolRegistry::new();
    for name in allowed {
        if name == "cos_delegate" {
            continue;
        }
        if let Some(tool) = source.get(name) {
            child.register(tool);
        }
    }
    child
}

fn format_result(result: &AskResult) -> String {
    format!(
        "delegate finished (provider={}, model={}, turns={})\n---\n{}",
        result.provider, result.model, result.turns, result.answer
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
    use crate::agent::llm::ToolCall;

    fn parent_cfg() -> AgentConfig {
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

    fn test_registry() -> ToolRegistry {
        // Builtins-only: avoids touching MemoryDb during delegate tests.
        crate::agent::tools::registry::builtin_only_registry()
    }

    #[test]
    fn input_schema_has_required_fields() {
        let schema = Delegate.input_schema();
        let required = schema["required"].as_array().unwrap();
        let names: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
        assert!(names.contains(&"task"));
        assert!(names.contains(&"allowed_tools"));
    }

    #[test]
    fn tool_metadata() {
        assert_eq!(Delegate.name(), "cos_delegate");
        assert!(Delegate.description().contains("sub-agent"));
    }

    #[test]
    fn build_child_registry_keeps_only_allowed() {
        let parent = test_registry();
        let allowed = vec!["echo".to_string()];
        let child = build_child_registry(parent, &allowed);
        assert!(child.get("echo").is_some());
        assert!(child.get("now").is_none());
    }

    #[test]
    fn build_child_registry_strips_cos_delegate() {
        let parent = test_registry();
        let allowed = vec!["cos_delegate".to_string(), "echo".to_string()];
        let child = build_child_registry(parent, &allowed);
        assert!(child.get("cos_delegate").is_none());
        assert!(child.get("echo").is_some());
    }

    #[test]
    fn build_child_registry_silently_drops_unknown_tool_names() {
        let parent = test_registry();
        let allowed = vec!["echo".to_string(), "ghost_tool".to_string()];
        let child = build_child_registry(parent, &allowed);
        assert_eq!(child.len(), 1);
        assert!(child.get("echo").is_some());
    }

    #[test]
    fn build_child_registry_empty_allowed_yields_empty_child() {
        let parent = test_registry();
        let child = build_child_registry(parent, &[]);
        assert_eq!(child.len(), 0);
    }

    #[test]
    fn current_depth_outside_scope_is_zero() {
        assert_eq!(current_depth(), 0);
    }

    #[tokio::test]
    async fn current_depth_inside_scope_reflects_value() {
        let observed = DELEGATE_DEPTH.scope(2u32, async { current_depth() }).await;
        assert_eq!(observed, 2);
    }

    #[tokio::test]
    async fn invalid_input_returns_tool_error() {
        // Missing required `task`.
        let result = Delegate.exec(json!({"allowed_tools": []})).await;
        assert!(result.is_error);
        assert!(result.content.contains("invalid delegate input"));
    }

    /// Build a delegate input that uses a fresh mock provider seeded with
    /// the given response queue. Stores the seeded mock in
    /// `agent/llm/providers/mock.rs`'s static stash so `registry::build`
    /// for "mock" picks it up. We do this by registering responses on a
    /// MockProvider before running the loop — but `registry::build` builds
    /// a fresh MockProvider each time, so the seeded one is lost.
    ///
    /// Instead, we exercise `run_delegate` directly with a registry/cfg
    /// that is paired manually below.
    fn fresh_input(task: &str, allowed: &[&str]) -> DelegateInput {
        DelegateInput {
            task: task.to_string(),
            allowed_tools: allowed.iter().map(|s| s.to_string()).collect(),
            provider: None,
            model: None,
            max_turns: Some(5),
            max_depth: None,
            timeout_secs: Some(30),
        }
    }

    #[tokio::test]
    async fn run_delegate_refuses_when_depth_already_at_max() {
        // Set depth = 3 (== DEFAULT_MAX_DEPTH); next call would be 4, refused.
        let cfg = parent_cfg();
        let input = fresh_input("anything", &["echo"]);
        let result = DELEGATE_DEPTH
            .scope(3u32, run_delegate(input, &cfg, test_registry))
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("delegate depth limit reached"));
    }

    #[tokio::test]
    async fn run_delegate_refuses_when_caller_lowers_max_depth_below_current() {
        let cfg = parent_cfg();
        let mut input = fresh_input("hi", &[]);
        input.max_depth = Some(1);
        // We're already at depth 1, so cur(1) + 1 = 2 > max_depth(1).
        let result = DELEGATE_DEPTH
            .scope(1u32, run_delegate(input, &cfg, test_registry))
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("depth limit"));
    }

    #[tokio::test]
    async fn run_delegate_clamps_max_depth_to_hard_cap() {
        // Caller asks for max_depth = 100; should clamp to HARD_MAX_DEPTH = 5.
        // At depth 5, the call should still be refused.
        let cfg = parent_cfg();
        let mut input = fresh_input("hi", &[]);
        input.max_depth = Some(100);
        let result = DELEGATE_DEPTH
            .scope(5u32, run_delegate(input, &cfg, test_registry))
            .await;
        assert!(result.is_error, "expected refusal at depth=5 with hard cap");
    }

    #[tokio::test]
    async fn run_delegate_unknown_provider_returns_error() {
        let mut cfg = parent_cfg();
        cfg.provider = "does-not-exist".into();
        let input = fresh_input("hi", &[]);
        let result = run_delegate(input, &cfg, test_registry).await;
        assert!(result.is_error);
        assert!(result.content.contains("failed to build provider"));
    }

    /// End-to-end happy path: configure mock, run delegate, observe child's
    /// answer comes back via tool result.
    ///
    /// We can't seed the mock through `registry::build` (which constructs a
    /// fresh `MockProvider` per call). Instead we wire the parent up so its
    /// MockProvider is configured to respond ToolUse(cos_delegate) → which
    /// forces our delegate to be reached via the real loop, then the
    /// child's MockProvider (also fresh) needs to respond. The mock's
    /// default `Text` echo behaviour is what the child uses.
    #[tokio::test]
    async fn run_delegate_happy_path_uses_mock_echo_default() {
        let _perms = crate::test_env::PermissiveModeGuard::new();
        let cfg = parent_cfg();
        let input = fresh_input("hello child agent", &["echo"]);
        let result = run_delegate(input, &cfg, test_registry).await;
        assert!(
            !result.is_error,
            "expected success, got: {}",
            result.content
        );
        // MockProvider's default echoes the user prompt back as a Text
        // message; ask_with terminates in 1 turn with that text. Our
        // formatted output should mention the provider and model.
        assert!(result.content.contains("provider=mock"));
        assert!(result.content.contains("model=mock-model"));
        assert!(result.content.contains("turns=1"));
        assert!(result.content.contains("hello child agent"));
    }

    #[tokio::test]
    async fn run_delegate_clamps_max_turns_to_hard_cap() {
        let _perms = crate::test_env::PermissiveModeGuard::new();
        let cfg = parent_cfg();
        let mut input = fresh_input("hi", &[]);
        input.max_turns = Some(9999);
        // Should not panic; child should run normally with max_turns = 50.
        let result = run_delegate(input, &cfg, test_registry).await;
        assert!(!result.is_error);
    }

    /// Cover the depth-increment path: at depth 0 a delegate call should
    /// succeed; the child running inside should observe depth 1.
    #[tokio::test]
    async fn run_delegate_increments_depth_for_child() {
        let _perms = crate::test_env::PermissiveModeGuard::new();
        // Construct a registry whose only tool inspects the depth.
        struct DepthInspector;
        #[async_trait]
        impl Tool for DepthInspector {
            fn name(&self) -> &'static str {
                "depth_inspector"
            }
            fn description(&self) -> &'static str {
                "report current delegate depth"
            }
            fn input_schema(&self) -> serde_json::Value {
                json!({"type":"object","properties":{},"additionalProperties":false})
            }
            async fn exec(&self, _input: serde_json::Value) -> ToolResult {
                ToolResult::ok(format!("depth={}", current_depth()))
            }
        }

        fn registry_with_inspector() -> ToolRegistry {
            let mut r = ToolRegistry::new();
            r.register(Arc::new(DepthInspector));
            r.register(Arc::new(crate::agent::tools::builtin::Echo));
            r
        }

        let cfg = parent_cfg();
        let input = fresh_input("inspect depth please", &["depth_inspector"]);
        let result = run_delegate(input, &cfg, registry_with_inspector).await;
        assert!(!result.is_error, "got error: {}", result.content);
        // Mock provider's default behaviour is to echo the user prompt as
        // text — it never calls tools. So we won't actually invoke
        // `depth_inspector` here. What this test does verify is that the
        // child path does not panic when a custom registry is plugged in
        // and that the depth scope is set up without error.
        assert!(result.content.contains("turns=1"));
    }

    #[tokio::test]
    async fn run_delegate_zero_timeout_panics_no_just_kidding_it_returns_timeout() {
        let _perms = crate::test_env::PermissiveModeGuard::new();
        // 1-second timeout against the (instant) mock echo; should *not*
        // time out — sanity check that the timeout wrapper doesn't fire
        // spuriously.
        let cfg = parent_cfg();
        let mut input = fresh_input("hi", &[]);
        input.timeout_secs = Some(1);
        let result = run_delegate(input, &cfg, test_registry).await;
        assert!(!result.is_error);
    }

    #[test]
    fn format_result_includes_metadata_block() {
        let r = AskResult {
            answer: "the moon".into(),
            turns: 4,
            provider: "anthropic".into(),
            model: "claude-haiku-4".into(),
            session_id: String::new(),
        };
        let s = format_result(&r);
        assert!(s.contains("provider=anthropic"));
        assert!(s.contains("model=claude-haiku-4"));
        assert!(s.contains("turns=4"));
        assert!(s.contains("the moon"));
    }

    #[test]
    fn delegate_input_with_unknown_extra_field_still_parses() {
        let v = json!({
            "task": "do x",
            "allowed_tools": ["echo"],
            "future_field_that_does_not_exist_yet": 42
        });
        let parsed: Result<DelegateInput, _> = serde_json::from_value(v);
        assert!(parsed.is_ok());
    }

    #[test]
    fn delegate_input_missing_task_fails() {
        let v = json!({"allowed_tools": []});
        let parsed: Result<DelegateInput, _> = serde_json::from_value(v);
        assert!(parsed.is_err());
    }

    #[test]
    fn delegate_input_missing_allowed_tools_defaults_to_empty() {
        // `allowed_tools` is `#[serde(default)]` so missing -> empty Vec.
        let v = json!({"task": "do x"});
        let parsed: DelegateInput = serde_json::from_value(v).unwrap();
        assert!(parsed.allowed_tools.is_empty());
    }
}
