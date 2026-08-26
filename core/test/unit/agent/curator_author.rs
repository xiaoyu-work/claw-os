use super::*;
use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
use crate::agent::skills::manifest;

fn sample_draft() -> SkillDraft {
    SkillDraft {
        suggested_id: "summarise-csv".into(),
        title: "Summarise CSV".into(),
        description: "Take a CSV file and produce a one-page summary.".into(),
        allowed_tools: vec!["cos_fs".into(), "cos_exec".into()],
        turns_used: 6,
        confidence: SkillConfidence::High,
    }
}

fn sample_turns() -> Vec<ConversationTurn> {
    vec![
        ConversationTurn {
            role: TurnRole::User,
            content: "Summarise sales.csv into one paragraph please".into(),
            tool_calls: vec![],
            user_acceptance: false,
        },
        ConversationTurn {
            role: TurnRole::Assistant,
            content: "I'll read the file first.".into(),
            tool_calls: vec!["cos_fs".into()],
            user_acceptance: false,
        },
        ConversationTurn {
            role: TurnRole::Tool,
            content: "year,total\n2023,100\n2024,150".into(),
            tool_calls: vec![],
            user_acceptance: false,
        },
        ConversationTurn {
            role: TurnRole::Assistant,
            content: "Sales went up 50% YoY.".into(),
            tool_calls: vec![],
            user_acceptance: false,
        },
        ConversationTurn {
            role: TurnRole::User,
            content: "Perfect, thanks.".into(),
            tool_calls: vec![],
            user_acceptance: true,
        },
    ]
}

#[test]
fn frontmatter_parses_back_through_manifest() {
    let draft = sample_draft();
    let fm = render_frontmatter(&draft);
    let document = format!("{fm}# Body\n\nLorem.\n");
    let parsed = manifest::parse(&document).expect("parse ok");
    assert_eq!(parsed.manifest.name, draft.title);
    assert_eq!(
        parsed.manifest.description.as_deref(),
        Some(draft.description.as_str())
    );
    assert_eq!(parsed.manifest.allowed_tools, draft.allowed_tools);
    assert!(parsed.body.contains("# Body"));
}

#[test]
fn yaml_escape_quotes_when_needed() {
    assert_eq!(yaml_escape("plain"), "plain");
    assert_eq!(yaml_escape("has:colon"), "\"has:colon\"");
    assert_eq!(yaml_escape("has\"quote"), "\"has\\\"quote\"");
    // Leading dash ambiguity with sequence syntax.
    assert_eq!(yaml_escape("-leading"), "\"-leading\"");
    // Newlines folded.
    assert_eq!(yaml_escape("a\nb"), "a b");
}

#[test]
fn fallback_body_includes_tools() {
    let body = fallback_body(&sample_draft());
    assert!(body.contains("Summarise CSV"));
    assert!(body.contains("cos_fs"));
    assert!(body.contains("cos_exec"));
    assert!(body.contains("auto-drafted"));
}

#[test]
fn build_author_prompt_truncates_long_turns() {
    let mut turns = sample_turns();
    turns[2].content = "x".repeat(5000);
    let cfg = AuthorConfig::for_model("test");
    let prompt = build_author_prompt(&sample_draft(), &turns, &cfg);
    assert!(prompt.contains("(truncated)"));
    assert!(prompt.len() < 5000 + 1000);
}

#[test]
fn build_author_prompt_caps_inlined_turns() {
    let mut turns = sample_turns();
    // Pad with synthetic turns so we exceed the inline cap.
    for i in 0..50 {
        turns.push(ConversationTurn {
            role: TurnRole::Assistant,
            content: format!("padding turn {i}"),
            tool_calls: vec![],
            user_acceptance: false,
        });
    }
    let mut cfg = AuthorConfig::for_model("test");
    cfg.max_turns_inlined = 10;
    let prompt = build_author_prompt(&sample_draft(), &turns, &cfg);
    // Last padding turn should be present; the early sample
    // turn at index 0 (the User question) must NOT appear
    // verbatim.
    assert!(prompt.contains("padding turn 49"));
    assert!(!prompt.contains("Summarise sales.csv into one paragraph please"));
}

#[tokio::test]
async fn author_with_mock_returns_llm_source() {
    let cfg = crate::config::AgentConfig::default();
    let mock = MockProvider::new("mock-author", &cfg);
    mock.push_response(MockResponse::Text(
        "## Steps\n1. Read the CSV.\n2. Summarise.".into(),
    ));
    let provider: Arc<dyn Provider> = Arc::new(mock);
    let acfg = AuthorConfig::for_model("mock-author");
    let result = author(provider, &acfg, &sample_draft(), &sample_turns()).await;
    assert_eq!(result.source, AuthorSource::Llm);
    assert!(result.error.is_none());
    assert!(result.document.contains("name: Summarise CSV"));
    assert!(result.document.contains("## Steps"));
    assert!(result.body_chars > 10);
}

#[tokio::test]
async fn author_falls_back_on_provider_error() {
    let cfg = crate::config::AgentConfig::default();
    let mock = MockProvider::new("mock-author", &cfg);
    mock.push_response(MockResponse::Error(LlmError::Auth));
    let provider: Arc<dyn Provider> = Arc::new(mock);
    let acfg = AuthorConfig::for_model("mock-author");
    let result = author(provider, &acfg, &sample_draft(), &sample_turns()).await;
    assert_eq!(result.source, AuthorSource::Fallback);
    let err = result.error.expect("error captured");
    assert!(err.contains("auth"), "got {err}");
    assert!(result.document.contains("auto-drafted"));
    assert!(result.document.contains("name: Summarise CSV"));
}

#[tokio::test]
async fn author_falls_back_on_empty_body() {
    let cfg = crate::config::AgentConfig::default();
    let mock = MockProvider::new("mock-author", &cfg);
    mock.push_response(MockResponse::Text("".into()));
    let provider: Arc<dyn Provider> = Arc::new(mock);
    let acfg = AuthorConfig::for_model("mock-author");
    let result = author(provider, &acfg, &sample_draft(), &sample_turns()).await;
    assert_eq!(result.source, AuthorSource::Fallback);
    assert!(result.error.unwrap().contains("empty"));
}

#[test]
fn strip_accidental_frontmatter_removes_top_block() {
    let body = "---\nname: Foo\ndesc: bar\n---\n\n## Real body\ncontent\n";
    let stripped = strip_accidental_frontmatter(body);
    assert!(!stripped.contains("---"));
    assert!(stripped.starts_with("## Real body"));
}

#[test]
fn strip_accidental_frontmatter_keeps_body_without_frontmatter() {
    let body = "## Body\nno frontmatter here\n";
    assert_eq!(strip_accidental_frontmatter(body), body);
}

#[tokio::test]
async fn authored_document_round_trips_through_parser() {
    let cfg = crate::config::AgentConfig::default();
    let mock = MockProvider::new("mock-author", &cfg);
    mock.push_response(MockResponse::Text(
        "## Overview\nThis skill summarises CSVs.\n\n## Steps\n1. Read.\n2. Summarise.\n"
            .into(),
    ));
    let provider: Arc<dyn Provider> = Arc::new(mock);
    let acfg = AuthorConfig::for_model("mock-author");
    let result = author(provider, &acfg, &sample_draft(), &sample_turns()).await;
    let parsed = manifest::parse(&result.document).expect("parse ok");
    assert_eq!(parsed.manifest.name, "Summarise CSV");
    assert!(parsed.body.contains("## Steps"));
}
