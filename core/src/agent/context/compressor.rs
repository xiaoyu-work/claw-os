//! Context compressor — shrinks long message histories so the agent
//! doesn't run out of context window on long conversations.
//!
//! Phase 5's first compressor implementation is **provider-backed
//! summarisation**: when the estimated request cost (system prompt, fixed tool
//! schemas, and messages) exceeds a configured trigger, the head (older
//! messages) is rendered as a compact transcript, fed to a Provider as a
//! single summarisation request, and the resulting summary replaces the head.
//! The tail is preserved verbatim so the agent doesn't lose its current task.
//!
//! Why this approach (vs. drop-oldest, RAG retrieval, or chunked
//! summary):
//!
//!   * Drop-oldest loses information silently and is easy to abuse —
//!     a long-running task quickly forgets its goal.
//!   * RAG retrieval needs an embedding store and a relevance signal,
//!     which adds infrastructure dependencies the kernel doesn't yet
//!     have everywhere.
//!   * Chunked / hierarchical summarisation is strictly more powerful
//!     but also strictly more code; a single-pass summary is a clean
//!     baseline and what most production agents start with.
//!
//! ## Token estimation
//!
//! We use a coarse `chars / 4` heuristic — fast, allocation-free, and
//! good to within ~30% for English / code. It deliberately *does not*
//! depend on a tokenizer (no model-specific tiktoken / sentencepiece
//! shipped). Providers that need exact accounting can swap in their own
//! [`Compressor`] implementation later.
//!
//! ## Tool-pair preservation
//!
//! The boundary between head and tail must never split a tool_use
//! (assistant) from its matching tool_result (user/tool). If the
//! natural boundary lands mid-pair, we walk the boundary backward
//! until the tail starts cleanly. This avoids tool-call orphans that
//! Anthropic / OpenAI strict-validators reject.
//!
//! ## Failure mode
//!
//! If the summarisation Provider call fails for any reason, we fall
//! back to **truncate-only** — the head is dropped without a summary
//! and the tail is returned. Better to lose old context than to crash
//! the agent loop mid-conversation.

use std::sync::Arc;

use async_trait::async_trait;

use crate::agent::llm::{
    types::{ContentBlock, FinishReason, Role, ToolChoice},
    ChatRequest, Message, Provider, Tool as LlmTool,
};

/// Default total context budget in tokens. Mirrors a conservative
/// 128k-class window minus headroom for the response. Caller config
/// can override.
pub const DEFAULT_TARGET_TOKENS: u32 = 80_000;

/// Default trigger — compress when estimated tokens reach this. ~75%
/// of `DEFAULT_TARGET_TOKENS`.
pub const DEFAULT_TRIGGER_TOKENS: u32 = 60_000;

/// Default tail budget — number of tokens of recent messages to
/// preserve verbatim. ~25% of the target.
pub const DEFAULT_KEEP_TAIL_TOKENS: u32 = 20_000;

/// Default cap on the summary's own token budget.
pub const DEFAULT_SUMMARY_MAX_TOKENS: u32 = 1024;

/// Marker prefix on the synthesised summary message. Used so future
/// compress passes can detect they're re-summarising a prior summary
/// (currently informational; the next pass still re-summarises).
pub const SUMMARY_MARKER: &str = "[CONTEXT SUMMARY]";

/// Cheap char-to-token heuristic. Mirrors the OpenAI rule of thumb of
/// ~4 chars per token for English / code; close enough for budget
/// decisions, deliberately fast.
///
/// We count *characters* (not bytes) so multi-byte UTF-8 input
/// — Chinese, Japanese, Korean, emoji-heavy markdown — doesn't
/// massively over-estimate. A 100-character Chinese sentence is 300
/// bytes but still around 100 tokens; counting bytes would charge
/// it 75 tokens instead of 25, blowing the budget early.
pub fn estimate_text_tokens(s: &str) -> u32 {
    let chars = s.chars().count() as u32;
    chars.div_ceil(4)
}

