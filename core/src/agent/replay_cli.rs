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
    let db = MemoryDb::open_default()
        .map_err(|e| format!("memory db unavailable: {e}"))?;
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
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(strs: &[&str]) -> Vec<String> {
        strs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_args_requires_session_id_or_last() {
        let err = parse_args(&args(&[])).unwrap_err();
        assert!(err.contains("missing session id"), "got {err}");
    }

    #[test]
    fn parse_args_accepts_positional_session_id() {
        let o = parse_args(&args(&["sess-1"])).unwrap();
        assert_eq!(o.session_id.as_deref(), Some("sess-1"));
        assert!(!o.use_last);
        assert_eq!(o.limit, 1000);
    }

    #[test]
    fn parse_args_accepts_last_flag() {
        let o = parse_args(&args(&["--last"])).unwrap();
        assert!(o.use_last);
        assert_eq!(o.session_id, None);
    }

    #[test]
    fn parse_args_accepts_session_flag() {
        let o = parse_args(&args(&["--session", "sess-2"])).unwrap();
        assert_eq!(o.session_id.as_deref(), Some("sess-2"));
    }

    #[test]
    fn parse_args_limit_validates_positive_int() {
        assert!(parse_args(&args(&["s", "--limit", "0"]))
            .unwrap_err()
            .contains("> 0"));
        assert!(parse_args(&args(&["s", "--limit", "abc"]))
            .unwrap_err()
            .contains("positive integer"));
        let o = parse_args(&args(&["s", "--limit", "50"])).unwrap();
        assert_eq!(o.limit, 50);
    }

    #[test]
    fn parse_args_role_normalises_case_and_validates() {
        let o = parse_args(&args(&["s", "--role", "USER"])).unwrap();
        assert_eq!(o.role_filter.as_deref(), Some("user"));
        let err = parse_args(&args(&["s", "--role", "robot"])).unwrap_err();
        assert!(err.contains("invalid --role"), "got {err}");
    }

    #[test]
    fn parse_args_rejects_unknown_flag() {
        let err = parse_args(&args(&["s", "--bogus"])).unwrap_err();
        assert!(err.contains("unknown flag"), "got {err}");
    }

    #[test]
    fn replay_with_empty_session_returns_zero_messages() {
        let db = MemoryDb::open_in_memory().unwrap();
        let opts = ReplayOpts {
            session_id: Some("ghost".into()),
            limit: 10,
            ..Default::default()
        };
        let v = replay_with(&db, &opts).unwrap();
        assert_eq!(v["session_id"], json!("ghost"));
        assert_eq!(v["message_count"], json!(0));
        assert_eq!(v["title"], json!(null));
    }

    #[test]
    fn replay_with_returns_messages_chronologically() {
        let db = MemoryDb::open_in_memory().unwrap();
        db.record_message("s1", "user", "first").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        db.record_message("s1", "assistant", "second").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        db.record_message("s1", "user", "third").unwrap();
        let opts = ReplayOpts {
            session_id: Some("s1".into()),
            limit: 100,
            ..Default::default()
        };
        let v = replay_with(&db, &opts).unwrap();
        assert_eq!(v["message_count"], json!(3));
        let msgs = v["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["content"], json!("first"));
        assert_eq!(msgs[1]["content"], json!("second"));
        assert_eq!(msgs[2]["content"], json!("third"));
    }

    #[test]
    fn replay_with_role_filter_keeps_only_matching() {
        let db = MemoryDb::open_in_memory().unwrap();
        db.record_message("s1", "user", "u1").unwrap();
        db.record_message("s1", "assistant", "a1").unwrap();
        db.record_message("s1", "user", "u2").unwrap();
        let opts = ReplayOpts {
            session_id: Some("s1".into()),
            limit: 100,
            role_filter: Some("user".into()),
            ..Default::default()
        };
        let v = replay_with(&db, &opts).unwrap();
        assert_eq!(v["message_count"], json!(2));
        let msgs = v["messages"].as_array().unwrap();
        for m in msgs {
            assert_eq!(m["role"], json!("user"));
        }
    }

    #[test]
    fn replay_with_limit_caps_results() {
        let db = MemoryDb::open_in_memory().unwrap();
        for i in 0..5 {
            db.record_message("s1", "user", &format!("m{i}")).unwrap();
        }
        let opts = ReplayOpts {
            session_id: Some("s1".into()),
            limit: 2,
            ..Default::default()
        };
        let v = replay_with(&db, &opts).unwrap();
        assert_eq!(v["message_count"], json!(2));
    }

    #[test]
    fn replay_with_last_picks_most_recent_session() {
        let db = MemoryDb::open_in_memory().unwrap();
        db.record_message("old-sess", "user", "ancient").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        db.record_message("new-sess", "user", "recent").unwrap();
        let opts = ReplayOpts {
            use_last: true,
            limit: 10,
            ..Default::default()
        };
        let v = replay_with(&db, &opts).unwrap();
        assert_eq!(v["session_id"], json!("new-sess"));
        assert_eq!(v["message_count"], json!(1));
    }

    #[test]
    fn replay_with_last_errors_on_empty_db() {
        let db = MemoryDb::open_in_memory().unwrap();
        let opts = ReplayOpts {
            use_last: true,
            limit: 10,
            ..Default::default()
        };
        let err = replay_with(&db, &opts).unwrap_err();
        assert!(err.contains("no sessions"), "got {err}");
    }

    #[test]
    fn replay_with_includes_title_when_set() {
        let db = MemoryDb::open_in_memory().unwrap();
        db.record_message("s1", "user", "hello").unwrap();
        db.set_title("s1", "Greeting Session").unwrap();
        let opts = ReplayOpts {
            session_id: Some("s1".into()),
            limit: 10,
            ..Default::default()
        };
        let v = replay_with(&db, &opts).unwrap();
        assert_eq!(v["title"], json!("Greeting Session"));
    }
}
