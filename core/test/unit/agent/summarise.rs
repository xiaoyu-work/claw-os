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
