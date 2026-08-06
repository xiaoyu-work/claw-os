//! Context compressor — shrinks long message histories so the agent
//! doesn't run out of context window on long conversations.
//!
//! Phase 5's first compressor implementation is **provider-backed
//! summarisation**: when the estimated token count of the messages
//! exceeds a configured trigger, the head (older messages) is rendered
//! as a compact transcript, fed to a Provider as a single summarisation
//! request, and the resulting summary becomes a single user-role
//! message that replaces the head. The tail (most recent messages) is
//! preserved verbatim so the agent doesn't lose its current task or
//! immediate context.
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
    ChatRequest, Message, Provider,
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
            extra: serde_json::Value::Null,
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
    use super::*;
    use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
    use crate::config::AgentConfig;

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

    fn user_msg(text: &str) -> Message {
        Message::user_text(text)
    }

    fn assistant_msg(text: &str) -> Message {
        Message::assistant_text(text)
    }

    fn tool_use_msg(id: &str, name: &str, input: serde_json::Value) -> Message {
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: id.to_string(),
                name: name.to_string(),
                input,
            }],
        }
    }

    fn tool_result_msg(tool_use_id: &str, content: &str) -> Message {
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: tool_use_id.to_string(),
                is_error: false,
                content: content.to_string(),
            }],
        }
    }

    fn make_compressor(cfg: CompressorConfig) -> LlmCompressor {
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new("mock-model", &parent_cfg()));
        LlmCompressor::new(provider, "mock-model").with_config(cfg)
    }

    #[test]
    fn estimate_text_tokens_rounds_up() {
        assert_eq!(estimate_text_tokens(""), 0);
        assert_eq!(estimate_text_tokens("a"), 1);
        assert_eq!(estimate_text_tokens("abcd"), 1);
        assert_eq!(estimate_text_tokens("abcde"), 2);
    }

    #[test]
    fn estimate_message_includes_overhead() {
        let m = user_msg("hi");
        // text "hi" -> 1 token + 4 overhead == 5.
        assert_eq!(estimate_message_tokens(&m), 5);
    }

    #[test]
    fn estimate_total_sums_system_and_messages() {
        let messages = vec![user_msg("hello"), assistant_msg("world")];
        // "hello" -> 2, "world" -> 2, each with 4 overhead = 12 messages
        // + system "sys" -> 1 == 13.
        let total = estimate_total_tokens(Some("sys"), &messages);
        assert_eq!(total, 13);
    }

    #[test]
    fn should_compress_false_when_below_trigger() {
        let mut cfg = CompressorConfig::default();
        cfg.trigger_tokens = 10_000;
        let c = make_compressor(cfg);
        let msgs = vec![
            user_msg("short"),
            assistant_msg("short"),
            user_msg("short"),
            assistant_msg("short"),
        ];
        assert!(!c.should_compress(None, &msgs));
    }

    #[test]
    fn should_compress_false_when_too_few_messages() {
        let cfg = CompressorConfig {
            trigger_tokens: 1, // any non-zero estimated total triggers
            ..Default::default()
        };
        let c = make_compressor(cfg);
        // Only 2 messages — must not trigger even with a trivially low
        // threshold.
        let msgs = vec![user_msg("hello"), assistant_msg("world")];
        assert!(!c.should_compress(None, &msgs));
    }

    #[test]
    fn should_compress_true_above_trigger_with_enough_messages() {
        let cfg = CompressorConfig {
            trigger_tokens: 10,
            ..Default::default()
        };
        let c = make_compressor(cfg);
        let msgs = vec![
            user_msg("hello"),
            assistant_msg("world"),
            user_msg("hello"),
            assistant_msg("world"),
        ];
        assert!(c.should_compress(None, &msgs));
    }

    #[test]
    fn raw_boundary_keeps_tail_within_budget() {
        let cfg = CompressorConfig {
            keep_tail_tokens: 10,
            ..Default::default()
        };
        let c = make_compressor(cfg);
        // Each message ~5 tokens. Tail budget = 10 → fits two messages
        // (10 tokens), boundary at index 4.
        let msgs: Vec<Message> = (0..6).map(|i| user_msg(&format!("m{i}"))).collect();
        let b = c.raw_boundary(&msgs);
        assert_eq!(b, 4);
        assert_eq!(msgs.len() - b, 2);
    }

    #[test]
    fn raw_boundary_returns_zero_when_nothing_fits_in_budget() {
        // Budget so tiny that even one message exceeds it. Boundary
        // walks back to len()-1 to keep at least the last message.
        let cfg = CompressorConfig {
            keep_tail_tokens: 0,
            ..Default::default()
        };
        let c = make_compressor(cfg);
        let msgs = vec![user_msg("a"), assistant_msg("b"), user_msg("c")];
        let b = c.raw_boundary(&msgs);
        // First iteration always accepts the last message regardless of
        // budget (so tail is never empty).
        assert_eq!(b, 2);
    }

    #[test]
    fn adjust_for_tool_pairs_keeps_pair_together_in_tail() {
        // Sequence: user, assistant(tool_use:t1), tool_result(t1), user.
        // Raw boundary lands at index 2 (tool_result alone in tail).
        // Adjust must move boundary back to 1 so the matching tool_use
        // also lives in the tail.
        let msgs = vec![
            user_msg("intro"),
            tool_use_msg("t1", "echo", serde_json::json!({"text": "hi"})),
            tool_result_msg("t1", "hi"),
            user_msg("ok"),
        ];
        let adjusted = LlmCompressor::adjust_for_tool_pairs(&msgs, 2);
        assert_eq!(adjusted, 1);
    }

    #[test]
    fn adjust_for_tool_pairs_no_change_when_pairs_already_aligned() {
        // user, assistant(tool_use:t1), tool_result(t1), user.
        // Raw boundary at 3 — tail is just [user]. Nothing to adjust.
        let msgs = vec![
            user_msg("intro"),
            tool_use_msg("t1", "echo", serde_json::json!({"text": "hi"})),
            tool_result_msg("t1", "hi"),
            user_msg("ok"),
        ];
        let adjusted = LlmCompressor::adjust_for_tool_pairs(&msgs, 3);
        assert_eq!(adjusted, 3);
    }

    #[test]
    fn adjust_for_tool_pairs_orphan_result_at_boundary_pulls_use_in() {
        // boundary lands ON a tool_result whose use is in the head.
        let msgs = vec![
            user_msg("intro"),
            tool_use_msg("t1", "echo", serde_json::json!({"text": "hi"})),
            tool_result_msg("t1", "hi"),
        ];
        // raw boundary index 2 == the tool_result. Tail is just the
        // tool_result with no matching tool_use in the tail. Adjust
        // walks back to include the tool_use at index 1.
        let adjusted = LlmCompressor::adjust_for_tool_pairs(&msgs, 2);
        assert_eq!(adjusted, 1);
    }

    #[test]
    fn render_transcript_marks_roles_and_tool_calls() {
        let msgs = vec![
            user_msg("hello"),
            tool_use_msg("t1", "echo", serde_json::json!({"text": "hi"})),
            tool_result_msg("t1", "hi"),
        ];
        let s = LlmCompressor::render_transcript(&msgs);
        assert!(s.contains("(user)"));
        assert!(s.contains("(assistant)"));
        assert!(s.contains("<tool_call:echo"));
        assert!(s.contains("<tool_result"));
    }

    #[test]
    fn make_summary_message_contains_marker_and_count() {
        let m = LlmCompressor::make_summary_message("the gist", 7);
        match &m.content[0] {
            ContentBlock::Text { text } => {
                assert!(text.contains(SUMMARY_MARKER));
                assert!(text.contains("7 prior messages"));
                assert!(text.contains("the gist"));
            }
            _ => panic!("expected text block"),
        }
    }

    #[tokio::test]
    async fn compress_below_trigger_returns_unchanged() {
        let cfg = CompressorConfig {
            trigger_tokens: 1_000_000,
            ..Default::default()
        };
        let c = make_compressor(cfg);
        let msgs = vec![
            user_msg("a"),
            assistant_msg("b"),
            user_msg("c"),
            assistant_msg("d"),
        ];
        let after = c.compress(None, msgs.clone()).await;
        assert_eq!(after.len(), msgs.len());
    }

    #[tokio::test]
    async fn compress_with_mock_provider_inserts_summary_before_tail() {
        let cfg = CompressorConfig {
            trigger_tokens: 5,
            keep_tail_tokens: 12,
            summary_max_tokens: 256,
            ..Default::default()
        };
        let provider_arc: Arc<MockProvider> =
            Arc::new(MockProvider::new("mock-model", &parent_cfg()));
        provider_arc.push_response(MockResponse::Text("compressed history goes here".into()));
        let provider: Arc<dyn Provider> = provider_arc.clone();
        let c = LlmCompressor::new(provider, "mock-model").with_config(cfg);
        let msgs: Vec<Message> = (0..8).map(|i| user_msg(&format!("msg-{i}"))).collect();
        let original_len = msgs.len();
        let after = c.compress(None, msgs).await;
        assert!(after.len() < original_len);
        // First message should be the synthesised summary.
        match &after[0].content[0] {
            ContentBlock::Text { text } => {
                assert!(text.contains(SUMMARY_MARKER));
                assert!(text.contains("compressed history"));
            }
            _ => panic!("expected text block"),
        }
        // Tail messages remain.
        let tail_text = match &after[1].content[0] {
            ContentBlock::Text { text } => text.clone(),
            _ => panic!(),
        };
        assert!(tail_text.starts_with("msg-"));
    }

    #[tokio::test]
    async fn compress_with_failing_provider_falls_back_to_truncate() {
        // Mock provider has no responses queued; chat() returns a
        // configurable empty error response — but our mock's default
        // behaviour is to echo. We force a failure by pushing an
        // error-shaped Text response that's blank (extract_text
        // returns "") — which the compressor treats as "empty
        // summary, fall back to truncate-only".
        let cfg = CompressorConfig {
            trigger_tokens: 5,
            keep_tail_tokens: 12,
            ..Default::default()
        };
        let provider_arc: Arc<MockProvider> =
            Arc::new(MockProvider::new("mock-model", &parent_cfg()));
        provider_arc.push_response(MockResponse::Text(String::new()));
        let provider: Arc<dyn Provider> = provider_arc.clone();
        let c = LlmCompressor::new(provider, "mock-model").with_config(cfg);
        let msgs: Vec<Message> = (0..8).map(|i| user_msg(&format!("msg-{i}"))).collect();
        let after = c.compress(None, msgs.clone()).await;
        // No summary inserted; tail is the truncated suffix only.
        for m in &after {
            match &m.content[0] {
                ContentBlock::Text { text } => assert!(!text.contains(SUMMARY_MARKER)),
                _ => {}
            }
        }
        assert!(after.len() < msgs.len());
    }

    #[tokio::test]
    async fn compress_preserves_tool_use_result_pair_in_tail() {
        // Construct a long history where a tool_use lands right at the
        // raw boundary. After compression the surviving tail must
        // start with the tool_use, not the tool_result.
        let cfg = CompressorConfig {
            trigger_tokens: 5,
            keep_tail_tokens: 30,
            summary_max_tokens: 256,
            ..Default::default()
        };
        let provider_arc: Arc<MockProvider> =
            Arc::new(MockProvider::new("mock-model", &parent_cfg()));
        provider_arc.push_response(MockResponse::Text("summary".into()));
        let provider: Arc<dyn Provider> = provider_arc;
        let c = LlmCompressor::new(provider, "mock-model").with_config(cfg);

        // 6 user msgs, then tool_use + tool_result + final user.
        let mut msgs: Vec<Message> = (0..6).map(|i| user_msg(&format!("m{i}"))).collect();
        msgs.push(tool_use_msg(
            "tx",
            "echo",
            serde_json::json!({"text": "ping"}),
        ));
        msgs.push(tool_result_msg("tx", "ping"));
        msgs.push(user_msg("done"));

        let after = c.compress(None, msgs).await;
        // Verify: there is no tool_result whose tool_use isn't in the
        // post-summary list (i.e., no orphaned tool_result).
        let all_use_ids: std::collections::HashSet<&str> = after
            .iter()
            .flat_map(|m| m.content.iter())
            .filter_map(|b| match b {
                ContentBlock::ToolUse { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        for m in &after {
            for b in &m.content {
                if let ContentBlock::ToolResult { tool_use_id, .. } = b {
                    assert!(
                        all_use_ids.contains(tool_use_id.as_str()),
                        "found tool_result for {tool_use_id} without matching tool_use in compressed list"
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn noop_compressor_never_compresses() {
        let c = NoopCompressor;
        let msgs = vec![user_msg("a"); 100];
        assert!(!c.should_compress(None, &msgs));
        let after = c.compress(None, msgs.clone()).await;
        assert_eq!(after.len(), msgs.len());
    }

    #[test]
    fn config_defaults_are_consistent() {
        let cfg = CompressorConfig::default();
        assert!(cfg.trigger_tokens < cfg.target_tokens);
        assert!(cfg.keep_tail_tokens < cfg.target_tokens);
    }

    #[tokio::test]
    async fn compress_does_not_crash_on_empty_messages() {
        let cfg = CompressorConfig::default();
        let c = make_compressor(cfg);
        let after = c.compress(None, Vec::new()).await;
        assert!(after.is_empty());
    }

    #[test]
    fn estimate_image_block_charges_baseline_overhead() {
        let m = Message {
            role: Role::User,
            content: vec![ContentBlock::Image {
                media_type: "image/png".into(),
                data: "x".repeat(1000),
            }],
        };
        // Should be at least the 256 baseline + 4 message overhead.
        let t = estimate_message_tokens(&m);
        assert!(t >= 260);
    }
}