/// Estimate the token cost of one [`Message`] across all its content
/// blocks. Tool-call inputs / tool-results count toward the total.
pub fn estimate_message_tokens(msg: &Message) -> u32 {
    let mut total: u32 = 0;
    // Per-message role / framing overhead.
    total = total.saturating_add(4);
    for block in &msg.content {
        total = total.saturating_add(match block {
            ContentBlock::Text { text } => estimate_text_tokens(text),
            ContentBlock::ToolUse { name, input, .. } => {
                let n = estimate_text_tokens(name);
                let i = estimate_text_tokens(&input.to_string());
                n.saturating_add(i).saturating_add(8)
            }
            ContentBlock::ToolResult { content, .. } => {
                estimate_text_tokens(content).saturating_add(4)
            }
            ContentBlock::ToolState {
                thought_signature, ..
            } => estimate_text_tokens(thought_signature).saturating_add(4),
            ContentBlock::Reasoning {
                summary,
                encrypted_content,
                ..
            } => {
                let summary_tokens = summary
                    .iter()
                    .fold(0u32, |total, text| total.saturating_add(estimate_text_tokens(text)));
                summary_tokens
                    .saturating_add(
                        encrypted_content
                            .as_deref()
                            .map(estimate_text_tokens)
                            .unwrap_or_default(),
                    )
                    .saturating_add(8)
            }
            ContentBlock::Image { data, .. } => {
                // Base64 image: charge a flat large cost; exact cost is
                // provider-specific (Anthropic charges by tile, OpenAI
                // by pixel area). Better to over-estimate than under.
                estimate_text_tokens(data) / 4 + 256
            }
        });
    }
    total
}

/// Estimate the token cost of `system + messages` together.
pub fn estimate_total_tokens(system: Option<&str>, messages: &[Message]) -> u32 {
    let mut total = system.map(estimate_text_tokens).unwrap_or(0);
    for m in messages {
        total = total.saturating_add(estimate_message_tokens(m));
    }
    total
}

/// Estimate the fixed token cost of model-visible tool definitions.
///
/// Provider requests resend each tool's name, description, and JSON Schema.
/// This overhead consumes the same context window as messages and must reduce
/// the history budget even though the compressor never summarizes tools.
pub fn estimate_tools_tokens(tools: &[LlmTool]) -> u32 {
    tools.iter().fold(0u32, |total, tool| {
        total
            .saturating_add(estimate_text_tokens(&tool.name))
            .saturating_add(estimate_text_tokens(&tool.description))
            .saturating_add(estimate_text_tokens(&tool.input_schema.to_string()))
            .saturating_add(8)
    })
}

/// Behaviour contract for context compressors. The trait is async
/// because the production impl runs an LLM call.
#[async_trait]
pub trait Compressor: Send + Sync {
    /// True if this compressor would shrink `messages`.
    fn should_compress(&self, system: Option<&str>, messages: &[Message]) -> bool;

    /// Compress `messages`. If compression is unnecessary or
    /// impossible (too few messages, head boundary at 0, etc.) the
    /// input is returned unchanged.
    async fn compress(&self, system: Option<&str>, messages: Vec<Message>) -> Vec<Message>;
}

/// Tunables for [`LlmCompressor`].
#[derive(Debug, Clone)]
pub struct CompressorConfig {
    /// Target total token budget (info only — the trigger drives
    /// behaviour; this is what callers should size the prompt to).
    pub target_tokens: u32,
    /// Compression triggers when estimated total reaches this value.
    pub trigger_tokens: u32,
    /// Token budget reserved for the verbatim tail.
    pub keep_tail_tokens: u32,
    /// Maximum tokens for the synthesised summary itself.
    pub summary_max_tokens: u32,
}

impl Default for CompressorConfig {
    fn default() -> Self {
        Self {
            target_tokens: DEFAULT_TARGET_TOKENS,
            trigger_tokens: DEFAULT_TRIGGER_TOKENS,
            keep_tail_tokens: DEFAULT_KEEP_TAIL_TOKENS,
            summary_max_tokens: DEFAULT_SUMMARY_MAX_TOKENS,
        }
    }
}

/// Provider-backed compressor: head summarised via LLM, tail kept
/// verbatim.
pub struct LlmCompressor {
    provider: Arc<dyn Provider>,
    model: String,
    cfg: CompressorConfig,
}

impl LlmCompressor {
    pub fn new(provider: Arc<dyn Provider>, model: impl Into<String>) -> Self {
        Self {
            provider,
            model: model.into(),
            cfg: CompressorConfig::default(),
        }
    }

    pub fn with_config(mut self, cfg: CompressorConfig) -> Self {
        self.cfg = cfg;
        self
    }

    pub fn config(&self) -> &CompressorConfig {
        &self.cfg
    }

    /// Walk from the end accumulating tokens until adding the next
    /// message would push the tail over `keep_tail_tokens`. Returns
    /// the boundary INDEX — messages[boundary..] is the tail.
    fn raw_boundary(&self, messages: &[Message]) -> usize {
        let mut acc: u32 = 0;
        let cap = self.cfg.keep_tail_tokens;
        let mut idx = messages.len();
        while idx > 0 {
            let candidate = idx - 1;
            let tokens = estimate_message_tokens(&messages[candidate]);
            if acc.saturating_add(tokens) > cap && idx < messages.len() {
                break;
            }
            acc = acc.saturating_add(tokens);
            idx = candidate;
        }
        idx
    }

