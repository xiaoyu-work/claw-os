//! LLM-driven short-text summarisation routed through the
//! auxiliary client.
//!
//! `summarise(aux, text, max_chars)` returns a compact summary of
//! `text` no longer than `max_chars`.
//!
//! Three-tier fallback chain:
//!
//!   1. **Empty / short input** — text shorter than `max_chars`
//!      after trim is returned verbatim. No model call.
//!   2. **Auxiliary path** — when `aux` is `Some`, ask the model
//!      with a tight system prompt and clamp the reply to
//!      `max_chars`. On non-empty output, return it.
//!   3. **Heuristic fallback** — auxiliary unavailable or
//!      empty/error reply → take the first sentence (or first
//!      `max_chars` chars) of the input and clamp.
//!
//! Errors from the auxiliary call are logged via `tracing::warn`
//! and swallowed; the caller never sees a hard failure.
//!
//! Companion to [`crate::agent::title`] and
//! [`crate::agent::classify`]; same design discipline.

use crate::agent::llm::auxiliary::AuxiliaryClient;

/// Hard cap on the prompt-side text length we'll send to the
/// auxiliary model. Long bodies are truncated from the tail
/// (recent content matters most). 16 KiB matches a comfortable
/// budget for short-summary tasks.
pub const MAX_INPUT_CHARS: usize = 16 * 1024;

const SYSTEM_PROMPT: &str = "You write extremely concise summaries. \
    Reply with a single sentence (no preamble, no quotes, no list). \
    Stick to the original meaning. Shorter is better.";

/// Summarise `text` to no more than `max_chars` characters.
///
/// `max_chars == 0` is treated as "no body wanted" and returns "".
pub async fn summarise(aux: Option<&AuxiliaryClient>, text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if char_len(trimmed) <= max_chars {
        return trimmed.to_string();
    }
    if let Some(client) = aux {
        let body = build_prompt(trimmed);
        match client.ask(Some(SYSTEM_PROMPT), &body).await {
            Ok(out) => {
                let cleaned = clean_output(&out);
                if !cleaned.is_empty() {
                    return clamp(&cleaned, max_chars);
                }
                // empty reply → fall through to heuristic
            }
            Err(e) => {
                tracing::warn!(
                    target: "cos.agent.summarise",
                    "auxiliary summarise failed: {e}"
                );
            }
        }
    }
    clamp(&heuristic(trimmed), max_chars)
}

/// First sentence of `text`, falling back to the head of the
/// string when no sentence terminator is found. Returned string
/// is *not* clamped — the caller is expected to clamp.
pub fn heuristic(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    // Find first occurrence of `.`, `!`, or `?` followed by space
    // OR end-of-string. Sentence boundary detection is kept
    // intentionally simple — full ICU segmentation is overkill
    // for a fallback path.
    let bytes = trimmed.as_bytes();
    let mut end: Option<usize> = None;
    for (i, b) in bytes.iter().enumerate() {
        if matches!(*b, b'.' | b'!' | b'?') {
            // Inclusive of the punctuation.
            end = Some(i + 1);
            // Look at the next byte: if it's whitespace or EOS,
            // we've hit a sentence boundary.
            match bytes.get(i + 1) {
                None => break,
                Some(c) if c.is_ascii_whitespace() => break,
                _ => {
                    // e.g. "3.14" — keep scanning.
                    end = None;
                }
            }
        }
    }
    match end {
        Some(n) => trimmed[..n].to_string(),
        None => trimmed.to_string(),
    }
}

/// Clamp `s` to at most `max_chars` UTF-8 chars. When clamping
/// happens, append a single ellipsis char (counted toward the
/// budget; "abcdefg" with max=4 → "abc…" not "abcd…").
pub fn clamp(s: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let total = char_len(s);
    if total <= max_chars {
        return s.to_string();
    }
    if max_chars == 1 {
        return "…".to_string();
    }
    let mut out: String = s.chars().take(max_chars - 1).collect();
    out.push('…');
    out
}

fn char_len(s: &str) -> usize {
    s.chars().count()
}

fn build_prompt(text: &str) -> String {
    if char_len(text) > MAX_INPUT_CHARS {
        let mut s: String = text.chars().take(MAX_INPUT_CHARS).collect();
        s.push_str(" […]");
        s
    } else {
        text.to_string()
    }
}

