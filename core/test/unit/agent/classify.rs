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
