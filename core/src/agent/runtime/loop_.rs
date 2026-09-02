//! Main agent loop — generic over `Provider` + `ToolRegistry`.
//!
//! Iterates turns until the LLM produces a final answer or `max_turns` is hit.
//! Provider-agnostic: works with the mock provider today, with anthropic /
//! openai / ollama / etc. once their adapters land.
//!
//! Every turn's appended messages are also persisted to the SQLite-FTS5
//! conversation history (when a [`MemoryDb`] is supplied) so the agent can
//! later recall what was said via the `cos_recall` tool.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::agent::context::compressor::{self, Compressor, CompressorConfig, LlmCompressor};
use crate::agent::context::think_scrub::ThinkScrubber;
use crate::agent::llm::accumulate::StreamSink;
use crate::agent::llm::{self, Message, Provider};
use crate::agent::memory::compaction::{BeginCompaction, CompactionSummary, NewCompaction};
use crate::agent::memory::sqlite_fts::{self, MemoryDb};
use crate::agent::prompt;
use crate::agent::runtime::auto_curator::AutoCurator;
use crate::agent::runtime::deps::RuntimeDeps;
use crate::agent::runtime::hooks;
use crate::agent::runtime::interrupt;
use crate::agent::runtime::progress::{self, ProgressSink};
use crate::agent::safety::redact::Redactor;
use crate::agent::tools::exposure::ToolExposureContext;
use crate::agent::tools::registry::{default_registry_with_deps, ToolRegistry};
use crate::agent::trust;
use crate::config::AgentConfig;

const TURN_LIMIT_FINALIZATION_PROMPT: &str = "\
This is the final allowed model turn for the current request. Do not call any \
tools. Use only the conversation and tool results already available to give \
the user the best concise answer now. Say what was completed, and if anything \
is unfinished, explain that this attempt reached its work limit and that the \
user can ask you to continue. Do not expose internal runtime details.";

const TURN_LIMIT_FALLBACK: &str = "\
I stopped this attempt after reaching its tool-work limit before I could \
finish the summary. Ask me to continue and I will resume from the results \
already collected.";

const MAX_COMPACTION_REPLANS: usize = 8;
const COMPACTION_BUSY_TIMEOUT: Duration = Duration::from_secs(120);
const COMPACTION_BUSY_POLL_INTERVAL: Duration = Duration::from_millis(50);

fn append_turn_limit_fallback(messages: &mut Vec<Message>) -> String {
    let answer = TURN_LIMIT_FALLBACK.to_string();
    messages.push(Message {
        role: llm::Role::Assistant,
        content: vec![llm::ContentBlock::Text {
            text: answer.clone(),
        }],
    });
    answer
}

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

    #[error("memory integrity failure: {0}")]
    MemoryIntegrity(String),

    #[error("context compression failed: {0}")]
    Compression(String),

    #[error("internal: {0}")]
    Internal(String),
}

#[derive(Debug, Clone, Copy)]
struct CompressionRetryPolicy {
    max_replans: usize,
    busy_timeout: Duration,
    busy_poll_interval: Duration,
}

impl Default for CompressionRetryPolicy {
    fn default() -> Self {
        Self {
            max_replans: MAX_COMPACTION_REPLANS,
            busy_timeout: COMPACTION_BUSY_TIMEOUT,
            busy_poll_interval: COMPACTION_BUSY_POLL_INTERVAL,
        }
    }
}

/// Result of a complete `ask` invocation.
#[derive(Debug, Clone)]
pub struct AskResult {
    /// Final answer text from the model.
    pub answer: String,
    /// Structural binding between answer citations and exact runtime tool results.
    pub evidence: super::evidence::EvidenceReport,
    /// Cross-provider fallback state when a chain was configured.
    pub fallback: Option<llm::ProviderFallbackState>,
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

#[derive(Debug, Clone)]
enum MessageOrigin {
    Raw { id: i64, replay: Message },
    Summary(Box<CompactionSummary>),
    Ephemeral,
}

#[derive(Debug, Clone, Default)]
struct ConversationSeed {
    messages: Vec<Message>,
    origins: Vec<MessageOrigin>,
}

impl ConversationSeed {
    fn empty() -> Self {
        Self::default()
    }
}

pub struct RuntimeRequest<'a> {
    provider: Arc<dyn Provider>,
    cfg: &'a AgentConfig,
    user_prompt: &'a str,
    tools: &'a ToolRegistry,
    exposure: Option<&'a ToolExposureContext>,
    recorder: Option<(&'a MemoryDb, &'a str)>,
    continuation_limit: Option<usize>,
    transient_context: Option<&'a str>,
    output: LifecycleOutput,
    progress: Arc<dyn ProgressSink>,
    interrupt_scope: Option<&'a str>,
    compress: bool,
    delegated: bool,
}

impl<'a> RuntimeRequest<'a> {
    pub fn buffered(
        provider: Arc<dyn Provider>,
        cfg: &'a AgentConfig,
        user_prompt: &'a str,
        tools: &'a ToolRegistry,
    ) -> Self {
        Self {
            provider,
            cfg,
            user_prompt,
            tools,
            exposure: None,
            recorder: None,
            continuation_limit: None,
            transient_context: None,
            output: LifecycleOutput::Buffered,
            progress: progress::null_progress(),
            interrupt_scope: None,
            compress: false,
            delegated: false,
        }
    }

    pub fn streaming(
        provider: Arc<dyn Provider>,
        cfg: &'a AgentConfig,
        user_prompt: &'a str,
        tools: &'a ToolRegistry,
        sink: Arc<dyn StreamSink>,
        progress: Arc<dyn ProgressSink>,
    ) -> Self {
        Self {
            output: LifecycleOutput::Streaming {
                sink: super::presentation::user_visible_stream_sink(sink),
            },
            progress: super::presentation::user_visible_progress_sink(progress),
            compress: true,
            ..Self::buffered(provider, cfg, user_prompt, tools)
        }
    }

    pub fn with_memory(mut self, db: &'a MemoryDb, session_id: &'a str) -> Self {
        self.recorder = Some((db, session_id));
        self
    }

    pub fn with_continuation(
        mut self,
        db: &'a MemoryDb,
        session_id: &'a str,
        history_limit: usize,
    ) -> Self {
        self.recorder = Some((db, session_id));
        self.continuation_limit = Some(history_limit);
        self.compress = true;
        self
    }

    pub fn with_transient_context(mut self, context: Option<&'a str>) -> Self {
        self.transient_context = context;
        self
    }

    pub fn with_interrupt_scope(mut self, scope: &'a str) -> Self {
        self.interrupt_scope = Some(scope);
        self
    }

    pub fn with_delegated(mut self, delegated: bool) -> Self {
        self.delegated = delegated;
        self
    }

    pub fn with_exposure(mut self, exposure: &'a ToolExposureContext) -> Self {
        self.exposure = Some(exposure);
        self
    }
}

/// Execute one request against an explicit runtime dependency set.
pub async fn run_with_deps(
    deps: &RuntimeDeps,
    request: RuntimeRequest<'_>,
) -> Result<AskResult, AgentError> {
    let compressor = request
        .compress
        .then(|| {
            compressor_from_cfg_with_exposure(
                request.provider.clone(),
                request.cfg,
                request.tools,
                request.exposure,
            )
        })
        .flatten();
    let initial_messages = request
        .continuation_limit
        .and_then(|limit| {
            request.recorder.map(|(db, session_id)| {
                load_continuation_messages(db, session_id, limit, compressor.is_some())
            })
        })
        .unwrap_or_default();
    ask_inner(LifecycleRequest {
        deps,
        provider: request.provider,
        cfg: request.cfg,
        user_prompt: request.user_prompt,
        tools: request.tools,
        exposure: request.exposure,
        recorder: request.recorder,
        compressor,
        initial_messages,
        transient_context: request.transient_context,
        output: request.output,
        progress: request.progress,
        interrupt_scope: request.interrupt_scope,
        delegated: request.delegated,
    })
    .await
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
    let deps = RuntimeDeps::compatibility(false);
    ask_inner(LifecycleRequest {
        deps: &deps,
        provider,
        cfg,
        user_prompt,
        tools,
        exposure: None,
        recorder: None,
        compressor: None,
        initial_messages: ConversationSeed::empty(),
        transient_context: None,
        output: LifecycleOutput::Buffered,
        progress: progress::null_progress(),
        interrupt_scope: None,
        delegated: false,
    })
    .await
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
    let deps = RuntimeDeps::compatibility(true);
    ask_inner(LifecycleRequest {
        deps: &deps,
        provider,
        cfg,
        user_prompt,
        tools,
        exposure: None,
        recorder: Some((db, session_id)),
        compressor: None,
        initial_messages: ConversationSeed::empty(),
        transient_context: None,
        output: LifecycleOutput::Buffered,
        progress: progress::null_progress(),
        interrupt_scope: None,
        delegated: false,
    })
    .await
}

