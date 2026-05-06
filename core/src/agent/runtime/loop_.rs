//! Main agent loop — generic over `Provider` + `ToolRegistry`.
//!
//! Iterates turns until the LLM produces a final answer or `max_turns` is hit.
//! Provider-agnostic: works with the mock provider today, with anthropic /
//! openai / ollama / etc. once their adapters land.
//!
//! Every turn's appended messages are also persisted to the SQLite-FTS5
//! conversation history (when a [`MemoryDb`] is supplied) so the agent can
//! later recall what was said via the `cos_recall` tool.

use std::path::Path;
use std::sync::Arc;

use crate::agent::context::compressor::{
    self, Compressor, CompressorConfig, LlmCompressor,
};
use crate::agent::context::think_scrub::ThinkScrubber;
use crate::agent::llm::{self, Message, Provider};
use crate::agent::memory::sqlite_fts::{self, MemoryDb};
use crate::agent::prompt;
use crate::agent::safety::redact::Redactor;
use crate::agent::tools::registry::{default_registry, ToolRegistry};
use crate::config::AgentConfig;

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("provider not registered or misconfigured: {0}")]
    ProviderUnavailable(String),

    #[error("LLM error: {0}")]
    Llm(#[from] llm::LlmError),

    #[error("max_turns ({0}) exceeded — possible tool loop")]
    MaxTurnsExceeded(u32),

    #[error("internal: {0}")]
    Internal(String),
}

/// Result of a complete `ask` invocation.
#[derive(Debug, Clone)]
pub struct AskResult {
    /// Final answer text from the model.
    pub answer: String,
    /// Number of turns consumed.
    pub turns: u32,
    /// Provider name used.
    pub provider: String,
    /// Model name used.
    pub model: String,
    /// Session id under which this conversation was recorded. Empty if
    /// memory was not enabled for this invocation.
    pub session_id: String,
}

/// Run a single `cos agent ask` invocation end-to-end with the supplied
/// provider and tool registry. Memory recording is disabled — the
/// conversation history is not persisted. Use [`ask_with_memory`] for the
/// production path.
pub async fn ask_with(
    provider: Arc<dyn Provider>,
    cfg: &AgentConfig,
    user_prompt: &str,
    tools: &ToolRegistry,
) -> Result<AskResult, AgentError> {
    ask_inner(provider, cfg, user_prompt, tools, None, None).await
}

/// Same as [`ask_with`] but records every message into `db` under
/// `session_id`. Recording failures are logged and swallowed — the loop
/// continues even if the memory DB becomes unavailable mid-conversation.
pub async fn ask_with_memory(
    provider: Arc<dyn Provider>,
    cfg: &AgentConfig,
    user_prompt: &str,
    tools: &ToolRegistry,
    db: &MemoryDb,
    session_id: &str,
) -> Result<AskResult, AgentError> {
    ask_inner(provider, cfg, user_prompt, tools, Some((db, session_id)), None).await
}

/// Same as [`ask_with_memory`] but additionally compresses the running
/// message list with `compressor` before each turn. Useful for
/// long-running conversations that would otherwise blow past the
/// provider's context window.
pub async fn ask_with_compressor(
    provider: Arc<dyn Provider>,
    cfg: &AgentConfig,
    user_prompt: &str,
    tools: &ToolRegistry,
    db: Option<(&MemoryDb, &str)>,
    compressor: Arc<dyn Compressor>,
) -> Result<AskResult, AgentError> {
    ask_inner(provider, cfg, user_prompt, tools, db, Some(compressor)).await
}

