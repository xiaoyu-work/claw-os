//! `cos agent replay` — re-render a saved conversation from the
//! SQLite-FTS memory store.
//!
//! Pure read; never mutates state. Useful for:
//!
//! - debugging (what did the agent see / say in turn N?)
//! - archival / handover (export a session as JSONL)
//! - feeding past dialog into other tools (`jq` can pull just user
//!   messages, etc.)
//!
//! ### CLI shape
//!
//! ```text
//! cos agent replay <session_id> [--limit N] [--role R]
//! cos agent replay --last [--limit N] [--role R]
//! ```
//!
//! Flags:
//!
//! - `--last` — pick the most-recently-active session instead of
//!   requiring the operator to copy a UUID.
//! - `--limit N` — cap the number of returned rows (default 1000;
//!   the underlying [`MemoryDb::recent`] returns oldest-first).
//! - Raw rows are never replaced by durable context summaries. Replay exports
//!   those rows plus compaction recovery metadata.
//! - `--role <user|assistant|tool|system>` — filter by role
//!   (post-query, after the limit). Use to extract just the user
//!   prompts or just the model's replies.
//!
//! Output is always JSON for pipe-friendliness; for human reading
//! pipe through `jq -r '.messages[] | "\(.role): \(.content)"'`.

use serde_json::{json, Value};

use crate::agent::memory::sqlite_fts::{MemoryDb, MessageRow};

/// Top-level dispatcher.
pub fn replay_cmd(args: &[String]) -> Result<Value, String> {
    let opts = parse_args(args)?;
    let db = MemoryDb::open_default().map_err(|e| format!("memory db unavailable: {e}"))?;
    replay_with(&db, &opts)
}

#[derive(Debug, Default, Clone)]
struct ReplayOpts {
    session_id: Option<String>,
    use_last: bool,
    limit: usize,
    role_filter: Option<String>,
}

fn parse_args(args: &[String]) -> Result<ReplayOpts, String> {
    let mut opts = ReplayOpts {
        limit: 1000,
        ..Default::default()
    };
    let mut iter = args.iter().peekable();
    while let Some(a) = iter.next() {
        match a.as_str() {
            "--last" => opts.use_last = true,
            "--limit" => {
                let n = iter
                    .next()
                    .ok_or_else(|| "--limit needs <N>".to_string())?
                    .parse::<usize>()
                    .map_err(|_| "--limit must be a positive integer".to_string())?;
                if n == 0 {
                    return Err("--limit must be > 0".into());
                }
                opts.limit = n;
            }
            "--role" => {
                let r = iter
                    .next()
                    .ok_or_else(|| "--role needs <user|assistant|tool|system>".to_string())?;
                let lower = r.to_ascii_lowercase();
                match lower.as_str() {
                    "user" | "assistant" | "tool" | "system" => {
                        opts.role_filter = Some(lower);
                    }
                    other => {
                        return Err(format!(
                            "invalid --role: {other} (expected user|assistant|tool|system)"
                        ));
                    }
                }
            }
            "--session" => {
                let id = iter
                    .next()
                    .ok_or_else(|| "--session needs <id>".to_string())?;
                opts.session_id = Some(id.clone());
            }
            other if other.starts_with("--") => {
                return Err(format!(
                    "unknown flag: {other}. try: --last | --limit <N> | --role <r> | --session <id>"
                ));
            }
            // positional session id (only the first one wins)
            other if opts.session_id.is_none() => {
                opts.session_id = Some(other.to_string());
            }
            other => {
                return Err(format!("unexpected positional arg: {other}"));
            }
        }
    }
    if opts.session_id.is_none() && !opts.use_last {
        return Err(
            "missing session id. usage: cos agent replay <session_id> [--limit N] | --last".into(),
        );
    }
    Ok(opts)
}

fn replay_with(db: &MemoryDb, opts: &ReplayOpts) -> Result<Value, String> {
    let session_id = if opts.use_last {
        let sessions = db
            .sessions(1)
            .map_err(|e| format!("sessions query failed: {e}"))?;
        sessions
            .first()
            .map(|s| s.session_id.clone())
            .ok_or_else(|| "no sessions recorded yet".to_string())?
    } else {
        opts.session_id.clone().expect("validated by parse_args")
    };

    let rows: Vec<MessageRow> = db
        .recent(&session_id, opts.limit)
        .map_err(|e| format!("messages query failed: {e}"))?;

    let title = db
        .title_for(&session_id)
        .map_err(|e| format!("title query failed: {e}"))?;
    let compactions = db
        .compactions_for_session(&session_id)
        .map_err(|e| format!("compaction query failed: {e}"))?;

    let filtered: Vec<&MessageRow> = match &opts.role_filter {
        Some(r) => rows.iter().filter(|m| &m.role == r).collect(),
        None => rows.iter().collect(),
    };

    let messages: Vec<Value> = filtered
        .iter()
        .map(|m| {
            json!({
                "id": m.id,
                "role": m.role,
                "content": m.content,
                "ts_ms": m.ts_ms,
            })
        })
        .collect();

    Ok(json!({
        "session_id": session_id,
        "title": title,
        "limit": opts.limit,
        "role_filter": opts.role_filter,
        "message_count": messages.len(),
        "messages": messages,
        "compactions": compactions,
    }))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/replay_cli.rs"
    ));
}