/// First non-empty line, trimmed, with wrapping quotes stripped
/// and trailing sentence punctuation kept (we *want* the period
/// at the end of a summary).
fn clean_output(s: &str) -> String {
    let line = s
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    let chars: Vec<char> = line.chars().collect();
    if chars.len() < 2 {
        return line.to_string();
    }
    let first = chars[0];
    let last = chars[chars.len() - 1];
    let matched = matches!(
        (first, last),
        ('"', '"') | ('\'', '\'') | ('`', '`') | ('“', '”')
    );
    if matched {
        chars[1..chars.len() - 1].iter().collect()
    } else {
        line.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::llm::auxiliary::AuxiliaryConfig;
    use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
    use crate::agent::llm::Provider;
    use crate::config::AgentConfig;
    use std::sync::Arc;

    fn aux_with(reply: &str) -> AuxiliaryClient {
        let cfg = AgentConfig::default();
        let provider = MockProvider::new("sum-mock", &cfg);
        provider.push_response(MockResponse::Text(reply.to_string()));
        AuxiliaryClient::new(
            Arc::new(provider) as Arc<dyn Provider>,
            AuxiliaryConfig::new("mock", "sum-mock"),
        )
    }

    #[tokio::test]
    async fn empty_input_returns_empty() {
        let aux = aux_with("never called");
        assert_eq!(summarise(Some(&aux), "  ", 100).await, "");
    }

    #[tokio::test]
    async fn max_chars_zero_returns_empty() {
        let aux = aux_with("never called");
        assert_eq!(summarise(Some(&aux), "hello world", 0).await, "");
    }

    #[tokio::test]
    async fn short_input_under_budget_returns_verbatim() {
        // No model call needed because input is already short.
        let out = summarise(None, "hello world", 100).await;
        assert_eq!(out, "hello world");
    }

    #[tokio::test]
    async fn no_aux_uses_heuristic() {
        let long = "First sentence. Second sentence is longer than the budget.";
        let out = summarise(None, long, 30).await;
        // Heuristic picks first sentence and clamps.
        assert!(out.contains("First sentence."));
        assert!(out.chars().count() <= 30);
    }

    #[tokio::test]
    async fn aux_path_returns_clamped_reply() {
        let aux = aux_with("Short summary.");
        let long = "The cat sat on the mat. The dog watched. It was a sunny day in the park, and many people were having picnics.";
        let out = summarise(Some(&aux), long, 50).await;
        assert_eq!(out, "Short summary.");
    }

    #[tokio::test]
    async fn aux_clamps_overlong_reply() {
        let reply = "x".repeat(200);
        let aux = aux_with(&reply);
        let long = "a".repeat(500);
        let out = summarise(Some(&aux), &long, 30).await;
        assert_eq!(out.chars().count(), 30);
        assert!(out.ends_with('…'));
    }

    #[tokio::test]
    async fn aux_empty_reply_falls_back_to_heuristic() {
        let aux = aux_with("   \n  ");
        let long = "First. Second sentence is much longer than the budget cap.";
        let out = summarise(Some(&aux), long, 20).await;
        assert!(out.contains("First."));
    }

    #[tokio::test]
    async fn aux_error_falls_back_to_heuristic() {
        let cfg = AgentConfig::default();
        let provider = MockProvider::new("err-mock", &cfg);
        provider.push_response(MockResponse::Error(crate::agent::llm::LlmError::Internal(
            "boom".into(),
        )));
        let aux = AuxiliaryClient::new(
            Arc::new(provider) as Arc<dyn Provider>,
            AuxiliaryConfig::new("mock", "err-mock"),
        );
        let long = "Heuristic wins. Because the model exploded.";
        let out = summarise(Some(&aux), long, 20).await;
        assert!(out.contains("Heuristic wins."));
    }

    #[tokio::test]
    async fn aux_strips_wrapping_quotes() {
        let aux = aux_with("\"wrapped reply.\"");
        let long = "long input that triggers the model path because it is over the budget";
        let out = summarise(Some(&aux), long, 30).await;
        assert_eq!(out, "wrapped reply.");
    }

    #[test]
    fn heuristic_picks_first_sentence() {
        assert_eq!(heuristic("First sentence. Second."), "First sentence.");
    }

    #[test]
    fn heuristic_handles_no_terminator() {
        assert_eq!(heuristic("plain text"), "plain text");
    }

    #[test]
    fn heuristic_doesnt_split_on_decimals() {
        // "3.14 is pi. The end." should pick "3.14 is pi." as the
        // first sentence (the period inside 3.14 isn't followed by
        // whitespace).
        assert_eq!(heuristic("3.14 is pi. The end."), "3.14 is pi.");
    }

    #[test]
    fn clamp_appends_ellipsis_when_over_budget() {
        assert_eq!(clamp("abcdefg", 4), "abc…");
        assert_eq!(clamp("abc", 4), "abc"); // under budget = unchanged
        assert_eq!(clamp("abcd", 4), "abcd"); // exactly = unchanged
    }

    #[test]
    fn clamp_handles_max_one() {
        assert_eq!(clamp("anything", 1), "…");
    }

    #[test]
    fn clamp_handles_max_zero() {
        assert_eq!(clamp("anything", 0), "");
    }

    #[test]
    fn clamp_is_multibyte_safe() {
        // 5 emoji chars → 5 char_len, even though they're 4 bytes
        // each. Clamping at 3 must NOT slice through a code point.
        let s = "🦀🦀🦀🦀🦀";
        let out = clamp(s, 3);
        // Result is "🦀🦀…" (2 emoji + ellipsis = 3 chars).
        assert_eq!(out.chars().count(), 3);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn build_prompt_truncates_long_input() {
        let huge = "a".repeat(MAX_INPUT_CHARS + 100);
        let prompt = build_prompt(&huge);
        assert!(prompt.ends_with("[…]"));
        assert!(prompt.chars().count() < MAX_INPUT_CHARS + 200);
    }
}
