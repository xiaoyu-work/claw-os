use super::memory;
use serde_json::{json, Value};

/// `cos agent learn <subcmd>` — memory curation, distinct from
/// `cos agent curator` (which is the *skill* curator).
///
///   extract --session <id> [--limit N] [--min-confidence F]
///                          [--dry-run]
///       Pull recent messages from <session>, send the transcript
///       to the auxiliary LLM, and append durable user facts to
///       MEMORY.md.
///   status [--session <id>]
///       Show curation log entries (all sessions, or one).
///   clear-log [--session <id> | --all]
///       Forget one session's curation cursor (next run will
///       re-extract from scratch) or wipe the whole log.
///   prompt
///       Print the embedded system prompt used for fact extraction.
///
/// Auxiliary provider/model come from `[agent] auxiliary_*` in
/// config.json — same source as `cos agent aux`.
pub(super) fn learn_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::memory::curator::{
        default_log_path, default_system_prompt, CurationLog, CuratorConfig, MemoryCurator,
    };
    use crate::agent::memory::notes::NotesStore;
    use crate::agent::memory::sqlite_fts::MemoryDb;

    let sub = args.first().map(|s| s.as_str()).unwrap_or("status");
    match sub {
        "extract" => {
            let mut session_id: Option<String> = None;
            let mut limit: Option<usize> = None;
            let mut min_confidence: Option<f32> = None;
            let mut dry_run = false;
            let mut i = 1usize;
            while i < args.len() {
                match args[i].as_str() {
                    "--session" | "--session-id" => {
                        session_id = Some(
                            args.get(i + 1)
                                .cloned()
                                .ok_or_else(|| "--session needs a value".to_string())?,
                        );
                        i += 2;
                    }
                    "--limit" => {
                        let n: usize = args
                            .get(i + 1)
                            .ok_or_else(|| "--limit needs a value".to_string())?
                            .parse()
                            .map_err(|e| format!("--limit not an integer: {e}"))?;
                        limit = Some(n);
                        i += 2;
                    }
                    "--min-confidence" | "--min_confidence" => {
                        let f: f32 = args
                            .get(i + 1)
                            .ok_or_else(|| "--min-confidence needs a value".to_string())?
                            .parse()
                            .map_err(|e| format!("--min-confidence not a float: {e}"))?;
                        if !(0.0..=1.0).contains(&f) {
                            return Err("--min-confidence must be in [0.0, 1.0]".to_string());
                        }
                        min_confidence = Some(f);
                        i += 2;
                    }
                    "--dry-run" | "--dry_run" => {
                        dry_run = true;
                        i += 1;
                    }
                    other => return Err(format!("unknown learn extract flag: {other}")),
                }
            }
            let session_id = session_id.ok_or_else(|| {
                "--session <id> required (use `cos agent sessions list` to find one)".to_string()
            })?;

            let cfg = &crate::config::get().agent;

            // For --dry-run, we don't need the auxiliary client; build
            // a placeholder that won't be invoked.
            let aux = if dry_run {
                // Best-effort build; fall through to placeholder mock if not configured.
                match crate::agent::runtime::loop_::auxiliary_from_cfg(cfg) {
                    Ok(Some(a)) => a,
                    _ => {
                        use crate::agent::llm::auxiliary::{AuxiliaryClient, AuxiliaryConfig};
                        use crate::agent::llm::providers::mock::MockProvider;
                        use crate::agent::llm::Provider;
                        use std::sync::Arc;
                        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(
                            "mock-aux",
                            &crate::config::AgentConfig::default(),
                        ));
                        AuxiliaryClient::new(provider, AuxiliaryConfig::new("mock", "mock-aux"))
                    }
                }
            } else {
                crate::agent::runtime::loop_::auxiliary_from_cfg(cfg)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| {
                        "auxiliary client not configured (set agent.auxiliary_provider + auxiliary_model in config); use --dry-run to preview without an LLM call".to_string()
                    })?
            };

            let notes = NotesStore::system_default();
            let log_path = default_log_path();

            let mut curator_cfg = CuratorConfig::default();
            if let Some(n) = limit {
                curator_cfg.max_messages = n;
            }
            if let Some(f) = min_confidence {
                curator_cfg.min_confidence = f;
            }
            let curator = MemoryCurator::new(aux, notes, log_path).with_config(curator_cfg);

            let db = MemoryDb::open_default().map_err(|e| format!("open memory db: {e}"))?;

            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("tokio runtime: {e}"))?;
            let outcome = runtime
                .block_on(curator.curate_session(&db, &session_id, dry_run))
                .map_err(|e| format!("curate: {e}"))?;

            let proposed_json: Vec<Value> = outcome
                .facts_proposed
                .iter()
                .map(|f| {
                    json!({
                        "category": f.category.as_str(),
                        "text": f.text,
                        "confidence": f.confidence,
                        "entity": f.entity,
                        "attribute": f.attribute,
                        "value": f.value,
                        "key": f.key(),
                    })
                })
                .collect();
            let added_json: Vec<Value> = outcome
                .facts_added
                .iter()
                .map(|f| {
                    json!({
                        "category": f.category.as_str(),
                        "text": f.text,
                        "confidence": f.confidence,
                        "entity": f.entity,
                        "attribute": f.attribute,
                        "value": f.value,
                        "key": f.key(),
                    })
                })
                .collect();

            Ok(json!({
                "ok": true,
                "session_id": outcome.session_id,
                "messages_examined": outcome.messages_examined,
                "last_message_id": outcome.last_message_id,
                "facts_proposed": proposed_json,
                "facts_added": added_json,
                "skipped_no_new_messages": outcome.skipped_no_new_messages,
                "dry_run": dry_run,
            }))
        }
        "status" => {
            let mut session_filter: Option<String> = None;
            let mut i = 1usize;
            while i < args.len() {
                match args[i].as_str() {
                    "--session" | "--session-id" => {
                        session_filter = Some(
                            args.get(i + 1)
                                .cloned()
                                .ok_or_else(|| "--session needs a value".to_string())?,
                        );
                        i += 2;
                    }
                    other => return Err(format!("unknown learn status flag: {other}")),
                }
            }
            let log_path = default_log_path();
            let log = CurationLog::load(&log_path);
            let entries: Vec<Value> = log
                .sessions
                .iter()
                .filter(|(sid, _)| {
                    session_filter
                        .as_deref()
                        .map(|f| f == sid.as_str())
                        .unwrap_or(true)
                })
                .map(|(sid, e)| {
                    json!({
                        "session_id": sid,
                        "last_curated_message_id": e.last_curated_message_id,
                        "last_run_unix_s": e.last_run_unix_s,
                        "facts_added_total": e.facts_added_total,
                    })
                })
                .collect();
            Ok(json!({
                "log_path": log_path.display().to_string(),
                "log_exists": log_path.exists(),
                "session_count": entries.len(),
                "sessions": entries,
            }))
        }
        "clear-log" | "clear_log" => {
            let mut session: Option<String> = None;
            let mut all = false;
            let mut i = 1usize;
            while i < args.len() {
                match args[i].as_str() {
                    "--session" | "--session-id" => {
                        session = Some(
                            args.get(i + 1)
                                .cloned()
                                .ok_or_else(|| "--session needs a value".to_string())?,
                        );
                        i += 2;
                    }
                    "--all" => {
                        all = true;
                        i += 1;
                    }
                    other => return Err(format!("unknown learn clear-log flag: {other}")),
                }
            }
            if !all && session.is_none() {
                return Err("must pass --session <id> or --all".to_string());
            }
            let log_path = default_log_path();
            let mut log = CurationLog::load(&log_path);
            let removed = if all {
                let n = log.sessions.len();
                log.sessions.clear();
                n
            } else {
                let sid = session.expect("session set above");
                if log.sessions.remove(&sid).is_some() {
                    1
                } else {
                    0
                }
            };
            log.save(&log_path).map_err(|e| e.to_string())?;
            Ok(json!({
                "ok": true,
                "removed_entries": removed,
                "log_path": log_path.display().to_string(),
            }))
        }
        "prompt" => Ok(json!({
            "system_prompt": default_system_prompt(),
        })),
        other => Err(format!(
            "unknown learn subcommand: {other}. try: extract | status | clear-log | prompt"
        )),
    }
}

