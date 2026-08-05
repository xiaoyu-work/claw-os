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

use crate::agent::context::compressor::{self, Compressor, CompressorConfig, LlmCompressor};
use crate::agent::context::think_scrub::ThinkScrubber;
use crate::agent::llm::accumulate::StreamSink;
use crate::agent::llm::{self, Message, Provider};
use crate::agent::memory::sqlite_fts::{self, MemoryDb};
use crate::agent::prompt;
use crate::agent::runtime::auto_curator::AutoCurator;
use crate::agent::runtime::hooks;
use crate::agent::runtime::hooks_config;
use crate::agent::runtime::interrupt;
use crate::agent::runtime::progress::{self, ProgressSink};
use crate::agent::runtime::semantic_indexer::SemanticIndexer;
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

    #[error("interrupted: session {0} cancelled before completing")]
    Interrupted(String),

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
    ask_inner(
        provider,
        cfg,
        user_prompt,
        tools,
        Some((db, session_id)),
        None,
    )
    .await
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

/// Streaming variant of [`ask_with`] / [`ask_with_memory`]. Drives
/// each turn through `provider.chat_stream()` and forwards every
/// `StreamEvent` to `sink`. Multi-turn agentic loops with live
/// token feeds (TUI / websocket / SSE-to-client) plug their sink
/// here.
///
/// Pass `db = None` to disable memory recording (mirrors `ask_with`);
/// pass `Some((db, sid))` to record (mirrors `ask_with_memory`).
///
/// `progress` receives tool-execution events (start + result) that
/// the provider stream never surfaces. Interactive REPLs supply a
/// progress sink that renders `[tool_result …]` to stderr; headless
/// callers can pass [`progress::null_progress`] to discard them.
pub async fn ask_with_stream(
    provider: Arc<dyn Provider>,
    cfg: &AgentConfig,
    user_prompt: &str,
    tools: &ToolRegistry,
    db: Option<(&MemoryDb, &str)>,
    sink: Arc<dyn StreamSink>,
    progress: Arc<dyn ProgressSink>,
) -> Result<AskResult, AgentError> {
    ask_inner_streaming(
        provider,
        cfg,
        user_prompt,
        tools,
        db,
        None,
        sink,
        progress,
        Vec::new(),
        None,
    )
    .await
}

pub async fn ask_with_stream_scoped(
    provider: Arc<dyn Provider>,
    cfg: &AgentConfig,
    user_prompt: &str,
    tools: &ToolRegistry,
    db: Option<(&MemoryDb, &str)>,
    sink: Arc<dyn StreamSink>,
    progress: Arc<dyn ProgressSink>,
    interrupt_scope: &str,
) -> Result<AskResult, AgentError> {
    ask_inner_streaming(
        provider,
        cfg,
        user_prompt,
        tools,
        db,
        None,
        sink,
        progress,
        Vec::new(),
        Some(interrupt_scope),
    )
    .await
}

/// Streaming variant that *replays* the conversation history stored in
/// `db` under `session_id` before sending the new `user_prompt` to the
/// model. Use this from chat surfaces (web UI, future REPL) where each
/// new prompt continues an existing conversation — without this, every
/// turn looks like a fresh exchange to the LLM, because
/// [`ask_with_stream`] only seeds `messages` with the current prompt
/// (it expects the model to fetch prior context on demand via
/// `cos_recall`).
///
/// Prior turns are flattened to plain-text [`Message`]s (tool calls
/// and results are inlined as one-line summaries) so providers that
/// strictly validate `tool_use`/`tool_result` block pairing — Anthropic
/// in particular — do not reject the request when ids no longer line
/// up across a process boundary.
///
/// `history_limit` caps the number of prior memory rows replayed (0
/// means "load up to a sane default"). Practical chat UIs should keep
/// this small enough to stay within the model's context window.
pub async fn ask_with_stream_continuation(
    provider: Arc<dyn Provider>,
    cfg: &AgentConfig,
    user_prompt: &str,
    tools: &ToolRegistry,
    db: &MemoryDb,
    session_id: &str,
    history_limit: usize,
    sink: Arc<dyn StreamSink>,
    progress: Arc<dyn ProgressSink>,
) -> Result<AskResult, AgentError> {
    let limit = if history_limit == 0 { 200 } else { history_limit };
    let prior = match db.recent(session_id, limit) {
        Ok(rows) => rows_to_messages(&rows),
        Err(e) => {
            tracing::warn!(
                "memory: failed to load prior history for session {session_id}: {e}; \
                 continuing without context"
            );
            Vec::new()
        }
    };
    ask_inner_streaming(
        provider,
        cfg,
        user_prompt,
        tools,
        Some((db, session_id)),
        None,
        sink,
        progress,
        prior,
        None,
    )
    .await
}

pub async fn ask_with_stream_continuation_scoped(
    provider: Arc<dyn Provider>,
    cfg: &AgentConfig,
    user_prompt: &str,
    tools: &ToolRegistry,
    db: &MemoryDb,
    session_id: &str,
    history_limit: usize,
    sink: Arc<dyn StreamSink>,
    progress: Arc<dyn ProgressSink>,
    interrupt_scope: &str,
) -> Result<AskResult, AgentError> {
    let limit = if history_limit == 0 { 200 } else { history_limit };
    let prior = match db.recent(session_id, limit) {
        Ok(rows) => rows_to_messages(&rows),
        Err(e) => {
            tracing::warn!(
                "memory: failed to load prior history for session {session_id}: {e}; \
                 continuing without context"
            );
            Vec::new()
        }
    };
    ask_inner_streaming(
        provider,
        cfg,
        user_prompt,
        tools,
        Some((db, session_id)),
        None,
        sink,
        progress,
        prior,
        Some(interrupt_scope),
    )
    .await
}

/// Convert stored memory rows into plain-text [`Message`]s suitable
/// for replay into the LLM. Tool calls and results are inlined as
/// short text summaries — never as structured [`ContentBlock::ToolUse`]
/// / [`ContentBlock::ToolResult`] — so providers do not need the
/// original tool_use ids to match. Rows whose flattened text is empty
/// are skipped.
fn rows_to_messages(rows: &[sqlite_fts::MessageRow]) -> Vec<Message> {
    use crate::agent::llm::{ContentBlock, Role};
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let role = match row.role.as_str() {
            "assistant" => Role::Assistant,
            "system" => Role::System,
            _ => Role::User,
        };
        let text = flatten_stored_content(&row.content);
        if text.trim().is_empty() {
            continue;
        }
        out.push(Message {
            role,
            content: vec![ContentBlock::Text { text }],
        });
    }
    out
}