    /// Walk the boundary backward so the tail does not start with a
    /// dangling tool_result whose tool_use is in the head, and so the
    /// head does not end with a dangling tool_use whose tool_result is
    /// in the tail. We collect the set of tool_use ids whose
    /// tool_result lives in the tail; if any of those ids' tool_use
    /// is in the head, we extend the tail backward to include them.
    fn adjust_for_tool_pairs(messages: &[Message], mut boundary: usize) -> usize {
        use std::collections::{HashMap, HashSet};
        // Pre-index where each ToolUse id lives so we don't re-scan
        // the whole conversation O(n²) just to ask "is this id's use
        // in the tail?". We build the map once and consult it on
        // every iteration of the boundary walk.
        let mut tool_use_msg: HashMap<&str, usize> = HashMap::new();
        for (idx, m) in messages.iter().enumerate() {
            for b in &m.content {
                if let ContentBlock::ToolUse { id, .. } = b {
                    tool_use_msg.insert(id.as_str(), idx);
                }
            }
        }
        // Set of tool_use ids referenced by tool_results in the tail.
        // Re-computed lazily when we shrink boundary, by *adding* the
        // newly-absorbed message's result ids — never re-scanning the
        // whole tail.
        let mut needed_ids: HashSet<String> = HashSet::new();
        for m in &messages[boundary..] {
            for b in &m.content {
                if let ContentBlock::ToolResult { tool_use_id, .. } = b {
                    needed_ids.insert(tool_use_id.clone());
                }
            }
        }
        loop {
            if boundary == 0 {
                break;
            }
            let prev = &messages[boundary - 1];
            let prev_has_needed_use = prev
                .content
                .iter()
                .any(|b| matches!(b, ContentBlock::ToolUse { id, .. } if needed_ids.contains(id)));
            // Does the message at `boundary` have a tool_result whose
            // matching tool_use is *not* in the tail? Use the prebuilt
            // index instead of an inner O(tail) scan.
            let boundary_msg_orphan_result = messages.get(boundary).is_some_and(|m| {
                m.content.iter().any(|b| match b {
                    ContentBlock::ToolResult { tool_use_id, .. } => {
                        match tool_use_msg.get(tool_use_id.as_str()) {
                            Some(&use_idx) => use_idx < boundary,
                            None => true,
                        }
                    }
                    _ => false,
                })
            });
            if prev_has_needed_use || boundary_msg_orphan_result {
                boundary -= 1;
                // Only absorb the newly-included message's results
                // into the needed set — the rest of needed_ids is
                // still valid for the new (larger) tail.
                for b in &messages[boundary].content {
                    if let ContentBlock::ToolResult { tool_use_id, .. } = b {
                        needed_ids.insert(tool_use_id.clone());
                    }
                }
                continue;
            }
            break;
        }
        boundary
    }

    /// Render the head into a compact transcript suitable for
    /// summarisation. Tool calls / results are summarised inline as
    /// `<tool:name args=...>` / `<result for=name>` to save tokens.
    fn render_transcript(messages: &[Message]) -> String {
        let mut out = String::new();
        for (idx, m) in messages.iter().enumerate() {
            let role = match m.role {
                Role::System => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "tool",
            };
            out.push_str(&format!("--- turn {} ({}) ---\n", idx + 1, role));
            for b in &m.content {
                match b {
                    ContentBlock::Text { text } => {
                        out.push_str(text);
                        if !text.ends_with('\n') {
                            out.push('\n');
                        }
                    }
                    ContentBlock::ToolUse { name, input, .. } => {
                        out.push_str(&format!("<tool_call:{name} args={input}>\n"));
                    }
                    ContentBlock::ToolResult {
                        is_error, content, ..
                    } => {
                        out.push_str(&format!(
                            "<tool_result error={is_error}>\n{content}\n</tool_result>\n"
                        ));
                    }
                    ContentBlock::Reasoning { summary, .. } => {
                        if !summary.is_empty() {
                            out.push_str("<reasoning_summary>\n");
                            out.push_str(&summary.join("\n"));
                            out.push_str("\n</reasoning_summary>\n");
                        }
                    }
                    ContentBlock::ToolState { .. } => {}
                    ContentBlock::Image { media_type, .. } => {
                        out.push_str(&format!("<image media_type={media_type}>\n"));
                    }
                }
            }
        }
        out
    }

