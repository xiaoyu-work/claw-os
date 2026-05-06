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

use crate::agent::llm::{self, Message, Provider};
use crate::agent::memory::sqlite_fts::{self, MemoryDb};
use crate::agent::prompt;
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
    ask_inner(provider, cfg, user_prompt, tools, None).await
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
    ask_inner(provider, cfg, user_prompt, tools, Some((db, session_id))).await
}

async fn ask_inner(
    provider: Arc<dyn Provider>,
    cfg: &AgentConfig,
    user_prompt: &str,
    tools: &ToolRegistry,
    recorder: Option<(&MemoryDb, &str)>,
) -> Result<AskResult, AgentError> {
    if let Some((db, sid)) = recorder {
        if let Err(e) = db.record_message(sid, "user", user_prompt) {
            tracing::warn!("memory: failed to record user prompt: {e}");
        }
    }

    let extra = cfg.system_prompt_path.as_deref().map(Path::new);
    let system = prompt::build_system_prompt(extra);

    let mut messages: Vec<Message> = vec![Message::user_text(user_prompt)];
    let llm_tools = tools.as_llm_tools();
    let session_id = recorder.map(|(_, sid)| sid.to_string()).unwrap_or_default();

    for turn in 1..=cfg.max_turns {
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
        // any tool-result message).
        if let Some((db, sid)) = recorder {
            for new_msg in &messages[len_before..] {
                let role = sqlite_fts::role_str(new_msg.role);
                let content = sqlite_fts::render_message_content(new_msg);
                if content.is_empty() {
                    continue;
                }
                if let Err(e) = db.record_message(sid, role, &content) {
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
    let tools = default_registry();
    let session_id = uuid::Uuid::new_v4().to_string();

    match MemoryDb::open_default() {
        Ok(db) => {
            ask_with_memory(provider, cfg, user_prompt, &tools, &db, &session_id).await
        }
        Err(e) => {
            tracing::warn!(
                "memory: default DB unavailable ({e}); running without history recording"
            );
            ask_with(provider, cfg, user_prompt, &tools).await
        }
    }
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
}