/// `cos agent semantic <subcmd>` — vector-memory operations.
///
///   index <namespace> <key> "<text>" — embed and store
///   search [--namespace NS] [--limit N] "<query>"
///   list   [--namespace NS] [--limit N]
///   count  [--namespace NS]
///   remove <namespace> <key>
///   clear  <namespace>
///   clear-all --yes — wipe every row (use when migrating embed model)
///   status — show DB path / row count / pinned model / configured embedder
///
/// All sub-commands respect `[embed]` from config.json.
pub(super) fn semantic_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::memory::semantic::{SemanticStore, SemanticStoreExt};

    let sub = args.first().map(|s| s.as_str()).ok_or(
        "usage: cos agent semantic <index|search|list|count|remove|clear|clear-all|status> ...",
    )?;
    let rest = &args[1..];

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;

    match sub {
        "status" => {
            let cfg = &crate::config::get().embed;
            let store_res = SemanticStore::open_default();
            let path = crate::paths::agent_semantic_db_path();
            let (configured, count, embedder_model, pinned) = match &store_res {
                Ok(Some(s)) => {
                    let n = s.count(None).unwrap_or(0);
                    let m = s.embedder().map(|e| e.model().to_string());
                    let p = s.pinned_model().ok().flatten();
                    (true, n, m, p)
                }
                Ok(None) => (false, 0, None, None),
                Err(_) => (false, 0, None, None),
            };
            // Surface a hint if the embedder model differs from what is
            // pinned in the corpus — that's the exact "you need to clear
            // before re-indexing" situation.
            let model_drift =
                matches!((&embedder_model, &pinned), (Some(a), Some(b)) if a != b);
            Ok(json!({
                "status": if configured { "ok" } else { "disabled" },
                "path": path.display().to_string(),
                "row_count": count,
                "provider": cfg.provider,
                "model_config": cfg.model,
                "embedder_model": embedder_model,
                "pinned_model": pinned,
                "model_drift": model_drift,
                "base_url": cfg.base_url,
            }))
        }
        "index" => {
            if rest.len() < 3 {
                return Err("usage: cos agent semantic index <namespace> <key> \"<text>\"".into());
            }
            let store = SemanticStore::open_default()
                .map_err(|e| e.to_string())?
                .ok_or("embedding disabled in [embed] config")?;
            let id = rt
                .block_on(store.index(&rest[0], &rest[1], &rest[2..].join(" ")))
                .map_err(|e| e.to_string())?;
            Ok(json!({ "id": id, "namespace": rest[0], "key": rest[1] }))
        }
        "search" => {
            let mut namespace: Option<String> = None;
            let mut limit: usize = 5;
            let mut positional: Vec<String> = Vec::new();
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--namespace" | "--ns" => {
                        namespace = Some(
                            rest.get(i + 1)
                                .cloned()
                                .ok_or("--namespace requires a value")?,
                        );
                        i += 2;
                    }
                    "--limit" => {
                        limit = rest
                            .get(i + 1)
                            .and_then(|s| s.parse().ok())
                            .ok_or("--limit requires a positive integer")?;
                        i += 2;
                    }
                    other if other.starts_with("--") => {
                        return Err(format!("unknown flag: {other}"));
                    }
                    _ => {
                        positional.push(rest[i].clone());
                        i += 1;
                    }
                }
            }
            if positional.is_empty() {
                return Err("usage: cos agent semantic search [--namespace NS] [--limit N] \"<query>\"".into());
            }
            let query = positional.join(" ");
            let store = SemanticStore::open_default()
                .map_err(|e| e.to_string())?
                .ok_or("embedding disabled in [embed] config")?;
            let hits = rt
                .block_on(store.search(namespace.as_deref(), &query, limit))
                .map_err(|e| e.to_string())?;
            let arr: Vec<Value> = hits
                .into_iter()
                .map(|h| {
                    json!({
                        "id": h.id,
                        "namespace": h.namespace,
                        "key": h.key,
                        "text": h.text,
                        "model": h.model,
                        "score": h.score,
                        "ts_ms": h.ts_ms,
                    })
                })
                .collect();
            Ok(json!({ "query": query, "count": arr.len(), "hits": arr }))
        }
        "list" => {
            let mut namespace: Option<String> = None;
            let mut limit: usize = 50;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--namespace" | "--ns" => {
                        namespace = Some(
                            rest.get(i + 1)
                                .cloned()
                                .ok_or("--namespace requires a value")?,
                        );
                        i += 2;
                    }
                    "--limit" => {
                        limit = rest
                            .get(i + 1)
                            .and_then(|s| s.parse().ok())
                            .ok_or("--limit requires a positive integer")?;
                        i += 2;
                    }
                    other => return Err(format!("unknown flag: {other}")),
                }
            }
            let store = SemanticStore::open_default()
                .map_err(|e| e.to_string())?
                .ok_or("embedding disabled in [embed] config")?;
            let rows = store
                .list(namespace.as_deref(), limit)
                .map_err(|e| e.to_string())?;
            let arr: Vec<Value> = rows
                .into_iter()
                .map(|r| {
                    json!({
                        "id": r.id,
                        "namespace": r.namespace,
                        "key": r.key,
                        "text": r.text,
                        "model": r.model,
                        "dim": r.dim,
                        "ts_ms": r.ts_ms,
                    })
                })
                .collect();
            Ok(json!({ "count": arr.len(), "rows": arr }))
        }
        "count" => {
            let mut namespace: Option<String> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--namespace" | "--ns" => {
                        namespace = Some(
                            rest.get(i + 1)
                                .cloned()
                                .ok_or("--namespace requires a value")?,
                        );
                        i += 2;
                    }
                    other => return Err(format!("unknown flag: {other}")),
                }
            }
            let store = SemanticStore::open_default()
                .map_err(|e| e.to_string())?
                .ok_or("embedding disabled in [embed] config")?;
            let n = store.count(namespace.as_deref()).map_err(|e| e.to_string())?;
            Ok(json!({ "count": n, "namespace": namespace }))
        }
        "remove" => {
            if rest.len() < 2 {
                return Err("usage: cos agent semantic remove <namespace> <key>".into());
            }
            let store = SemanticStore::open_default()
                .map_err(|e| e.to_string())?
                .ok_or("embedding disabled in [embed] config")?;
            let removed = store.remove(&rest[0], &rest[1]).map_err(|e| e.to_string())?;
            Ok(json!({ "removed": removed }))
        }
        "clear" => {
            if rest.is_empty() {
                return Err("usage: cos agent semantic clear <namespace>".into());
            }
            let store = SemanticStore::open_default()
                .map_err(|e| e.to_string())?
                .ok_or("embedding disabled in [embed] config")?;
            let n = store.clear_namespace(&rest[0]).map_err(|e| e.to_string())?;
            Ok(json!({ "deleted": n, "namespace": rest[0] }))
        }
        "clear-all" => {
            // Mutating + total — require --yes (matches sessions clear /
            // sessions purge convention). The whole point of this command
            // is the "I'm switching embed model" foot-gun, so insist on
            // an explicit confirmation.
            let confirmed = rest.iter().any(|a| a == "--yes");
            if !confirmed {
                return Err(
                    "refusing to wipe semantic.db without --yes. usage: cos agent semantic clear-all --yes"
                        .into(),
                );
            }
            // Open without an embedder so a misconfigured / unreachable
            // provider can't block the cleanup path. We still need a
            // SemanticStore handle for clear_all().
            let store = SemanticStore::open_default_without_embedder()
                .map_err(|e| e.to_string())?;
            let pinned_before = store.pinned_model().ok().flatten();
            let n = store.clear_all().map_err(|e| e.to_string())?;
            Ok(json!({
                "ok": true,
                "deleted": n,
                "previously_pinned_model": pinned_before,
            }))
        }
        other => Err(format!(
            "unknown semantic subcommand: {other}. try: index | search | list | count | remove | clear | clear-all | status"
        )),
    }
}