    fn build_summary_prompt(transcript: &str) -> String {
        format!(
            "You are summarising the earlier portion of a longer agent \
             conversation so the agent can keep working without \
             losing key facts. Preserve named entities, decisions, \
             tool results, file paths, error messages, and any explicit \
             user goals. Drop pleasantries and repetition. Output ONLY \
             the summary text — no preamble, no markdown headers. Aim \
             for 200-400 words.\n\n--- BEGIN TRANSCRIPT ---\n{transcript}\
             \n--- END TRANSCRIPT ---"
        )
    }

    /// Build the synthesised summary message. We use the *assistant*
    /// role rather than user: the summary is a recap of prior turns
    /// produced by the model, not new input from the user, and most
    /// chat-completion APIs require strict user/assistant alternation.
    /// Surfacing the summary as `assistant` keeps the boundary clean
    /// when the immediately-preserved tail message is from the user.
    fn make_summary_message(summary: &str, head_count: usize) -> Message {
        Message::assistant_text(format!(
            "{SUMMARY_MARKER} (compressed {head_count} prior messages)\n\n{summary}"
        ))
    }
}

#[async_trait]
impl Compressor for LlmCompressor {
    fn should_compress(&self, system: Option<&str>, messages: &[Message]) -> bool {
        if messages.len() < 4 {
            // Need at least a few messages before compression makes sense.
            return false;
        }
        estimate_total_tokens(system, messages) >= self.cfg.trigger_tokens
    }

    async fn compress(&self, system: Option<&str>, messages: Vec<Message>) -> Vec<Message> {
        if !self.should_compress(system, &messages) {
            return messages;
        }
        let raw = self.raw_boundary(&messages);
        let boundary = Self::adjust_for_tool_pairs(&messages, raw);
        // If the boundary is at or before 1, there isn't a meaningful
        // head to summarise — return unchanged.
        if boundary <= 1 {
            return messages;
        }
        let head = &messages[..boundary];
        let tail = messages[boundary..].to_vec();

        let transcript = Self::render_transcript(head);
        let prompt = Self::build_summary_prompt(&transcript);

        let request = ChatRequest {
            model: self.model.clone(),
            messages: vec![Message::user_text(prompt)],
            system: Some(
                "You compress conversation histories. Be terse, \
                 factual, and structured."
                    .to_string(),
            ),
            tools: Vec::new(),
            tool_choice: ToolChoice::Auto,
            max_tokens: Some(self.cfg.summary_max_tokens),
            temperature: Some(0.0),
            top_p: None,
            stop_sequences: Vec::new(),
            extra: serde_json::json!({"_cos_initiator": "agent"}),
        };

        match self.provider.chat(request).await {
            Ok(resp) => {
                let summary_text = extract_text(&resp.content);
                if summary_text.is_empty() {
                    // Provider gave us nothing useful — fall back to
                    // truncate-only. Better than carrying a "" summary.
                    tracing::warn!(
                        "context compressor: provider returned empty summary; truncating without summary"
                    );
                    return tail;
                }
                let mut out = Vec::with_capacity(tail.len() + 1);
                out.push(Self::make_summary_message(&summary_text, head.len()));
                out.extend(tail);
                let _ = resp.finish_reason; // currently informational
                out
            }
            Err(e) => {
                tracing::warn!(
                    "context compressor: provider call failed ({e}); truncating without summary"
                );
                tail
            }
        }
    }
}

fn extract_text(blocks: &[ContentBlock]) -> String {
    let mut out = String::new();
    for b in blocks {
        if let ContentBlock::Text { text } = b {
            out.push_str(text);
        }
    }
    out.trim().to_string()
}

/// A no-op compressor used as a default when context compression is
/// disabled. `should_compress` always returns false; `compress`
/// returns the input unchanged. Useful as the runtime default and in
/// tests.
pub struct NoopCompressor;

#[async_trait]
impl Compressor for NoopCompressor {
    fn should_compress(&self, _system: Option<&str>, _messages: &[Message]) -> bool {
        false
    }

    async fn compress(&self, _system: Option<&str>, messages: Vec<Message>) -> Vec<Message> {
        messages
    }
}

// Mark the FinishReason import used in tests / future use; without
// this, rustc may flag it as unused in builds where compress() never
// observes the field.
#[allow(dead_code)]
const _UNUSED_FINISH_REASON_REF: Option<FinishReason> = None;

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/context/compressor.rs"
    ));
}