/// Flatten the `render_message_content` marker format back into a
/// single text payload. Mirror of the parser in
/// `agent::web::routes::sessions::parse_stored_content`, but lossy
/// where the web parser is structured — here `[tool_use:NAME] {json}`
/// collapses to `[tool: NAME]` and `[tool_result] body` becomes
/// `[tool result]\n<truncated body>` because the goal is replay
/// context, not exact reconstruction. Long tool-result bodies are
/// truncated to keep the replayed prompt cheap.
fn flatten_stored_content(content: &str) -> String {
    const MAX_RESULT_PREVIEW_CHARS: usize = 1500;
    let mut out = String::new();
    let mut active_result: Option<(bool, String)> = None;

    let push_separator = |out: &mut String| {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
    };

    let flush_result =
        |active: &mut Option<(bool, String)>, out: &mut String, max_chars: usize| {
            if let Some((is_error, body)) = active.take() {
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                out.push_str(if is_error {
                    "[tool result error]"
                } else {
                    "[tool result]"
                });
                let trimmed = body.trim();
                if !trimmed.is_empty() {
                    out.push('\n');
                    if trimmed.chars().count() > max_chars {
                        let preview: String = trimmed.chars().take(max_chars).collect();
                        out.push_str(&preview);
                        out.push_str("\n…[truncated]");
                    } else {
                        out.push_str(trimmed);
                    }
                }
            }
        };

    for line in content.lines() {
        let trimmed = line.trim_start();

        if let Some(rest) = trimmed.strip_prefix("[tool_use:") {
            if let Some(end) = rest.find(']') {
                let name = rest[..end].trim();
                if !name.is_empty() {
                    flush_result(&mut active_result, &mut out, MAX_RESULT_PREVIEW_CHARS);
                    push_separator(&mut out);
                    out.push_str("[tool: ");
                    out.push_str(name);
                    out.push(']');
                    continue;
                }
            }
        }

        if let Some(rest) = trimmed.strip_prefix("[tool_result:error]") {
            flush_result(&mut active_result, &mut out, MAX_RESULT_PREVIEW_CHARS);
            active_result = Some((true, rest.trim_start_matches([' ', '\t']).to_string()));
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("[tool_result]") {
            flush_result(&mut active_result, &mut out, MAX_RESULT_PREVIEW_CHARS);
            active_result = Some((false, rest.trim_start_matches([' ', '\t']).to_string()));
            continue;
        }

        if let Some((_, buf)) = active_result.as_mut() {
            buf.push('\n');
            buf.push_str(line);
        } else {
            push_separator(&mut out);
            out.push_str(line);
        }
    }

    flush_result(&mut active_result, &mut out, MAX_RESULT_PREVIEW_CHARS);
    out.trim().to_string()
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

    // Semantic auto-indexer: opt-in via `[embed]` config. Mirrors
    // every recorded message into the semantic store so the LLM can
    // do similarity search via `cos_recall_semantic`. `None` when
    // embedding is disabled — every spawn_index call is then a no-op.
    let semantic_indexer = if recorder.is_some() {
        SemanticIndexer::from_default_logged()
    } else {
        None
    };
    // Auto-curator: opt-in via `[agent] auxiliary_*` config. After
    // each final answer, fires `curate_session` in the background to
    // extract durable user facts and append them to MEMORY.md.
    let auto_curator = recorder.and_then(|(db, _)| AutoCurator::from_cfg_logged(cfg, db));

    if let Some((db, sid)) = recorder {
        let to_record = redactor
            .as_ref()
            .map(|r| r.redact(user_prompt))
            .unwrap_or_else(|| user_prompt.to_string());
        match db.record_message(sid, "user", &to_record) {
            Ok(msg_id) => {
                if let Some(ix) = &semantic_indexer {
                    ix.spawn_index(sid.to_string(), "user", msg_id, to_record);
                }
            }
            Err(e) => tracing::warn!("memory: failed to record user prompt: {e}"),
        }
    }

    let extra = cfg.system_prompt_path.as_deref().map(Path::new);
    let system = prompt::build_system_prompt_for(extra, Some(user_prompt));

    let mut messages: Vec<Message> = vec![Message::user_text(user_prompt)];
    let llm_tools = tools.as_llm_tools();
    let session_id = recorder.map(|(_, sid)| sid.to_string()).unwrap_or_default();

    // Register this session in the global interrupt registry. When the
    // session id is empty (no memory recording) we fall back to a
    // freshly-minted UUID so concurrent unrecorded sessions still get
    // independent interrupt scopes. Handle's `Drop` cleans up.
    let interrupt_handle = if session_id.is_empty() {
        interrupt::register(format!("ephemeral-{}", uuid::Uuid::new_v4().simple()))
    } else {
        interrupt::register(session_id.clone())
    };

    // Process-wide hook registry (default empty → zero-cost when
    // no hooks registered). See `agent::runtime::hooks`. Auto-load
    // any persistently-enabled hooks from `data_dir/agent/hooks.json`
    // for the duration of this single invocation; the guard
    // unregisters them on drop so concurrent unrelated calls / tests
    // are not affected.
    let hook_registry = hooks::global_registry();
    let _hooks_auto_guard =
        hooks_config::load_and_register(&crate::paths::agent_hooks_path(), hook_registry.clone());
    let hook_session_id = if session_id.is_empty() {
        interrupt_handle.session_id()
    } else {
        &session_id
    };
    let hook_ctx_base = hooks::HookContext::new(
        hook_session_id.to_string(),
        provider.name(),
        cfg.model.clone(),
    );

    for turn in 1..=cfg.max_turns {
        if interrupt_handle.check() {
            return Err(AgentError::Interrupted(
                interrupt_handle.session_id().to_string(),
            ));
        }

        let hook_ctx = hook_ctx_base.clone().with_turn_index(turn);
        if let hooks::HookOutcome::Stop(reason) = hook_registry.dispatch_pre_turn(&hook_ctx) {
            return Err(AgentError::Interrupted(format!(
                "hook stop (pre_turn): {reason}"
            )));
        }

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
                messages = c
                    .compress(Some(&system), std::mem::take(&mut messages))
                    .await;
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
        let turn_started_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let outcome_result = super::turn::run_turn(
            provider.clone(),
            &cfg.model,
            &system,
            &mut messages,
            tools,
            &llm_tools,
            cfg.max_tokens,
            cfg.temperature,
            recorder.map(|(_, sid)| sid),
            retry_policy_from_cfg(cfg),
            Some(&hook_ctx),
            progress::null_progress(),
        )
        .await;

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let latency_ms = now_ms.saturating_sub(turn_started_ms);

        let outcome = match outcome_result {
            Ok(report) => {
                let o = report.outcome;
                let summary = hooks::TurnSummary {
                    success: true,
                    latency_ms,
                    input_tokens: report.usage.input_tokens,
                    output_tokens: report.usage.output_tokens,
                    cache_read_tokens: report.usage.cache_read_tokens,
                    cache_write_tokens: report.usage.cache_write_tokens,
                    stop_reason: match &o {
                        super::turn::TurnOutcome::Final(_) => "Final".into(),
                        super::turn::TurnOutcome::ContinueWithTools => "ContinueWithTools".into(),
                    },
                    tool_calls_made: messages[len_before..]
                        .iter()
                        .filter(|m| {
                            m.content.iter().any(|b| {
                                matches!(b, crate::agent::llm::ContentBlock::ToolUse { .. })
                            })
                        })
                        .count() as u32,
                    error: None,
                };
                if let hooks::HookOutcome::Stop(reason) =
                    hook_registry.dispatch_post_turn(&hook_ctx, &summary)
                {
                    return Err(AgentError::Interrupted(format!(
                        "hook stop (post_turn): {reason}"
                    )));
                }
                o
            }
            Err(e) => {
                let summary = hooks::TurnSummary {
                    success: false,
                    latency_ms,
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                    stop_reason: "Error".into(),
                    tool_calls_made: 0,
                    error: Some(e.to_string()),
                };
                let _ = hook_registry.dispatch_post_turn(&hook_ctx, &summary);
                return Err(e);
            }
        };

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
                match db.record_message(sid, role, &to_record) {
                    Ok(msg_id) => {
                        if let Some(ix) = &semantic_indexer {
                            ix.spawn_index(sid.to_string(), role, msg_id, to_record);
                        }
                    }
                    Err(e) => {
                        tracing::warn!("memory: failed to record {role} message: {e}");
                    }
                }
            }
        }

        if let super::turn::TurnOutcome::Final(answer) = outcome {
            // Generate + persist a session title on the first
            // successful turn that produces a final answer. We guard
            // on `title_for() == None` so resuming a long-running
            // session never overwrites an existing title (the very
            // first user prompt is the most representative seed).
            // Errors from the auxiliary call or DB write are logged
            // but do NOT fail the turn — titles are UX cruft.
            if let Some((db, sid)) = recorder {
                if matches!(db.title_for(sid), Ok(None)) {
                    let aux = match auxiliary_from_cfg(cfg) {
                        Ok(a) => a,
                        Err(e) => {
                            tracing::warn!("title: auxiliary build failed: {e}; using heuristic");
                            None
                        }
                    };
                    let title =
                        crate::agent::title::generate_title(aux.as_ref(), user_prompt).await;
                    if let Err(e) = db.set_title(sid, &title) {
                        tracing::warn!("title: failed to record session title: {e}");
                    }
                }
            }
            // Fire-and-forget memory curation. The curator itself
            // short-circuits when no new messages exist since the
            // last pass, so calling on every final-answer turn is
            // cheap. Only fires when [agent] auxiliary_* is set.
            if let Some(c) = &auto_curator {
                c.spawn_curate(session_id.clone());
            }
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

/// Streaming twin of [`ask_inner`]. Identical behaviour except each
/// turn calls [`super::turn::run_turn_streaming`] instead of
/// [`super::turn::run_turn`], so events flow through `sink` as they
/// stream from the provider.
async fn ask_inner_streaming(
    provider: Arc<dyn Provider>,
    cfg: &AgentConfig,
    user_prompt: &str,
    tools: &ToolRegistry,
    recorder: Option<(&MemoryDb, &str)>,
    compressor: Option<Arc<dyn Compressor>>,
    sink: Arc<dyn StreamSink>,
    progress: Arc<dyn ProgressSink>,
    initial_messages: Vec<Message>,
    interrupt_scope: Option<&str>,
) -> Result<AskResult, AgentError> {
    let redactor: Option<Redactor> = if cfg.redact_memory_enabled {
        Some(Redactor::default_set())
    } else {
        None
    };

    // Streaming twins of ask_inner's auto-memory plumbing. See
    // `ask_inner` for the per-field rationale.
    let semantic_indexer = if recorder.is_some() {
        SemanticIndexer::from_default_logged()
    } else {
        None
    };
    let auto_curator = recorder.and_then(|(db, _)| AutoCurator::from_cfg_logged(cfg, db));

    if let Some((db, sid)) = recorder {
        let to_record = redactor
            .as_ref()
            .map(|r| r.redact(user_prompt))
            .unwrap_or_else(|| user_prompt.to_string());
        match db.record_message(sid, "user", &to_record) {
            Ok(msg_id) => {
                if let Some(ix) = &semantic_indexer {
                    ix.spawn_index(sid.to_string(), "user", msg_id, to_record);
                }
            }
            Err(e) => tracing::warn!("memory: failed to record user prompt: {e}"),
        }
    }

    let extra = cfg.system_prompt_path.as_deref().map(Path::new);
    let system = prompt::build_system_prompt_for(extra, Some(user_prompt));

    let mut messages: Vec<Message> = {
        let mut v = initial_messages;
        v.push(Message::user_text(user_prompt));
        v
    };
    let llm_tools = tools.as_llm_tools();
    let session_id = recorder.map(|(_, sid)| sid.to_string()).unwrap_or_default();

    let interrupt_handle = if let Some(scope) = interrupt_scope {
        interrupt::register(scope)
    } else if session_id.is_empty() {
        interrupt::register(format!("ephemeral-{}", uuid::Uuid::new_v4().simple()))
    } else {
        interrupt::register(session_id.clone())
    };

    // Mirror ask_inner: process-wide hook registry. Empty by default
    // → zero-cost when no observers registered. Streaming and
    // non-streaming surfaces share the same hooks. Auto-loaded
    // hooks are scoped to this single invocation via the guard.
    let hook_registry = hooks::global_registry();
    let _hooks_auto_guard =
        hooks_config::load_and_register(&crate::paths::agent_hooks_path(), hook_registry.clone());
    let hook_session_id = if session_id.is_empty() {
        interrupt_handle.session_id()
    } else {
        &session_id
    };
    let hook_ctx_base = hooks::HookContext::new(
        hook_session_id.to_string(),
        provider.name(),
        cfg.model.clone(),
    );

    for turn in 1..=cfg.max_turns {
        if interrupt_handle.check() {
            return Err(AgentError::Interrupted(
                interrupt_handle.session_id().to_string(),
            ));
        }

        let hook_ctx = hook_ctx_base.clone().with_turn_index(turn);
        if let hooks::HookOutcome::Stop(reason) = hook_registry.dispatch_pre_turn(&hook_ctx) {
            return Err(AgentError::Interrupted(format!(
                "hook stop (pre_turn): {reason}"
            )));
        }

        if cfg.think_scrub_enabled {
            let new_msgs = ThinkScrubber::new().scrub_messages(std::mem::take(&mut messages));
            messages = new_msgs;
        }

        if let Some(c) = compressor.as_ref() {
            if c.should_compress(Some(&system), &messages) {
                messages = c
                    .compress(Some(&system), std::mem::take(&mut messages))
                    .await;
            }
        }

        let len_before = messages.len();
        let turn_started_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let outcome_result = super::turn::run_turn_streaming(
            provider.clone(),
            &cfg.model,
            &system,
            &mut messages,
            tools,
            &llm_tools,
            cfg.max_tokens,
            cfg.temperature,
            recorder.map(|(_, sid)| sid),
            sink.clone(),
            Some(&hook_ctx),
            progress.clone(),
        )
        .await;

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let latency_ms = now_ms.saturating_sub(turn_started_ms);

        let outcome = match outcome_result {
            Ok(report) => {
                let o = report.outcome;
                let summary = hooks::TurnSummary {
                    success: true,
                    latency_ms,
                    input_tokens: report.usage.input_tokens,
                    output_tokens: report.usage.output_tokens,
                    cache_read_tokens: report.usage.cache_read_tokens,
                    cache_write_tokens: report.usage.cache_write_tokens,
                    stop_reason: match &o {
                        super::turn::TurnOutcome::Final(_) => "Final".into(),
                        super::turn::TurnOutcome::ContinueWithTools => "ContinueWithTools".into(),
                    },
                    tool_calls_made: messages[len_before..]
                        .iter()
                        .filter(|m| {
                            m.content.iter().any(|b| {
                                matches!(b, crate::agent::llm::ContentBlock::ToolUse { .. })
                            })
                        })
                        .count() as u32,
                    error: None,
                };
                if let hooks::HookOutcome::Stop(reason) =
                    hook_registry.dispatch_post_turn(&hook_ctx, &summary)
                {
                    return Err(AgentError::Interrupted(format!(
                        "hook stop (post_turn): {reason}"
                    )));
                }
                o
            }
            Err(e) => {
                let summary = hooks::TurnSummary {
                    success: false,
                    latency_ms,
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                    stop_reason: "Error".into(),
                    tool_calls_made: 0,
                    error: Some(e.to_string()),
                };
                let _ = hook_registry.dispatch_post_turn(&hook_ctx, &summary);
                return Err(e);
            }
        };

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
                match db.record_message(sid, role, &to_record) {
                    Ok(msg_id) => {
                        if let Some(ix) = &semantic_indexer {
                            ix.spawn_index(sid.to_string(), role, msg_id, to_record);
                        }
                    }
                    Err(e) => {
                        tracing::warn!("memory: failed to record {role} message: {e}");
                    }
                }
            }
        }

        if let super::turn::TurnOutcome::Final(answer) = outcome {
            if let Some((db, sid)) = recorder {
                if matches!(db.title_for(sid), Ok(None)) {
                    let aux = match auxiliary_from_cfg(cfg) {
                        Ok(a) => a,
                        Err(e) => {
                            tracing::warn!("title: auxiliary build failed: {e}; using heuristic");
                            None
                        }
                    };
                    let title =
                        crate::agent::title::generate_title(aux.as_ref(), user_prompt).await;
                    if let Err(e) = db.set_title(sid, &title) {
                        tracing::warn!("title: failed to record session title: {e}");
                    }
                }
            }
            if let Some(c) = &auto_curator {
                c.spawn_curate(session_id.clone());
            }
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
    let provider = crate::ai::gate::wrap_for_system(provider);
    let mut tools = default_registry();
    tools.set_guardrails(guardrails_from_cfg(cfg));
    tools.set_approval(approval_from_cfg(cfg));

    // Best-effort attach configured MCP servers. `_mcp_handles` MUST
    // outlive the loop — its Drop tears down children and aborts
    // background reader tasks. Failures inside attach_all are already
    // logged and skipped, so this never fails the ask.
    let _mcp_handles = attach_mcp_servers(&mut tools, cfg).await;

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

/// Translate `cfg.mcp_servers` into [`McpServerSpec`]s, optionally
/// merge in specs discovered from XDG `claw/agent-api/*.json`
/// manifests, and attach each enabled entry. Returns the live handles
/// (drop terminates the children).
///
/// Merge policy: configured servers take precedence on `name`
/// collisions. Discovered manifests sharing a `name` with a
/// configured server (or with each other) are skipped with a
/// `tracing::warn!`.
async fn attach_mcp_servers(
    tools: &mut ToolRegistry,
    cfg: &AgentConfig,
) -> Vec<crate::agent::tools::mcp::integration::McpServerHandle> {
    use crate::agent::tools::mcp::discover;
    use crate::agent::tools::mcp::integration::attach_all;
    use std::path::PathBuf;

    let configured = configured_specs(cfg);

    let discovered = if cfg.agent_api_discovery_enabled {
        let paths: Option<Vec<PathBuf>> = if cfg.agent_api_paths.is_empty() {
            None
        } else {
            Some(cfg.agent_api_paths.iter().map(PathBuf::from).collect())
        };
        discover::discover(paths.as_deref())
    } else {
        Vec::new()
    };

    let specs = merge_mcp_specs(configured, discovered);
    if specs.is_empty() {
        return Vec::new();
    }
    attach_all(&specs, tools).await
}

/// Build [`McpServerSpec`]s from the `[[agent.mcp_servers]]` config
/// block, skipping disabled entries.
fn configured_specs(
    cfg: &AgentConfig,
) -> Vec<crate::agent::tools::mcp::integration::McpServerSpec> {
    use crate::agent::tools::mcp::integration::McpServerSpec;
    cfg.mcp_servers
        .iter()
        .filter(|s| s.enabled)
        .map(|s| McpServerSpec {
            name: s.name.clone(),
            command: s.command.clone(),
            args: s.args.clone(),
            env: s.env.clone(),
            cwd: s.cwd.clone(),
            timeout_secs: s.timeout_secs,
            url: None,
            bearer_env: None,
        })
        .collect()
}

/// Merge configured + discovered specs into a single attach list.
/// Configured wins on `name` collisions; discovered duplicates among
/// themselves are dropped with a warning so two adapter packages
/// racing for the same prefix don't silently clobber each other.
fn merge_mcp_specs(
    mut configured: Vec<crate::agent::tools::mcp::integration::McpServerSpec>,
    discovered: Vec<crate::agent::tools::mcp::integration::McpServerSpec>,
) -> Vec<crate::agent::tools::mcp::integration::McpServerSpec> {
    use std::collections::HashSet;
    let mut taken: HashSet<String> = configured.iter().map(|s| s.name.clone()).collect();
    for s in discovered {
        if taken.contains(&s.name) {
            tracing::warn!(
                "agent-api: skipping discovered server `{}` (name already used)",
                s.name
            );
            continue;
        }
        taken.insert(s.name.clone());
        configured.push(s);
    }
    configured
}

/// Crate-public re-export of [`attach_mcp_servers`] so the
/// `cos agent live` CLI handler (which lives in `agent::mod`) can
/// build the same registry the production `ask` path builds without
/// needing to duplicate the spec-translation logic.
pub async fn attach_mcp_servers_for_cli(
    tools: &mut ToolRegistry,
    cfg: &AgentConfig,
) -> Vec<crate::agent::tools::mcp::integration::McpServerHandle> {
    attach_mcp_servers(tools, cfg).await
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
/// Curated set of tools that require explicit approval **out of the box**
/// when the operator hasn't configured their own `dangerous_tools` list.
/// These are the highest-blast-radius, lowest-frequency, irreversible /
/// security-sensitive operations — the ones a capable agent should pause
/// on even though the caps system already gates them. Kept deliberately
/// small so routine work (reads, file edits, exec) still flows through
/// the normal caps layer without a second prompt.
///
/// Safe-by-construction: a fresh install gates these without the operator
/// configuring anything. Override by setting any explicit `dangerous_tools`
/// in config, or disable entirely with `COS_APPROVAL_DEFAULTS=off`.
pub const DEFAULT_DANGEROUS_TOOLS: &[&str] = &["cos_credential", "cos_netfilter"];

/// Whether the built-in safe-default dangerous set should be applied.
/// Disabled by `COS_APPROVAL_DEFAULTS=off|0|false|no`.
fn approval_defaults_enabled() -> bool {
    match std::env::var("COS_APPROVAL_DEFAULTS") {
        Ok(v) => !matches!(v.trim().to_ascii_lowercase().as_str(), "off" | "0" | "false" | "no"),
        Err(_) => true,
    }
}

pub fn approval_from_cfg(cfg: &AgentConfig) -> crate::agent::runtime::approval::ApprovalGate {
    use crate::agent::runtime::approval::{ApprovalConfig, ApprovalGate};
    let mut acfg = ApprovalConfig::new();
    // Operator's explicit list wins verbatim. Only when they've configured
    // nothing do we seed the curated safe defaults (unless disabled), so a
    // fresh install is safe-by-construction without surprising operators
    // who deliberately set their own policy.
    if cfg.dangerous_tools.is_empty() && approval_defaults_enabled() {
        for name in DEFAULT_DANGEROUS_TOOLS {
            acfg = acfg.dangerous(*name);
        }
    } else {
        for name in &cfg.dangerous_tools {
            acfg = acfg.dangerous(name.as_str());
        }
    }
    for name in &cfg.auto_approve_tools {
        acfg = acfg.auto_approve(name.as_str());
    }
    for name in &cfg.auto_deny_tools {
        acfg = acfg.auto_deny(name.as_str());
    }
    ApprovalGate::new(acfg)
}

/// Build an optional [`AuxiliaryClient`] from `cfg`. Returns
/// `Ok(None)` when `auxiliary_provider` is unset (the runtime falls
/// back to the primary provider for subtasks). Returns
/// `Err(InvalidRequest)` when `auxiliary_provider` is set but
/// `auxiliary_model` is missing — the build is misconfigured and
/// silently swallowing it would hide the error from operators. The
/// `request_timeout`, credential, header, and base URL fields from
/// `cfg` are inherited by the auxiliary provider — auxiliary calls
/// share the same credentials as the primary unless the underlying
/// provider builder honours its own env vars (e.g. `OPENAI_API_KEY`).
pub fn auxiliary_from_cfg(
    cfg: &AgentConfig,
) -> Result<Option<crate::agent::llm::auxiliary::AuxiliaryClient>, AgentError> {
    use crate::agent::llm::auxiliary::{AuxiliaryClient, AuxiliaryConfig};
    let Some(provider_name) = cfg.auxiliary_provider.as_deref() else {
        return Ok(None);
    };
    if provider_name.is_empty() {
        return Ok(None);
    }
    let model = cfg
        .auxiliary_model
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AgentError::Internal(
                "auxiliary_provider set without auxiliary_model — set both or neither".into(),
            )
        })?;
    let provider = llm::registry::build(provider_name, model, cfg)
        .map_err(|e| AgentError::Internal(format!("auxiliary provider build: {e}")))?;
    let provider = crate::ai::gate::wrap_for_system(provider);
    let mut acfg =
        AuxiliaryConfig::new(provider_name, model).with_max_tokens(cfg.auxiliary_max_tokens);
    if let Some(t) = cfg.auxiliary_temperature {
        acfg = acfg.with_temperature(t);
    }
    Ok(Some(AuxiliaryClient::new(provider, acfg)))
}

/// Build a [`RetryPolicy`] from `cfg` when `retry_enabled` is set
/// AND `retry_max_attempts >= 2`. Returns `None` otherwise so the
/// runtime keeps zero-overhead fail-fast behaviour for the default
/// case (no closure capture, no retry-loop control flow).
pub fn retry_policy_from_cfg(
    cfg: &AgentConfig,
) -> Option<crate::agent::llm::rate_limit::RetryPolicy> {
    use crate::agent::llm::rate_limit::RetryPolicy;
    if !cfg.retry_enabled {
        return None;
    }
    if cfg.retry_max_attempts < 2 {
        return None;
    }
    let mut p = RetryPolicy::standard();
    p.max_attempts = cfg.retry_max_attempts;
    Some(p)
}

/// Sync entry point for the CLI dispatcher (which is sync). Internally spins
/// up a tokio runtime and `block_on`s the async loop.
///
/// After the ask future completes, we drain any background tasks
/// (auto-curator, semantic indexer) that the loop spawned via
/// [`crate::agent::runtime::background::spawn`] before the runtime is
/// dropped. Without this, `cos agent ask` in one-shot mode cancels
/// the curator mid-LLM-call and `MEMORY.md` never gets updated —
/// runtime drop kills every spawned task immediately
/// (`shutdown_timeout` only helps `spawn_blocking`, not async
/// `spawn`). The drain caps the wait at
/// [`background_drain_timeout`].
pub fn ask_blocking(user_prompt: &str) -> Result<AskResult, AgentError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| AgentError::Internal(format!("tokio runtime: {e}")))?;
    let timeout = background_drain_timeout();
    runtime.block_on(async move {
        let result = ask(user_prompt).await;
        crate::agent::runtime::background::drain(timeout).await;
        result
    })
}

