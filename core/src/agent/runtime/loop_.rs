//! Main agent loop — generic over `Provider` + `ToolRegistry`.
//!
//! Iterates turns until the LLM produces a final answer or `max_turns` is hit.
//! Provider-agnostic: works with the mock provider today, with anthropic /
//! openai / ollama / etc. once their adapters land.

use std::path::Path;
use std::sync::Arc;

use crate::agent::llm::{self, Message, Provider};
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
}

/// Run a single `cos agent ask` invocation end-to-end with the supplied
/// provider and tool registry.
pub async fn ask_with(
    provider: Arc<dyn Provider>,
    cfg: &AgentConfig,
    user_prompt: &str,
    tools: &ToolRegistry,
) -> Result<AskResult, AgentError> {
    let extra = cfg.system_prompt_path.as_deref().map(Path::new);
    let system = prompt::build_system_prompt(extra);

    let mut messages: Vec<Message> = vec![Message::user_text(user_prompt)];
    let llm_tools = tools.as_llm_tools();

    for turn in 1..=cfg.max_turns {
        let outcome = super::turn::run_turn(
            provider.clone(),
            &cfg.model,
            &system,
            &mut messages,
            tools,
            &llm_tools,
            cfg.max_tokens,
            cfg.temperature,
        )
        .await?;

        if let super::turn::TurnOutcome::Final(answer) = outcome {
            return Ok(AskResult {
                answer,
                turns: turn,
                provider: provider.name().to_string(),
                model: cfg.model.clone(),
            });
        }
    }

    Err(AgentError::MaxTurnsExceeded(cfg.max_turns))
}

/// Convenience: read `cfg` from global config, build the default tool
/// registry, construct the registered provider, and run `ask_with`.
pub async fn ask(user_prompt: &str) -> Result<AskResult, AgentError> {
    let cfg = &crate::config::get().agent;
    let provider = llm::registry::build(&cfg.provider, &cfg.model, cfg)
        .map_err(|e| AgentError::ProviderUnavailable(e.to_string()))?;
    let tools = default_registry();
    ask_with(provider, cfg, user_prompt, &tools).await
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

    fn cfg() -> AgentConfig {
        AgentConfig {
            provider: "mock".into(),
            model: "mock-model".into(),
            max_turns: 5,
            max_tokens: 1024,
            temperature: 0.0,
            system_prompt_path: None,
        }
    }

    #[tokio::test]
    async fn echo_path_terminates_in_one_turn() {
        let cfg = cfg();
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(&cfg.model, &cfg));
        let tools = default_registry();
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
        let tools = default_registry();
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
        let tools = default_registry();
        let result = ask_with(provider, &cfg, "do bad thing", &tools).await.unwrap();
        // Loop should not panic; final answer arrives turn 2.
        assert_eq!(result.answer, "recovered");
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
        let tools = default_registry();
        let err = ask_with(provider, &cfg, "loop forever", &tools)
            .await
            .unwrap_err();
        match err {
            AgentError::MaxTurnsExceeded(3) => {}
            other => panic!("expected MaxTurnsExceeded(3), got {other:?}"),
        }
    }
}