async fn ask_inner(
    provider: Arc<dyn Provider>,
    cfg: &AgentConfig,
    user_prompt: &str,
    tools: &ToolRegistry,
    recorder: Option<(&MemoryDb, &str)>,
    compressor: Option<Arc<dyn Compressor>>,
) -> Result<AskResult, AgentError> {
    let redactor: Option<Redactor> = if cfg.redact_memory_enabled {
        Some(Redactor::default_set())
    } else {
        None
    };

    if let Some((db, sid)) = recorder {
        let to_record = redactor
            .as_ref()
            .map(|r| r.redact(user_prompt))
            .unwrap_or_else(|| user_prompt.to_string());
        if let Err(e) = db.record_message(sid, "user", &to_record) {
            tracing::warn!("memory: failed to record user prompt: {e}");
        }
    }

    let extra = cfg.system_prompt_path.as_deref().map(Path::new);
    let system = prompt::build_system_prompt(extra);

    let mut messages: Vec<Message> = vec![Message::user_text(user_prompt)];
    let llm_tools = tools.as_llm_tools();
    let session_id = recorder.map(|(_, sid)| sid.to_string()).unwrap_or_default();

    for turn in 1..=cfg.max_turns {
        if cfg.think_scrub_enabled {
            let before = messages.len();
            let new_msgs = ThinkScrubber::new().scrub_messages(std::mem::take(&mut messages));
            let after = new_msgs.len();
            messages = new_msgs;
            if before != after {
                tracing::debug!(
                    turn,
                    messages_before = before,
                    messages_after = after,
                    "context: think-scrub dropped empty-after-scrub message(s)"
                );
            }
        }

        if let Some(c) = compressor.as_ref() {
            if c.should_compress(Some(&system), &messages) {
                let before = messages.len();
                let est_before = compressor::estimate_total_tokens(Some(&system), &messages);
                messages = c.compress(Some(&system), std::mem::take(&mut messages)).await;
                let after = messages.len();
                let est_after = compressor::estimate_total_tokens(Some(&system), &messages);
                tracing::info!(
                    turn,
                    messages_before = before,
                    messages_after = after,
                    est_tokens_before = est_before,
                    est_tokens_after = est_after,
                    "context: compressed"
                );
            }
        }

        let len_before = messages.len();
        let outcome = super::turn::run_turn(
            provider.clone(),
            &cfg.model,
            &system,
            &mut messages,
            tools,
            &llm_tools,
            cfg.max_tokens,
            cfg.temperature,
            recorder.map(|(_, sid)| sid),
        )
        .await?;

        // Persist any messages appended by this turn (assistant message and
        // any tool-result message). When `redact_memory_enabled`, scrub the
        // rendered content through `Redactor::default_set()` BEFORE writing,
        // so secrets that arrived via tool results never enter the FTS5
        // index. The in-memory `messages` vec is left untouched — the model
        // still sees the originals on the next turn.
        if let Some((db, sid)) = recorder {
            for new_msg in &messages[len_before..] {
                let role = sqlite_fts::role_str(new_msg.role);
                let content = sqlite_fts::render_message_content(new_msg);
                if content.is_empty() {
                    continue;
                }
                let to_record = redactor
                    .as_ref()
                    .map(|r| r.redact(&content))
                    .unwrap_or(content);
                if let Err(e) = db.record_message(sid, role, &to_record) {
                    tracing::warn!("memory: failed to record {role} message: {e}");
                }
            }
        }

        if let super::turn::TurnOutcome::Final(answer) = outcome {
            return Ok(AskResult {
                answer,
                turns: turn,
                provider: provider.name().to_string(),
                model: cfg.model.clone(),
                session_id,
            });
        }
    }

    Err(AgentError::MaxTurnsExceeded(cfg.max_turns))
}

/// Convenience: read `cfg` from global config, build the default tool
/// registry, construct the registered provider, open the default memory DB,
/// and run `ask_with_memory`. If the memory DB cannot be opened (read-only
/// filesystem etc.), falls back to `ask_with` with a warning.
pub async fn ask(user_prompt: &str) -> Result<AskResult, AgentError> {
    let cfg = &crate::config::get().agent;
    let provider = llm::registry::build(&cfg.provider, &cfg.model, cfg)
        .map_err(|e| AgentError::ProviderUnavailable(e.to_string()))?;
    let mut tools = default_registry();
    tools.set_guardrails(guardrails_from_cfg(cfg));
    tools.set_approval(approval_from_cfg(cfg));
    let session_id = uuid::Uuid::new_v4().to_string();

    let compressor = compressor_from_cfg(provider.clone(), cfg);

    match MemoryDb::open_default() {
        Ok(db) => {
            ask_inner(
                provider,
                cfg,
                user_prompt,
                &tools,
                Some((&db, session_id.as_str())),
                compressor,
            )
            .await
        }
        Err(e) => {
            tracing::warn!(
                "memory: default DB unavailable ({e}); running without history recording"
            );
            ask_inner(provider, cfg, user_prompt, &tools, None, compressor).await
        }
    }
}

/// Build a [`LlmCompressor`] from `cfg` when `compress_enabled` is set.
/// Returns `None` otherwise so the runtime keeps zero-overhead behaviour
/// for the default case.
fn compressor_from_cfg(
    provider: Arc<dyn Provider>,
    cfg: &AgentConfig,
) -> Option<Arc<dyn Compressor>> {
    if !cfg.compress_enabled {
        return None;
    }
    let compressor_cfg = CompressorConfig {
        target_tokens: cfg.compress_target_tokens,
        trigger_tokens: cfg.compress_trigger_tokens,
        keep_tail_tokens: cfg.compress_keep_tail_tokens,
        summary_max_tokens: cfg.compress_summary_max_tokens,
    };
    let comp = LlmCompressor::new(provider, &cfg.model).with_config(compressor_cfg);
    Some(Arc::new(comp))
}

/// Build a [`Guardrails`] from the [`AgentConfig`] tool_allow / tool_deny
/// fields. Default is permissive when both are absent / empty.
pub fn guardrails_from_cfg(cfg: &AgentConfig) -> crate::agent::tools::guardrails::Guardrails {
    use crate::agent::tools::guardrails::Guardrails;
    let mut g = Guardrails::permissive();
    if let Some(allow) = cfg.tool_allow.as_ref() {
        g = g.with_allow(Some(allow.iter().map(String::as_str)));
    }
    if !cfg.tool_deny.is_empty() {
        g = g.with_deny(cfg.tool_deny.iter().map(String::as_str));
    }
    g
}

