//! `GET /api/sessions` and friends — memory DB session inspection.

use axum::extract::Path;
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

use crate::agent::memory::sqlite_fts::MemoryDb;

pub async fn list() -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let db = MemoryDb::open_default()
        .map_err(|e| internal(format!("open memory: {e}")))?;
    let rows = db
        .sessions(200)
        .map_err(|e| internal(format!("read sessions: {e}")))?;

    let mut sessions = Vec::with_capacity(rows.len());
    for s in rows {
        sessions.push(json!({
            "id": s.session_id,
            "title": s.title,
            "last_ts_ms": s.last_ts_ms,
            "message_count": s.message_count,
        }));
    }
    Ok(Json(json!({ "n": sessions.len(), "sessions": sessions })))
}

pub async fn detail(
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let db = MemoryDb::open_default()
        .map_err(|e| internal(format!("open memory: {e}")))?;
    let title = db
        .title_for(&id)
        .map_err(|e| internal(format!("title: {e}")))?;
    Ok(Json(json!({
        "id": id,
        "title": title,
    })))
}

pub async fn history(
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let db = MemoryDb::open_default()
        .map_err(|e| internal(format!("open memory: {e}")))?;
    let rows = db
        .recent(&id, 500)
        .map_err(|e| internal(format!("read history: {e}")))?;

    // The memory layer serialises a Message's content blocks into a
    // single text payload (see `render_message_content` in
    // sqlite_fts.rs) — tool calls become `[tool_use:NAME] {json}` lines
    // and tool results become `[tool_result] <body>` rows. Replay the
    // parse here so the web client can render history with the same
    // ToolCard widgets the live SSE stream uses, instead of dumping
    // raw markers into the assistant bubble.
    let mut messages = Vec::with_capacity(rows.len());
    for r in rows {
        let parsed = parse_stored_content(&r.role, &r.content);
        messages.push(json!({
            "role": r.role,
            "content": r.content,
            "text": parsed.text,
            "tool_calls": parsed.tool_calls,
            "tool_results": parsed.tool_results,
            "ts_ms": r.ts_ms,
        }));
    }
    Ok(Json(json!({
        "session_id": id,
        "n": messages.len(),
        "messages": messages,
    })))
}

#[derive(Default)]
struct ParsedRow {
    text: String,
    tool_calls: Vec<Value>,
    tool_results: Vec<Value>,
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
fn parse_stored_content(_role: &str, content: &str) -> ParsedRow {
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

fn internal(msg: String) -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": msg })),
    )
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
    fn tool_row_strips_result_marker() {
        let body = "[tool_result] {\"files\":[]}";
        // Tool results are stored under role="user" by the runtime (Anthropic
        // convention) — make sure we still detect the marker.
        let p = parse_stored_content("user", body);
        assert!(p.text.is_empty());
        assert!(p.tool_calls.is_empty());
        assert_eq!(p.tool_results.len(), 1);
        assert_eq!(p.tool_results[0]["text"], "{\"files\":[]}");
        assert_eq!(p.tool_results[0]["is_error"], false);
    }

    #[test]
    fn tool_row_marks_errors() {
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
    fn user_row_passes_through_as_text() {
        let body = "what's the largest file?";
        let p = parse_stored_content("user", body);
        assert_eq!(p.text, body);
        assert!(p.tool_calls.is_empty());
    }
}

