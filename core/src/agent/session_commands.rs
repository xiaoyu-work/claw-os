use super::memory;
use serde_json::{json, Value};

/// `cos agent recall <query> [limit]` — FTS5 search across all
/// recorded conversation messages. Returns ranked hits (best first).
pub(super) fn recall_cmd(args: &[String]) -> Result<Value, String> {
    let query = args.first().cloned().unwrap_or_default();
    if query.is_empty() {
        return Err("usage: cos agent recall \"<query>\" [limit]".into());
    }
    let limit: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(10);
    let db = memory::sqlite_fts::MemoryDb::open_default()
        .map_err(|e| format!("memory db unavailable: {e}"))?;
    let hits = db
        .search(&query, limit)
        .map_err(|e| format!("search failed: {e}"))?;
    let rendered: Vec<Value> = hits
        .iter()
        .map(|h| {
            let content = memory::history::sanitize_stored_content(&h.row.role, &h.row.content);
            json!({
                "id": h.row.id,
                "session_id": h.row.session_id,
                "role": h.row.role,
                "content": content,
                "ts_ms": h.row.ts_ms,
                "rank": h.rank,
            })
        })
        .collect();
    Ok(json!({
        "query": query,
        "limit": limit,
        "n": rendered.len(),
        "hits": rendered,
    }))
}

/// `cos agent sessions [limit]` — recent conversation sessions
/// ordered by most-recent activity.
pub(super) fn sessions_cmd(args: &[String]) -> Result<Value, String> {
    // `cos agent sessions [N]` keeps working as the list shortcut
    // when N parses as a number. Otherwise the first arg is treated
    // as a verb: list / title / set-title / count / clear.
    let first = args.first().map(|s| s.as_str()).unwrap_or("list");
    if first.parse::<usize>().is_ok() {
        return sessions_list(args);
    }
    match first {
        "list" | "" => sessions_list(&args[1..]),
        "title" => sessions_title(&args[1..]),
        "set-title" => sessions_set_title(&args[1..]),
        "count" => sessions_count(&args[1..]),
        "clear" => sessions_clear(&args[1..]),
        "purge" => sessions_purge(&args[1..]),
        "stats" => sessions_stats(&args[1..]),
        "top" => sessions_top(&args[1..]),
        other => Err(format!(
            "unknown sessions subcommand: {other}. try: list [N] | top [N] | title <id> | set-title <id> \"<title>\" | count [<id>] | clear <id> --yes | purge --older-than <days> [--dry-run] [--yes] | stats"
        )),
    }
}

fn sessions_list(args: &[String]) -> Result<Value, String> {
    let limit: usize = args.first().and_then(|s| s.parse().ok()).unwrap_or(20);
    let db = memory::sqlite_fts::MemoryDb::open_default()
        .map_err(|e| format!("memory db unavailable: {e}"))?;
    sessions_list_with(&db, limit)
}

fn sessions_list_with(db: &memory::sqlite_fts::MemoryDb, limit: usize) -> Result<Value, String> {
    let sessions = db
        .sessions(limit)
        .map_err(|e| format!("sessions query failed: {e}"))?;
    let rendered: Vec<Value> = sessions
        .iter()
        .map(|s| {
            json!({
                "session_id": s.session_id,
                "last_ts_ms": s.last_ts_ms,
                "message_count": s.message_count,
                "title": s.title,
            })
        })
        .collect();
    Ok(json!({
        "limit": limit,
        "n": rendered.len(),
        "sessions": rendered,
    }))
}

/// `cos agent sessions top [N]` — like `sessions list` but ordered
/// by message count desc (with last-activity ts as tiebreaker).
/// Designed to point at exactly the sessions worth `sessions clear
/// <id> --yes`-ing when memory.db is fat.
fn sessions_top(args: &[String]) -> Result<Value, String> {
    let limit: usize = args.first().and_then(|s| s.parse().ok()).unwrap_or(20);
    let db = memory::sqlite_fts::MemoryDb::open_default()
        .map_err(|e| format!("memory db unavailable: {e}"))?;
    sessions_top_with(&db, limit)
}

fn sessions_top_with(db: &memory::sqlite_fts::MemoryDb, limit: usize) -> Result<Value, String> {
    let sessions = db
        .sessions_top(limit)
        .map_err(|e| format!("sessions_top query failed: {e}"))?;
    let rendered: Vec<Value> = sessions
        .iter()
        .map(|s| {
            json!({
                "session_id": s.session_id,
                "last_ts_ms": s.last_ts_ms,
                "message_count": s.message_count,
                "title": s.title,
            })
        })
        .collect();
    Ok(json!({
        "ok": true,
        "limit": limit,
        "n": rendered.len(),
        "ordered_by": "message_count_desc",
        "sessions": rendered,
    }))
}