/// `cos agent memory [list|show|search|forget]` — user-facing
/// inspect/redact view of app-emitted memory rows. Apps push entries
/// in via the hidden `cos __memory remember` bridge under the
/// `memory.write` capability; this CLI surfaces what's been stored
/// and lets the user delete entries per row or per source.
pub(super) fn memory_cmd(args: &[String]) -> Result<Value, String> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("list");
    let rest: &[String] = if args.is_empty() { &[] } else { &args[1..] };
    match sub {
        "list" | "" => memory_list(rest),
        "show" => memory_show(rest),
        "search" => memory_search(rest),
        "forget" => memory_forget(rest),
        other => Err(format!(
            "unknown memory subcommand: {other}. try: list [--source <id>] [--limit N] | show <row_id> | search \"<query>\" [--source <id>] [--limit N] | forget {{--row <id> | --source <id>}} [--yes]"
        )),
    }
}

fn memory_list(args: &[String]) -> Result<Value, String> {
    let mut source: Option<String> = None;
    let mut limit: usize = 20;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--source" if i + 1 < args.len() => {
                source = Some(args[i + 1].clone());
                i += 2;
            }
            "--limit" if i + 1 < args.len() => {
                limit = args[i + 1]
                    .parse()
                    .map_err(|e| format!("--limit must be a positive integer: {e}"))?;
                i += 2;
            }
            other => return Err(format!("memory list: unexpected arg {other}")),
        }
    }
    let db = memory::sqlite_fts::MemoryDb::open_default()
        .map_err(|e| format!("memory db unavailable: {e}"))?;
    let rows = memory::app_memory::list(&db, source.as_deref(), limit)
        .map_err(|e| format!("memory list: {e}"))?;
    let n = rows.len();
    Ok(json!({
        "n": n,
        "rows": rows,
    }))
}