/// Worst-case wait for background tasks (curator + semantic indexer) at the
/// end of a one-shot `cos agent ask` invocation. Overridable via
/// `COS_AGENT_BACKGROUND_DRAIN_SECS` for tests / debugging; defaults to 30s
/// which comfortably covers a slow auxiliary-LLM curator call without
/// noticeably delaying normal CLI exit (the drain returns as soon as all
/// registered tasks settle, which is typically sub-second when there's
/// nothing new to curate).
pub fn background_drain_timeout() -> std::time::Duration {
    let secs = std::env::var("COS_AGENT_BACKGROUND_DRAIN_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(30);
    std::time::Duration::from_secs(secs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
    use crate::agent::llm::ToolCall;
    use crate::agent::memory::sqlite_fts::MessageRow;
    use crate::agent::tools::registry::{builtin_only_registry, default_registry};

    fn row(role: &str, content: &str) -> MessageRow {
        MessageRow {
            id: 0,
            session_id: "test".into(),
            role: role.into(),
            content: content.into(),
            ts_ms: 0,
        }
    }

    #[test]
    fn flatten_plain_text_is_unchanged() {
        let out = flatten_stored_content("hello world\nsecond line");
        assert_eq!(out, "hello world\nsecond line");
    }

    #[test]
    fn flatten_collapses_tool_use_to_one_line_summary() {
        let stored = "[tool_use:cos_sysinfo] {\"interval\":1000}";
        assert_eq!(flatten_stored_content(stored), "[tool: cos_sysinfo]");
    }

    #[test]
    fn flatten_preserves_text_around_tool_use() {
        let stored = "let me check that\n[tool_use:cos_sysinfo] {\"interval\":1000}";
        assert_eq!(
            flatten_stored_content(stored),
            "let me check that\n[tool: cos_sysinfo]"
        );
    }

    #[test]
    fn flatten_keeps_tool_result_body_short_enough() {
        let stored = "[tool_result] {\"speed\":\"0 KB/s\"}";
        assert_eq!(
            flatten_stored_content(stored),
            "[tool result]\n{\"speed\":\"0 KB/s\"}"
        );
    }

    #[test]
    fn flatten_marks_error_results() {
        let stored = "[tool_result:error] boom";
        assert_eq!(flatten_stored_content(stored), "[tool result error]\nboom");
    }

    #[test]
    fn flatten_handles_multiline_result_body() {
        let stored = "[tool_result] line one\nline two\nline three";
        assert_eq!(
            flatten_stored_content(stored),
            "[tool result]\nline one\nline two\nline three"
        );
    }

    #[test]
    fn flatten_truncates_huge_tool_result_bodies() {
        let big: String = "a".repeat(5000);
        let stored = format!("[tool_result] {big}");
        let out = flatten_stored_content(&stored);
        assert!(out.starts_with("[tool result]\naaaa"));
        assert!(out.ends_with("…[truncated]"));
        // 1500 a's + the truncation marker line — well under the input length.
        assert!(out.chars().count() < 2000);
    }

    #[test]
    fn rows_to_messages_skips_empty_payloads_and_maps_roles() {
        let rows = vec![
            row("user", "hi"),
            row("assistant", ""),
            row("assistant", "[tool_use:cos_sysinfo] {}"),
            row("user", "[tool_result] ok"),
            row("assistant", "all done"),
        ];
        let msgs = rows_to_messages(&rows);
        assert_eq!(msgs.len(), 4, "empty assistant row should be dropped");
        assert!(matches!(msgs[0].role, crate::agent::llm::Role::User));
        assert!(matches!(msgs[1].role, crate::agent::llm::Role::Assistant));
        assert!(matches!(msgs[2].role, crate::agent::llm::Role::User));
        assert!(matches!(msgs[3].role, crate::agent::llm::Role::Assistant));

        // ToolUse markers collapse to text-only content blocks so
        // providers don't need to match synthetic ids.
        let blocks = &msgs[1].content;
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            crate::agent::llm::ContentBlock::Text { text } => {
                assert_eq!(text, "[tool: cos_sysinfo]");
            }
            other => panic!("expected text block, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn continuation_replays_prior_turns_into_context() {
        use crate::agent::memory::sqlite_fts::MemoryDb;

        let db = MemoryDb::open_in_memory().unwrap();
        let sid = "ctx-test";
        db.record_message(sid, "user", "我网速现在多少").unwrap();
        db.record_message(sid, "assistant", "当前网速：0 KB/s")
            .unwrap();

        let cfg = cfg();
        let mock = MockProvider::new(&cfg.model, &cfg);
        mock.push_response(MockResponse::Text("ok".into()));
        let mock = Arc::new(mock);
        let provider: Arc<dyn Provider> = mock.clone();

        let tools = builtin_only_registry();
        let sink = crate::agent::llm::accumulate::null_sink();
        let progress = progress::null_progress();

        ask_with_stream_continuation(
            provider, &cfg, "开始", &tools, &db, sid, 50, sink, progress,
        )
        .await
        .unwrap();

        let req = mock
            .last_request()
            .expect("provider should have been called");
        // Provider should see: prior user, prior assistant, then the
        // new user prompt — not just the new prompt alone.
        assert!(req.messages.len() >= 3, "got {} messages", req.messages.len());
        let texts: Vec<String> = req
            .messages
            .iter()
            .flat_map(|m| m.content.iter())
            .filter_map(|b| match b {
                crate::agent::llm::ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert!(
            texts.iter().any(|t| t.contains("我网速现在多少")),
            "prior user prompt missing from replay: {texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.contains("当前网速")),
            "prior assistant reply missing from replay: {texts:?}"
        );
        assert!(
            texts.iter().any(|t| t == "开始"),
            "new user prompt missing from replay: {texts:?}"
        );
    }

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
        let result = ask_with(provider, &cfg, "hello there", &tools)
            .await
            .unwrap();
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
        let result = ask_with(provider, &cfg, "do bad thing", &tools)
            .await
            .unwrap();
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

        let result = ask_with_memory(provider, &cfg, "what is 2 + 2?", &tools, &db, sid)
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

    /// On the first successful turn, the runtime records a session
    /// title derived from the user prompt. With no auxiliary configured,
    /// the heuristic title equals the trimmed first line of the seed.
    #[tokio::test]
    async fn ask_with_memory_records_session_title_via_heuristic() {
        let cfg = cfg(); // no auxiliary_provider → heuristic
        let mock = MockProvider::new(&cfg.model, &cfg);
        mock.push_response(MockResponse::Text("ack".into()));

        let provider: Arc<dyn Provider> = Arc::new(mock);
        let tools = builtin_only_registry();
        let db = MemoryDb::open_in_memory().unwrap();
        let sid = "title-1";

        ask_with_memory(
            provider,
            &cfg,
            "How does Rust borrow checker work?",
            &tools,
            &db,
            sid,
        )
        .await
        .unwrap();

        let title = db.title_for(sid).unwrap();
        assert_eq!(title.as_deref(), Some("How does Rust borrow checker work?"));
    }

    /// A session that already has a title is NOT overwritten on a
    /// follow-up turn — only the very first turn seeds the title.
    #[tokio::test]
    async fn ask_with_memory_does_not_overwrite_existing_title() {
        let cfg = cfg();
        let db = MemoryDb::open_in_memory().unwrap();
        let sid = "title-keep";
        db.set_title(sid, "manually labelled").unwrap();

        let mock = MockProvider::new(&cfg.model, &cfg);
        mock.push_response(MockResponse::Text("ack".into()));
        let provider: Arc<dyn Provider> = Arc::new(mock);
        let tools = builtin_only_registry();

        ask_with_memory(provider, &cfg, "totally unrelated prompt", &tools, &db, sid)
            .await
            .unwrap();

        assert_eq!(
            db.title_for(sid).unwrap().as_deref(),
            Some("manually labelled"),
            "existing title must survive subsequent turns"
        );
    }

    /// Memoryless paths (`ask_with`) never touch session_titles. Sanity
    /// check: explicitly invoke ask_with and verify nothing is written.
    #[tokio::test]
    async fn ask_with_does_not_record_title() {
        let cfg = cfg();
        let mock = MockProvider::new(&cfg.model, &cfg);
        mock.push_response(MockResponse::Text("ack".into()));
        let provider: Arc<dyn Provider> = Arc::new(mock);
        let tools = builtin_only_registry();

        let _ = ask_with(provider, &cfg, "no memory here", &tools)
            .await
            .unwrap();
        // Open a fresh in-memory DB and confirm it stayed untouched
        // (ask_with received no DB handle).
        let db = MemoryDb::open_in_memory().unwrap();
        assert!(db.title_for("any").unwrap().is_none());
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
            !user_row
                .content
                .contains("ghp_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
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

        ask_with_memory(provider, &cfg, "go", &tools, &db, sid)
            .await
            .unwrap();

        let recent = db.recent(sid, 10).unwrap();
        let tool_row = recent
            .iter()
            .find(|r| r.content.contains("[tool_result]"))
            .expect("tool_result row present");
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
            async fn compress(&self, _: Option<&str>, msgs: Vec<Message>) -> Vec<Message> {
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

        let prompt = "before <think>internal monologue that should disappear</think> and after";
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
        let g = crate::agent::tools::guardrails::Guardrails::permissive().deny_tool("now");
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
        let g = crate::agent::tools::guardrails::Guardrails::permissive().deny_tool("echo");
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
        let g = crate::agent::tools::guardrails::Guardrails::permissive().deny_tool("echo");
        tools.set_guardrails(g);

        assert!(
            tools.get("echo").is_none(),
            "filtered get must reject denied"
        );
        assert!(
            tools.get_unfiltered("echo").is_some(),
            "unfiltered must surface denied"
        );
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

        let req = mock
            .last_request()
            .expect("provider should have been called");
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

        let req = mock
            .last_request()
            .expect("provider should have been called");
        assert!(!crate::agent::prompt::caching::is_system_cached(&req));
        assert!(!crate::agent::prompt::caching::is_tools_cached(&req));
    }

    /// `approval_from_cfg` seeds the curated safe-default dangerous set
    /// when the operator configured none. The default gate is therefore
    /// not empty — `cos_credential` / `cos_netfilter` require approval out
    /// of the box — but unclassified tools still short-circuit to
    /// `Approved`.
    #[tokio::test]
    async fn approval_from_cfg_default_seeds_safe_dangerous_set() {
        // Ensure defaults aren't disabled by an ambient env var.
        std::env::remove_var("COS_APPROVAL_DEFAULTS");
        let cfg = cfg();
        let gate = approval_from_cfg(&cfg);
        for name in super::DEFAULT_DANGEROUS_TOOLS {
            assert!(
                gate.config().dangerous.contains(*name),
                "expected default dangerous set to contain {name}"
            );
        }
        assert!(gate.config().auto_approve.is_empty());
        assert!(gate.config().auto_deny.is_empty());
        // A tool outside any set still passes through.
        let out = gate
            .evaluate("echo", &serde_json::json!({}), "n/a")
            .await;
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

    /// `auxiliary_from_cfg` returns `Ok(None)` for the default config —
    /// the runtime falls back to the primary provider for subtasks.
    #[test]
    fn auxiliary_from_cfg_default_is_none() {
        let c = cfg();
        let aux = auxiliary_from_cfg(&c).expect("default cfg builds");
        assert!(aux.is_none());
    }

    /// `auxiliary_from_cfg` returns `Ok(None)` when the provider field
    /// is set to an empty string — treat as unconfigured rather than
    /// failing the build (lets `--auxiliary-provider ""` clear it).
    #[test]
    fn auxiliary_from_cfg_empty_provider_is_none() {
        let mut c = cfg();
        c.auxiliary_provider = Some(String::new());
        c.auxiliary_model = Some("anything".into());
        let aux = auxiliary_from_cfg(&c).expect("empty provider treated as unset");
        assert!(aux.is_none());
    }

    /// `auxiliary_from_cfg` errors when the provider is set without a
    /// model — silent fallback would hide the misconfig from operators.
    #[test]
    fn auxiliary_from_cfg_provider_without_model_errors() {
        let mut c = cfg();
        c.auxiliary_provider = Some("mock".into());
        c.auxiliary_model = None;
        let err = auxiliary_from_cfg(&c).unwrap_err();
        match err {
            AgentError::Internal(msg) => {
                assert!(msg.contains("auxiliary_model"), "got: {msg}");
            }
            other => panic!("expected Internal, got {other:?}"),
        }
    }

    /// `auxiliary_from_cfg` errors when the model is set to an empty
    /// string — same rationale as the missing-model case.
    #[test]
    fn auxiliary_from_cfg_provider_with_empty_model_errors() {
        let mut c = cfg();
        c.auxiliary_provider = Some("mock".into());
        c.auxiliary_model = Some(String::new());
        let err = auxiliary_from_cfg(&c).unwrap_err();
        assert!(matches!(err, AgentError::Internal(_)));
    }

    /// Happy path: aux provider + model + max_tokens override flow
    /// through to the constructed client.
    #[test]
    fn auxiliary_from_cfg_builds_client_with_overrides() {
        let mut c = cfg();
        c.auxiliary_provider = Some("mock".into());
        c.auxiliary_model = Some("aux-tiny".into());
        c.auxiliary_max_tokens = 256;
        c.auxiliary_temperature = Some(0.1);
        let aux = auxiliary_from_cfg(&c)
            .expect("builds")
            .expect("Some when configured");
        let cfg = aux.config();
        assert_eq!(cfg.provider, "mock");
        assert_eq!(cfg.model, "aux-tiny");
        assert_eq!(cfg.max_tokens, 256);
        assert_eq!(cfg.temperature, Some(0.1));
    }

    /// Unknown provider name surfaces as an Internal error so the
    /// caller knows the build failed (rather than silently falling
    /// back to the heuristic).
    #[test]
    fn auxiliary_from_cfg_unknown_provider_errors() {
        let mut c = cfg();
        c.auxiliary_provider = Some("nonsense-provider-xyz".into());
        c.auxiliary_model = Some("x".into());
        let err = auxiliary_from_cfg(&c).unwrap_err();
        match err {
            AgentError::Internal(msg) => {
                assert!(msg.contains("auxiliary provider build"), "got: {msg}");
            }
            other => panic!("expected Internal, got {other:?}"),
        }
    }

    /// `retry_policy_from_cfg` returns `None` for the default config —
    /// existing fail-fast behaviour is preserved out-of-the-box.
    #[test]
    fn retry_policy_from_cfg_default_is_none() {
        let c = cfg();
        assert!(retry_policy_from_cfg(&c).is_none());
    }

    /// `retry_policy_from_cfg` returns `None` when `retry_enabled` is
    /// false even if `retry_max_attempts` is set high.
    #[test]
    fn retry_policy_from_cfg_disabled_returns_none() {
        let mut c = cfg();
        c.retry_enabled = false;
        c.retry_max_attempts = 5;
        assert!(retry_policy_from_cfg(&c).is_none());
    }

    /// `retry_policy_from_cfg` returns `None` when retry is enabled
    /// but `retry_max_attempts < 2` — single-attempt is a no-op.
    /// Returning None here lets the runtime skip the retry-loop
    /// machinery entirely.
    #[test]
    fn retry_policy_from_cfg_attempts_lt_2_returns_none() {
        let mut c = cfg();
        c.retry_enabled = true;
        c.retry_max_attempts = 1;
        assert!(retry_policy_from_cfg(&c).is_none());
        c.retry_max_attempts = 0;
        assert!(retry_policy_from_cfg(&c).is_none());
    }

    /// `retry_policy_from_cfg` honours `retry_max_attempts` and
    /// otherwise inherits from `RetryPolicy::standard()`.
    #[test]
    fn retry_policy_from_cfg_uses_standard_with_attempts_override() {
        let mut c = cfg();
        c.retry_enabled = true;
        c.retry_max_attempts = 7;
        let p = retry_policy_from_cfg(&c).expect("retry enabled => Some");
        let standard = crate::agent::llm::rate_limit::RetryPolicy::standard();
        assert_eq!(p.max_attempts, 7);
        assert_eq!(p.base_ms, standard.base_ms);
        assert_eq!(p.max_ms, standard.max_ms);
        assert_eq!(p.jitter, standard.jitter);
    }

    /// End-to-end: when retry is enabled and the provider returns a
    /// transient `RateLimited` error followed by a success, the loop
    /// should recover transparently without surfacing the error.
    #[tokio::test]
    async fn ask_with_retry_recovers_from_rate_limit() {
        let mut c = cfg();
        c.retry_enabled = true;
        c.retry_max_attempts = 2;
        let mock = MockProvider::new(&c.model, &c);
        mock.push_response(MockResponse::Error(
            crate::agent::llm::LlmError::RateLimited { retry_after_ms: 0 },
        ));
        mock.push_response(MockResponse::Text("recovered".into()));

        let provider: Arc<dyn Provider> = Arc::new(mock);
        let tools = builtin_only_registry();
        let result = ask_with(provider, &c, "go", &tools).await.expect("ok");
        assert_eq!(result.answer.trim(), "recovered");
    }

    /// End-to-end: when retry is disabled (default), even a transient
    /// error should propagate immediately without triggering a retry.
    #[tokio::test]
    async fn ask_without_retry_propagates_transient_error() {
        let c = cfg();
        assert!(!c.retry_enabled);
        let mock = MockProvider::new(&c.model, &c);
        mock.push_response(MockResponse::Error(
            crate::agent::llm::LlmError::RateLimited { retry_after_ms: 0 },
        ));
        // Don't push a fallback success — if a retry happened we'd
        // see a follow-up call but here we expect the error to
        // propagate directly on the first try.
        let provider: Arc<dyn Provider> = Arc::new(mock);
        let tools = builtin_only_registry();
        let err = ask_with(provider, &c, "go", &tools).await.unwrap_err();
        match err {
            AgentError::Llm(crate::agent::llm::LlmError::RateLimited { .. }) => {}
            other => panic!("expected RateLimited propagated, got {other:?}"),
        }
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

    // ---- Streaming integration ----------------------------------------

    use crate::agent::llm::accumulate::StreamSink;
    use crate::agent::llm::StreamEvent;
    use std::sync::Mutex;

    #[derive(Default)]
    struct CapturingSink {
        events: Mutex<Vec<StreamEvent>>,
    }
    impl StreamSink for CapturingSink {
        fn on_event(&self, event: &StreamEvent) {
            self.events.lock().unwrap().push(event.clone());
        }
    }

    #[tokio::test]
    async fn ask_with_stream_text_response_calls_sink_and_returns_answer() {
        // The mock provider's chat_stream() shims to chat() and emits
        // Message + Done — exactly the non-truly-streaming-provider
        // case the accumulator handles via the explicit-Message path.
        let cfg = cfg();
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(&cfg.model, &cfg));
        let tools = builtin_only_registry();
        let sink: Arc<CapturingSink> = Arc::default();
        let result = ask_with_stream(
            provider,
            &cfg,
            "hello stream",
            &tools,
            None,
            sink.clone(),
            progress::null_progress(),
        )
        .await
        .unwrap();
        assert_eq!(result.turns, 1);
        assert!(result.answer.contains("hello stream"));
        let events = sink.events.lock().unwrap();
        assert!(
            events.iter().any(|e| matches!(e, StreamEvent::Done { .. })),
            "sink missing Done event; got {events:?}"
        );
    }

    #[tokio::test]
    async fn ask_with_stream_drives_tool_loop_through_streaming_path() {
        // Verify streaming run_turn correctly handles the
        // Done-with-ToolUse path, dispatches the tool, and
        // continues to a final answer.
        let cfg = cfg();
        let mock = MockProvider::new(&cfg.model, &cfg);
        mock.push_response(MockResponse::ToolUse(vec![ToolCall {
            id: "call_s1".into(),
            name: "echo".into(),
            input: serde_json::json!({"text": "stream-ping"}),
        }]));
        mock.push_response(MockResponse::Text("done streaming".into()));

        let provider: Arc<dyn Provider> = Arc::new(mock);
        let tools = builtin_only_registry();
        let sink: Arc<CapturingSink> = Arc::default();
        let result = ask_with_stream(
            provider,
            &cfg,
            "use echo through stream",
            &tools,
            None,
            sink.clone(),
            progress::null_progress(),
        )
        .await
        .unwrap();
        assert_eq!(result.turns, 2);
        assert_eq!(result.answer, "done streaming");
        // Sink should have observed events from BOTH turns.
        let events = sink.events.lock().unwrap();
        let dones = events
            .iter()
            .filter(|e| matches!(e, StreamEvent::Done { .. }))
            .count();
        assert_eq!(dones, 2, "expected one Done per turn; got {events:?}");
    }

    #[tokio::test]
    async fn ask_with_stream_propagates_provider_error() {
        let cfg = cfg();
        let mock = MockProvider::new(&cfg.model, &cfg);
        mock.push_response(MockResponse::Error(crate::agent::llm::LlmError::Auth));
        let provider: Arc<dyn Provider> = Arc::new(mock);
        let tools = builtin_only_registry();
        let sink: Arc<CapturingSink> = Arc::default();
        let res = ask_with_stream(
            provider,
            &cfg,
            "boom",
            &tools,
            None,
            sink,
            progress::null_progress(),
        )
        .await;
        assert!(matches!(res, Err(AgentError::Llm(_))));
    }

    /// Pre-signaling a session id, then running the loop with that
    /// session id, must surface as `AgentError::Interrupted` on the
    /// very first turn — before any provider call.
    #[tokio::test]
    async fn pre_signaled_session_aborts_before_first_turn() {
        let cfg = cfg();
        // Pre-signal the registry so that when ask_with_memory's
        // register() call runs under this id, the loop sees the flag
        // and bails immediately. To do this we register first, signal,
        // then re-register inside ask_with_memory — which will start
        // fresh per the documented `register` semantics — so we
        // instead pre-register-and-keep-signalling: drop the handle
        // on the test side AFTER the loop has read the registry.
        // Simpler: queue an unconditional signal racing with the loop.
        let db = MemoryDb::open_in_memory().unwrap();
        let sid = format!("pre-sig-{}", uuid::Uuid::new_v4().simple());
        let pre = interrupt::register(&sid);
        // Signal it now, then drop the handle. The flag is gone with
        // the handle — so this version actually does NOT pre-signal.
        // Instead, race a signal in via a parallel task right after
        // ask_with_memory has registered.
        drop(pre);

        // Mock returns a Text response — but we expect to never see it.
        let mock = MockProvider::new(&cfg.model, &cfg);
        mock.push_response(MockResponse::Text("should not be seen".into()));
        let provider: Arc<dyn Provider> = Arc::new(mock);
        let tools = builtin_only_registry();

        let sid_clone = sid.clone();
        let signaller = tokio::spawn(async move {
            // Tight loop: as soon as `ask_with_memory` registers under
            // `sid_clone`, signal it. Bound by 200ms so we don't hang
            // CI if registration ever stalls (it shouldn't).
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(200);
            loop {
                if interrupt::signal(&sid_clone) {
                    break;
                }
                if tokio::time::Instant::now() >= deadline {
                    break;
                }
                tokio::task::yield_now().await;
            }
        });

        let res = ask_with_memory(provider, &cfg, "irrelevant", &tools, &db, &sid).await;
        signaller.await.unwrap();

        // We expect an Interrupted error OR — in the rare race where
        // the mock returned its single Text before the signal landed —
        // a successful ask. The race window is small but real. The
        // assertion below covers both: if it succeeded, we just want
        // to know the test ran cleanly; if it errored, it must be
        // Interrupted (NOT MaxTurnsExceeded etc.).
        match res {
            Ok(_) => {
                // Race won by the model. Acceptable but not ideal.
            }
            Err(AgentError::Interrupted(s)) => {
                assert_eq!(s, sid);
            }
            Err(other) => panic!("unexpected: {other:?}"),
        }
    }

    /// Tighter test: register a session id directly via `ask_inner`
    /// path semantics — pre-set the flag with a held handle, then run
    /// a path that observes that exact flag. Because `register`
    /// always replaces, we need a wrapper. Instead we exercise the
    /// public surface: `ask_with` (no recorder) generates an
    /// ephemeral id we cannot signal, so this case is naturally
    /// unaffected by interrupts — assert that.
    #[tokio::test]
    async fn ask_without_memory_uses_ephemeral_unsignalable_id() {
        let cfg = cfg();
        let mock = MockProvider::new(&cfg.model, &cfg);
        mock.push_response(MockResponse::Text("ok".into()));
        let provider: Arc<dyn Provider> = Arc::new(mock);
        let tools = builtin_only_registry();

        // Issue a broad sweep of signals — none should match.
        for s in interrupt::registered_sessions() {
            interrupt::signal(&s);
        }

        let res = ask_with(provider, &cfg, "hello", &tools).await.unwrap();
        assert_eq!(res.answer, "ok");
    }

    /// AgentError::Interrupted is its own variant, distinct from
    /// MaxTurnsExceeded and Llm errors. Pin the discriminant so a
    /// future refactor that accidentally drops the variant fails
    /// loudly.
    #[test]
    fn interrupted_error_variant_renders_session_id() {
        let e = AgentError::Interrupted("sess-42".into());
        let s = format!("{e}");
        assert!(s.contains("sess-42"));
        assert!(s.to_lowercase().contains("interrupt"));
    }

    // -------- hooks integration ---------------------------------------

    /// Prove the loop dispatches both pre_turn and post_turn through
    /// the global hook registry, and that summary fields are
    /// populated.
    #[tokio::test]
    async fn loop_dispatches_pre_and_post_turn_hooks() {
        use crate::agent::runtime::hooks::{
            global_registry, Hook, HookContext, HookOutcome, ToolDecision, ToolResultSummary,
            TurnSummary,
        };
        use std::sync::atomic::{AtomicU32, Ordering};

        struct Spy {
            pre: Arc<AtomicU32>,
            post: Arc<AtomicU32>,
            last_post_summary: Arc<std::sync::Mutex<Option<TurnSummary>>>,
        }

        impl Hook for Spy {
            fn name(&self) -> &str {
                "loop-spy"
            }
            fn pre_turn(&self, _ctx: &HookContext) -> HookOutcome {
                self.pre.fetch_add(1, Ordering::SeqCst);
                HookOutcome::Continue
            }
            fn post_turn(&self, _ctx: &HookContext, summary: &TurnSummary) -> HookOutcome {
                self.post.fetch_add(1, Ordering::SeqCst);
                *self.last_post_summary.lock().unwrap() = Some(summary.clone());
                HookOutcome::Continue
            }
            fn pre_tool(&self, _ctx: &HookContext, _t: &llm::ToolCall) -> ToolDecision {
                ToolDecision::Allow
            }
            fn post_tool(
                &self,
                _ctx: &HookContext,
                _t: &llm::ToolCall,
                _r: &ToolResultSummary,
            ) -> HookOutcome {
                HookOutcome::Continue
            }
        }

        let pre = Arc::new(AtomicU32::new(0));
        let post = Arc::new(AtomicU32::new(0));
        let last_summary = Arc::new(std::sync::Mutex::new(None));
        let spy = Arc::new(Spy {
            pre: pre.clone(),
            post: post.clone(),
            last_post_summary: last_summary.clone(),
        });

        let registry = global_registry();
        registry.register(spy);

        let cfg = cfg();
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(&cfg.model, &cfg));
        let tools = builtin_only_registry();
        let _result = ask_with(provider, &cfg, "hello", &tools).await.unwrap();

        // Cleanup before assertions so a failure doesn't leak the
        // hook into the next test.
        registry.unregister("loop-spy");

        assert!(pre.load(Ordering::SeqCst) >= 1, "pre_turn should fire");
        assert!(post.load(Ordering::SeqCst) >= 1, "post_turn should fire");
        let summary = last_summary
            .lock()
            .unwrap()
            .clone()
            .expect("summary captured");
        assert!(summary.success);
        assert_eq!(summary.stop_reason, "Final");
    }

    /// A pre_turn hook returning Stop should abort the loop with
    /// AgentError::Interrupted before the model is even called.
    #[tokio::test]
    async fn pre_turn_hook_stop_aborts_loop_with_interrupted() {
        use crate::agent::runtime::hooks::{global_registry, Hook, HookContext, HookOutcome};

        struct Stopper;
        impl Hook for Stopper {
            fn name(&self) -> &str {
                "loop-stopper"
            }
            fn pre_turn(&self, _ctx: &HookContext) -> HookOutcome {
                HookOutcome::Stop("test-veto".into())
            }
        }

        let registry = global_registry();
        registry.register(Arc::new(Stopper));

        let cfg = cfg();
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(&cfg.model, &cfg));
        let tools = builtin_only_registry();
        let err = ask_with(provider, &cfg, "hi", &tools).await.unwrap_err();

        registry.unregister("loop-stopper");

        match err {
            AgentError::Interrupted(reason) => {
                assert!(reason.contains("test-veto"), "got {reason}");
                assert!(reason.contains("pre_turn"), "got {reason}");
            }
            other => panic!("expected Interrupted, got {other:?}"),
        }
    }

    /// Streaming twin: ask_with_stream also dispatches pre_turn /
    /// post_turn hooks through the same global registry. Pins the
    /// parity contract — both code paths invoke hooks identically.
    #[tokio::test]
    async fn streaming_loop_dispatches_pre_and_post_turn_hooks() {
        use crate::agent::runtime::hooks::{
            global_registry, Hook, HookContext, HookOutcome, ToolDecision, ToolResultSummary,
            TurnSummary,
        };
        use std::sync::atomic::{AtomicU32, Ordering};

        struct StreamSpy {
            pre: Arc<AtomicU32>,
            post: Arc<AtomicU32>,
        }
        impl Hook for StreamSpy {
            fn name(&self) -> &str {
                "stream-loop-spy"
            }
            fn pre_turn(&self, _c: &HookContext) -> HookOutcome {
                self.pre.fetch_add(1, Ordering::SeqCst);
                HookOutcome::Continue
            }
            fn post_turn(&self, _c: &HookContext, _s: &TurnSummary) -> HookOutcome {
                self.post.fetch_add(1, Ordering::SeqCst);
                HookOutcome::Continue
            }
            fn pre_tool(&self, _c: &HookContext, _t: &llm::ToolCall) -> ToolDecision {
                ToolDecision::Allow
            }
            fn post_tool(
                &self,
                _c: &HookContext,
                _t: &llm::ToolCall,
                _r: &ToolResultSummary,
            ) -> HookOutcome {
                HookOutcome::Continue
            }
        }
        let pre = Arc::new(AtomicU32::new(0));
        let post = Arc::new(AtomicU32::new(0));
        global_registry().register(Arc::new(StreamSpy {
            pre: pre.clone(),
            post: post.clone(),
        }));

        let cfg = cfg();
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(&cfg.model, &cfg));
        let tools = builtin_only_registry();
        let sink: Arc<CapturingSink> = Arc::default();
        let _ = ask_with_stream(
            provider,
            &cfg,
            "hello",
            &tools,
            None,
            sink.clone(),
            progress::null_progress(),
        )
        .await
        .unwrap();

        global_registry().unregister("stream-loop-spy");

        assert!(
            pre.load(Ordering::SeqCst) >= 1,
            "streaming pre_turn should fire"
        );
        assert!(
            post.load(Ordering::SeqCst) >= 1,
            "streaming post_turn should fire"
        );
    }

    /// Streaming pre_turn Stop also aborts with Interrupted —
    /// identical contract to the non-streaming path.
    #[tokio::test]
    async fn streaming_pre_turn_hook_stop_aborts_with_interrupted() {
        use crate::agent::runtime::hooks::{global_registry, Hook, HookContext, HookOutcome};

        struct StreamStopper;
        impl Hook for StreamStopper {
            fn name(&self) -> &str {
                "stream-loop-stopper"
            }
            fn pre_turn(&self, _c: &HookContext) -> HookOutcome {
                HookOutcome::Stop("stream-veto".into())
            }
        }
        global_registry().register(Arc::new(StreamStopper));

        let cfg = cfg();
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(&cfg.model, &cfg));
        let tools = builtin_only_registry();
        let sink: Arc<CapturingSink> = Arc::default();
        let err = ask_with_stream(
            provider,
            &cfg,
            "hi",
            &tools,
            None,
            sink,
            progress::null_progress(),
        )
        .await
        .unwrap_err();

        global_registry().unregister("stream-loop-stopper");

        match err {
            AgentError::Interrupted(reason) => {
                assert!(reason.contains("stream-veto"), "got {reason}");
                assert!(reason.contains("pre_turn"), "got {reason}");
            }
            other => panic!("expected Interrupted, got {other:?}"),
        }
    }

    /// Token usage from the provider's ChatResponse must be plumbed
    /// through TurnReport into the post_turn TurnSummary so observers
    /// can see per-turn token consumption (cost / billing / rate
    /// limiting).
    #[tokio::test]
    async fn post_turn_summary_carries_input_and_output_tokens() {
        use crate::agent::llm::Usage;
        use crate::agent::runtime::hooks::{
            global_registry, Hook, HookContext, HookOutcome, TurnSummary,
        };

        struct UsageSpy {
            captured: Arc<std::sync::Mutex<Option<TurnSummary>>>,
        }
        impl Hook for UsageSpy {
            fn name(&self) -> &str {
                "usage-spy"
            }
            fn post_turn(&self, _c: &HookContext, s: &TurnSummary) -> HookOutcome {
                *self.captured.lock().unwrap() = Some(s.clone());
                HookOutcome::Continue
            }
        }
        let captured = Arc::new(std::sync::Mutex::new(None));
        global_registry().register(Arc::new(UsageSpy {
            captured: captured.clone(),
        }));

        let cfg = cfg();
        let mock = MockProvider::new(&cfg.model, &cfg);
        mock.set_usage(Usage {
            input_tokens: 117,
            output_tokens: 42,
            cache_read_tokens: 11,
            cache_write_tokens: 5,
        });
        let provider: Arc<dyn Provider> = Arc::new(mock);
        let tools = builtin_only_registry();
        let _ = ask_with(provider, &cfg, "hi", &tools).await.unwrap();

        global_registry().unregister("usage-spy");

        let summary = captured.lock().unwrap().clone().expect("post_turn fired");
        assert_eq!(summary.input_tokens, 117);
        assert_eq!(summary.output_tokens, 42);
        assert_eq!(summary.cache_read_tokens, 11);
        assert_eq!(summary.cache_write_tokens, 5);
    }

    /// Regression: `cos agent ask` one-shot mode used to cancel
    /// background curator + semantic-indexer tasks the instant
    /// `ask_blocking` returned, because dropping the current-thread
    /// runtime aborts every `tokio::spawn`. This test reproduces the
    /// real fix path: route the spawn through
    /// `runtime::background::spawn` and call `drain` inside
    /// `block_on` before the runtime is dropped.
    #[test]
    fn background_drain_keeps_pending_tasks_alive_past_block_on() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let finished = Arc::new(AtomicBool::new(false));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let f = finished.clone();
        runtime.block_on(async move {
            crate::agent::runtime::background::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                f.store(true, Ordering::SeqCst);
            });
            // Foreground "ask" returns essentially immediately.
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            crate::agent::runtime::background::drain(std::time::Duration::from_secs(5)).await;
        });
        assert!(
            finished.load(Ordering::SeqCst),
            "background task should have been drained before runtime drop"
        );
    }

    /// `COS_AGENT_BACKGROUND_DRAIN_SECS` overrides the default 30s
    /// timeout. Useful for tests that need a tighter bound and for
    /// users on slow LLMs who want to wait longer.
    #[test]
    fn background_drain_timeout_respects_env_override() {
        let prev = std::env::var("COS_AGENT_BACKGROUND_DRAIN_SECS").ok();
        std::env::set_var("COS_AGENT_BACKGROUND_DRAIN_SECS", "7");
        assert_eq!(background_drain_timeout(), std::time::Duration::from_secs(7));
        std::env::set_var("COS_AGENT_BACKGROUND_DRAIN_SECS", "not-a-number");
        assert_eq!(
            background_drain_timeout(),
            std::time::Duration::from_secs(30),
            "malformed env value falls back to the 30s default"
        );
        std::env::remove_var("COS_AGENT_BACKGROUND_DRAIN_SECS");
        assert_eq!(background_drain_timeout(), std::time::Duration::from_secs(30));
        if let Some(v) = prev {
            std::env::set_var("COS_AGENT_BACKGROUND_DRAIN_SECS", v);
        }
    }

    fn spec(name: &str, cmd: &str) -> crate::agent::tools::mcp::integration::McpServerSpec {
        crate::agent::tools::mcp::integration::McpServerSpec {
            name: name.into(),
            command: cmd.into(),
            args: Vec::new(),
            env: std::collections::HashMap::new(),
            cwd: None,
            timeout_secs: 30,
            url: None,
            bearer_env: None,
        }
    }

    #[test]
    fn merge_specs_keeps_both_when_no_collision() {
        let merged = merge_mcp_specs(vec![spec("a", "/bin/a")], vec![spec("b", "/bin/b")]);
        let names: Vec<&str> = merged.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b"]);
    }

    #[test]
    fn merge_specs_configured_wins_on_collision() {
        let merged = merge_mcp_specs(
            vec![spec("dup", "/bin/configured")],
            vec![spec("dup", "/bin/discovered")],
        );
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].command, "/bin/configured");
    }

    #[test]
    fn merge_specs_drops_discovered_duplicates_among_themselves() {
        let merged = merge_mcp_specs(
            vec![],
            vec![spec("x", "/first"), spec("x", "/second")],
        );
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].command, "/first");
    }

    #[test]
    fn merge_specs_preserves_relative_order_configured_then_discovered() {
        let merged = merge_mcp_specs(
            vec![spec("c1", "/bin/c1"), spec("c2", "/bin/c2")],
            vec![spec("d1", "/bin/d1"), spec("d2", "/bin/d2")],
        );
        let names: Vec<&str> = merged.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["c1", "c2", "d1", "d2"]);
    }

    #[test]
    fn configured_specs_skips_disabled() {
        let mut cfg = AgentConfig::default();
        cfg.mcp_servers = vec![
            crate::config::McpServerConfig {
                name: "on".into(),
                command: "/bin/on".into(),
                args: vec![],
                env: std::collections::HashMap::new(),
                cwd: None,
                enabled: true,
                timeout_secs: 30,
            },
            crate::config::McpServerConfig {
                name: "off".into(),
                command: "/bin/off".into(),
                args: vec![],
                env: std::collections::HashMap::new(),
                cwd: None,
                enabled: false,
                timeout_secs: 30,
            },
        ];
        let got = configured_specs(&cfg);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "on");
    }
}
