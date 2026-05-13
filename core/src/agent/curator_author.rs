//! Curator authorship — turn a [`SkillDraft`] into a publishable
//! `SKILL.md` document by asking the configured LLM provider to
//! write the body.
//!
//! The deterministic half ([`super::propose`]) gives us the
//! recurring-task signature: title, description, tools used,
//! confidence. This module fills in the missing piece: the
//! markdown body the user (and future agent invocations) actually
//! reads to learn how to repeat the task.
//!
//! Design notes
//! ------------
//!
//! * **Provider-pluggable.** Takes a [`Provider`] trait object so
//!   the runtime can route through the auxiliary client (cheap
//!   model) by default and only fall back to the primary if the
//!   user explicitly opts in. This module doesn't pick the
//!   provider — callers do.
//! * **Deterministic frontmatter, LLM body.** The frontmatter
//!   block is assembled here from `SkillDraft` fields so the
//!   resulting document is guaranteed to round-trip through
//!   [`super::super::skills::manifest::parse`] and the tool list
//!   matches what the curator observed. The LLM sees the
//!   conversation transcript and writes only the markdown body.
//! * **Length cap.** Skill bodies live in `SKILL.md` files that
//!   the agent may load at any moment; we cap response tokens at
//!   a deliberately small number ([`AUTHOR_MAX_TOKENS`]) so a
//!   runaway model can't produce a 50K-token document that bloats
//!   every future prompt.
//! * **Safe fallback.** If the LLM call fails we still produce a
//!   minimal body from the draft so callers can persist *something*
//!   the user can edit by hand. The error is returned in addition
//!   to the fallback document so callers can surface it.
//! * **No PII heuristics here.** Redaction is out of scope for
//!   this module; if the conversation contained credentials, the
//!   safety layer should have stripped them before reaching us.

use std::sync::Arc;

use super::curator::{ConversationTurn, SkillConfidence, SkillDraft, TurnRole};
use crate::agent::llm::types::{ChatRequest, Message, ToolChoice};
use crate::agent::llm::{LlmError, Provider};

/// Hard cap on the number of tokens the authoring call may emit.
/// Skill bodies are small by convention; runaway essays defeat
/// the purpose of caching them in every future prompt.
pub const AUTHOR_MAX_TOKENS: u32 = 1500;

/// Hard cap on conversation turns we'll inline into the prompt.
/// Long sessions are already truncated upstream by
/// [`super::CuratorConfig::max_turns`]; this is a defensive
/// second cap so a misconfigured caller can't blow the prompt
/// budget.
pub const AUTHOR_MAX_TURNS_INLINED: usize = 40;

/// Hard cap on individual turn body length when inlining. Tool
/// outputs in particular can be very large; we keep the head so
/// the model sees the *shape* of what happened without paying
/// for the full payload.
pub const AUTHOR_MAX_TURN_BYTES: usize = 1200;

/// Configuration for [`author`]. Defaults are tuned for Anthropic
/// / OpenAI-flagship models; lower temperature for deterministic
/// markdown output, modest token cap.
#[derive(Debug, Clone)]
pub struct AuthorConfig {
    pub model: String,
    pub max_tokens: u32,
    pub temperature: f32,
    /// Override [`AUTHOR_MAX_TURNS_INLINED`] if the caller wants
    /// less context (e.g. when delegating to a small local
    /// model with a tiny context window).
    pub max_turns_inlined: usize,
    /// Override [`AUTHOR_MAX_TURN_BYTES`].
    pub max_turn_bytes: usize,
}

impl AuthorConfig {
    /// Construct a config with the given model and the module's
    /// default token / temperature / inlining caps.
    pub fn for_model(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            max_tokens: AUTHOR_MAX_TOKENS,
            temperature: 0.2,
            max_turns_inlined: AUTHOR_MAX_TURNS_INLINED,
            max_turn_bytes: AUTHOR_MAX_TURN_BYTES,
        }
    }
}

/// Outcome of an authoring attempt. Always contains a `document`
/// the caller can write to disk; `error` is non-None when the
/// LLM call failed and we fell back to a minimal hand-written
/// body so the user has something to edit.
#[derive(Debug)]
pub struct AuthoredSkill {
    /// Full SKILL.md content (frontmatter + body), ready to write.
    pub document: String,
    /// Authoring source: "llm" if the body came from the model,
    /// "fallback" if we synthesised it locally because the LLM
    /// call failed.
    pub source: AuthorSource,
    /// Length of the LLM body in characters, for logging.
    pub body_chars: usize,
    /// LLM error, if any. Set when `source == AuthorSource::Fallback`.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorSource {
    Llm,
    Fallback,
}