fn memory_show(args: &[String]) -> Result<Value, String> {
    let id: i64 = args
        .first()
        .ok_or_else(|| "usage: cos agent memory show <row_id>".to_string())?
        .parse()
        .map_err(|e| format!("row_id must be an integer: {e}"))?;
    let db = memory::sqlite_fts::MemoryDb::open_default()
        .map_err(|e| format!("memory db unavailable: {e}"))?;
    let row = memory::app_memory::show(&db, id).map_err(|e| format!("memory show: {e}"))?;
    Ok(json!({ "row": row }))
}

fn memory_search(args: &[String]) -> Result<Value, String> {
    if args.is_empty() {
        return Err(
            "usage: cos agent memory search \"<query>\" [--source <id>] [--limit N]".into(),
        );
    }
    let query = args[0].clone();
    let mut source: Option<String> = None;
    let mut limit: usize = 20;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--source" if i + 1 < args.len() => {
                source = Some(args[i + 1].clone());
                i += 2;
            }
            "--limit" if i + 1 < args.len() => {
                limit = args[i + 1]
                    .parse()
                    .map_err(|e| format!("--limit must be a positive integer: {e}"))?;
                i += 2;
            }
            other => return Err(format!("memory search: unexpected arg {other}")),
        }
    }
    let db = memory::sqlite_fts::MemoryDb::open_default()
        .map_err(|e| format!("memory db unavailable: {e}"))?;
    let rows = memory::app_memory::search(&db, &query, source.as_deref(), limit)
        .map_err(|e| format!("memory search: {e}"))?;
    Ok(json!({
        "query": query,
        "limit": limit,
        "n": rows.len(),
        "rows": rows,
    }))
}