/// Same as [`ask_with_memory`], but replays recent rows from `session_id`
/// before the new prompt. Use this for non-streaming conversational surfaces
/// where short follow-ups depend on the immediately preceding exchange.
pub async fn ask_with_memory_continuation(
    provider: Arc<dyn Provider>,
    cfg: &AgentConfig,
    user_prompt: &str,
    tools: &ToolRegistry,
    db: &MemoryDb,
    session_id: &str,
    history_limit: usize,
) -> Result<AskResult, AgentError> {
    let deps = RuntimeDeps::compatibility(true);
    let compressor = compressor_from_cfg(provider.clone(), cfg, tools);
    let prior = load_continuation_messages(db, session_id, history_limit, compressor.is_some());
    ask_inner(LifecycleRequest {
        deps: &deps,
        provider,
        cfg,
        user_prompt,
        tools,
        exposure: None,
        recorder: Some((db, session_id)),
        compressor,
        initial_messages: prior,
        transient_context: None,
        output: LifecycleOutput::Buffered,
        progress: progress::null_progress(),
        interrupt_scope: None,
        delegated: false,
    })
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
    let deps = RuntimeDeps::compatibility(db.is_some());
    ask_inner(LifecycleRequest {
        deps: &deps,
        provider,
        cfg,
        user_prompt,
        tools,
        exposure: None,
        recorder: db,
        compressor: Some(compressor),
        initial_messages: ConversationSeed::empty(),
        transient_context: None,
        output: LifecycleOutput::Buffered,
        progress: progress::null_progress(),
        interrupt_scope: None,
        delegated: false,
    })
    .await
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
    let deps = RuntimeDeps::compatibility(db.is_some());
    let compressor = compressor_from_cfg(provider.clone(), cfg, tools);
    ask_inner_streaming(
        &deps,
        provider,
        cfg,
        user_prompt,
        None,
        tools,
        db,
        compressor,
        sink,
        progress,
        ConversationSeed::empty(),
        None,
    )
    .await
}