/// Render the deterministic frontmatter for a [`SkillDraft`].
///
/// The output is a YAML block delimited by `---` that
/// [`crate::agent::skills::manifest::parse`] will accept. Fields
/// with empty values are omitted to keep the file readable.
pub fn render_frontmatter(draft: &SkillDraft) -> String {
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("name: {}\n", yaml_escape(&draft.title)));
    out.push_str(&format!(
        "description: {}\n",
        yaml_escape(&draft.description)
    ));
    out.push_str(&format!("version: {}\n", "0.1.0"));
    out.push_str(&format!("author: {}\n", "cos-curator"));
    out.push_str(&format!(
        "confidence: {}\n",
        match draft.confidence {
            SkillConfidence::Low => "low",
            SkillConfidence::Medium => "medium",
            SkillConfidence::High => "high",
        }
    ));
    if !draft.allowed_tools.is_empty() {
        out.push_str("allowed_tools:\n");
        for tool in &draft.allowed_tools {
            out.push_str(&format!("  - {}\n", yaml_escape(tool)));
        }
    }
    out.push_str("---\n\n");
    out
}

/// Minimal-effort fallback body. Used when the LLM call fails so
/// the user always gets *something* on disk they can edit.
pub fn fallback_body(draft: &SkillDraft) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {}\n\n", draft.title));
    out.push_str(&format!("{}\n\n", draft.description));
    if !draft.allowed_tools.is_empty() {
        out.push_str("## Tools used\n\n");
        for tool in &draft.allowed_tools {
            out.push_str(&format!("- `{tool}`\n"));
        }
        out.push('\n');
    }
    out.push_str(
        "_This skill was auto-drafted by `cos agent curator` but the LLM authoring step did not run. Edit this file to describe the steps the agent should take._\n",
    );
    out
}

/// Build the user prompt that ships the conversation transcript
/// and asks the model to write the body.
fn build_author_prompt(
    draft: &SkillDraft,
    turns: &[ConversationTurn],
    cfg: &AuthorConfig,
) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "You are documenting a recurring task as a reusable skill. The user requested help with:\n\n> {}\n\n",
        draft.description.trim()
    ));
    out.push_str(&format!(
        "Title: {}\nTools the agent used: {}\n\n",
        draft.title,
        if draft.allowed_tools.is_empty() {
            "(none)".to_string()
        } else {
            draft.allowed_tools.join(", ")
        }
    ));
    out.push_str(
        "Below is a transcript of the conversation that solved the task. Write a concise SKILL.md body in Markdown that teaches a future agent how to perform the same task. Required structure:\n\n",
    );
    out.push_str("1. A short overview paragraph (2-3 sentences).\n");
    out.push_str(
        "2. A '## Steps' section with numbered, concrete steps the agent should follow.\n",
    );
    out.push_str("3. A '## Tools' section listing each tool used and what it was used for.\n");
    out.push_str(
        "4. A '## Notes' section with caveats, error-handling tips, or things to verify.\n\n",
    );
    out.push_str(
        "Do NOT include any frontmatter (no `---` blocks). Output Markdown only. Keep total length under 1200 words.\n\n",
    );
    out.push_str("--- Transcript ---\n");

    let take = cfg.max_turns_inlined.min(turns.len());
    let start = turns.len().saturating_sub(take);
    for (i, turn) in turns[start..].iter().enumerate() {
        let role = match turn.role {
            TurnRole::User => "User",
            TurnRole::Assistant => "Assistant",
            TurnRole::Tool => "Tool",
        };
        let body = if turn.content.len() > cfg.max_turn_bytes {
            // Truncate at a char boundary so we don't slice
            // through a UTF-8 codepoint mid-byte.
            let mut end = cfg.max_turn_bytes;
            while end > 0 && !turn.content.is_char_boundary(end) {
                end -= 1;
            }
            format!("{}…(truncated)", &turn.content[..end])
        } else {
            turn.content.clone()
        };
        out.push_str(&format!("\n[Turn {}] {role}:\n{body}\n", i + 1));
        if !turn.tool_calls.is_empty() {
            out.push_str(&format!("  (tool_calls: {})\n", turn.tool_calls.join(", ")));
        }
    }
    out.push_str("\n--- End transcript ---\n");
    out
}

