//! LLM-driven text classification routed through the auxiliary
//! client.
//!
//! `classify(aux, text, labels)` asks the auxiliary model to pick
//! exactly one label from `labels` for `text`. The result is the
//! matching label string when the model's reply maps unambiguously
//! to one entry (case-insensitive match against the trimmed
//! reply); otherwise `None`.
//!
//! Design choices:
//!
//!   * **No heuristic fallback.** Classification is a model task —
//!     a heuristic over a single text snippet would be either
//!     trivial (keyword lookup, the caller can do that themselves)
//!     or wrong-by-default. When `aux` is `None` the function
//!     returns `None` so callers must explicitly opt in to LLM
//!     classification.
//!   * **Empty / single-label inputs short-circuit.** Empty `text`
//!     → `None`. A single-label `labels` slice → that label
//!     (no point asking the model). Empty `labels` → `None`.
//!   * **Conservative parsing.** The model's reply is normalised
//!     (first non-empty line, lower-case) and matched exactly
//!     against each label. Substring matches are *not* accepted —
//!     they would silently mis-classify when one label is a prefix
//!     of another (e.g. "yes" vs "yes-with-caveats").
//!   * **Errors are logged + swallowed** to stay consistent with
//!     the title generator. Classification failures should never
//!     break the loop; they degrade to "couldn't decide".
//!
//! See also [`crate::agent::summarise`] for the symmetric
//! summarisation helper, [`crate::agent::title`] for the title
//! generator (the original auxiliary consumer), and
//! [`crate::agent::llm::auxiliary::AuxiliaryClient::ask`].

use crate::agent::llm::auxiliary::AuxiliaryClient;

/// Hard cap on the prompt-side text length we'll send to the
/// auxiliary model. Long bodies are truncated from the tail to
/// preserve the most recent / typically most informative content.
/// 8 KiB matches a small-prompt budget with comfortable headroom
/// for the system prompt + label list.
pub const MAX_INPUT_CHARS: usize = 8 * 1024;

/// Classify `text` into exactly one entry from `labels`. Returns
/// `None` when:
///
///   * `text` is empty after trim
///   * `labels` is empty
///   * `aux` is `None` (no auxiliary configured)
///   * the model's reply doesn't unambiguously match any label
///   * the auxiliary call errored (the error is logged via
///     `tracing::warn`)
///
/// On success the returned string is the matching label *exactly
/// as it appeared in `labels`* — callers can compare with `==`.
pub async fn classify(
    aux: Option<&AuxiliaryClient>,
    text: &str,
    labels: &[&str],
) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() || labels.is_empty() {
        return None;
    }
    if labels.len() == 1 {
        return Some(labels[0].to_string());
    }
    let client = aux?;
    let prompt = build_prompt(trimmed, labels);
    let system = system_prompt(labels);
    let reply = match client.ask(Some(&system), &prompt).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                target: "cos.agent.classify",
                "auxiliary classify failed: {e}"
            );
            return None;
        }
    };
    match_label(&reply, labels)
}

/// System prompt for the auxiliary model. Lists the candidate
/// labels and instructs the model to reply with one of them
/// verbatim.
fn system_prompt(labels: &[&str]) -> String {
    let joined = labels.join(", ");
    format!(
        "You are a classifier. Reply with exactly ONE of: {joined}. \
         Reply with only the label, no quotes, no explanation, no punctuation."
    )
}

fn build_prompt(text: &str, labels: &[&str]) -> String {
    let truncated = if text.chars().count() > MAX_INPUT_CHARS {
        let mut s: String = text.chars().take(MAX_INPUT_CHARS).collect();
        s.push_str(" […]");
        s
    } else {
        text.to_string()
    };
    let joined = labels.join(" | ");
    format!("Labels: {joined}\n\nClassify this text:\n{truncated}")
}

/// Normalise `reply` (first non-empty line, trim, lower-case) and
/// look for an exact match against any label (compared
/// case-insensitively). Returns the original-case label on hit.
///
/// Pure function — no I/O, no logging — so callers can unit-test
/// it without an auxiliary mock.
pub fn match_label(reply: &str, labels: &[&str]) -> Option<String> {
    let line = reply.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let cleaned = strip_wrap_chars(line.trim()).to_lowercase();
    if cleaned.is_empty() {
        return None;
    }
    let mut hit: Option<String> = None;
    for label in labels {
        if label.eq_ignore_ascii_case(&cleaned) {
            // Multiple labels equal-ignore-case to the cleaned
            // reply would be a caller bug (duplicate labels);
            // pick the first match deterministically.
            return Some((*label).to_string());
        }
    }
    // Second pass: accept the reply if it equals one label IGNORING
    // surrounding sentence punctuation. This is a small forgiveness
    // for models that append a period despite the system prompt.
    let stripped: String = cleaned
        .trim_end_matches(['.', '!', '?', ':', ','])
        .to_string();
    if stripped != cleaned {
        for label in labels {
            if label.eq_ignore_ascii_case(&stripped) {
                hit = Some((*label).to_string());
                break;
            }
        }
    }
    hit
}

