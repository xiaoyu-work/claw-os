//! Session title generation via the auxiliary client.
//!
//! Sessions in the memory DB are keyed by an opaque uuid. For UI
//! purposes (`cos agent sessions`) a short human-readable title makes
//! browsing past conversations far easier. Generating titles in the
//! main agent loop would burn flagship-tier tokens on a one-shot
//! summarisation that doesn't need them — so this routes through
//! [`crate::agent::llm::auxiliary::AuxiliaryClient`] when configured,
//! and falls back to a deterministic heuristic when not.
//!
//! ## Design
//!
//! * **Pure.** No persistence. Callers receive a `String` and decide
//!   what to do with it (write to DB, print to TUI, etc.).
//! * **Bounded output.** Titles are clamped to [`MAX_TITLE_CHARS`]
//!   regardless of what the model returns. The auxiliary system
//!   prompt asks for ≤ 6 words, but adversarial / sloppy outputs
//!   could exceed that — clamping is a hard guarantee.
//! * **Strict fallback chain.**
//!     1. Auxiliary client + non-empty seed → first non-empty line of
//!        the model's response, post-clamped.
//!     2. Auxiliary client returns empty / whitespace → heuristic.
//!     3. No auxiliary client configured → heuristic.
//!     4. Auxiliary call errors → heuristic (error logged via
//!        `tracing::warn` but not surfaced to the caller).
//! * **Heuristic.** Take the first line of the seed, strip leading
//!   slash-commands (`/foo`), trim, and clamp. Empty seed →
//!   `"untitled"`.
//!
//! Library-only this commit. The runtime can call
//! [`generate_title`] when starting/resuming a session.

use crate::agent::llm::auxiliary::AuxiliaryClient;

/// Hard cap on title length in characters (UTF-8 char count, not bytes).
/// Stops adversarial/sloppy auxiliary output from polluting UI.
pub const MAX_TITLE_CHARS: usize = 80;

const SYSTEM_PROMPT: &str = "You generate short titles for chat sessions. \
    Reply with only the title, no quotes, no punctuation at the end, \
    no explanation. Maximum 6 words.";

/// Generate a title from the first user turn `seed`.
///
/// `aux` is `None` when no auxiliary provider is configured (see
/// [`crate::agent::runtime::loop_::auxiliary_from_cfg`]). Errors
/// from the auxiliary call fall back to the heuristic.
pub async fn generate_title(aux: Option<&AuxiliaryClient>, seed: &str) -> String {
    let trimmed = seed.trim();
    if trimmed.is_empty() {
        return "untitled".to_string();
    }

    if let Some(client) = aux {
        match client.ask(Some(SYSTEM_PROMPT), trimmed).await {
            Ok(out) => {
                let candidate = clean_model_output(&out);
                if !candidate.is_empty() {
                    return clamp(&candidate);
                }
                // empty / whitespace from model → fall through to
                // heuristic; not an error worth logging.
            }
            Err(e) => {
                tracing::warn!(
                    target: "cos.agent.title",
                    "auxiliary title generation failed: {e}; using heuristic"
                );
            }
        }
    }

    clamp(&heuristic(trimmed))
}

/// Strip leading slash-commands, take the first line, trim. Returns
/// `"untitled"` when the seed has no usable content.
pub fn heuristic(seed: &str) -> String {
    let line = seed.lines().next().unwrap_or("").trim();
    let line = line
        .strip_prefix('/')
        .map(|rest| {
            // Drop the verb itself: `/ask hello` → `hello`.
            rest.split_once(char::is_whitespace)
                .map(|(_, rest)| rest)
                .unwrap_or("")
        })
        .unwrap_or(line)
        .trim();
    if line.is_empty() {
        "untitled".to_string()
    } else {
        line.to_string()
    }
}

/// Clamp a string to [`MAX_TITLE_CHARS`] (UTF-8 char count).
pub fn clamp(s: &str) -> String {
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i >= MAX_TITLE_CHARS {
            break;
        }
        out.push(ch);
    }
    out
}

