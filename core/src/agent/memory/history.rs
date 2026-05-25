//! Parse persisted memory rows back into structured form for chat
//! surfaces that need to render historical tool calls / tool results
//! as discrete UI elements instead of opaque `[tool_use:NAME] {…}` /
//! `[tool_result] …` markers.
//!
//! `render_message_content` (memory/sqlite_fts.rs) serialises a
//! `Message`'s `ContentBlock` vector into a single text payload per
//! DB row. This module reverses that encoding into a [`ParsedRow`]
//! holding the conversational text plus per-row tool call / result
//! views, suitable for direct JSON serialisation to any frontend.
//!
//! The web UI (`agent/web/routes/sessions.rs::history`) and clawd's
//! `memory.history` command both call into here so the desktop
//! `cos-agent-ui` and the React-free web client see identical history.

use serde::Serialize;
use serde_json::{json, Value};

use super::sqlite_fts::{MemoryDb, MemoryError};

/// Parsed view of a single stored memory row.
#[derive(Debug, Default, Serialize)]
pub struct ParsedRow {
    /// Plain conversational text with all `[tool_*]` markers stripped.
    pub text: String,
    /// `[tool_use:NAME] {input_json}` markers, in declaration order.
    pub tool_calls: Vec<Value>,
    /// `[tool_result]` / `[tool_result:error]` blocks, in declaration order.
    pub tool_results: Vec<Value>,
}

/// One fully decoded memory row ready for JSON serialisation.
#[derive(Debug, Serialize)]
pub struct HistoryMessage {
    pub role: String,
    pub content: String,
    pub text: String,
    pub tool_calls: Vec<Value>,
    pub tool_results: Vec<Value>,
    pub ts_ms: i64,
}

/// Load the most recent `limit` rows for `session_id` from `db` and
/// decode each via [`parse_stored_content`].
pub fn load_history(
    db: &MemoryDb,
    session_id: &str,
    limit: usize,
) -> Result<Vec<HistoryMessage>, MemoryError> {
    let rows = db.recent(session_id, limit)?;
    Ok(rows
        .into_iter()
        .map(|r| {
            let parsed = parse_stored_content(&r.role, &r.content);
            HistoryMessage {
                role: r.role,
                content: r.content,
                text: parsed.text,
                tool_calls: parsed.tool_calls,
                tool_results: parsed.tool_results,
                ts_ms: r.ts_ms,
            }
        })
        .collect())
}

/// Parse a stored memory row back into structured text + tool calls +
/// tool results. `render_message_content` (memory/sqlite_fts.rs) emits
/// tool uses as `[tool_use:NAME] {compact_json}` lines and tool results
/// as a row containing `[tool_result] <body>` (or
/// `[tool_result:error] <body>`). Tool-result *bodies* can wrap across
/// multiple lines, so each marker captures everything up to the next
/// marker line.
///
/// Note the marker recognition runs regardless of role: Anthropic puts
/// ToolResult blocks inside `role="user"` messages, so we'd otherwise
/// see the raw `[tool_result] ...` text on the user side of the chat.
pub fn parse_stored_content(_role: &str, content: &str) -> ParsedRow {
    let mut out = ParsedRow::default();
    let mut text_buf: Vec<&str> = Vec::new();
    let mut active_result: Option<(bool, String)> = None;

    let flush_result = |active: &mut Option<(bool, String)>, out: &mut ParsedRow| {
        if let Some((is_error, body)) = active.take() {
            out.tool_results.push(json!({
                "text": body,
                "is_error": is_error,
            }));
        }
    };

    for line in content.lines() {
        let trimmed = line.trim_start();

        if let Some(rest) = trimmed.strip_prefix("[tool_use:") {
            if let Some(end) = rest.find(']') {
                let name = rest[..end].trim().to_string();
                if !name.is_empty() {
                    flush_result(&mut active_result, &mut out);
                    let input_raw = rest[end + 1..].trim_start();
                    let input: Value = serde_json::from_str(input_raw)
                        .unwrap_or_else(|_| Value::String(input_raw.to_string()));
                    out.tool_calls.push(json!({
                        "name": name,
                        "input": input,
                    }));
                    continue;
                }
            }
        }

        if let Some(rest) = trimmed.strip_prefix("[tool_result:error]") {
            flush_result(&mut active_result, &mut out);
            active_result = Some((true, rest.trim_start_matches([' ', '\t']).to_string()));
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("[tool_result]") {
            flush_result(&mut active_result, &mut out);
            active_result = Some((false, rest.trim_start_matches([' ', '\t']).to_string()));
            continue;
        }

        if let Some((_, buf)) = active_result.as_mut() {
            buf.push('\n');
            buf.push_str(line);
        } else {
            text_buf.push(line);
        }
    }

    flush_result(&mut active_result, &mut out);

    while matches!(text_buf.last(), Some(l) if l.trim().is_empty()) {
        text_buf.pop();
    }
    out.text = text_buf.join("\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assistant_row_splits_text_and_tool_use() {
        let body = "Let me check.\n[tool_use:cos_sysinfo] {\"command\":\"largest_files\",\"args\":[\"/\"]}";
        let p = parse_stored_content("assistant", body);
        assert_eq!(p.text, "Let me check.");
        assert_eq!(p.tool_calls.len(), 1);
        assert_eq!(p.tool_calls[0]["name"], "cos_sysinfo");
        assert_eq!(p.tool_calls[0]["input"]["command"], "largest_files");
    }

    #[test]
    fn assistant_row_with_only_tool_use_has_empty_text() {
        let body = "[tool_use:cos_sysinfo] {\"command\":\"info\"}";
        let p = parse_stored_content("assistant", body);
        assert!(p.text.is_empty());
        assert_eq!(p.tool_calls.len(), 1);
    }

    #[test]
    fn user_row_tool_result_is_extracted() {
        let body = "[tool_result] {\"files\":[]}";
        let p = parse_stored_content("user", body);
        assert!(p.text.is_empty());
        assert_eq!(p.tool_results.len(), 1);
        assert_eq!(p.tool_results[0]["text"], "{\"files\":[]}");
        assert_eq!(p.tool_results[0]["is_error"], false);
    }

    #[test]
    fn tool_result_error_marker_sets_is_error() {
        let body = "[tool_result:error] EACCES";
        let p = parse_stored_content("user", body);
        assert_eq!(p.tool_results[0]["is_error"], true);
        assert_eq!(p.tool_results[0]["text"], "EACCES");
    }

    #[test]
    fn multiline_tool_result_body_is_captured() {
        let body = "[tool_result] {\n  \"a\": 1,\n  \"b\": 2\n}";
        let p = parse_stored_content("user", body);
        assert_eq!(p.tool_results.len(), 1);
        let txt = p.tool_results[0]["text"].as_str().unwrap();
        assert!(txt.contains("\"a\": 1"));
        assert!(txt.contains("\"b\": 2"));
    }

    #[test]
    fn malformed_tool_use_stays_in_text() {
        let body = "Plain prose\n[tool_use:unterminated";
        let p = parse_stored_content("assistant", body);
        assert!(p.tool_calls.is_empty());
        assert!(p.text.contains("[tool_use:unterminated"));
    }

    #[test]
    fn empty_body_yields_empty_parsed_row() {
        let body = "";
        let p = parse_stored_content("user", body);
        assert!(p.text.is_empty());
        assert!(p.tool_calls.is_empty());
        assert!(p.tool_results.is_empty());
    }
}