pub async fn ask_with_stream_scoped(
    provider: Arc<dyn Provider>,
    cfg: &AgentConfig,
    user_prompt: &str,
    transient_context: Option<&str>,
    tools: &ToolRegistry,
    db: Option<(&MemoryDb, &str)>,
    sink: Arc<dyn StreamSink>,
    progress: Arc<dyn ProgressSink>,
    interrupt_scope: &str,
) -> Result<AskResult, AgentError> {
    let deps = RuntimeDeps::compatibility(db.is_some());
    let compressor = compressor_from_cfg(provider.clone(), cfg, tools);
    ask_inner_streaming(
        &deps,
        provider,
        cfg,
        user_prompt,
        transient_context,
        tools,
        db,
        compressor,
        sink,
        progress,
        ConversationSeed::empty(),
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
/// `history_limit` caps the number of prior conversation rows replayed
/// (0 means "load up to a sane default"). Audit-only injected prompt
/// rows do not consume this budget. Practical chat UIs should keep the
/// limit small enough to stay within the model's context window.
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
    let deps = RuntimeDeps::compatibility(true);
    let compressor = compressor_from_cfg(provider.clone(), cfg, tools);
    let prior = load_continuation_messages(db, session_id, history_limit, compressor.is_some());
    ask_inner_streaming(
        &deps,
        provider,
        cfg,
        user_prompt,
        None,
        tools,
        Some((db, session_id)),
        compressor,
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
    transient_context: Option<&str>,
    tools: &ToolRegistry,
    db: &MemoryDb,
    session_id: &str,
    history_limit: usize,
    sink: Arc<dyn StreamSink>,
    progress: Arc<dyn ProgressSink>,
    interrupt_scope: &str,
) -> Result<AskResult, AgentError> {
    let deps = RuntimeDeps::compatibility(true);
    let compressor = compressor_from_cfg(provider.clone(), cfg, tools);
    let prior = load_continuation_messages(db, session_id, history_limit, compressor.is_some());
    ask_inner_streaming(
        &deps,
        provider,
        cfg,
        user_prompt,
        transient_context,
        tools,
        Some((db, session_id)),
        compressor,
        sink,
        progress,
        prior,
        Some(interrupt_scope),
    )
    .await
}

fn load_continuation_messages(
    db: &MemoryDb,
    session_id: &str,
    history_limit: usize,
    full_history_without_summary: bool,
) -> ConversationSeed {
    match db.continuation_projection(session_id, history_limit, full_history_without_summary) {
        Ok(projection) => {
            if projection.recovered_interrupted > 0 {
                tracing::warn!(
                    session_id,
                    attempts = projection.recovered_interrupted,
                    "memory: recovered interrupted compaction lifecycle"
                );
            }
            if projection.rejected_invalid > 0 {
                tracing::warn!(
                    session_id,
                    summaries = projection.rejected_invalid,
                    "memory: rejected invalid durable compaction summaries"
                );
            }
            projection_to_seed(projection)
        }
        Err(e) => {
            tracing::warn!(
                "memory: failed to load prior history for session {session_id}: {e}; \
                 continuing without context"
            );
            ConversationSeed::empty()
        }
    }
}

/// Convert stored memory rows into plain-text [`Message`]s suitable
/// for replay into the LLM. Tool calls and results are inlined as
/// short text summaries — never as structured [`ContentBlock::ToolUse`]
/// / [`ContentBlock::ToolResult`] — so providers do not need the
/// original tool_use ids to match. Rows whose flattened text is empty
/// are skipped.
///
/// Replay is a trust boundary. A stored row is *content*, so it is
/// re-labelled by [`trust::LabeledSegment::from_stored`]: an intact
/// fence keeps its recorded source at or below
/// [`trust::TrustClass::parse_ceiling`], and anything else — including
/// every row written before labelling existed — comes back as
/// [`trust::TrustClass::LegacyUnknown`]. Assistant rows stay in the
/// assistant channel unfenced because that channel is already
/// non-authoritative; user rows are re-fenced under the live seal so a
/// stale or crafted marker cannot masquerade as this request's.
fn rows_to_messages(rows: &[sqlite_fts::MessageRow]) -> Vec<Message> {
    rows_to_seed(rows).messages
}

fn rows_to_seed(rows: &[sqlite_fts::MessageRow]) -> ConversationSeed {
    let mut out = ConversationSeed {
        messages: Vec::with_capacity(rows.len()),
        origins: Vec::with_capacity(rows.len()),
    };
    for row in rows {
        // Keep this guard even though continuation loading filters in SQL:
        // injected rows are audit evidence, never conversation content.
        if row.role == sqlite_fts::INJECTED_ROLE {
            continue;
        }
        let Some(message) = stored_replay_message(row) else {
            continue;
        };
        out.messages.push(message.clone());
        out.origins.push(MessageOrigin::Raw {
            id: row.id,
            replay: message,
        });
    }
    out
}

fn stored_replay_message(row: &sqlite_fts::MessageRow) -> Option<Message> {
    replay_persisted_content(&row.role, &row.content, row.trust_source())
}

fn replay_persisted_content(
    stored_role: &str,
    content: &str,
    source: trust::SourceKind,
) -> Option<Message> {
    use crate::agent::llm::{ContentBlock, Role};
    if stored_role == sqlite_fts::INJECTED_ROLE {
        return None;
    }
    let seal = trust::envelope::process_seal();
    let role = match stored_role {
        "assistant" => Role::Assistant,
        "system" => Role::System,
        _ => Role::User,
    };
    let text = super::evidence::strip_markers(&flatten_stored_content_for_replay(content));
    if text.trim().is_empty() {
        return None;
    }
    let text = match role {
        // Model output is replayed as the model's own prior text.
        // It is never policy, and encoding marker digraphs prevents
        // a stale or crafted fence from becoming active.
        Role::Assistant => trust::envelope::encode(&text),
        _ => {
            let in_band = trust::LabeledSegment::from_stored(&text);
            if in_band.kind() != trust::SourceKind::LegacyStoredRow {
                in_band.render_fenced(seal)
            } else {
                match source {
                    trust::SourceKind::LegacyStoredRow | trust::SourceKind::UserMessage => {
                        trust::envelope::encode(&text)
                    }
                    kind => trust::LabeledSegment::of(kind, text).render_fenced(seal),
                }
            }
        }
    };
    Some(Message {
        role,
        content: vec![ContentBlock::Text { text }],
    })
}

fn projection_to_seed(
    projection: crate::agent::memory::compaction::ContinuationProjection,
) -> ConversationSeed {
    let mut seed = ConversationSeed::empty();
    if let Some(summary) = projection.summary {
        seed.messages
            .push(replay_compression_summary(&summary.summary));
        seed.origins.push(MessageOrigin::Summary(Box::new(summary)));
    }
    let tail = rows_to_seed(&projection.tail);
    seed.messages.extend(tail.messages);
    seed.origins.extend(tail.origins);
    seed
}

fn replay_compression_summary(summary: &str) -> Message {
    let recovered = trust::LabeledSegment::from_stored(summary);
    let segment = if recovered.kind() == trust::SourceKind::LegacyStoredRow {
        trust::LabeledSegment::of(trust::SourceKind::ModelCompressionSummary, summary)
    } else {
        recovered
    };
    Message::assistant_text(segment.render_fenced(trust::envelope::process_seal()))
}

fn adopt_compaction_projection(
    projection: crate::agent::memory::compaction::ContinuationProjection,
    live_messages: &[Message],
    live_origins: &[MessageOrigin],
) -> Result<ConversationSeed, AgentError> {
    merge_compaction_projection(projection, live_messages, live_origins).map_err(|reason| {
        AgentError::Compression(format!(
            "could not safely adopt concurrent compaction: {reason}"
        ))
    })
}

fn merge_compaction_projection(
    projection: crate::agent::memory::compaction::ContinuationProjection,
    live_messages: &[Message],
    live_origins: &[MessageOrigin],
) -> Result<ConversationSeed, String> {
    if live_messages.len() != live_origins.len() {
        return Err("live messages and origins have different lengths".to_string());
    }
    validate_origin_order(live_origins)?;

    let covered_end = projection
        .summary
        .as_ref()
        .map(|summary| summary.record.source_end_id);
    let mut adopted = projection_to_seed(projection);
    validate_origin_order(&adopted.origins)?;

    let mut live_by_id: HashMap<i64, (&Message, &MessageOrigin)> = HashMap::new();
    for (message, origin) in live_messages.iter().zip(live_origins) {
        match origin {
            MessageOrigin::Raw { id, .. } => {
                if live_by_id.insert(*id, (message, origin)).is_some() {
                    return Err(format!("live conversation repeats raw message id {id}"));
                }
            }
            MessageOrigin::Summary(summary) => match covered_end {
                Some(end) if summary.record.source_end_id <= end => {}
                Some(end) => {
                    return Err(format!(
                        "live summary ends at {} after winner boundary {end}",
                        summary.record.source_end_id
                    ));
                }
                None => {
                    return Err(
                        "winner has no summary but live conversation already has one".to_string(),
                    );
                }
            },
            MessageOrigin::Ephemeral => {}
        }
    }

    let adopted_raw_order: Vec<i64> = adopted
        .origins
        .iter()
        .filter_map(|origin| match origin {
            MessageOrigin::Raw { id, .. } => Some(*id),
            MessageOrigin::Summary(_) | MessageOrigin::Ephemeral => None,
        })
        .collect();
    let adopted_raw_ids: std::collections::HashSet<i64> =
        adopted_raw_order.iter().copied().collect();
    for id in live_by_id.keys().copied() {
        if covered_end.is_some_and(|end| id <= end) {
            continue;
        }
        if !adopted_raw_ids.contains(&id) {
            return Err(format!(
                "live raw message {id} is absent from the winner's uncompacted tail"
            ));
        }
    }

    for (message, origin) in adopted.messages.iter_mut().zip(&mut adopted.origins) {
        let MessageOrigin::Raw { id, .. } = origin else {
            continue;
        };
        if let Some((live_message, live_origin)) = live_by_id.get(id) {
            *message = (*live_message).clone();
            *origin = (*live_origin).clone();
        }
    }

    let next_durable = next_durable_anchors(live_origins);
    let mut before_raw: HashMap<i64, Vec<(Message, MessageOrigin)>> = HashMap::new();
    let mut after_tail = Vec::new();
    let mut previous = None;
    let mut index = 0;
    while index < live_origins.len() {
        match &live_origins[index] {
            MessageOrigin::Ephemeral => {
                let start = index;
                while index < live_origins.len()
                    && matches!(live_origins[index], MessageOrigin::Ephemeral)
                {
                    index += 1;
                }
                let next = next_durable[index];
                ensure_ephemeral_outside_covered_prefix(previous, next, covered_end)?;
                ensure_ephemeral_slot_is_unambiguous(
                    previous,
                    next,
                    covered_end,
                    &adopted_raw_order,
                )?;
                let run: Vec<(Message, MessageOrigin)> = live_messages[start..index]
                    .iter()
                    .cloned()
                    .zip(live_origins[start..index].iter().cloned())
                    .collect();
                match next {
                    Some(DurableAnchor::Raw(id)) => {
                        if !adopted_raw_ids.contains(&id) {
                            return Err(format!(
                                "cannot position ephemeral messages before missing raw id {id}"
                            ));
                        }
                        before_raw.entry(id).or_default().extend(run);
                    }
                    Some(DurableAnchor::Summary(_)) => {
                        return Err(
                            "ephemeral messages cannot be positioned before a live summary"
                                .to_string(),
                        );
                    }
                    None => after_tail.extend(run),
                }
            }
            origin => {
                previous = durable_anchor(origin);
                index += 1;
            }
        }
    }

    let mut merged = ConversationSeed {
        messages: Vec::with_capacity(
            adopted.messages.len()
                + before_raw.values().map(Vec::len).sum::<usize>()
                + after_tail.len(),
        ),
        origins: Vec::with_capacity(
            adopted.origins.len()
                + before_raw.values().map(Vec::len).sum::<usize>()
                + after_tail.len(),
        ),
    };
    for (message, origin) in adopted.messages.into_iter().zip(adopted.origins) {
        if let MessageOrigin::Raw { id, .. } = &origin {
            if let Some(run) = before_raw.remove(id) {
                for (ephemeral_message, ephemeral_origin) in run {
                    merged.messages.push(ephemeral_message);
                    merged.origins.push(ephemeral_origin);
                }
            }
        }
        merged.messages.push(message);
        merged.origins.push(origin);
    }
    if !before_raw.is_empty() {
        return Err(
            "ephemeral insertion anchors were not present in winner projection".to_string(),
        );
    }
    for (message, origin) in after_tail {
        merged.messages.push(message);
        merged.origins.push(origin);
    }
    validate_active_projection(&merged)?;
    Ok(merged)
}

fn ensure_ephemeral_slot_is_unambiguous(
    previous: Option<DurableAnchor>,
    next: Option<DurableAnchor>,
    covered_end: Option<i64>,
    adopted_raw_ids: &[i64],
) -> Result<(), String> {
    let lower = previous
        .map(DurableAnchor::end_id)
        .or(covered_end)
        .unwrap_or(i64::MIN);
    let unseen = match next {
        Some(DurableAnchor::Raw(upper)) => adopted_raw_ids
            .iter()
            .copied()
            .find(|id| *id > lower && *id < upper),
        Some(DurableAnchor::Summary(_)) => {
            return Err(
                "ephemeral messages cannot be positioned before a live summary".to_string(),
            );
        }
        None => adopted_raw_ids.iter().copied().find(|id| *id > lower),
    };
    if let Some(id) = unseen {
        return Err(format!(
            "cannot position live ephemeral messages relative to unseen winner raw id {id}"
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum DurableAnchor {
    Summary(i64),
    Raw(i64),
}

impl DurableAnchor {
    fn end_id(self) -> i64 {
        match self {
            Self::Summary(id) | Self::Raw(id) => id,
        }
    }
}

fn durable_anchor(origin: &MessageOrigin) -> Option<DurableAnchor> {
    match origin {
        MessageOrigin::Summary(summary) => {
            Some(DurableAnchor::Summary(summary.record.source_end_id))
        }
        MessageOrigin::Raw { id, .. } => Some(DurableAnchor::Raw(*id)),
        MessageOrigin::Ephemeral => None,
    }
}

fn next_durable_anchors(origins: &[MessageOrigin]) -> Vec<Option<DurableAnchor>> {
    let mut result = vec![None; origins.len() + 1];
    let mut next = None;
    for index in (0..origins.len()).rev() {
        if let Some(anchor) = durable_anchor(&origins[index]) {
            next = Some(anchor);
        }
        result[index] = next;
    }
    result
}

fn ensure_ephemeral_outside_covered_prefix(
    previous: Option<DurableAnchor>,
    next: Option<DurableAnchor>,
    covered_end: Option<i64>,
) -> Result<(), String> {
    let Some(covered_end) = covered_end else {
        return Ok(());
    };
    if previous.is_some_and(|anchor| anchor.end_id() >= covered_end) {
        return Ok(());
    }
    if matches!(next, Some(DurableAnchor::Raw(id)) if id <= covered_end) {
        return Err(format!(
            "live ephemeral messages fall inside winner boundary {covered_end} but are absent from its durable compaction input"
        ));
    }
    Err(format!(
        "cannot prove whether live ephemeral messages are before or after winner boundary {covered_end}"
    ))
}

fn validate_ephemerals_outside_covered_prefix(
    origins: &[MessageOrigin],
    covered_end: i64,
) -> Result<(), String> {
    let next_durable = next_durable_anchors(origins);
    let mut previous = None;
    let mut index = 0;
    while index < origins.len() {
        match &origins[index] {
            MessageOrigin::Ephemeral => {
                while index < origins.len() && matches!(origins[index], MessageOrigin::Ephemeral) {
                    index += 1;
                }
                ensure_ephemeral_outside_covered_prefix(
                    previous,
                    next_durable[index],
                    Some(covered_end),
                )?;
            }
            origin => {
                previous = durable_anchor(origin);
                index += 1;
            }
        }
    }
    Ok(())
}

fn validate_origin_order(origins: &[MessageOrigin]) -> Result<(), String> {
    let mut last_end = None;
    let mut saw_summary = false;
    for (index, origin) in origins.iter().enumerate() {
        match origin {
            MessageOrigin::Summary(summary) => {
                if saw_summary || index != 0 {
                    return Err(
                        "conversation summary origin must appear exactly once at the head"
                            .to_string(),
                    );
                }
                saw_summary = true;
                last_end = Some(summary.record.source_end_id);
            }
            MessageOrigin::Raw { id, .. } => {
                if last_end.is_some_and(|last| *id <= last) {
                    return Err(format!(
                        "raw message id {id} is not ordered after durable boundary {}",
                        last_end.unwrap_or_default()
                    ));
                }
                last_end = Some(*id);
            }
            MessageOrigin::Ephemeral => {}
        }
    }
    Ok(())
}

fn validate_active_projection(seed: &ConversationSeed) -> Result<(), String> {
    if seed.messages.len() != seed.origins.len() {
        return Err("projected messages and origins have different lengths".to_string());
    }
    validate_origin_order(&seed.origins)?;

    let mut structured_tools = std::collections::HashSet::new();
    let mut flattened_tools = 0usize;
    let mut has_real_user = false;
    for (message, origin) in seed.messages.iter().zip(&seed.origins) {
        if matches!(origin, MessageOrigin::Summary(_)) {
            continue;
        }
        has_real_user |= message_is_real_user(message);
        for block in &message.content {
            match block {
                llm::ContentBlock::ToolUse { id, .. } => {
                    if !structured_tools.insert(id.as_str()) {
                        return Err(format!("tool use id {id} appears more than once"));
                    }
                }
                llm::ContentBlock::ToolResult { tool_use_id, .. } => {
                    if !structured_tools.remove(tool_use_id.as_str()) {
                        return Err(format!(
                            "tool result {tool_use_id} has no preceding live tool use"
                        ));
                    }
                }
                llm::ContentBlock::Text { text } => {
                    for line in text.lines().map(str::trim_start) {
                        if is_flattened_tool_use(line) {
                            flattened_tools = flattened_tools.saturating_add(1);
                        } else if is_flattened_tool_result(line) {
                            if flattened_tools == 0 {
                                return Err("flattened tool result has no preceding live tool use"
                                    .to_string());
                            }
                            flattened_tools -= 1;
                        }
                    }
                }
                llm::ContentBlock::ToolState { .. }
                | llm::ContentBlock::Reasoning { .. }
                | llm::ContentBlock::Image { .. } => {}
            }
        }
    }
    if let Some(id) = structured_tools.iter().next() {
        return Err(format!("live tool use {id} has no result"));
    }
    if flattened_tools > 0 {
        return Err(format!(
            "{flattened_tools} flattened live tool use(s) have no result"
        ));
    }
    if !has_real_user {
        return Err("projected conversation has no real user anchor".to_string());
    }
    Ok(())
}

fn message_is_real_user(message: &Message) -> bool {
    message.role == llm::Role::User
        && message.content.iter().any(|block| match block {
            llm::ContentBlock::Text { text } => text
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .is_some_and(|line| {
                    !is_flattened_tool_result(line) && !is_flattened_tool_use(line)
                }),
            _ => false,
        })
}

fn is_flattened_tool_use(line: &str) -> bool {
    line.starts_with("[tool: ") || line.starts_with("[tool_use:")
}

fn is_flattened_tool_result(line: &str) -> bool {
    line.starts_with("[tool result]")
        || line.starts_with("[tool result error]")
        || line.starts_with("[tool_result]")
        || line.starts_with("[tool_result:error]")
}

/// Re-fence one replayed user-channel row under the live seal.
///
/// The row's own stored provenance is preferred; an in-band fence is
/// the fallback; anything else is a legacy row. All three paths are
/// clamped, so a row can only ever come back at or below
/// [`trust::TrustClass::parse_ceiling`].
fn replayed_user_text(seal: &trust::Seal, row: &sqlite_fts::MessageRow, text: &str) -> String {
    let in_band = trust::LabeledSegment::from_stored(text);
    if in_band.kind() != trust::SourceKind::LegacyStoredRow {
        return in_band.render_fenced(seal);
    }
    match row.trust_source() {
        // A row written before provenance columns existed, or written
        // without a label. It keeps the user channel verbatim, but any
        // marker digraph it carries is encoded so it cannot open or
        // close a fence.
        trust::SourceKind::LegacyStoredRow => trust::envelope::encode(text),
        trust::SourceKind::UserMessage => trust::envelope::encode(text),
        kind => trust::LabeledSegment::of(kind, text).render_fenced(seal),
    }
}

/// Derive a message's provenance from its content blocks.
///
/// Structural, never byte-asserted: an assistant turn is model output,
/// a tool-result block keeps whatever label its ingestion adapter
/// fenced it with, and an unfenced tool result falls back to
/// [`trust::SourceKind::BuiltinToolResult`]. The result takes the
/// least-trusted class across the blocks.
fn message_provenance(message: &Message) -> trust::LabeledSegment {
    use crate::agent::llm::{ContentBlock, Role};

    let base = match message.role {
        Role::Assistant => trust::SourceKind::ModelResponse,
        Role::System => trust::SourceKind::SessionExtras,
        Role::Tool => trust::SourceKind::BuiltinToolResult,
        Role::User => trust::SourceKind::ReplayedUserTurn,
    };
    let mut segment = trust::LabeledSegment::of(base, "");
    for block in &message.content {
        let next = match block {
            ContentBlock::ToolResult { content, .. } => {
                let recovered = trust::LabeledSegment::from_stored(content);
                if recovered.kind() == trust::SourceKind::LegacyStoredRow {
                    trust::LabeledSegment::of(trust::SourceKind::BuiltinToolResult, "")
                } else {
                    trust::LabeledSegment::of(recovered.kind(), "")
                }
            }
            ContentBlock::Text { text } => {
                let recovered = trust::LabeledSegment::from_stored(text);
                if recovered.kind() == trust::SourceKind::LegacyStoredRow {
                    continue;
                }
                trust::LabeledSegment::of(recovered.kind(), "")
            }
            ContentBlock::Reasoning { .. } => {
                trust::LabeledSegment::of(trust::SourceKind::ModelReasoning, "")
            }
            ContentBlock::ToolUse { .. } | ContentBlock::ToolState { .. } => continue,
            ContentBlock::Image { .. } => {
                trust::LabeledSegment::of(trust::SourceKind::MediaTranscript, "")
            }
        };
        segment = segment.concat(&next);
    }
    segment
}

#[derive(Debug, Clone, Copy)]
struct DurableCoverage {
    start_id: i64,
    end_id: i64,
    count: usize,
    previous_compaction_id: Option<i64>,
}

fn coverage_for_prefix(origins: &[MessageOrigin], count: usize) -> Option<DurableCoverage> {
    if count == 0 || count > origins.len() {
        return None;
    }
    let mut coverage: Option<DurableCoverage> = None;
    for (index, origin) in origins[..count].iter().enumerate() {
        match origin {
            MessageOrigin::Raw { id, .. } => {
                let current = coverage.get_or_insert(DurableCoverage {
                    start_id: *id,
                    end_id: *id,
                    count: 0,
                    previous_compaction_id: None,
                });
                if *id <= current.end_id && current.count > 0 {
                    return None;
                }
                current.end_id = *id;
                current.count = current.count.saturating_add(1);
            }
            MessageOrigin::Summary(summary) if index == 0 && coverage.is_none() => {
                coverage = Some(DurableCoverage {
                    start_id: summary.record.source_start_id,
                    end_id: summary.record.source_end_id,
                    count: summary.record.source_count,
                    previous_compaction_id: Some(summary.record.id),
                });
            }
            MessageOrigin::Summary(_) | MessageOrigin::Ephemeral => return None,
        }
    }
    coverage
}

fn first_raw_origin_id(origins: &[MessageOrigin]) -> Option<i64> {
    origins.iter().find_map(|origin| match origin {
        MessageOrigin::Raw { id, .. } => Some(*id),
        MessageOrigin::Summary(_) | MessageOrigin::Ephemeral => None,
    })
}

fn last_real_raw_user_id(origins: &[MessageOrigin]) -> Option<i64> {
    origins.iter().rev().find_map(|origin| match origin {
        MessageOrigin::Raw { id, replay } if message_is_real_user(replay) => Some(*id),
        MessageOrigin::Raw { .. } | MessageOrigin::Summary(_) | MessageOrigin::Ephemeral => None,
    })
}

fn durable_projection_message(origin: &MessageOrigin, fallback: &Message) -> Message {
    match origin {
        MessageOrigin::Raw { replay, .. } => replay.clone(),
        MessageOrigin::Summary(summary) => replay_compression_summary(&summary.summary),
        MessageOrigin::Ephemeral => fallback.clone(),
    }
}

fn redact_message(message: &mut Message, redactor: Option<&Redactor>) {
    let Some(redactor) = redactor else {
        return;
    };
    for block in &mut message.content {
        match block {
            llm::ContentBlock::Text { text }
            | llm::ContentBlock::ToolResult { content: text, .. } => {
                *text = redactor.redact(text);
            }
            llm::ContentBlock::ToolUse { input, .. } => {
                let serialized = input.to_string();
                let redacted = redactor.redact(&serialized);
                if let Ok(value) = serde_json::from_str(&redacted) {
                    *input = value;
                }
            }
            llm::ContentBlock::Reasoning { summary, .. } => {
                for text in summary {
                    *text = redactor.redact(text);
                }
            }
            llm::ContentBlock::ToolState { .. } | llm::ContentBlock::Image { .. } => {}
        }
    }
}

fn scrub_messages_with_origins(
    messages: Vec<Message>,
    origins: Vec<MessageOrigin>,
) -> (Vec<Message>, Vec<MessageOrigin>) {
    let scrubber = ThinkScrubber::new();
    let mut scrubbed_messages = Vec::with_capacity(messages.len());
    let mut scrubbed_origins = Vec::with_capacity(origins.len());
    for (message, origin) in messages.into_iter().zip(origins) {
        let mut scrubbed = scrubber.scrub_messages(vec![message]);
        if let Some(message) = scrubbed.pop() {
            scrubbed_messages.push(message);
            scrubbed_origins.push(origin);
        }
    }
    (scrubbed_messages, scrubbed_origins)
}

#[allow(clippy::too_many_arguments)]
async fn maybe_compress_messages(
    compressor: &Arc<dyn Compressor>,
    system: &str,
    messages: &mut Vec<Message>,
    origins: &mut Vec<MessageOrigin>,
    recorder: Option<(&MemoryDb, &str)>,
    redactor: Option<&Redactor>,
    provider_name: &str,
    model_name: &str,
) -> Result<bool, AgentError> {
    maybe_compress_messages_with_policy(
        compressor,
        system,
        messages,
        origins,
        recorder,
        redactor,
        provider_name,
        model_name,
        CompressionRetryPolicy::default(),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn maybe_compress_messages_with_policy(
    compressor: &Arc<dyn Compressor>,
    system: &str,
    messages: &mut Vec<Message>,
    origins: &mut Vec<MessageOrigin>,
    recorder: Option<(&MemoryDb, &str)>,
    redactor: Option<&Redactor>,
    provider_name: &str,
    model_name: &str,
    retry: CompressionRetryPolicy,
) -> Result<bool, AgentError> {
    if messages.len() != origins.len() {
        return Err(AgentError::Internal(
            "conversation origin tracking diverged from messages".to_string(),
        ));
    }

    let mut current_messages = std::mem::take(messages);
    let mut current_origins = std::mem::take(origins);
    let mut changed = false;
    let mut replans = 0;

    'replan: loop {
        if !compressor.should_compress(Some(system), &current_messages) {
            *messages = current_messages;
            *origins = current_origins;
            return Ok(changed);
        }
        if replans >= retry.max_replans {
            *messages = current_messages;
            *origins = current_origins;
            return Err(AgentError::Compression(format!(
                "context remained above the configured compression trigger after {} bounded replans",
                retry.max_replans
            )));
        }
        if current_messages.len() != current_origins.len() {
            *messages = current_messages;
            *origins = current_origins;
            return Err(AgentError::Internal(
                "conversation origin tracking diverged during compression".to_string(),
            ));
        }

        let plan_origins: Vec<MessageOrigin> = if recorder.is_some() {
            current_origins
                .iter()
                .filter(|origin| !matches!(origin, MessageOrigin::Ephemeral))
                .cloned()
                .collect()
        } else {
            current_origins.clone()
        };
        let mut projection_messages: Vec<Message> = current_messages
            .iter()
            .zip(&current_origins)
            .filter(|(_, origin)| recorder.is_none() || !matches!(origin, MessageOrigin::Ephemeral))
            .map(|(message, origin)| durable_projection_message(origin, message))
            .collect();
        let planning_system = recorder.map(|_| {
            let ephemeral_tokens = current_messages
                .iter()
                .zip(&current_origins)
                .filter(|(_, origin)| matches!(origin, MessageOrigin::Ephemeral))
                .fold(0_u32, |total, (message, _)| {
                    total.saturating_add(compressor::estimate_message_tokens(message))
                });
            let mut synthetic = String::with_capacity(
                system
                    .len()
                    .saturating_add(ephemeral_tokens as usize * 4)
                    .saturating_add(1),
            );
            synthetic.push_str(system);
            synthetic.push('\n');
            synthetic.extend(std::iter::repeat_n('x', ephemeral_tokens as usize * 4));
            synthetic
        });
        let planning_system = planning_system.as_deref().unwrap_or(system);
        for message in &mut projection_messages {
            redact_message(message, redactor);
        }

        let Some(plan) = compressor.prepare_compaction(Some(planning_system), projection_messages)
        else {
            if recorder.is_some() {
                *messages = current_messages;
                *origins = current_origins;
                return Err(AgentError::Compression(
                    "over-threshold context cannot be reduced from its durable rows without dropping request-scoped evidence"
                        .to_string(),
                ));
            }
            current_messages = compressor
                .compress(Some(system), std::mem::take(&mut current_messages))
                .await;
            current_origins = vec![MessageOrigin::Ephemeral; current_messages.len()];
            changed = true;
            replans += 1;
            continue;
        };
        let source_count = plan.source_message_count();
        if source_count > current_messages.len() {
            *messages = current_messages;
            *origins = current_origins;
            return Err(AgentError::Internal(
                "compressor returned an invalid source boundary".to_string(),
            ));
        }

        let Some((db, session_id)) = recorder else {
            let execution = compressor.execute_compaction(plan).await;
            if execution.projection.is_none() {
                *messages = current_messages;
                *origins = current_origins;
                return Err(AgentError::Compression(format!(
                    "summary generation failed: {}",
                    execution.failure.unwrap_or("unknown_failure")
                )));
            }
            current_messages = execution.messages;
            current_origins = vec![MessageOrigin::Ephemeral; current_messages.len()];
            changed = true;
            replans += 1;
            continue;
        };
        let Some(coverage) = coverage_for_prefix(&plan_origins, source_count) else {
            *messages = current_messages;
            *origins = current_origins;
            return Err(AgentError::Compression(
                "over-threshold context has no durably reconstructable source range".to_string(),
            ));
        };
        if let Err(reason) =
            validate_ephemerals_outside_covered_prefix(&current_origins, coverage.end_id)
        {
            *messages = current_messages;
            *origins = current_origins;
            return Err(AgentError::Compression(format!(
                "over-threshold context has request-scoped evidence inside its durable source range: {reason}"
            )));
        }
        let Some(protected_tail_start_id) = first_raw_origin_id(&plan_origins[source_count..])
        else {
            *messages = current_messages;
            *origins = current_origins;
            return Err(AgentError::Compression(
                "over-threshold context has no durable protected tail".to_string(),
            ));
        };
        let Some(protected_user_message_id) = last_real_raw_user_id(&plan_origins[source_count..])
        else {
            *messages = current_messages;
            *origins = current_origins;
            return Err(AgentError::Compression(
                "over-threshold context has no durable real-user anchor".to_string(),
            ));
        };
        let spec = NewCompaction {
            source_start_id: coverage.start_id,
            source_end_id: coverage.end_id,
            source_count: coverage.count,
            protected_tail_start_id: Some(protected_tail_start_id),
            protected_user_message_id: Some(protected_user_message_id),
            algorithm: plan.algorithm().to_string(),
            algorithm_version: plan.algorithm_version(),
            provider: provider_name.to_string(),
            model: model_name.to_string(),
            previous_compaction_id: coverage.previous_compaction_id,
            pruned_tool_results: plan.pruned_tool_results(),
        };

        let busy_started = tokio::time::Instant::now();
        let attempt = loop {
            match db.begin_compaction(session_id, spec.clone()) {
                Ok(BeginCompaction::Started(attempt)) => break attempt,
                Ok(BeginCompaction::Busy) => {
                    if busy_started.elapsed() >= retry.busy_timeout {
                        *messages = current_messages;
                        *origins = current_origins;
                        return Err(AgentError::Compression(format!(
                            "timed out after {:?} waiting for active compaction of session {session_id}",
                            retry.busy_timeout
                        )));
                    }
                    tokio::time::sleep(retry.busy_poll_interval).await;
                }
                Ok(BeginCompaction::AlreadyCovered(projection)) => {
                    tracing::debug!(
                        session_id,
                        "context: adopting concurrently completed durable compaction"
                    );
                    let adopted = match adopt_compaction_projection(
                        projection,
                        &current_messages,
                        &current_origins,
                    ) {
                        Ok(adopted) => adopted,
                        Err(error) => {
                            *messages = current_messages;
                            *origins = current_origins;
                            return Err(error);
                        }
                    };
                    current_messages = adopted.messages;
                    current_origins = adopted.origins;
                    changed = true;
                    replans += 1;
                    continue 'replan;
                }
                Ok(BeginCompaction::StalePlan(projection)) => {
                    tracing::debug!(
                        session_id,
                        "context: compaction plan became stale; adopting winner projection"
                    );
                    let adopted = match adopt_compaction_projection(
                        projection,
                        &current_messages,
                        &current_origins,
                    ) {
                        Ok(adopted) => adopted,
                        Err(error) => {
                            *messages = current_messages;
                            *origins = current_origins;
                            return Err(error);
                        }
                    };
                    current_messages = adopted.messages;
                    current_origins = adopted.origins;
                    changed = true;
                    replans += 1;
                    continue 'replan;
                }
                Err(error) if error.is_integrity_failure() => {
                    *messages = current_messages;
                    *origins = current_origins;
                    return Err(AgentError::MemoryIntegrity(format!(
                        "refusing compaction for session {session_id}: {error}"
                    )));
                }
                Err(error) => {
                    *messages = current_messages;
                    *origins = current_origins;
                    return Err(AgentError::Compression(format!(
                        "could not start durable compaction for session {session_id}: {error}"
                    )));
                }
            }
        };

        let execution = compressor.execute_compaction(plan).await;
        let Some(projection) = execution.projection else {
            let failure = execution.failure.unwrap_or("summary_generation_failed");
            if let Err(error) = attempt.fail(failure) {
                tracing::warn!(session_id, %error, "context: failed to close compaction attempt");
            }
            *messages = current_messages;
            *origins = current_origins;
            return Err(AgentError::Compression(format!(
                "summary generation failed for session {session_id}: {failure}"
            )));
        };
        let mut summary_text = super::evidence::strip_markers(&projection.summary_text);
        if let Some(redactor) = redactor {
            summary_text = redactor.redact(&summary_text);
        }
        let summary_text = summary_text.trim().to_string();
        match attempt.complete(&summary_text) {
            Ok(_) => {
                let projection = match db.continuation_projection(session_id, 0, true) {
                    Ok(projection) => projection,
                    Err(error) if error.is_integrity_failure() => {
                        *messages = current_messages;
                        *origins = current_origins;
                        return Err(AgentError::MemoryIntegrity(format!(
                            "failed to reload committed compaction for session {session_id}: {error}"
                        )));
                    }
                    Err(error) => {
                        *messages = current_messages;
                        *origins = current_origins;
                        return Err(AgentError::Compression(format!(
                            "failed to reload committed compaction for session {session_id}: {error}"
                        )));
                    }
                };
                let compacted = match adopt_compaction_projection(
                    projection,
                    &current_messages,
                    &current_origins,
                ) {
                    Ok(compacted) => compacted,
                    Err(error) => {
                        *messages = current_messages;
                        *origins = current_origins;
                        return Err(error);
                    }
                };
                current_messages = compacted.messages;
                current_origins = compacted.origins;
                changed = true;
                replans += 1;
            }
            Err(error) if error.is_integrity_failure() => {
                *messages = current_messages;
                *origins = current_origins;
                return Err(AgentError::MemoryIntegrity(format!(
                    "failed to commit compaction for session {session_id}: {error}"
                )));
            }
            Err(error) => {
                *messages = current_messages;
                *origins = current_origins;
                return Err(AgentError::Compression(format!(
                    "failed to persist compaction for session {session_id}: {error}"
                )));
            }
        }
    }
}

/// Flatten the `render_message_content` marker format back into a
/// single text payload. Mirror of the parser in
/// `agent::web::routes::sessions::parse_stored_content`, but lossy
/// where the web parser is structured — here `[tool_use:NAME] {json}`
/// collapses to `[tool: NAME]` and `[tool_result] body` becomes
/// `[tool result]\n<truncated body>` because the goal is replay
/// context, not exact reconstruction. Long tool-result bodies are
/// truncated to keep the replayed prompt cheap. Runtime evidence markers are
/// stripped so stale call ids cannot be cited in a later invocation.
fn flatten_stored_content(content: &str) -> String {
    const MAX_RESULT_PREVIEW_CHARS: usize = 1500;
    flatten_stored_content_with_limit(content, Some(MAX_RESULT_PREVIEW_CHARS))
}

fn flatten_stored_content_for_replay(content: &str) -> String {
    flatten_stored_content_with_limit(content, None)
}

fn flatten_stored_content_with_limit(content: &str, max_result_chars: Option<usize>) -> String {
    let mut out = String::new();
    let mut active_result: Option<(bool, String)> = None;

    let push_separator = |out: &mut String| {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
    };

    let flush_result =
        |active: &mut Option<(bool, String)>, out: &mut String, max_chars: Option<usize>| {
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
                    match max_chars {
                        Some(max_chars) if trimmed.chars().count() > max_chars => {
                            let preview: String = trimmed.chars().take(max_chars).collect();
                            out.push_str(&preview);
                            out.push_str("\n…[truncated]");
                        }
                        _ => out.push_str(trimmed),
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
                    flush_result(&mut active_result, &mut out, max_result_chars);
                    push_separator(&mut out);
                    out.push_str("[tool: ");
                    out.push_str(name);
                    out.push(']');
                    continue;
                }
            }
        }

        if let Some(rest) = trimmed.strip_prefix("[tool_result:error]") {
            flush_result(&mut active_result, &mut out, max_result_chars);
            active_result = Some((true, rest.trim_start_matches([' ', '\t']).to_string()));
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("[tool_result]") {
            flush_result(&mut active_result, &mut out, max_result_chars);
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

    flush_result(&mut active_result, &mut out, max_result_chars);
    out.trim().to_string()
}

fn record_injected_segments(
    recorder: Option<(&MemoryDb, &str)>,
    segments: &[prompt::InjectedSegment],
) {
    let Some((db, sid)) = recorder else {
        return;
    };
    for segment in segments {
        if let Err(error) = db.record_injected(sid, segment.source(), &segment.content) {
            tracing::warn!(
                source = segment.source(),
                trust = %segment.class(),
                %error,
                "memory: failed to record model-visible context"
            );
        }
    }
}

/// Resolve the request's typed channel split.
///
/// The policy channel is the session's frozen, content-addressed
/// snapshot — the compiled scaffold plus, when ownership verification
/// passes, a root-owned operator policy file. Everything else is
/// prelude data rebuilt per request: memory notes, the Skill catalogue,
/// an owner-writable prompt file, due reminders and transient App
/// context. None of it can reach `system`, because
/// [`trust::PromptProjection::push`] routes by class.
fn resolve_projection(
    deps: &RuntimeDeps,
    cfg: &AgentConfig,
    user_prompt: &str,
    transient_context: Option<&str>,
    recorder: Option<(&MemoryDb, &str)>,
) -> Result<trust::PromptProjection, AgentError> {
    let extra = cfg.system_prompt_path.as_deref().map(Path::new);
    let mut projection = match deps.paths() {
        Some(paths) => {
            let options = crate::agent::skills::loader::LoadOptions {
                include_body: false,
                ..Default::default()
            };
            let skills = crate::agent::skills::loader::load_layered_with_origin(
                &paths.system_skills_dir,
                &paths.user_skills_dir,
                &options,
                paths.system_skills_origin,
            );
            prompt::build_projection(extra, Some(user_prompt), &skills, deps.notes())
        }
        None => {
            let skills = crate::agent::skills::loader::load_catalog_default();
            prompt::build_projection(extra, Some(user_prompt), &skills, deps.notes())
        }
    };

    projection.extend_prelude(turn_context_segments(deps, transient_context));
    projection.push(trust::LabeledSegment::of(
        trust::SourceKind::UserMessage,
        user_prompt,
    ));

    freeze_policy(recorder, &mut projection)?;
    record_projection(recorder, &projection);
    debug_assert!(
        projection.channels_are_separated(),
        "prompt projection mixed trust channels"
    );
    Ok(projection)
}

/// Request-local segments that are not part of prompt assembly.
fn turn_context_segments(
    deps: &RuntimeDeps,
    transient_context: Option<&str>,
) -> Vec<trust::LabeledSegment> {
    let mut segments = match deps.paths() {
        Some(paths) => prompt::build_turn_context_segments_with(
            &crate::agent::nudge::NudgeStore::new(&paths.nudges_path),
            deps.now_ms() / 1_000,
        ),
        None => prompt::build_turn_context_segments(),
    }
    .into_iter()
    .map(|segment| trust::LabeledSegment::of(segment.kind, segment.raw))
    .collect::<Vec<_>>();

    if let Some(context) = transient_context.filter(|value| !value.trim().is_empty()) {
        segments.push(trust::LabeledSegment::of(
            trust::SourceKind::TransientAppContext,
            context.trim(),
        ));
    }
    segments
}

/// Freeze the policy channel so a session keeps a stable, cacheable
/// prefix, and restore it on later turns.
///
/// Only policy is frozen. Prelude data is deliberately *not* frozen:
/// it changes per turn, and freezing it is what previously let a
/// version-3 snapshot carry owner-controlled bytes in `system`.
fn freeze_policy(
    recorder: Option<(&MemoryDb, &str)>,
    projection: &mut trust::PromptProjection,
) -> Result<(), AgentError> {
    let Some((db, sid)) = recorder else {
        return Ok(());
    };
    match db.system_prompt_for(sid, prompt::CANONICAL_PROMPT_VERSION) {
        Ok(Some(frozen)) => {
            projection.replace_policy(frozen);
            return Ok(());
        }
        Ok(None) => {}
        Err(error) if error.is_integrity_failure() => {
            return Err(AgentError::MemoryIntegrity(format!(
                "refusing session {sid}: {error}; run `cos agent sessions health` and explicit repair"
            )));
        }
        Err(error) => {
            tracing::warn!(
                session_id = sid,
                %error,
                "memory: failed to restore frozen system prompt; rebuilding"
            );
        }
    }
    let candidate = projection.system_text();
    match db.freeze_system_prompt(sid, &candidate, prompt::CANONICAL_PROMPT_VERSION) {
        Ok(snapshot) => {
            projection.replace_policy(snapshot.prompt);
            Ok(())
        }
        Err(error) if error.is_integrity_failure() => Err(AgentError::MemoryIntegrity(format!(
            "refusing session {sid}: {error}; run `cos agent sessions health` and explicit repair"
        ))),
        Err(error) => {
            tracing::warn!(
                session_id = sid,
                %error,
                "memory: failed to freeze system prompt; using request-local candidate"
            );
            Ok(())
        }
    }
}

/// Record every model-visible segment as an `injected` audit row.
///
/// The owner's own turn is recorded through the normal message path, so
/// it is skipped here to avoid a duplicate row.
fn record_projection(recorder: Option<(&MemoryDb, &str)>, projection: &trust::PromptProjection) {
    let seal = trust::envelope::process_seal();
    let segments = projection
        .policy_segments()
        .iter()
        .filter(|segment| segment.kind() != trust::SourceKind::SystemScaffold)
        .chain(projection.prelude_segments())
        .map(|segment| prompt::InjectedSegment {
            kind: segment.kind(),
            content: segment.render(seal),
            raw: segment.content().to_string(),
        })
        .collect::<Vec<_>>();
    record_injected_segments(recorder, &segments);
}

/// The provider-response adapter for the shared ask lifecycle. All recording,
/// hooks, compression, evidence, and terminal transitions stay in
/// [`ask_inner`].
enum LifecycleOutput {
    Buffered,
    Streaming { sink: Arc<dyn StreamSink> },
}

impl LifecycleOutput {
    fn emit_fallback(&self, answer: &str) {
        if let Self::Streaming { sink } = self {
            sink.on_event(&llm::StreamEvent::TextDelta {
                text: answer.to_string(),
            });
        }
    }
}

struct LifecycleRequest<'a> {
    deps: &'a RuntimeDeps,
    provider: Arc<dyn Provider>,
    cfg: &'a AgentConfig,
    user_prompt: &'a str,
    tools: &'a ToolRegistry,
    exposure: Option<&'a ToolExposureContext>,
    recorder: Option<(&'a MemoryDb, &'a str)>,
    compressor: Option<Arc<dyn Compressor>>,
    initial_messages: ConversationSeed,
    transient_context: Option<&'a str>,
    output: LifecycleOutput,
    progress: Arc<dyn ProgressSink>,
    interrupt_scope: Option<&'a str>,
    delegated: bool,
}

/// Shared lifecycle state machine for buffered and streaming asks.
///
/// The only mode-specific operation is provider response delivery through
/// [`LifecycleOutput`]. Preparation, turn boundaries, persistence,
/// finalization, and terminal-state rules are owned here.
async fn ask_inner(request: LifecycleRequest<'_>) -> Result<AskResult, AgentError> {
    let hooks = request.deps.hooks().clone();
    crate::agent::runtime::hooks::with_registry(hooks, ask_inner_scoped(request)).await
}

async fn ask_inner_scoped(request: LifecycleRequest<'_>) -> Result<AskResult, AgentError> {
    let LifecycleRequest {
        deps,
        provider,
        cfg,
        user_prompt,
        tools,
        exposure,
        recorder,
        compressor,
        initial_messages,
        transient_context,
        output,
        progress,
        interrupt_scope,
        delegated,
    } = request;
    let fallback_exposure = ToolExposureContext::isolated(tools.guardrails().clone());
    let exposure = exposure.unwrap_or(&fallback_exposure);
    let redactor: Option<Redactor> = if cfg.redact_memory_enabled {
        Some(Redactor::default_set())
    } else {
        None
    };
    let progress = match recorder {
        Some((db, sid)) => {
            progress::recording_progress(progress, db.clone(), sid, cfg.redact_memory_enabled)
        }
        None => progress,
    };

    // Semantic auto-indexer: opt-in via `[embed]` config. Mirrors
    // every recorded message into the semantic store so the LLM can
    // do similarity search via `cos_recall_semantic`. `None` when
    // embedding is disabled — every spawn_index call is then a no-op.
    let semantic_indexer = if recorder.is_some() {
        deps.semantic_indexer()
    } else {
        None
    };
    // Auto-curator: opt-in via `[agent] auxiliary_*` config. After
    // each final answer, fires `curate_session` in the background to
    // extract durable user facts and append them to MEMORY.md.
    let auto_curator = recorder.and_then(|(db, _)| {
        let config = deps
            .config_snapshot()
            .unwrap_or_else(crate::config::current_snapshot);
        AutoCurator::from_snapshot_with_runtime_paths(
            config,
            db,
            deps.notes().clone(),
            deps.routed_paths(),
            deps.curation_log().to_path_buf(),
        )
    });

    let mut user_origin = MessageOrigin::Ephemeral;
    if let Some((db, sid)) = recorder {
        let to_record = redactor
            .as_ref()
            .map(|r| r.redact(user_prompt))
            .unwrap_or_else(|| user_prompt.to_string());
        let segment = trust::LabeledSegment::of(trust::SourceKind::UserMessage, "");
        match db.record_labeled_message(sid, "user", &segment, &to_record) {
            Ok(msg_id) => {
                if let Some(replay) = replay_persisted_content("user", &to_record, segment.kind()) {
                    user_origin = MessageOrigin::Raw { id: msg_id, replay };
                }
                if let Some(ix) = &semantic_indexer {
                    ix.spawn_index(sid.to_string(), "user", msg_id, to_record.clone());
                }
            }
            Err(e) => tracing::warn!("memory: failed to record user prompt: {e}"),
        }
    }

    let projection = resolve_projection(deps, cfg, user_prompt, transient_context, recorder)?;
    let system = projection.system_text();

    let ConversationSeed {
        mut messages,
        mut origins,
    } = initial_messages;
    let request_messages = projection.request_messages(trust::envelope::process_seal());
    let instruction_offset = projection
        .instruction_segment()
        .is_some()
        .then(|| request_messages.len().saturating_sub(1));
    let request_start = messages.len();
    origins.resize(
        request_start + request_messages.len(),
        MessageOrigin::Ephemeral,
    );
    messages.extend(request_messages);
    if let Some(offset) = instruction_offset {
        origins[request_start + offset] = user_origin;
    }
    let llm_tools = if cfg.progressive_tools_enabled {
        tools.as_llm_tools_for(exposure)
    } else {
        tools.direct_llm_tools_for(exposure)
    };
    let session_id = recorder.map(|(_, sid)| sid.to_string()).unwrap_or_default();

    // Register this session in the global interrupt registry. When the
    // session id is empty (no memory recording) we fall back to a
    // freshly-minted UUID so concurrent unrecorded sessions still get
    // independent interrupt scopes. Handle's `Drop` cleans up.
    let interrupt_handle = if let Some(scope) = interrupt_scope {
        interrupt::register(scope)
    } else if session_id.is_empty() {
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
    let hook_registry = deps.hooks().clone();
    let hook_session_id = if session_id.is_empty() {
        interrupt_handle.session_id().to_string()
    } else {
        session_id.clone()
    };
    let mut evidence_ledger = super::evidence::EvidenceLedger::default();

    let turn_limit = cfg.max_turns.max(1);
    for turn in 1..=turn_limit {
        let force_finalize = turn == turn_limit;
        let finalization_system =
            force_finalize.then(|| format!("{system}\n\n{TURN_LIMIT_FINALIZATION_PROMPT}"));
        let turn_system = finalization_system.as_deref().unwrap_or(&system);

        if interrupt_handle.check() {
            return Err(AgentError::Interrupted(
                interrupt_handle.session_id().to_string(),
            ));
        }

        let turn_started_ms = deps.now_ms();
        let hook_ctx = hooks::HookContext::new(
            hook_session_id.clone(),
            provider.effective_provider_name(),
            provider.effective_model_name(&cfg.model),
        )
        .with_started_at_ms(turn_started_ms)
        .with_delegated(delegated)
        .with_turn_index(turn);
        if let hooks::HookOutcome::Stop(reason) = hook_registry.dispatch_pre_turn(&hook_ctx) {
            return Err(AgentError::Interrupted(format!(
                "hook stop (pre_turn): {reason}"
            )));
        }
        if force_finalize {
            if let Some((db, sid)) = recorder {
                if let Err(e) = db.record_injected(
                    sid,
                    "turn_limit_finalization",
                    TURN_LIMIT_FINALIZATION_PROMPT,
                ) {
                    tracing::warn!("memory: failed to record turn-limit finalization: {e}");
                }
            }
        }

        if cfg.think_scrub_enabled {
            let before = messages.len();
            (messages, origins) = scrub_messages_with_origins(
                std::mem::take(&mut messages),
                std::mem::take(&mut origins),
            );
            let after = messages.len();
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
            let before = messages.len();
            let est_before = compressor::estimate_total_tokens(Some(turn_system), &messages);
            let provider_name = provider.effective_provider_name();
            let model_name = provider.effective_model_name(&cfg.model);
            if maybe_compress_messages(
                c,
                turn_system,
                &mut messages,
                &mut origins,
                recorder,
                redactor.as_ref(),
                &provider_name,
                &model_name,
            )
            .await?
            {
                let after = messages.len();
                let est_after = compressor::estimate_total_tokens(Some(turn_system), &messages);
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
        let outcome_result = match (&output, force_finalize) {
            (LifecycleOutput::Buffered, true) => {
                super::turn::run_final_turn_interruptible(
                    provider.clone(),
                    &cfg.model,
                    turn_system,
                    &mut messages,
                    tools,
                    exposure,
                    &llm_tools,
                    cfg.max_tokens,
                    cfg.temperature,
                    recorder.map(|(_, sid)| sid),
                    retry_policy_from_cfg(cfg),
                    Some(&hook_ctx),
                    progress.clone(),
                    &interrupt_handle,
                )
                .await
            }
            (LifecycleOutput::Buffered, false) => {
                super::turn::run_turn_interruptible(
                    provider.clone(),
                    &cfg.model,
                    turn_system,
                    &mut messages,
                    tools,
                    exposure,
                    &llm_tools,
                    cfg.max_tokens,
                    cfg.temperature,
                    recorder.map(|(_, sid)| sid),
                    retry_policy_from_cfg(cfg),
                    Some(&hook_ctx),
                    progress.clone(),
                    &interrupt_handle,
                )
                .await
            }
            (LifecycleOutput::Streaming { sink }, true) => {
                super::turn::run_final_turn_streaming_interruptible(
                    provider.clone(),
                    &cfg.model,
                    turn_system,
                    &mut messages,
                    tools,
                    exposure,
                    &llm_tools,
                    cfg.max_tokens,
                    cfg.temperature,
                    recorder.map(|(_, sid)| sid),
                    sink.clone(),
                    Some(&hook_ctx),
                    progress.clone(),
                    &interrupt_handle,
                )
                .await
            }
            (LifecycleOutput::Streaming { sink }, false) => {
                super::turn::run_turn_streaming_interruptible(
                    provider.clone(),
                    &cfg.model,
                    turn_system,
                    &mut messages,
                    tools,
                    exposure,
                    &llm_tools,
                    cfg.max_tokens,
                    cfg.temperature,
                    recorder.map(|(_, sid)| sid),
                    sink.clone(),
                    Some(&hook_ctx),
                    progress.clone(),
                    &interrupt_handle,
                )
                .await
            }
        };

        let now_ms = deps.now_ms();
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
                let mut post_hook_ctx = hook_ctx.clone();
                post_hook_ctx.provider = provider.effective_provider_name();
                post_hook_ctx.model = provider.effective_model_name(&cfg.model);
                if let hooks::HookOutcome::Stop(reason) =
                    hook_registry.dispatch_post_turn(&post_hook_ctx, &summary)
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
                let mut post_hook_ctx = hook_ctx.clone();
                post_hook_ctx.provider = provider.effective_provider_name();
                post_hook_ctx.model = provider.effective_model_name(&cfg.model);
                let _ = hook_registry.dispatch_post_turn(&post_hook_ctx, &summary);
                if matches!(&e, AgentError::Interrupted(_)) {
                    return Err(e);
                }
                if force_finalize {
                    tracing::warn!("turn-limit finalization failed; using fallback: {e}");
                    let answer = append_turn_limit_fallback(&mut messages);
                    output.emit_fallback(&answer);
                    super::turn::TurnOutcome::Final(answer)
                } else {
                    return Err(e);
                }
            }
        };
        if interrupt_handle.check() {
            return Err(AgentError::Interrupted(
                interrupt_handle.session_id().to_string(),
            ));
        }
        let outcome = match outcome {
            super::turn::TurnOutcome::Final(answer)
                if force_finalize && answer.trim().is_empty() =>
            {
                let answer = append_turn_limit_fallback(&mut messages);
                output.emit_fallback(&answer);
                super::turn::TurnOutcome::Final(answer)
            }
            other => other,
        };
        evidence_ledger.observe(&messages[len_before..]);

        // Persist any messages appended by this turn (assistant message and
        // any tool-result message). When `redact_memory_enabled`, scrub the
        // rendered content through `Redactor::default_set()` BEFORE writing,
        // so secrets that arrived via tool results never enter the FTS5
        // index. The in-memory `messages` vec is left untouched — the model
        // still sees the originals on the next turn.
        let mut appended_origins = Vec::with_capacity(messages.len().saturating_sub(len_before));
        if let Some((db, sid)) = recorder {
            for new_msg in &messages[len_before..] {
                let role = sqlite_fts::role_str(new_msg.role);
                let content = sqlite_fts::render_message_content(new_msg);
                let content = if role == "assistant" {
                    super::evidence::strip_markers(&content)
                } else {
                    content
                };
                if content.is_empty() {
                    appended_origins.push(MessageOrigin::Ephemeral);
                    continue;
                }
                let to_record = redactor
                    .as_ref()
                    .map(|r| r.redact(&content))
                    .unwrap_or(content);
                // Provenance travels with the row: a replayed assistant
                // turn is model output, and a turn carrying tool results
                // takes the least-trusted class across its blocks.
                let segment = message_provenance(new_msg);
                match db.record_labeled_message(sid, role, &segment, &to_record) {
                    Ok(msg_id) => {
                        let origin = replay_persisted_content(role, &to_record, segment.kind())
                            .map(|replay| MessageOrigin::Raw { id: msg_id, replay })
                            .unwrap_or(MessageOrigin::Ephemeral);
                        appended_origins.push(origin);
                        if let Some(ix) = &semantic_indexer {
                            ix.spawn_index(sid.to_string(), role, msg_id, to_record.clone());
                        }
                    }
                    Err(e) => {
                        appended_origins.push(MessageOrigin::Ephemeral);
                        tracing::warn!("memory: failed to record {role} message: {e}");
                    }
                }
            }
        } else {
            appended_origins.resize(
                messages.len().saturating_sub(len_before),
                MessageOrigin::Ephemeral,
            );
        }
        origins.extend(appended_origins);

        if let super::turn::TurnOutcome::Final(answer) = outcome {
            let evidence = evidence_ledger.verify(user_prompt, &answer);
            let answer = super::evidence::strip_markers(&answer);
            let fallback = provider.fallback_state();
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
                evidence,
                fallback,
                turns: turn,
                provider: provider.effective_provider_name(),
                model: provider.effective_model_name(&cfg.model),
                session_id,
            });
        }
    }

    Err(AgentError::MaxTurnsExceeded(turn_limit))
}

/// Streaming adapter for [`ask_inner`]. It projects the full runtime events
/// into the public presentation contract, then delegates every lifecycle
/// transition to the shared owner.
async fn ask_inner_streaming(
    deps: &RuntimeDeps,
    provider: Arc<dyn Provider>,
    cfg: &AgentConfig,
    user_prompt: &str,
    transient_context: Option<&str>,
    tools: &ToolRegistry,
    recorder: Option<(&MemoryDb, &str)>,
    compressor: Option<Arc<dyn Compressor>>,
    sink: Arc<dyn StreamSink>,
    progress: Arc<dyn ProgressSink>,
    initial_messages: ConversationSeed,
    interrupt_scope: Option<&str>,
) -> Result<AskResult, AgentError> {
    let sink = super::presentation::user_visible_stream_sink(sink);
    let progress = super::presentation::user_visible_progress_sink(progress);
    ask_inner(LifecycleRequest {
        deps,
        provider,
        cfg,
        user_prompt,
        tools,
        exposure: None,
        recorder,
        compressor,
        initial_messages,
        transient_context,
        output: LifecycleOutput::Streaming { sink },
        progress,
        interrupt_scope,
        delegated: false,
    })
    .await
}

/// Convenience: read `cfg` from global config, build the default tool
/// registry, construct the registered provider, open the default memory DB,
/// and run `ask_with_memory`. If the memory DB cannot be opened (read-only
/// filesystem etc.), falls back to `ask_with` with a warning.
pub async fn ask(user_prompt: &str) -> Result<AskResult, AgentError> {
    let config = crate::config::current_snapshot();
    let cfg = &config.agent;
    let provider = crate::ai::gate::build_system_provider(cfg)
        .map_err(|e| AgentError::ProviderUnavailable(e.to_string()))?;
    let registry_deps = crate::agent::tools::registry::RegistryDeps::load(
        Arc::clone(&config),
        crate::agent::tools::registry::RegistryPaths::from_process(),
    );
    let runtime_deps = registry_deps.runtime.clone();
    let mut tools = default_registry_with_deps(&registry_deps);
    tools.set_guardrails(guardrails_from_cfg(cfg));
    tools.set_approval(approval_from_cfg(cfg));

    // Best-effort attach configured MCP servers. `_mcp_handles` MUST
    // outlive the loop — its Drop tears down children and aborts
    // background reader tasks. Failures inside attach_all are already
    // logged and skipped, so this never fails the ask.
    let exposure = ToolExposureContext::isolated(guardrails_from_cfg(cfg));
    let _mcp_handles = attach_mcp_servers(&mut tools, cfg, &exposure).await;

    let session_id = uuid::Uuid::new_v4().to_string();

    let compressor =
        compressor_from_cfg_with_exposure(provider.clone(), cfg, &tools, Some(&exposure));

    match registry_deps.memory.as_ref() {
        Some(db) => {
            ask_inner(LifecycleRequest {
                deps: &runtime_deps,
                provider,
                cfg,
                user_prompt,
                tools: &tools,
                exposure: Some(&exposure),
                recorder: Some((db, session_id.as_str())),
                compressor,
                initial_messages: ConversationSeed::empty(),
                transient_context: None,
                output: LifecycleOutput::Buffered,
                progress: progress::null_progress(),
                interrupt_scope: None,
                delegated: false,
            })
            .await
        }
        None => {
            ask_inner(LifecycleRequest {
                deps: &runtime_deps,
                provider,
                cfg,
                user_prompt,
                tools: &tools,
                exposure: Some(&exposure),
                recorder: None,
                compressor,
                initial_messages: ConversationSeed::empty(),
                transient_context: None,
                output: LifecycleOutput::Buffered,
                progress: progress::null_progress(),
                interrupt_scope: None,
                delegated: false,
            })
            .await
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
    exposure: &ToolExposureContext,
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
    attach_all(&specs, tools, exposure).await
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
            package: None,
            // Operator configuration, not an installed package: the
            // machine owner wrote this into config.json themselves, so
            // there is no publisher to authenticate. Package provenance
            // applies to discovered agent-API packages.
            provenance: None,
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
    exposure: &ToolExposureContext,
) -> Vec<crate::agent::tools::mcp::integration::McpServerHandle> {
    // MCP clients are model-visible transport the broker must never
    // hold. Fail closed (no servers attached) rather than dialling out
    // from a root process.
    if let Err(error) = crate::agentd::guard::ensure_agent_runtime_allowed("MCP attachment") {
        tracing::error!(error = %error, "refusing to attach MCP servers");
        return Vec::new();
    }
    attach_mcp_servers(tools, cfg, exposure).await
}

/// Build a [`LlmCompressor`] from `cfg` when `compress_enabled` is set.
/// Returns `None` otherwise so the runtime keeps zero-overhead behaviour
/// for the default case.
fn compressor_from_cfg(
    provider: Arc<dyn Provider>,
    cfg: &AgentConfig,
    tools: &ToolRegistry,
) -> Option<Arc<dyn Compressor>> {
    compressor_from_cfg_with_exposure(provider, cfg, tools, None)
}

fn compressor_from_cfg_with_exposure(
    provider: Arc<dyn Provider>,
    cfg: &AgentConfig,
    tools: &ToolRegistry,
    exposure: Option<&ToolExposureContext>,
) -> Option<Arc<dyn Compressor>> {
    if !cfg.compress_enabled {
        return None;
    }
    let fallback_exposure = ToolExposureContext::isolated(tools.guardrails().clone());
    let exposure = exposure.unwrap_or(&fallback_exposure);
    let tool_tokens = compressor::estimate_tools_tokens(&tools.as_llm_tools_for(exposure));
    let target_tokens = cfg
        .compress_target_tokens
        .saturating_sub(tool_tokens)
        .max(1);
    let trigger_tokens = cfg
        .compress_trigger_tokens
        .saturating_sub(tool_tokens)
        .max(1);
    let compressor_cfg = CompressorConfig {
        target_tokens,
        trigger_tokens,
        keep_tail_tokens: cfg.compress_keep_tail_tokens.min(target_tokens),
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

/// Build an optional tool-level [`ApprovalGate`] from explicit operator
/// configuration. Capability risk is enforced separately: high- and
/// critical-risk capability denials create durable approval requests in the
/// kernel, so the default tool-name gate stays empty and cannot intercept a
/// call before its precise verb and scope are known.
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
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/runtime/loop_.rs"
    ));
}