/// Strip wrapping quotes, surrounding whitespace, and trailing
/// sentence punctuation that the model often appends despite the
/// system prompt's instructions.
fn clean_model_output(s: &str) -> String {
    let line = s.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let line = line.trim();
    // Drop matched wrapping quotes (",',`,“”).
    let line = match (line.chars().next(), line.chars().last()) {
        (Some('"'), Some('"'))
        | (Some('\''), Some('\''))
        | (Some('`'), Some('`'))
        | (Some('“'), Some('”'))
            if line.chars().count() >= 2 =>
        {
            let mut chars: Vec<char> = line.chars().collect();
            chars.remove(0);
            chars.pop();
            chars.iter().collect::<String>()
        }
        _ => line.to_string(),
    };
    // Strip trailing `.`, `!`, `?`, `:`, `,`.
    let line = line
        .trim_end_matches(['.', '!', '?', ':', ','])
        .trim();
    line.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::llm::auxiliary::{AuxiliaryClient, AuxiliaryConfig};
    use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
    use crate::config::AgentConfig;
    use std::sync::Arc;

    fn aux_client_with(responses: Vec<MockResponse>) -> AuxiliaryClient {
        let cfg = AgentConfig::default();
        let provider = MockProvider::new("title-mock", &cfg);
        for r in responses {
            provider.push_response(r);
        }
        AuxiliaryClient::new(
            Arc::new(provider),
            AuxiliaryConfig::new("mock", "title-mock"),
        )
    }

    #[test]
    fn heuristic_drops_slash_command() {
        assert_eq!(heuristic("/ask what is rust"), "what is rust");
        assert_eq!(heuristic("/help"), "untitled");
    }

    #[test]
    fn heuristic_takes_first_line_only() {
        assert_eq!(heuristic("first\nsecond\nthird"), "first");
    }

    #[test]
    fn heuristic_empty_seed_yields_untitled() {
        assert_eq!(heuristic(""), "untitled");
        assert_eq!(heuristic("   \n\t  "), "untitled");
    }

    #[test]
    fn clamp_truncates_at_char_boundary() {
        let s = "a".repeat(MAX_TITLE_CHARS + 50);
        assert_eq!(clamp(&s).chars().count(), MAX_TITLE_CHARS);
    }

    #[test]
    fn clamp_handles_multibyte_correctly() {
        // Each emoji is 1 char but multiple bytes; ensure clamp counts
        // chars not bytes so we don't slice through a codepoint.
        let s = "🚀".repeat(MAX_TITLE_CHARS + 5);
        let out = clamp(&s);
        assert_eq!(out.chars().count(), MAX_TITLE_CHARS);
    }

    #[test]
    fn clean_strips_double_quotes_and_trailing_period() {
        assert_eq!(clean_model_output("\"Hello world.\""), "Hello world");
    }

    #[test]
    fn clean_strips_smart_quotes() {
        assert_eq!(clean_model_output("“Hello world”"), "Hello world");
    }

    #[test]
    fn clean_takes_first_nonempty_line() {
        assert_eq!(
            clean_model_output("\n\nReal title\nfollow-up"),
            "Real title"
        );
    }

    #[tokio::test]
    async fn generate_with_no_aux_uses_heuristic() {
        let title = generate_title(None, "How do I install rust?").await;
        assert_eq!(title, "How do I install rust?");
    }

    #[tokio::test]
    async fn generate_with_aux_uses_model_output() {
        let aux = aux_client_with(vec![MockResponse::Text("Installing Rust toolchain".into())]);
        let title = generate_title(Some(&aux), "How do I install rust?").await;
        assert_eq!(title, "Installing Rust toolchain");
    }

    #[tokio::test]
    async fn generate_with_aux_strips_quotes_and_punct() {
        let aux = aux_client_with(vec![MockResponse::Text("\"Installing Rust.\"".into())]);
        let title = generate_title(Some(&aux), "How do I install rust?").await;
        assert_eq!(title, "Installing Rust");
    }

    #[tokio::test]
    async fn generate_with_aux_empty_response_falls_back() {
        let aux = aux_client_with(vec![MockResponse::Text("   \n  ".into())]);
        let title = generate_title(Some(&aux), "/ask How do I install rust?").await;
        // Heuristic strips `/ask`.
        assert_eq!(title, "How do I install rust?");
    }

    #[tokio::test]
    async fn generate_empty_seed_returns_untitled() {
        let title = generate_title(None, "   ").await;
        assert_eq!(title, "untitled");
    }

    #[tokio::test]
    async fn generate_clamps_long_model_output() {
        let long = "Very ".repeat(50);
        let aux = aux_client_with(vec![MockResponse::Text(long)]);
        let title = generate_title(Some(&aux), "seed").await;
        assert!(title.chars().count() <= MAX_TITLE_CHARS);
    }
}