fn sessions_title(args: &[String]) -> Result<Value, String> {
    let id = args
        .first()
        .cloned()
        .filter(|s| !s.is_empty() && !s.starts_with("--"))
        .ok_or_else(|| "usage: cos agent sessions title <session_id>".to_string())?;
    let db = memory::sqlite_fts::MemoryDb::open_default()
        .map_err(|e| format!("memory db unavailable: {e}"))?;
    sessions_title_with(&db, &id)
}

fn sessions_title_with(db: &memory::sqlite_fts::MemoryDb, id: &str) -> Result<Value, String> {
    let title = db
        .title_for(id)
        .map_err(|e| format!("title lookup failed: {e}"))?;
    Ok(json!({
        "session_id": id,
        "title": title,
        "set": title.is_some(),
    }))
}

fn sessions_set_title(args: &[String]) -> Result<Value, String> {
    let (id, title) = parse_set_title_args(args)?;
    let db = memory::sqlite_fts::MemoryDb::open_default()
        .map_err(|e| format!("memory db unavailable: {e}"))?;
    sessions_set_title_with(&db, &id, &title)
}

fn parse_set_title_args(args: &[String]) -> Result<(String, String), String> {
    let id = args
        .first()
        .cloned()
        .filter(|s| !s.is_empty() && !s.starts_with("--"))
        .ok_or_else(|| {
            "usage: cos agent sessions set-title <session_id> \"<title>\"".to_string()
        })?;
    let title_parts: Vec<String> = args
        .iter()
        .skip(1)
        .take_while(|s| !s.starts_with("--"))
        .cloned()
        .collect();
    if title_parts.is_empty() {
        return Err("usage: cos agent sessions set-title <session_id> \"<title>\"".into());
    }
    let title = title_parts.join(" ").trim().to_string();
    if title.is_empty() {
        return Err("title cannot be empty".into());
    }
    Ok((id, title))
}

fn sessions_set_title_with(
    db: &memory::sqlite_fts::MemoryDb,
    id: &str,
    title: &str,
) -> Result<Value, String> {
    db.set_title(id, title)
        .map_err(|e| format!("set-title failed: {e}"))?;
    Ok(json!({
        "session_id": id,
        "title": title,
        "ok": true,
    }))
}

fn sessions_count(args: &[String]) -> Result<Value, String> {
    let id = args
        .first()
        .cloned()
        .filter(|s| !s.is_empty() && !s.starts_with("--"));
    let db = memory::sqlite_fts::MemoryDb::open_default()
        .map_err(|e| format!("memory db unavailable: {e}"))?;
    sessions_count_with(&db, id.as_deref())
}

fn sessions_count_with(
    db: &memory::sqlite_fts::MemoryDb,
    id: Option<&str>,
) -> Result<Value, String> {
    match id {
        Some(sid) => {
            let n = db
                .count_session(sid)
                .map_err(|e| format!("count failed: {e}"))?;
            Ok(json!({
                "session_id": sid,
                "messages": n,
            }))
        }
        None => {
            let n = db.count_total().map_err(|e| format!("count failed: {e}"))?;
            Ok(json!({
                "total_messages": n,
            }))
        }
    }
}

fn sessions_clear(args: &[String]) -> Result<Value, String> {
    let id = args
        .first()
        .cloned()
        .filter(|s| !s.is_empty() && !s.starts_with("--"))
        .ok_or_else(|| "usage: cos agent sessions clear <session_id> --yes".to_string())?;
    if !args.iter().any(|a| a == "--yes") {
        return Err(format!(
            "refusing to clear session {id} without --yes (would drop all recorded messages for this session)"
        ));
    }
    let db = memory::sqlite_fts::MemoryDb::open_default()
        .map_err(|e| format!("memory db unavailable: {e}"))?;
    sessions_clear_with(&db, &id)
}

fn sessions_clear_with(db: &memory::sqlite_fts::MemoryDb, id: &str) -> Result<Value, String> {
    let n = db
        .clear_session(id)
        .map_err(|e| format!("clear failed: {e}"))?;
    Ok(json!({
        "session_id": id,
        "messages_cleared": n,
        "ok": true,
    }))
}

/// `cos agent sessions purge --older-than <days> [--dry-run] [--yes]`
/// — bulk-delete every message older than the threshold. Implements
/// the convention from `sessions clear`: destructive operations
/// require an explicit `--yes`, with `--dry-run` reporting the
/// counts without mutating anything.
fn sessions_purge(args: &[String]) -> Result<Value, String> {
    let mut older_than_days: Option<u64> = None;
    let mut dry_run = false;
    let mut yes = false;
    let mut iter = args.iter();
    while let Some(a) = iter.next() {
        match a.as_str() {
            "--older-than" => {
                let raw = iter
                    .next()
                    .ok_or_else(|| "--older-than needs <days>".to_string())?;
                let days = raw.parse::<u64>().map_err(|_| {
                    format!("--older-than must be a positive integer (got '{raw}')")
                })?;
                if days == 0 {
                    return Err("--older-than must be > 0".into());
                }
                older_than_days = Some(days);
            }
            "--dry-run" => dry_run = true,
            "--yes" => yes = true,
            other => {
                return Err(format!(
                    "unknown purge arg: {other}. try: --older-than <days> | --dry-run | --yes"
                ));
            }
        }
    }
    let days = older_than_days.ok_or_else(|| {
        "missing --older-than <days>. usage: cos agent sessions purge --older-than <days> [--dry-run] [--yes]"
            .to_string()
    })?;
    if !dry_run && !yes {
        return Err(format!(
            "refusing to purge messages older than {days}d without --yes (would delete rows). \
            preview with --dry-run, then re-run with --yes to commit"
        ));
    }
    let now_ms: i64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let cutoff_ms = now_ms.saturating_sub((days as i64).saturating_mul(86_400_000));
    let db = memory::sqlite_fts::MemoryDb::open_default()
        .map_err(|e| format!("memory db unavailable: {e}"))?;
    sessions_purge_with(&db, cutoff_ms, days, dry_run)
}