fn memory_forget(args: &[String]) -> Result<Value, String> {
    let mut source: Option<String> = None;
    let mut row: Option<i64> = None;
    let mut confirmed = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--source" if i + 1 < args.len() => {
                source = Some(args[i + 1].clone());
                i += 2;
            }
            "--row" if i + 1 < args.len() => {
                row = Some(
                    args[i + 1]
                        .parse()
                        .map_err(|e| format!("--row must be an integer: {e}"))?,
                );
                i += 2;
            }
            "--yes" | "-y" => {
                confirmed = true;
                i += 1;
            }
            other => return Err(format!("memory forget: unexpected arg {other}")),
        }
    }
    if source.is_some() == row.is_some() {
        return Err("usage: cos agent memory forget {--row <id> | --source <id>} [--yes]".into());
    }
    let db = memory::sqlite_fts::MemoryDb::open_default()
        .map_err(|e| format!("memory db unavailable: {e}"))?;
    let store = memory::app_memory::open_default_store();
    if let Some(s) = source {
        if !confirmed {
            return Err(format!(
                "memory forget --source {s}: refusing to delete all rows for source `{s}` without --yes"
            ));
        }
        let n = memory::app_memory::forget_source(&db, store.as_ref(), &s)
            .map_err(|e| format!("memory forget: {e}"))?;
        return Ok(json!({ "removed": n, "source": s }));
    }
    let id = row.unwrap();
    let removed = memory::app_memory::forget_row(&db, store.as_ref(), id)
        .map_err(|e| format!("memory forget: {e}"))?;
    Ok(json!({
        "removed": if removed { 1 } else { 0 },
        "row_id": id,
    }))
}