/// Author a `SKILL.md` document from a curator [`SkillDraft`] +
/// the conversation that produced it. On LLM failure, returns a
/// fallback document with the error captured in
/// [`AuthoredSkill::error`].
pub async fn author(
    provider: Arc<dyn Provider>,
    cfg: &AuthorConfig,
    draft: &SkillDraft,
    turns: &[ConversationTurn],
) -> AuthoredSkill {
    let prompt = build_author_prompt(draft, turns, cfg);
    let request = ChatRequest {
        model: cfg.model.clone(),
        messages: vec![Message::user_text(&prompt)],
        system: Some(
            "You are a careful technical writer. Produce concise, actionable Markdown skill documentation. Do not invent steps that didn't happen in the transcript.".into(),
        ),
        tools: Vec::new(),
        tool_choice: ToolChoice::Auto,
        max_tokens: Some(cfg.max_tokens),
        temperature: Some(cfg.temperature),
        top_p: None,
        stop_sequences: Vec::new(),
        extra: serde_json::Value::Null,
    };

    let frontmatter = render_frontmatter(draft);
    match provider.chat(request).await {
        Ok(resp) => {
            let body = collect_text(&resp);
            if body.trim().is_empty() {
                let fallback = fallback_body(draft);
                let document = format!("{frontmatter}{fallback}");
                let chars = document.len();
                AuthoredSkill {
                    document,
                    source: AuthorSource::Fallback,
                    body_chars: chars,
                    error: Some("LLM returned an empty body".into()),
                }
            } else {
                let cleaned = strip_accidental_frontmatter(&body);
                let document = format!("{frontmatter}{cleaned}");
                let chars = cleaned.len();
                AuthoredSkill {
                    document,
                    source: AuthorSource::Llm,
                    body_chars: chars,
                    error: None,
                }
            }
        }
        Err(e) => {
            let err = format_llm_error(&e);
            let fallback = fallback_body(draft);
            let document = format!("{frontmatter}{fallback}");
            let chars = fallback.len();
            AuthoredSkill {
                document,
                source: AuthorSource::Fallback,
                body_chars: chars,
                error: Some(err),
            }
        }
    }
}

/// Concatenate all `Text` content blocks from the response. Tool
/// blocks (which a misbehaving model might emit despite no tools)
/// are ignored.
fn collect_text(resp: &crate::agent::llm::types::ChatResponse) -> String {
    use crate::agent::llm::types::ContentBlock;
    let mut out = String::new();
    for block in &resp.content {
        if let ContentBlock::Text { text } = block {
            out.push_str(text);
        }
    }
    out
}

/// If the model ignored the "no frontmatter" instruction and
/// emitted its own `---` block at the top, strip it so we don't
/// produce a document with two competing frontmatter blocks.
fn strip_accidental_frontmatter(body: &str) -> String {
    let trimmed = body.trim_start();
    if !trimmed.starts_with("---") {
        return body.to_string();
    }
    // Look for the closing `---` after the opening one.
    let after_open = &trimmed[3..];
    let line_break = match after_open.find('\n') {
        Some(i) => i + 1,
        None => return body.to_string(),
    };
    let body_start = &after_open[line_break..];
    if let Some(close) = body_start.find("\n---") {
        let after_close = &body_start[close + 4..];
        // Skip the trailing newline after `---` if present.
        let after_close = after_close.strip_prefix('\n').unwrap_or(after_close);
        return after_close.trim_start().to_string();
    }
    body.to_string()
}

/// YAML-quote a scalar that may contain colons, quotes, or
/// newlines. Single-line values use double-quoting with `\"` /
/// `\\` escapes; multi-line values are folded into a single line
/// (skill metadata isn't expected to be multi-paragraph).
fn yaml_escape(s: &str) -> String {
    let single_line = s.replace('\n', " ").replace('\r', " ");
    let needs_quote = single_line
        .chars()
        .any(|c| matches!(c, ':' | '#' | '"' | '\'' | '\\' | '\t'))
        || single_line.starts_with(['-', '?', '|', '>', '!', '%', '@', '`', '['])
        || single_line.trim() != single_line;
    if !needs_quote && !single_line.is_empty() {
        single_line
    } else {
        let escaped = single_line.replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{escaped}\"")
    }
}

fn format_llm_error(e: &LlmError) -> String {
    match e {
        LlmError::Auth => "auth: invalid or missing credential".into(),
        LlmError::RateLimited { retry_after_ms } => {
            format!("rate-limited (retry after {retry_after_ms}ms)")
        }
        LlmError::Provider { status, message } => {
            format!("provider {status}: {message}")
        }
        LlmError::Stream(msg) => format!("stream: {msg}"),
        other => format!("{other}"),
    }
}

#[cfg(test)]
mod tests {
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
}
