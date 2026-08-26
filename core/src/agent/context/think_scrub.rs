//! Reasoning / thinking block scrubber.
//!
//! Modern reasoning models — DeepSeek R1, Qwen QwQ, Anthropic
//! extended-thinking, OpenAI o-series — emit a long internal
//! reasoning trace that the user (and the next turn) generally
//! shouldn't see again. The final answer summarises everything
//! relevant; the trace is sometimes 10× the size of the answer and
//! costs full input tokens on every subsequent turn.
//!
//! ## What we strip
//!
//! Three concrete patterns covering what real models actually emit:
//!
//!   * `<think>…</think>`        — DeepSeek R1, Qwen QwQ.
//!   * `<thinking>…</thinking>`  — common community convention; some
//!     llama.cpp finetunes use it.
//!   * `<reasoning>…</reasoning>` — older Hermes / Nous finetunes.
//!
//! Multiline content within tags is supported; the regex uses
//! `(?s)` so `.` matches newlines.
//!
//! Anthropic's structured `thinking` content blocks are *not* handled
//! here — they arrive as a non-Text variant and are filtered by the
//! provider parsing layer (the gemini / anthropic providers use a
//! catch-all `Other(serde_json::Value)` arm or equivalent that drops
//! unknown blocks before they reach us). If a future provider starts
//! emitting them as Text, this scrubber will catch them.
//!
//! ## What we DON'T strip
//!
//!   * Plain prose that happens to start with "I'm thinking" or
//!     similar — far too high a false-positive rate.
//!   * Tagged code (`<code>`, `<pre>`, `<answer>`) — not reasoning.
//!   * Tool call / tool result content — those have their own
//!     structured representations and are never reasoning traces.
//!
//! ## Output
//!
//! `ThinkScrubber::scrub(&str) -> String` returns the input with
//! every reasoning block removed and any leading/trailing whitespace
//! tightened. `scrub_messages(Vec<Message>) -> Vec<Message>` walks
//! every Text content block in every message; messages that become
//! entirely empty (only had reasoning text and nothing else) are
//! dropped to avoid creating empty assistant turns that some
//! providers reject.

use std::sync::OnceLock;

use regex::Regex;

use crate::agent::llm::{ContentBlock, Message};

/// Compiled regex set, lazily initialised. Patterns are kept as a
/// fixed list rather than configurable so behaviour is
/// version-pinned across the codebase.
static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();

fn patterns() -> &'static [Regex] {
    PATTERNS
        .get_or_init(|| {
            vec![
                // DeepSeek R1, Qwen QwQ.
                Regex::new(r"(?si)<think>.*?</think>").unwrap(),
                // Common community convention.
                Regex::new(r"(?si)<thinking>.*?</thinking>").unwrap(),
                // Older Hermes / Nous finetunes.
                Regex::new(r"(?si)<reasoning>.*?</reasoning>").unwrap(),
            ]
        })
        .as_slice()
}

/// Stateless scrubber facade. Kept as a struct so future toggles
/// (e.g. preserve-trailing-rationale, depth-limited stripping) can be
/// added without breaking callers.
#[derive(Debug, Default, Clone, Copy)]
pub struct ThinkScrubber;

impl ThinkScrubber {
    pub fn new() -> Self {
        Self
    }

    /// Strip every recognised reasoning-block from `text`. Trims
    /// resulting leading / trailing whitespace.
    pub fn scrub(&self, text: &str) -> String {
        let mut out = text.to_string();
        for re in patterns() {
            // `replace_all` returns Cow; clone-into-owned only if it
            // actually replaced anything.
            out = re.replace_all(&out, "").into_owned();
        }
        out.trim().to_string()
    }

    /// True if `text` contains any recognised reasoning block.
    pub fn has_thinking(&self, text: &str) -> bool {
        patterns().iter().any(|re| re.is_match(text))
    }

    /// Apply [`Self::scrub`] to every Text content block in every
    /// message. Messages whose content becomes entirely empty after
    /// scrubbing (no remaining Text/ToolUse/ToolResult/Image blocks)
    /// are dropped, since an empty message will be rejected by some
    /// providers.
    ///
    /// Tool-use / tool-result / image blocks are passed through
    /// untouched.
    pub fn scrub_messages(&self, messages: Vec<Message>) -> Vec<Message> {
        let mut out: Vec<Message> = Vec::with_capacity(messages.len());
        for mut m in messages {
            let mut new_blocks: Vec<ContentBlock> = Vec::with_capacity(m.content.len());
            for block in m.content.drain(..) {
                match block {
                    ContentBlock::Text { text } => {
                        let scrubbed = self.scrub(&text);
                        if !scrubbed.is_empty() {
                            new_blocks.push(ContentBlock::Text { text: scrubbed });
                        }
                    }
                    other => new_blocks.push(other),
                }
            }
            if !new_blocks.is_empty() {
                m.content = new_blocks;
                out.push(m);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/context/think_scrub.rs"
    ));
}