fn sessions_purge_with(
    db: &memory::sqlite_fts::MemoryDb,
    cutoff_ts_ms: i64,
    older_than_days: u64,
    dry_run: bool,
) -> Result<Value, String> {
    let stats = if dry_run {
        db.count_older_than_ms(cutoff_ts_ms)
            .map_err(|e| format!("count failed: {e}"))?
    } else {
        db.purge_older_than_ms(cutoff_ts_ms)
            .map_err(|e| format!("purge failed: {e}"))?
    };
    Ok(json!({
        "ok": true,
        "dry_run": dry_run,
        "older_than_days": older_than_days,
        "cutoff_ts_ms": cutoff_ts_ms,
        "messages_deleted": stats.messages_deleted,
        "sessions_emptied": stats.sessions_emptied,
        "titles_deleted": stats.titles_deleted,
    }))
}

/// `cos agent sessions stats [--session <id>]` — read-only aggregate
/// over the memory.db (pairs naturally with `sessions purge` so users
/// can see what a given `--older-than <days>` would actually delete).
/// With `--session <id>` the result is scoped to one conversation.
fn sessions_stats(args: &[String]) -> Result<Value, String> {
    // Optional --session <id> selects a per-session subset of stats.
    let mut session_filter: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--session" => {
                let v = args.get(i + 1).ok_or_else(|| {
                    "sessions stats --session requires an id argument".to_string()
                })?;
                if v.is_empty() {
                    return Err("sessions stats --session must not be empty".to_string());
                }
                session_filter = Some(v.clone());
                i += 2;
            }
            other => {
                return Err(format!(
                    "sessions stats: unexpected argument '{other}'. usage: cos agent sessions stats [--session <id>]"
                ));
            }
        }
    }
    let db = memory::sqlite_fts::MemoryDb::open_default()
        .map_err(|e| format!("memory db unavailable: {e}"))?;
    let now_ms: i64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    match session_filter {
        Some(sid) => sessions_stats_session_with(&db, &sid, now_ms),
        None => sessions_stats_with(&db, now_ms),
    }
}

fn sessions_stats_with(db: &memory::sqlite_fts::MemoryDb, now_ms: i64) -> Result<Value, String> {
    let stats = db.stats(now_ms).map_err(|e| format!("stats failed: {e}"))?;
    let by_role = stats
        .by_role
        .iter()
        .map(|(r, n)| json!({"role": r, "count": *n as u64}))
        .collect::<Vec<_>>();
    Ok(json!({
        "ok": true,
        "scope": "global",
        "now_ms": now_ms,
        "total_messages": stats.total_messages as u64,
        "total_sessions": stats.total_sessions as u64,
        "titled_sessions": stats.titled_sessions as u64,
        "messages_last_1d": stats.messages_last_1d as u64,
        "messages_last_7d": stats.messages_last_7d as u64,
        "messages_last_30d": stats.messages_last_30d as u64,
        "by_role": by_role,
        "oldest_ts_ms": stats.oldest_ts_ms,
        "newest_ts_ms": stats.newest_ts_ms,
    }))
}

fn sessions_stats_session_with(
    db: &memory::sqlite_fts::MemoryDb,
    session_id: &str,
    now_ms: i64,
) -> Result<Value, String> {
    let stats = db
        .stats_for_session(session_id, now_ms)
        .map_err(|e| format!("stats failed: {e}"))?;
    let by_role = stats
        .by_role
        .iter()
        .map(|(r, n)| json!({"role": r, "count": *n as u64}))
        .collect::<Vec<_>>();
    Ok(json!({
        "ok": true,
        "scope": "session",
        "session_id": stats.session_id,
        "title": stats.title,
        "now_ms": now_ms,
        "total_messages": stats.total_messages as u64,
        "messages_last_1d": stats.messages_last_1d as u64,
        "messages_last_7d": stats.messages_last_7d as u64,
        "messages_last_30d": stats.messages_last_30d as u64,
        "by_role": by_role,
        "oldest_ts_ms": stats.oldest_ts_ms,
        "newest_ts_ms": stats.newest_ts_ms,
    }))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/session_commands.rs"
    ));
}
