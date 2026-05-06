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
    use super::*;
    use crate::agent::llm::types::Role;

    fn s() -> ThinkScrubber {
        ThinkScrubber::new()
    }

    #[test]
    fn passes_through_unchanged_when_no_tags() {
        let t = "the final answer is 42";
        assert_eq!(s().scrub(t), t);
        assert!(!s().has_thinking(t));
    }

    #[test]
    fn strips_think_tag() {
        let t = "<think>let me reason about this carefully</think>The answer is 42.";
        let out = s().scrub(t);
        assert_eq!(out, "The answer is 42.");
        assert!(!out.contains("reason"));
    }

    #[test]
    fn strips_thinking_tag() {
        let t = "<thinking>step by step</thinking>final.";
        assert_eq!(s().scrub(t), "final.");
    }

    #[test]
    fn strips_reasoning_tag() {
        let t = "<reasoning>chain of thought goes here</reasoning>OK done.";
        assert_eq!(s().scrub(t), "OK done.");
    }

    #[test]
    fn strips_multiline_reasoning() {
        let t = "<think>\nline 1\nline 2\nline 3\n</think>\nresult.";
        let out = s().scrub(t);
        assert_eq!(out, "result.");
    }

    #[test]
    fn strips_multiple_thinking_blocks() {
        let t =
            "<think>first</think>middle text<thinking>second</thinking>end<reasoning>third</reasoning>";
        let out = s().scrub(t);
        assert_eq!(out, "middle textend");
    }

    #[test]
    fn case_insensitive_tag_matching() {
        let t = "<THINK>upper</THINK><Thinking>mixed</Thinking>kept.";
        let out = s().scrub(t);
        assert_eq!(out, "kept.");
    }

    #[test]
    fn missing_close_tag_left_alone() {
        // Deliberately conservative: never strip without a matching
        // close tag. Lazy `.*?` ensures we don't gobble the rest of
        // the document.
        let t = "<think>hanging open\nrest of text";
        assert_eq!(s().scrub(t), t);
    }

    #[test]
    fn nested_tags_outermost_close_wins() {
        // Lazy match means inner tag closes the outer too. This is a
        // known limitation; comment so future readers don't expect
        // proper nesting support.
        let t = "<think>outer <think>inner</think> trailing</think>final.";
        let out = s().scrub(t);
        // Lazy regex: matches `<think>outer <think>inner</think>`,
        // leaving ` trailing</think>final.` behind.
        assert!(out.contains("trailing"));
        assert!(out.ends_with("final."));
    }

    #[test]
    fn has_thinking_truthy() {
        assert!(s().has_thinking("<think>x</think>"));
        assert!(s().has_thinking("<thinking>x</thinking>"));
        assert!(s().has_thinking("<reasoning>x</reasoning>"));
        assert!(!s().has_thinking("plain text"));
    }

    #[test]
    fn scrub_messages_strips_text_blocks_in_place() {
        let messages = vec![
            Message::user_text("ask <think>hidden</think>question"),
            Message::assistant_text("<thinking>reasoning</thinking>answer"),
        ];
        let out = s().scrub_messages(messages);
        assert_eq!(out.len(), 2);
        match &out[0].content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "ask question"),
            _ => panic!("expected text"),
        }
        match &out[1].content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "answer"),
            _ => panic!("expected text"),
        }
    }

    #[test]
    fn scrub_messages_drops_empty_messages_after_scrubbing() {
        // The first message is *only* a thinking block — should drop.
        let messages = vec![
            Message::assistant_text("<think>only reasoning, no answer</think>"),
            Message::user_text("next question"),
        ];
        let out = s().scrub_messages(messages);
        assert_eq!(out.len(), 1);
        match &out[0].content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "next question"),
            _ => panic!(),
        }
    }

    #[test]
    fn scrub_messages_preserves_tool_use_and_result_blocks() {
        let messages = vec![Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text {
                    text: "<thinking>plan</thinking>calling tool".into(),
                },
                ContentBlock::ToolUse {
                    id: "t1".into(),
                    name: "echo".into(),
                    input: serde_json::json!({"text": "ping"}),
                },
            ],
        }];
        let out = s().scrub_messages(messages);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].content.len(), 2);
        match &out[0].content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "calling tool"),
            _ => panic!(),
        }
        match &out[0].content[1] {
            ContentBlock::ToolUse { name, .. } => assert_eq!(name, "echo"),
            _ => panic!(),
        }
    }

    #[test]
    fn scrub_messages_keeps_message_when_only_tool_blocks_remain() {
        // Text was entirely thinking; tool blocks remain → message kept.
        let messages = vec![Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text {
                    text: "<think>just reasoning</think>".into(),
                },
                ContentBlock::ToolUse {
                    id: "t1".into(),
                    name: "echo".into(),
                    input: serde_json::json!({}),
                },
            ],
        }];
        let out = s().scrub_messages(messages);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].content.len(), 1);
        matches!(out[0].content[0], ContentBlock::ToolUse { .. });
    }

    #[test]
    fn scrub_messages_image_blocks_pass_through() {
        let messages = vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Image {
                media_type: "image/png".into(),
                data: "abc".into(),
            }],
        }];
        let out = s().scrub_messages(messages);
        assert_eq!(out.len(), 1);
        matches!(out[0].content[0], ContentBlock::Image { .. });
    }

    #[test]
    fn scrub_idempotent() {
        let s = s();
        let t = "<think>x</think>answer";
        let once = s.scrub(t);
        let twice = s.scrub(&once);
        assert_eq!(once, twice);
    }

    #[test]
    fn scrub_tool_result_text_passes_through_unmodified() {
        // tool_result content is a String field, not a Text block —
        // the scrubber doesn't touch it. (By design: tool output is
        // structured data, not model reasoning.)
        let messages = vec![Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "t1".into(),
                is_error: false,
                content: "<think>this stays</think>raw output".into(),
            }],
        }];
        let out = s().scrub_messages(messages);
        match &out[0].content[0] {
            ContentBlock::ToolResult { content, .. } => {
                assert!(content.contains("<think>this stays</think>"));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn scrub_messages_empty_input_returns_empty() {
        let out = s().scrub_messages(Vec::new());
        assert!(out.is_empty());
    }

    #[test]
    fn scrub_trims_leading_trailing_whitespace_left_behind() {
        let t = "  <think>x</think>  ";
        assert_eq!(s().scrub(t), "");
    }
}