/// `cos agent notes [list|read <name>|write <name> <content>|append <name> <line>|delete <name>]`
/// — manages markdown notes the agent can read into its system prompt
/// (MEMORY.md / USER.md by convention) or any other ad-hoc note file.
pub(super) fn notes_cmd(args: &[String]) -> Result<Value, String> {
    let store = memory::notes::NotesStore::system_default();
    let sub = args.first().map(|s| s.as_str()).unwrap_or("list");
    match sub {
        "list" | "" => {
            let names = store.list().map_err(|e| format!("list failed: {e}"))?;
            Ok(json!({
                "dir": store.dir().display().to_string(),
                "n": names.len(),
                "notes": names,
            }))
        }
        "read" => {
            let name = args.get(1).cloned().unwrap_or_default();
            if name.is_empty() {
                return Err("usage: cos agent notes read <name>".into());
            }
            let content = store
                .read(&name)
                .map_err(|e| format!("read failed: {e}"))?;
            Ok(json!({
                "name": name,
                "exists": content.is_some(),
                "content": content,
            }))
        }
        "write" => {
            let name = args.get(1).cloned().unwrap_or_default();
            let content = args.get(2).cloned().unwrap_or_default();
            if name.is_empty() {
                return Err("usage: cos agent notes write <name> <content>".into());
            }
            store
                .write(&name, &content)
                .map_err(|e| format!("write failed: {e}"))?;
            Ok(json!({
                "name": name,
                "bytes_written": content.len(),
            }))
        }
        "append" => {
            let name = args.get(1).cloned().unwrap_or_default();
            let line = args.get(2).cloned().unwrap_or_default();
            if name.is_empty() {
                return Err("usage: cos agent notes append <name> <line>".into());
            }
            store
                .append(&name, &line)
                .map_err(|e| format!("append failed: {e}"))?;
            Ok(json!({
                "name": name,
                "appended_bytes": line.len(),
            }))
        }
        "delete" => {
            let name = args.get(1).cloned().unwrap_or_default();
            if name.is_empty() {
                return Err("usage: cos agent notes delete <name>".into());
            }
            store
                .delete(&name)
                .map_err(|e| format!("delete failed: {e}"))?;
            Ok(json!({ "name": name, "deleted": true }))
        }
        other => Err(format!(
            "unknown notes subcommand: {other}. try: list | read <name> | write <name> <content> | append <name> <line> | delete <name>"
        )),
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/memory_commands.rs"
    ));
}