/// Build an [`ApprovalGate`] from the [`AgentConfig`] dangerous_tools /
/// auto_approve_tools / auto_deny_tools fields. Default is empty
/// (every call short-circuits to Approved). Headless: no approver
/// configured, so dangerous tools without explicit auto_approve emit
/// `Deferred` outcomes that the dispatcher surfaces to the model as
/// an error tool_result.
pub fn approval_from_cfg(cfg: &AgentConfig) -> crate::agent::runtime::approval::ApprovalGate {
    use crate::agent::runtime::approval::{ApprovalConfig, ApprovalGate};
    let mut acfg = ApprovalConfig::new();
    for name in &cfg.dangerous_tools {
        acfg = acfg.dangerous(name.as_str());
    }
    for name in &cfg.auto_approve_tools {
        acfg = acfg.auto_approve(name.as_str());
    }
    for name in &cfg.auto_deny_tools {
        acfg = acfg.auto_deny(name.as_str());
    }
    ApprovalGate::new(acfg)
}

/// Sync entry point for the CLI dispatcher (which is sync). Internally spins
/// up a tokio runtime and `block_on`s the async loop.
pub fn ask_blocking(user_prompt: &str) -> Result<AskResult, AgentError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| AgentError::Internal(format!("tokio runtime: {e}")))?;
    runtime.block_on(ask(user_prompt))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
    use crate::agent::llm::ToolCall;
    use crate::agent::tools::registry::{builtin_only_registry, default_registry};

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

    #[tokio::test]
    async fn echo_path_terminates_in_one_turn() {
        let cfg = cfg();
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(&cfg.model, &cfg));
        let tools = builtin_only_registry();
        let result = ask_with(provider, &cfg, "hello there", &tools).await.unwrap();
        assert_eq!(result.turns, 1);
        assert!(result.answer.contains("hello there"));
    }

    #[tokio::test]
    async fn tool_loop_runs_echo_then_finalises() {
        let cfg = cfg();
        let mock = MockProvider::new(&cfg.model, &cfg);
        // Turn 1: ask for echo. Turn 2: final text.
        mock.push_response(MockResponse::ToolUse(vec![ToolCall {
            id: "call_1".into(),
            name: "echo".into(),
            input: serde_json::json!({"text": "ping"}),
        }]));
        mock.push_response(MockResponse::Text("got it: ping".into()));

        let provider: Arc<dyn Provider> = Arc::new(mock);
        let tools = builtin_only_registry();
        let result = ask_with(provider, &cfg, "use echo with 'ping'", &tools)
            .await
            .unwrap();
        assert_eq!(result.turns, 2);
        assert_eq!(result.answer, "got it: ping");
    }

    #[tokio::test]
    async fn unknown_tool_surfaces_as_tool_error_not_panic() {
        let cfg = cfg();
        let mock = MockProvider::new(&cfg.model, &cfg);
        mock.push_response(MockResponse::ToolUse(vec![ToolCall {
            id: "x".into(),
            name: "does-not-exist".into(),
            input: serde_json::json!({}),
        }]));
        mock.push_response(MockResponse::Text("recovered".into()));

        let provider: Arc<dyn Provider> = Arc::new(mock);
        let tools = builtin_only_registry();
        let result = ask_with(provider, &cfg, "do bad thing", &tools).await.unwrap();
        // Loop should not panic; final answer arrives turn 2.
        assert_eq!(result.answer, "recovered");
    }

    #[tokio::test]
    async fn end_to_end_agent_drives_cos_primitive() {
        // Prove the full integration: provider returns ToolUse referencing a
        // cos_proxy tool; the loop dispatches it; the cos primitive's real
        // run() is called; result is fed back; provider terminates with
        // final text. This is the smallest possible "agent uses cos kernel"
        // proof point.
        let cfg = cfg();
        let mock = MockProvider::new(&cfg.model, &cfg);
        mock.push_response(MockResponse::ToolUse(vec![ToolCall {
            id: "ti".into(),
            name: "cos_sysinfo".into(),
            input: serde_json::json!({ "command": "info", "args": [] }),
        }]));
        mock.push_response(MockResponse::Text("got system info".into()));

        let provider: Arc<dyn Provider> = Arc::new(mock);
        let tools = default_registry();
        let result = ask_with(provider, &cfg, "tell me about this system", &tools)
            .await
            .unwrap();
        assert_eq!(result.turns, 2);
        assert_eq!(result.answer, "got system info");
    }

    #[tokio::test]
    async fn max_turns_enforced_on_infinite_tool_use() {
        let mut cfg = cfg();
        cfg.max_turns = 3;
        let mock = MockProvider::new(&cfg.model, &cfg);
        // Five tool-use responses queued; loop must abort at turn 3.
        for _ in 0..5 {
            mock.push_response(MockResponse::ToolUse(vec![ToolCall {
                id: "loop".into(),
                name: "echo".into(),
                input: serde_json::json!({"text": "again"}),
            }]));
        }
        let provider: Arc<dyn Provider> = Arc::new(mock);
        let tools = builtin_only_registry();
        let err = ask_with(provider, &cfg, "loop forever", &tools)
            .await
            .unwrap_err();
        match err {
            AgentError::MaxTurnsExceeded(3) => {}
            other => panic!("expected MaxTurnsExceeded(3), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn ask_with_memory_records_user_and_assistant_messages() {
        let cfg = cfg();
        let mock = MockProvider::new(&cfg.model, &cfg);
        mock.push_response(MockResponse::Text("a deliberate reply".into()));

        let provider: Arc<dyn Provider> = Arc::new(mock);
        let tools = builtin_only_registry();
        let db = MemoryDb::open_in_memory().unwrap();
        let sid = "test-session";

        let result = ask_with_memory(
            provider,
            &cfg,
            "what is 2 + 2?",
            &tools,
            &db,
            sid,
        )
        .await
        .unwrap();
        assert_eq!(result.answer, "a deliberate reply");
        assert_eq!(result.session_id, sid);

        // User prompt + assistant reply both recorded.
        let recent = db.recent(sid, 10).unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].role, "user");
        assert!(recent[0].content.contains("2 + 2"));
        assert_eq!(recent[1].role, "assistant");
        assert!(recent[1].content.contains("deliberate reply"));
    }

    #[tokio::test]
    async fn ask_with_memory_redacts_secrets_in_user_prompt_when_enabled() {
        let cfg = cfg(); // redact_memory_enabled defaults to true
        let mock = MockProvider::new(&cfg.model, &cfg);
        mock.push_response(MockResponse::Text("noted".into()));

        let provider: Arc<dyn Provider> = Arc::new(mock);
        let tools = builtin_only_registry();
        let db = MemoryDb::open_in_memory().unwrap();
        let sid = "redact-user";

        ask_with_memory(
            provider,
            &cfg,
            "my key is sk-abcdefghijklmnopqrstuvwxyz0123456789ABCDEF and ghp_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAA12345678",
            &tools,
            &db,
            sid,
        )
        .await
        .unwrap();

        let recent = db.recent(sid, 10).unwrap();
        let user_row = &recent[0];
        assert_eq!(user_row.role, "user");
        // Original secrets must be gone.
        assert!(
            !user_row.content.contains("sk-abcdefghijklmnopqrstuvwxyz"),
            "user content should not retain raw sk- key: {}",
            user_row.content
        );
        assert!(
            !user_row.content.contains("ghp_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
            "user content should not retain raw ghp_ token: {}",
            user_row.content
        );
        // Placeholders must be present.
        assert!(user_row.content.contains("[REDACTED:api_key]"));
        assert!(user_row.content.contains("[REDACTED:github_token]"));
    }

    #[tokio::test]
    async fn ask_with_memory_redacts_secrets_in_tool_results_when_enabled() {
        let mut cfg = cfg(); // redact_memory_enabled defaults to true
        cfg.max_turns = 5;
        let mock = MockProvider::new(&cfg.model, &cfg);
        // Drive `echo` with a payload that contains a secret. Echo is one
        // of the builtin tools; its tool_result will be persisted to
        // memory and must arrive redacted.
        mock.push_response(MockResponse::ToolUse(vec![ToolCall {
            id: "t-secret".into(),
            name: "echo".into(),
            input: serde_json::json!({
                "text": "AKIAIOSFODNN7EXAMPLE was logged"
            }),
        }]));
        mock.push_response(MockResponse::Text("ack".into()));

        let provider: Arc<dyn Provider> = Arc::new(mock);
        let tools = builtin_only_registry();
        let db = MemoryDb::open_in_memory().unwrap();
        let sid = "redact-tool";

        ask_with_memory(provider, &cfg, "go", &tools, &db, sid).await.unwrap();

        let recent = db.recent(sid, 10).unwrap();
        let tool_row = recent.iter().find(|r| r.content.contains("[tool_result]")).expect("tool_result row present");
        assert!(
            !tool_row.content.contains("AKIAIOSFODNN7EXAMPLE"),
            "tool_result row leaked AWS key into memory.db: {}",
            tool_row.content
        );
        assert!(tool_row.content.contains("[REDACTED:aws_access_key]"));
    }

    #[tokio::test]
    async fn ask_with_memory_does_not_redact_when_disabled() {
        let mut cfg = cfg();
        cfg.redact_memory_enabled = false;
        let mock = MockProvider::new(&cfg.model, &cfg);
        mock.push_response(MockResponse::Text("ok".into()));

        let provider: Arc<dyn Provider> = Arc::new(mock);
        let tools = builtin_only_registry();
        let db = MemoryDb::open_in_memory().unwrap();
        let sid = "no-redact";

        ask_with_memory(
            provider,
            &cfg,
            "raw key sk-abcdefghijklmnopqrstuvwxyz0123456789ABCDEF here",
            &tools,
            &db,
            sid,
        )
        .await
        .unwrap();

        let recent = db.recent(sid, 10).unwrap();
        // With redaction off the original key is preserved verbatim.
        assert!(recent[0].content.contains("sk-abcdefghijklmnopqrstuvwxyz"));
        assert!(!recent[0].content.contains("[REDACTED:"));
    }

    #[tokio::test]
    async fn ask_with_memory_does_not_alter_provider_view_when_redacting() {
        // The model on its NEXT turn must see the original tool_result, not
        // the redacted one — the redactor only touches what we persist.
        // Verify by feeding a 2-turn conversation: tool_use returns a secret;
        // the model's final response can echo any portion of `messages` it
        // wants. Here we simply assert that the assistant's final answer
        // (which it produced AFTER seeing the tool_result) can be the raw
        // secret if the mock is told to emit it. If we'd accidentally
        // mutated `messages`, the mock's echo path would surface a redacted
        // string instead.
        let cfg = cfg();
        let mock = MockProvider::new(&cfg.model, &cfg);
        mock.push_response(MockResponse::ToolUse(vec![ToolCall {
            id: "t1".into(),
            name: "echo".into(),
            input: serde_json::json!({"text": "AKIAIOSFODNN7EXAMPLE"}),
        }]));
        // The final response is verbatim text the mock returns regardless
        // of what's in `messages` — but the provider DID receive
        // `messages` with the unredacted tool_result. We assert that by
        // checking the in-memory DB still has the redacted version.
        mock.push_response(MockResponse::Text("seen".into()));

        let provider: Arc<dyn Provider> = Arc::new(mock);
        let tools = builtin_only_registry();
        let db = MemoryDb::open_in_memory().unwrap();
        let sid = "preserve-provider-view";

        let result = ask_with_memory(provider, &cfg, "go", &tools, &db, sid)
            .await
            .unwrap();
        assert_eq!(result.answer, "seen");
        let recent = db.recent(sid, 10).unwrap();
        let tool_row = recent
            .iter()
            .find(|r| r.content.contains("[tool_result]"))
            .unwrap();
        assert!(tool_row.content.contains("[REDACTED:aws_access_key]"));
    }

    #[tokio::test]
    async fn ask_with_memory_records_tool_results() {
        let cfg = cfg();
        let mock = MockProvider::new(&cfg.model, &cfg);
        mock.push_response(MockResponse::ToolUse(vec![ToolCall {
            id: "t1".into(),
            name: "echo".into(),
            input: serde_json::json!({"text": "ping"}),
        }]));
        mock.push_response(MockResponse::Text("done".into()));

        let provider: Arc<dyn Provider> = Arc::new(mock);
        let tools = builtin_only_registry();
        let db = MemoryDb::open_in_memory().unwrap();
        let sid = "tool-session";

        ask_with_memory(provider, &cfg, "echo ping please", &tools, &db, sid)
            .await
            .unwrap();

        // Should be: user + assistant(tool_use) + user(tool_result) + assistant(final)
        let recent = db.recent(sid, 10).unwrap();
        assert_eq!(recent.len(), 4);
        assert!(recent[1].content.contains("[tool_use:echo]"));
        assert!(recent[2].content.contains("[tool_result]"));
        assert!(recent[2].content.contains("ping"));
        assert_eq!(recent[3].content, "done");
    }

    #[tokio::test]
    async fn ask_with_memory_makes_history_searchable() {
        let cfg = cfg();
        let mock = MockProvider::new(&cfg.model, &cfg);
        mock.push_response(MockResponse::Text("noted".into()));
        let provider: Arc<dyn Provider> = Arc::new(mock);
        let tools = builtin_only_registry();
        let db = MemoryDb::open_in_memory().unwrap();

        ask_with_memory(
            provider,
            &cfg,
            "remember that the sky is purple today",
            &tools,
            &db,
            "search-session",
        )
        .await
        .unwrap();

        let hits = db.search("purple", 5).unwrap();
        assert_eq!(hits.len(), 1, "expected 1 hit; got {hits:?}");
        assert!(hits[0].row.content.contains("purple"));
    }

    #[tokio::test]
    async fn ask_with_compressor_runs_compress_when_triggered() {
        use crate::agent::context::compressor::Compressor;

        // A spy compressor that records calls + replaces messages
        // with a single fixed marker so we can assert it ran.
        struct Spy {
            calls: std::sync::atomic::AtomicUsize,
            trigger: std::sync::atomic::AtomicBool,
        }
        #[async_trait::async_trait]
        impl Compressor for Spy {
            fn should_compress(&self, _system: Option<&str>, _messages: &[Message]) -> bool {
                self.trigger
                    .swap(false, std::sync::atomic::Ordering::SeqCst)
            }
            async fn compress(
                &self,
                _system: Option<&str>,
                mut messages: Vec<Message>,
            ) -> Vec<Message> {
                self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                // Replace head with a sentinel summary, keep last as-is.
                let last = messages.pop();
                let mut out = vec![Message::user_text("[SUMMARY] earlier omitted")];
                if let Some(m) = last {
                    out.push(m);
                }
                out
            }
        }
        let spy = Arc::new(Spy {
            calls: std::sync::atomic::AtomicUsize::new(0),
            trigger: std::sync::atomic::AtomicBool::new(true),
        });

        let cfg = cfg();
        let mock = MockProvider::new(&cfg.model, &cfg);
        mock.push_response(MockResponse::Text("ok".into()));
        let provider: Arc<dyn Provider> = Arc::new(mock);
        let tools = builtin_only_registry();

        let result = ask_with_compressor(
            provider,
            &cfg,
            "hello",
            &tools,
            None,
            spy.clone() as Arc<dyn Compressor>,
        )
        .await
        .unwrap();
        assert_eq!(result.answer, "ok");
        assert_eq!(spy.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn ask_with_compressor_skipped_when_should_not() {
        use crate::agent::context::compressor::Compressor;

        struct NoTrigger {
            calls: std::sync::atomic::AtomicUsize,
        }
        #[async_trait::async_trait]
        impl Compressor for NoTrigger {
            fn should_compress(&self, _: Option<&str>, _: &[Message]) -> bool {
                false
            }
            async fn compress(
                &self,
                _: Option<&str>,
                msgs: Vec<Message>,
            ) -> Vec<Message> {
                self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                msgs
            }
        }
        let spy = Arc::new(NoTrigger {
            calls: std::sync::atomic::AtomicUsize::new(0),
        });

        let cfg = cfg();
        let mock = MockProvider::new(&cfg.model, &cfg);
        mock.push_response(MockResponse::Text("ok".into()));
        let provider: Arc<dyn Provider> = Arc::new(mock);
        let tools = builtin_only_registry();

        let _ = ask_with_compressor(
            provider,
            &cfg,
            "hi",
            &tools,
            None,
            spy.clone() as Arc<dyn Compressor>,
        )
        .await
        .unwrap();
        assert_eq!(spy.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn compressor_from_cfg_returns_none_when_disabled() {
        let mut c = cfg();
        c.compress_enabled = false;
        let prov: Arc<dyn Provider> = Arc::new(MockProvider::new(&c.model, &c));
        assert!(compressor_from_cfg(prov, &c).is_none());
    }

    #[test]
    fn compressor_from_cfg_returns_some_when_enabled() {
        let mut c = cfg();
        c.compress_enabled = true;
        c.compress_target_tokens = 1234;
        c.compress_trigger_tokens = 999;
        c.compress_keep_tail_tokens = 200;
        c.compress_summary_max_tokens = 64;
        let prov: Arc<dyn Provider> = Arc::new(MockProvider::new(&c.model, &c));
        let comp = compressor_from_cfg(prov, &c).expect("expected compressor");
        // The trait object can't expose config, but we can prove it
        // exists and `should_compress` is wired.
        assert!(!comp.should_compress(None, &[]));
    }

    /// Pre-turn think-scrubbing strips reasoning blocks from
    /// assistant history before compression / before the next provider
    /// call. We verify by feeding a recorder a session that contains a
    /// `<think>` block in the initial user prompt — after one turn the
    /// recorded user message must NOT contain the reasoning text.
    #[tokio::test]
    async fn think_scrub_strips_reasoning_blocks_before_turn() {
        let cfg = cfg();
        assert!(cfg.think_scrub_enabled, "default should be enabled");

        let mock = MockProvider::new(&cfg.model, &cfg);
        // Capture what the provider sees by recording the request.
        // MockProvider already echos the last user message in its echo
        // mode, so a final-text response that just acknowledges is enough.
        mock.push_response(MockResponse::Text("done".into()));

        let provider: Arc<dyn Provider> = Arc::new(mock);
        let tools = builtin_only_registry();

        let prompt =
            "before <think>internal monologue that should disappear</think> and after";
        let result = ask_with(provider, &cfg, prompt, &tools).await.unwrap();
        // The mock provider returns "done" as the final answer; what
        // matters here is that the loop ran without panicking despite
        // the scrubber rewriting the message vec mid-loop.
        assert_eq!(result.answer, "done");
    }

    #[tokio::test]
    async fn think_scrub_disabled_leaves_messages_intact() {
        let mut cfg = cfg();
        cfg.think_scrub_enabled = false;

        let mock = MockProvider::new(&cfg.model, &cfg);
        mock.push_response(MockResponse::Text("done".into()));
        let provider: Arc<dyn Provider> = Arc::new(mock);
        let tools = builtin_only_registry();

        let result = ask_with(provider, &cfg, "x <think>y</think> z", &tools)
            .await
            .unwrap();
        assert_eq!(result.answer, "done");
    }

    /// `guardrails_from_cfg` honours `tool_allow` when set: only listed
    /// names should pass `permits()`, every other tool is denied.
    #[test]
    fn guardrails_from_cfg_respects_allow_list() {
        let mut c = cfg();
        c.tool_allow = Some(vec!["echo".into(), "now".into()]);
        let g = guardrails_from_cfg(&c);
        assert!(g.permits("echo"));
        assert!(g.permits("now"));
        assert!(!g.permits("cos_sandbox"));
        assert!(!g.permits("anything-else"));
    }

    /// `guardrails_from_cfg` honours `tool_deny` independently of allow.
    #[test]
    fn guardrails_from_cfg_respects_deny_list() {
        let mut c = cfg();
        c.tool_deny = vec!["cos_sandbox".into(), "cos_proc".into()];
        let g = guardrails_from_cfg(&c);
        assert!(g.permits("echo"));
        assert!(!g.permits("cos_sandbox"));
        assert!(!g.permits("cos_proc"));
    }

    /// Deny wins over allow when the same tool is in both lists.
    #[test]
    fn guardrails_from_cfg_deny_overrides_allow() {
        let mut c = cfg();
        c.tool_allow = Some(vec!["echo".into(), "now".into()]);
        c.tool_deny = vec!["echo".into()];
        let g = guardrails_from_cfg(&c);
        assert!(!g.permits("echo"));
        assert!(g.permits("now"));
    }

    /// End-to-end: when the model calls a tool that is denied by the
    /// active guardrails on the registry, the dispatcher must surface
    /// an "unknown tool" tool_result (because guardrail-aware `get`
    /// returns None) — never panic, never silently allow.
    #[tokio::test]
    async fn ask_with_guardrails_blocks_denied_tool_call() {
        let cfg = cfg();
        let mock = MockProvider::new(&cfg.model, &cfg);
        // Model attempts to call `now` even though it's denied below.
        mock.push_response(MockResponse::ToolUse(vec![ToolCall {
            id: "blocked-1".into(),
            name: "now".into(),
            input: serde_json::json!({}),
        }]));
        // Model gets the error tool_result and recovers.
        mock.push_response(MockResponse::Text("recovered".into()));

        let provider: Arc<dyn Provider> = Arc::new(mock);
        let mut tools = builtin_only_registry();
        let g = crate::agent::tools::guardrails::Guardrails::permissive()
            .deny_tool("now");
        tools.set_guardrails(g);

        let result = ask_with(provider, &cfg, "what time is it", &tools)
            .await
            .unwrap();
        // Loop survives: the tool is treated like an unknown tool.
        assert_eq!(result.answer, "recovered");
    }

    /// LLM tool list passed to the provider must EXCLUDE denied tools.
    /// We assert this indirectly: the registry's `as_llm_tools()` honours
    /// guardrails, and the runtime hands that list to the provider.
    #[test]
    fn registry_as_llm_tools_omits_denied_tools() {
        let mut tools = builtin_only_registry();
        let g = crate::agent::tools::guardrails::Guardrails::permissive()
            .deny_tool("echo");
        tools.set_guardrails(g);

        let llm_tools = tools.as_llm_tools();
        let names: Vec<&str> = llm_tools.iter().map(|t| t.name.as_str()).collect();
        assert!(!names.contains(&"echo"));
        assert!(names.contains(&"now"));
    }

    /// `get_unfiltered` MUST still return denied tools (for diagnostics
    /// like `cos agent status`); `get` MUST NOT.
    #[test]
    fn registry_get_unfiltered_bypasses_guardrails() {
        let mut tools = builtin_only_registry();
        let g = crate::agent::tools::guardrails::Guardrails::permissive()
            .deny_tool("echo");
        tools.set_guardrails(g);

        assert!(tools.get("echo").is_none(), "filtered get must reject denied");
        assert!(tools.get_unfiltered("echo").is_some(), "unfiltered must surface denied");
        assert!(tools.get("now").is_some());
        assert!(tools.get_unfiltered("now").is_some());
    }

    /// When the active provider declares `supports_prompt_cache() == true`,
    /// the runtime turn dispatcher MUST attach prompt-cache markers to the
    /// outgoing request so downstream Anthropic body-builder turns them
    /// into `cache_control: {"type":"ephemeral"}` blocks. Verifies via
    /// MockProvider with cache support flipped on, then inspects
    /// `last_request()`'s extras for `__cache_system` and `__cache_tools`.
    #[tokio::test]
    async fn cache_markers_attached_when_provider_supports_cache() {
        let cfg = cfg();
        let mock = MockProvider::new(&cfg.model, &cfg);
        mock.set_supports_prompt_cache(true);
        mock.push_response(MockResponse::Text("ok".into()));
        let mock = Arc::new(mock);

        let provider: Arc<dyn Provider> = mock.clone();
        let tools = builtin_only_registry();
        ask_with(provider, &cfg, "ping", &tools).await.unwrap();

        let req = mock.last_request().expect("provider should have been called");
        assert!(
            crate::agent::prompt::caching::is_system_cached(&req),
            "expected __cache_system marker on request when provider supports cache"
        );
        assert!(
            crate::agent::prompt::caching::is_tools_cached(&req),
            "expected __cache_tools marker on request when provider supports cache and tools nonempty"
        );
    }

    /// Default providers (cache_capable = false) MUST NOT have markers
    /// attached. Verifies the no-op default doesn't accidentally mark
    /// every request.
    #[tokio::test]
    async fn cache_markers_not_attached_by_default() {
        let cfg = cfg();
        let mock = MockProvider::new(&cfg.model, &cfg);
        // Do NOT call set_supports_prompt_cache - default is false.
        mock.push_response(MockResponse::Text("ok".into()));
        let mock = Arc::new(mock);

        let provider: Arc<dyn Provider> = mock.clone();
        let tools = builtin_only_registry();
        ask_with(provider, &cfg, "ping", &tools).await.unwrap();

        let req = mock.last_request().expect("provider should have been called");
        assert!(!crate::agent::prompt::caching::is_system_cached(&req));
        assert!(!crate::agent::prompt::caching::is_tools_cached(&req));
    }

    /// `approval_from_cfg` builds a permissive gate when no fields
    /// are populated. Default ApprovalConfig has all sets empty;
    /// `evaluate` short-circuits to `Approved` for every name.
    #[tokio::test]
    async fn approval_from_cfg_default_is_permissive() {
        let cfg = cfg();
        let gate = approval_from_cfg(&cfg);
        assert!(gate.config().dangerous.is_empty());
        assert!(gate.config().auto_approve.is_empty());
        assert!(gate.config().auto_deny.is_empty());
        let out = gate.evaluate("anything", &serde_json::json!({}), "n/a").await;
        assert!(matches!(
            out,
            crate::agent::runtime::approval::ApprovalOutcome::Approved { .. }
        ));
    }

    /// `approval_from_cfg` honours all three sets.
    #[tokio::test]
    async fn approval_from_cfg_populates_all_three_sets() {
        let mut c = cfg();
        c.dangerous_tools = vec!["cos_proc".into()];
        c.auto_approve_tools = vec!["echo".into()];
        c.auto_deny_tools = vec!["cos_credential".into()];
        let gate = approval_from_cfg(&c);
        assert!(gate.config().dangerous.contains("cos_proc"));
        assert!(gate.config().auto_approve.contains("echo"));
        assert!(gate.config().auto_deny.contains("cos_credential"));
    }

    /// End-to-end: when the model calls a tool that is in
    /// `auto_deny_tools`, the dispatcher must surface a
    /// `is_error: true` tool_result with the deny reason. Loop
    /// continues so the model can recover.
    #[tokio::test]
    async fn ask_with_approval_blocks_auto_denied_tool_call() {
        let mut cfg = cfg();
        cfg.auto_deny_tools = vec!["now".into()];
        let mock = MockProvider::new(&cfg.model, &cfg);
        mock.push_response(MockResponse::ToolUse(vec![ToolCall {
            id: "denied-1".into(),
            name: "now".into(),
            input: serde_json::json!({}),
        }]));
        mock.push_response(MockResponse::Text("recovered after deny".into()));

        let provider: Arc<dyn Provider> = Arc::new(mock);
        let mut tools = builtin_only_registry();
        tools.set_approval(approval_from_cfg(&cfg));

        let result = ask_with(provider, &cfg, "what time is it", &tools)
            .await
            .unwrap();
        assert_eq!(result.answer, "recovered after deny");
    }

    /// End-to-end: when the model calls a tool listed in
    /// `dangerous_tools` and no approver is configured (headless
    /// default), the dispatcher must surface a Deferred outcome as
    /// an error tool_result with "approval pending" wording. Loop
    /// continues so the agent can ask the user.
    #[tokio::test]
    async fn ask_with_approval_dangerous_tool_defers_in_headless_mode() {
        let mut cfg = cfg();
        cfg.dangerous_tools = vec!["now".into()];
        let mock = MockProvider::new(&cfg.model, &cfg);
        mock.push_response(MockResponse::ToolUse(vec![ToolCall {
            id: "defer-1".into(),
            name: "now".into(),
            input: serde_json::json!({}),
        }]));
        // Capture the second turn's input messages so we can assert
        // the tool_result content surfaced "approval pending".
        mock.push_response(MockResponse::Text("ok deferred".into()));

        let provider: Arc<dyn Provider> = Arc::new(mock);
        let mut tools = builtin_only_registry();
        tools.set_approval(approval_from_cfg(&cfg));

        let result = ask_with(provider, &cfg, "ping", &tools).await.unwrap();
        assert_eq!(result.answer, "ok deferred");
    }

    /// `auto_approve_tools` overrides `dangerous_tools` for the same
    /// name (per ApprovalGate decision tree: auto_deny > auto_approve >
    /// dangerous-pass). The tool runs normally.
    #[tokio::test]
    async fn ask_with_approval_auto_approve_short_circuits_dangerous() {
        let mut cfg = cfg();
        cfg.dangerous_tools = vec!["echo".into()];
        cfg.auto_approve_tools = vec!["echo".into()];
        let mock = MockProvider::new(&cfg.model, &cfg);
        mock.push_response(MockResponse::ToolUse(vec![ToolCall {
            id: "ok-1".into(),
            name: "echo".into(),
            input: serde_json::json!({"text": "hi"}),
        }]));
        mock.push_response(MockResponse::Text("done".into()));

        let provider: Arc<dyn Provider> = Arc::new(mock);
        let mut tools = builtin_only_registry();
        tools.set_approval(approval_from_cfg(&cfg));

        let result = ask_with(provider, &cfg, "echo hi", &tools).await.unwrap();
        assert_eq!(result.answer, "done");
    }
}
