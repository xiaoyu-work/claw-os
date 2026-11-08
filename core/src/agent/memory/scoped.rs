//! Read-only model recall over a Root-selected MemoryDb projection.

use super::sqlite_fts::{MemoryDb, MessageRow};
use crate::workspaces::gateway::RecallInput;
use serde_json::{json, Value};

pub(crate) fn recall(db: &MemoryDb, input: RecallInput) -> Result<Value, String> {
    input.validate()?;
    let preview = |rows: &[MessageRow]| {
        let chars = (4096 / rows.len().max(1)).min(512);
        rows.iter()
            .map(|row| row_value(row, 0, chars, None))
            .collect::<Result<Vec<_>, _>>()
    };
    match input.command.as_str() {
        "search" => {
            let mut hits = db
                .search_history(&input.query, input.session_id.as_deref(), input.limit + 1)
                .map_err(|e| e.to_string())?;
            let has_more = hits.len() > input.limit;
            hits.truncate(input.limit);
            let chars = (4096 / hits.len().max(1)).min(512);
            let hits = hits
                .iter()
                .map(|hit| {
                    let mut value = row_value(&hit.row, 0, chars, None)?;
                    value["rank"] = json!(hit.rank);
                    Ok(value)
                })
                .collect::<Result<Vec<_>, String>>()?;
            Ok(
                json!({"query":input.query,"session_id":input.session_id,"limit":input.limit,
                "has_more":has_more,"match_kind":"fts5","score_kind":"bm25_not_confidence","hits":hits}),
            )
        }
        "recent" => {
            let sid = input
                .session_id
                .as_deref()
                .ok_or("recent requires a session")?;
            let mut rows = db
                .recent_replayable(sid, input.limit + 1)
                .map_err(|e| e.to_string())?;
            let has_more = rows.len() > input.limit;
            if has_more {
                rows.remove(0);
            }
            Ok(
                json!({"session_id":sid,"limit":input.limit,"has_more":has_more,"messages":preview(&rows)?}),
            )
        }
        "show" => {
            let row = db
                .replayable_message(input.message_id.ok_or("show requires a message")?)
                .map_err(|e| e.to_string())?
                .filter(|row| {
                    row.role != super::sqlite_fts::INJECTED_ROLE
                        && input
                            .session_id
                            .as_ref()
                            .is_none_or(|id| row.session_id == *id)
                })
                .ok_or("source message not found in this context domain")?;
            Ok(
                json!({"message":row_value(&row,input.offset,input.max_chars,input.revision.as_deref())?}),
            )
        }
        "sessions" => {
            let mut sessions = db.sessions(input.limit + 1).map_err(|e| e.to_string())?;
            let has_more = sessions.len() > input.limit;
            sessions.truncate(input.limit);
            Ok(
                json!({"limit":input.limit,"has_more":has_more,"sessions":sessions.iter().map(|s|
                json!({"session_id":s.session_id,"last_ts_ms":s.last_ts_ms,"message_count":s.message_count})
            ).collect::<Vec<_>>()}),
            )
        }
        "stats" => {
            let total = match input.session_id.as_deref() {
                Some(id) => db.count_session(id),
                None => db.count_total(),
            }
            .map_err(|e| e.to_string())?;
            Ok(json!({"total_messages":total,"session_id":input.session_id,
                "session_messages":input.session_id.as_ref().map(|_|total)}))
        }
        _ => Err("unsupported recall command".into()),
    }
}

fn row_value(
    row: &MessageRow,
    offset: usize,
    chars: usize,
    revision: Option<&str>,
) -> Result<Value, String> {
    let source_revision = row.provenance().digest;
    if revision.is_some_and(|id| id != source_revision) {
        return Err("memory source changed; restart the read".into());
    }
    let text = super::history::sanitize_stored_content(&row.role, &row.content);
    let page = super::history::text_page(&text, offset, chars)?;
    Ok(
        json!({"id":row.id,"session_id":row.session_id,"role":row.role,
        "content":page.content,"revision":source_revision,"ts_ms":row.ts_ms,
        "offset":page.offset,"next_offset":page.next_offset,"total_chars":page.total_chars,
        "source_complete":page.source_complete,"provenance":row.provenance(),
        "read":{"tool":"cos_recall","command":"show","message_id":row.id,
            "session_id":row.session_id,"revision":source_revision,"offset":page.next_offset.unwrap_or(0)}}),
    )
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/memory/scoped.rs"
    ));
}
