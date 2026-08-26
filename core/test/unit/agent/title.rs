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