/// Strip a single matched pair of wrapping quotes / backticks /
/// smart quotes from `s`. Used to forgive models that wrap their
/// one-token reply in quotes despite the system instruction.
fn strip_wrap_chars(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() < 2 {
        return s.to_string();
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
        s.to_string()
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
        let provider = MockProvider::new("classify-mock", &cfg);
        provider.push_response(MockResponse::Text(reply.to_string()));
        AuxiliaryClient::new(
            Arc::new(provider) as Arc<dyn Provider>,
            AuxiliaryConfig::new("mock", "classify-mock"),
        )
    }

    #[tokio::test]
    async fn empty_text_returns_none() {
        let aux = aux_with("yes");
        let out = classify(Some(&aux), "  ", &["yes", "no"]).await;
        assert!(out.is_none());
    }

    #[tokio::test]
    async fn empty_labels_returns_none() {
        let aux = aux_with("yes");
        let out = classify(Some(&aux), "anything", &[]).await;
        assert!(out.is_none());
    }

    #[tokio::test]
    async fn no_aux_returns_none() {
        let out = classify(None, "anything", &["yes", "no"]).await;
        assert!(out.is_none());
    }

    #[tokio::test]
    async fn single_label_short_circuits() {
        // Single-label classification doesn't need the model.
        let out = classify(None, "anything", &["only"]).await;
        assert_eq!(out.as_deref(), Some("only"));
    }

    #[tokio::test]
    async fn matches_exact_lowercase_reply() {
        let aux = aux_with("yes");
        let out = classify(Some(&aux), "is the sky blue?", &["yes", "no"]).await;
        assert_eq!(out.as_deref(), Some("yes"));
    }

    #[tokio::test]
    async fn matches_case_insensitive() {
        let aux = aux_with("YES");
        let out = classify(Some(&aux), "is the sky blue?", &["yes", "no"]).await;
        assert_eq!(out.as_deref(), Some("yes"));
    }

    #[tokio::test]
    async fn forgives_trailing_period() {
        let aux = aux_with("no.");
        let out = classify(Some(&aux), "is fire wet?", &["yes", "no"]).await;
        assert_eq!(out.as_deref(), Some("no"));
    }

    #[tokio::test]
    async fn forgives_wrapping_quotes() {
        let aux = aux_with("\"yes\"");
        let out = classify(Some(&aux), "x", &["yes", "no"]).await;
        assert_eq!(out.as_deref(), Some("yes"));
    }

    #[tokio::test]
    async fn rejects_unmatched_reply() {
        let aux = aux_with("maybe");
        let out = classify(Some(&aux), "x", &["yes", "no"]).await;
        assert!(out.is_none());
    }

    #[tokio::test]
    async fn rejects_substring_match() {
        // Reply "yes-please" should NOT match "yes" — substring
        // matching would silently mis-classify on prefix overlap.
        let aux = aux_with("yes-please");
        let out = classify(Some(&aux), "x", &["yes", "no"]).await;
        assert!(out.is_none());
    }

    #[tokio::test]
    async fn returns_label_in_original_case() {
        let aux = aux_with("INTENT_QUERY");
        let out = classify(Some(&aux), "x", &["intent_query", "intent_command"]).await;
        // Returned exactly as it appears in `labels`.
        assert_eq!(out.as_deref(), Some("intent_query"));
    }

    #[tokio::test]
    async fn aux_error_returns_none() {
        let cfg = AgentConfig::default();
        let provider = MockProvider::new("err-mock", &cfg);
        provider.push_response(MockResponse::Error(crate::agent::llm::LlmError::Internal(
            "boom".into(),
        )));
        let aux = AuxiliaryClient::new(
            Arc::new(provider) as Arc<dyn Provider>,
            AuxiliaryConfig::new("mock", "err-mock"),
        );
        let out = classify(Some(&aux), "x", &["yes", "no"]).await;
        assert!(out.is_none());
    }

    #[test]
    fn match_label_picks_first_line() {
        assert_eq!(
            match_label("\nyes\nbecause sky is blue", &["yes", "no"]),
            Some("yes".to_string())
        );
    }

    #[test]
    fn match_label_empty_reply_is_none() {
        assert_eq!(match_label("", &["yes", "no"]), None);
        assert_eq!(match_label("   \n  ", &["yes", "no"]), None);
    }

    #[test]
    fn match_label_returns_first_when_duplicate_labels() {
        // Caller-provided duplicate labels are a bug, but the
        // function must be deterministic. Pick the first.
        assert_eq!(match_label("yes", &["yes", "yes"]), Some("yes".to_string()));
    }

    #[test]
    fn build_prompt_truncates_long_input() {
        let huge = "a".repeat(MAX_INPUT_CHARS + 100);
        let prompt = build_prompt(&huge, &["x", "y"]);
        // Truncation marker is appended.
        assert!(prompt.ends_with("[…]"));
        assert!(prompt.chars().count() < MAX_INPUT_CHARS + 200);
    }

    #[test]
    fn system_prompt_includes_all_labels() {
        let s = system_prompt(&["alpha", "beta", "gamma"]);
        assert!(s.contains("alpha"));
        assert!(s.contains("beta"));
        assert!(s.contains("gamma"));
    }

    #[test]
    fn strip_wrap_chars_handles_smart_quotes() {
        assert_eq!(strip_wrap_chars("“hello”"), "hello");
        assert_eq!(strip_wrap_chars("`x`"), "x");
        assert_eq!(strip_wrap_chars("plain"), "plain");
        // Mismatched bookends are NOT stripped.
        assert_eq!(strip_wrap_chars("\"unmatched'"), "\"unmatched'");
    }
}
