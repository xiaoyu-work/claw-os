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
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/summarise.rs"
    ));
}
