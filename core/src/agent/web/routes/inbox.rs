//! `GET /api/inbox` — tail of clawd's append-only context-events log.
//!
//! clawd writes one JSONL line per agent context event (run started,
//! approval emitted, system operation succeeded, …) to
//! [`crate::paths::context_events_log_path`]. The web UI tails the
//! last N entries so the user can see at a glance "what has my agent
//! been doing while I wasn't looking?".
//!
//! We deliberately do NOT subscribe live (file watchers, tail -f
//! semantics, websocket push). The inbox auto-refreshes every 5s
//! from the client; the cost is one syscall + parse of the last
//! ~64KB of the file.

use axum::extract::{Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};

use crate::agent::web::state::AppState;

#[derive(Debug, Deserialize, Default)]
pub struct ListQuery {
    #[serde(default)]
    pub limit: Option<usize>,
}

pub async fn list(State(state): State<AppState>, Query(q): Query<ListQuery>) -> Json<Value> {
    let limit = q.limit.unwrap_or(100).min(1000);
    let path = crate::paths::context_events_log_path();

    let entries: Vec<Value> = match std::fs::File::open(&path) {
        Ok(mut f) => {
            // Read the last ~256KB; that's enough for hundreds of
            // events even on a busy day.
            const MAX_TAIL_BYTES: u64 = 256 * 1024;
            let len = f.metadata().map(|m| m.len()).unwrap_or(0);
            let start = len.saturating_sub(MAX_TAIL_BYTES);
            let starts_at_line_boundary = if start == 0 {
                true
            } else {
                let _ = f.seek(SeekFrom::Start(start - 1));
                let mut previous = [0_u8; 1];
                f.read_exact(&mut previous).is_ok() && previous[0] == b'\n'
            };
            let _ = f.seek(SeekFrom::Start(start));
            let mut buf = Vec::with_capacity(MAX_TAIL_BYTES as usize);
            let _ = f.take(MAX_TAIL_BYTES).read_to_end(&mut buf);
            let reader = BufReader::new(&buf[..]);
            let mut parsed: Vec<Value> = Vec::new();
            for (index, line) in reader.lines().map_while(Result::ok).enumerate() {
                if index == 0 && !starts_at_line_boundary {
                    continue;
                }
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
                    if crate::clawd::context_events::event_visible_to(
                        &v,
                        Some(state.inner.owner_uid),
                    ) {
                        parsed.push(v);
                    }
                }
            }
            let skip = parsed.len().saturating_sub(limit);
            parsed.into_iter().skip(skip).collect()
        }
        Err(_) => Vec::new(),
    };

    Json(json!({
        "path": path.display().to_string(),
        "n": entries.len(),
        "events": entries,
    }))
}
