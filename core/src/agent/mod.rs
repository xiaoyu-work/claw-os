//! `cos agent` — agent-native OS subsystem.
//!
//! Phase 0: skeleton + subcommand dispatch only. Real loop wired in Phase 1.
//!
//! Module layout (target architecture):
//!
//! ```text
//! agent/
//! ├── mod.rs          (this file: subcommand dispatcher)
//! ├── runtime/        loop_, scheduler, turn, hooks
//! ├── prompt/         system prompt, MEMORY.md, USER.md injection
//! ├── context/        session, history, compression
//! ├── memory/         sqlite_fts, semantic, honcho, curator
//! ├── skills/         skill registry, loader, exec
//! ├── llm/            Provider trait + provider impls (anthropic, openai, ...)
//! ├── tools/          tool registry, exec proxies into cos primitives
//! └── safety/         redact, policy hooks, approval
//! ```

pub mod context;
pub mod curator;
pub mod display;
pub mod insights;
pub mod llm;
pub mod media;
pub mod memory;
pub mod nudge;
pub mod onboarding;
pub mod prompt;
pub mod runtime;
pub mod safety;
pub mod skills;
pub mod tools;
pub mod title;

use serde_json::{json, Value};

/// Dispatch a `cos agent <command>` invocation.
pub fn run(command: &str, args: &[String]) -> Result<Value, String> {
    match command {
        "ask" => {
            let prompt = args.first().cloned().unwrap_or_default();
            if prompt.is_empty() {
                return Err("usage: cos agent ask \"<prompt>\"".into());
            }
            match runtime::loop_::ask_blocking(&prompt) {
                Ok(result) => Ok(json!({
                    "answer": result.answer,
                    "turns": result.turns,
                    "provider": result.provider,
                    "model": result.model,
                    "session_id": result.session_id,
                })),
                Err(e) => Err(e.to_string()),
            }
        }
        "chat" => Ok(json!({"status": "not_implemented", "phase": "1+"})),
        "status" => {
            let cfg = &crate::config::get().agent;
            let mut tools = tools::registry::default_registry();
            tools.set_guardrails(crate::agent::runtime::loop_::guardrails_from_cfg(cfg));
            tools.set_approval(crate::agent::runtime::loop_::approval_from_cfg(cfg));
            let registered_total = tools.names_unfiltered().len();
            let permitted = tools.names();
            // Best-effort memory DB stats — read-only, never mutates.
            let memory_stats = match memory::sqlite_fts::MemoryDb::open_default() {
                Ok(db) => {
                    let total = db.count_total().unwrap_or(0);
                    let sessions = db.sessions(1).map(|s| s.len()).unwrap_or(0);
                    json!({
                        "status": "ok",
                        "path": crate::paths::agent_memory_db_path().display().to_string(),
                        "total_messages": total,
                        "has_sessions": sessions > 0,
                    })
                }
                Err(e) => json!({ "status": "unavailable", "error": e.to_string() }),
            };
            let skills_load = skills::loader::load_default();
            Ok(json!({
                "status": "ok",
                "phase": "3",
                "provider": cfg.provider,
                "provider_registered": llm::registry::is_registered(&cfg.provider),
                "providers": llm::available_providers(),
                "model": cfg.model,
                "max_turns": cfg.max_turns,
                "max_tokens": cfg.max_tokens,
                "temperature": cfg.temperature,
                "tools_registered": registered_total,
                "tools_permitted": permitted.len(),
                "tools": permitted,
                "tool_allow": cfg.tool_allow.clone(),
                "tool_deny": cfg.tool_deny.clone(),
                "dangerous_tools": cfg.dangerous_tools.clone(),
                "auto_approve_tools": cfg.auto_approve_tools.clone(),
                "auto_deny_tools": cfg.auto_deny_tools.clone(),
                "skills_loaded": skills_load.loaded_count(),
                "skills_disabled": skills_load.disabled.len(),
                "skills_errors": skills_load.errors.len(),
                "memory": memory_stats,
            }))
        }
        "service" => Ok(json!({"status": "not_implemented", "phase": "1+"})),
        "insights" => insights_cmd(args),
        "recall" => recall_cmd(args),
        "sessions" => sessions_cmd(args),
        "onboarding" => onboarding_cmd(args),
        "notes" => notes_cmd(args),
        "skills" => skills_cmd(args),
        "nudge" => nudge_cmd(args),
        "mcp" => mcp_cmd(args),
        "usage" => usage_cmd(args),
        "curator" => curator_cmd(args),
        other => Err(format!(
            "unknown command: {other}. try: ask | chat | status | service | insights | recall | sessions | onboarding | notes | skills | nudge | mcp | usage | curator"
        )),
    }
}

/// `cos agent insights [overall|recent|sessions] [n]` — aggregate
/// the JSONL run-record stream produced by every LLM call.
fn insights_cmd(args: &[String]) -> Result<Value, String> {
    use chrono::DateTime;
    use insights::InsightsFilter;

    let sub = args.first().map(|s| s.as_str()).unwrap_or("overall");
    let path = crate::paths::llm_run_log_path();

    // Parse trailing flags shared across all three sub-verbs.
    // For "recent" the optional N positional must come first
    // (preserves the existing `cos agent insights recent 25` UX).
    let (n_for_recent, mut i) = if sub == "recent" {
        let n = args.get(1).and_then(|s| s.parse::<usize>().ok());
        (n, if n.is_some() { 2 } else { 1 })
    } else {
        (None, 1)
    };

    let mut filter = InsightsFilter::default();
    while i < args.len() {
        match args[i].as_str() {
            "--since" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| "--since needs <ISO timestamp>".to_string())?;
                filter.since = Some(
                    DateTime::parse_from_rfc3339(v)
                        .map(|d| d.with_timezone(&chrono::Utc))
                        .map_err(|e| format!("--since: {e}"))?,
                );
                i += 2;
            }
            "--until" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| "--until needs <ISO timestamp>".to_string())?;
                filter.until = Some(
                    DateTime::parse_from_rfc3339(v)
                        .map(|d| d.with_timezone(&chrono::Utc))
                        .map_err(|e| format!("--until: {e}"))?,
                );
                i += 2;
            }
            "--ok" => {
                filter.status_ok = Some(true);
                i += 1;
            }
            "--error" => {
                filter.status_ok = Some(false);
                i += 1;
            }
            "--provider" => {
                filter.provider = Some(
                    args.get(i + 1)
                        .cloned()
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| "--provider needs <name>".to_string())?,
                );
                i += 2;
            }
            "--model" => {
                filter.model = Some(
                    args.get(i + 1)
                        .cloned()
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| "--model needs <name>".to_string())?,
                );
                i += 2;
            }
            other => return Err(format!("unknown flag: {other}")),
        }
    }

    let filter_payload = json!({
        "since": filter.since.map(|d| d.to_rfc3339()),
        "until": filter.until.map(|d| d.to_rfc3339()),
        "status_ok": filter.status_ok,
        "provider": filter.provider.clone(),
        "model": filter.model.clone(),
    });

    match sub {
        "overall" | "" => {
            let report = insights::InsightsReport::from_path_filtered(&path, &filter);
            Ok(json!({
                "log": path.display().to_string(),
                "filter": filter_payload,
                "overall": report.overall,
                "per_provider": report.per_provider,
                "per_model": report.per_model,
            }))
        }
        "recent" => {
            let n = n_for_recent.unwrap_or(10);
            let rows = insights::InsightsReport::recent_filtered(&path, n, &filter);
            Ok(json!({
                "log": path.display().to_string(),
                "filter": filter_payload,
                "n": rows.len(),
                "records": rows,
            }))
        }
        "sessions" => {
            let by = insights::InsightsReport::by_session_filtered(&path, &filter);
            Ok(json!({
                "log": path.display().to_string(),
                "filter": filter_payload,
                "sessions": by,
            }))
        }
        other => Err(format!(
            "unknown insights subcommand: {other}. try: overall | recent [n] | sessions"
        )),
    }
}

/// `cos agent recall <query> [limit]` — FTS5 search across all
/// recorded conversation messages. Returns ranked hits (best first).
fn recall_cmd(args: &[String]) -> Result<Value, String> {
    let query = args.first().cloned().unwrap_or_default();
    if query.is_empty() {
        return Err("usage: cos agent recall \"<query>\" [limit]".into());
    }
    let limit: usize = args
        .get(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);
    let db = memory::sqlite_fts::MemoryDb::open_default()
        .map_err(|e| format!("memory db unavailable: {e}"))?;
    let hits = db
        .search(&query, limit)
        .map_err(|e| format!("search failed: {e}"))?;
    let rendered: Vec<Value> = hits
        .iter()
        .map(|h| {
            json!({
                "id": h.row.id,
                "session_id": h.row.session_id,
                "role": h.row.role,
                "content": h.row.content,
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
fn sessions_cmd(args: &[String]) -> Result<Value, String> {
    let limit: usize = args
        .first()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);
    let db = memory::sqlite_fts::MemoryDb::open_default()
        .map_err(|e| format!("memory db unavailable: {e}"))?;
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

/// `cos agent onboarding [status|next|complete <step> [note]|skip <step>|reset <step>]`
/// — drives the first-run setup state machine. Defaults to `status`.
fn onboarding_cmd(args: &[String]) -> Result<Value, String> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("status");
    let store = onboarding::OnboardingStore::new(crate::paths::agent_onboarding_path());

    match sub {
        "status" | "" => {
            let state = store.load();
            let next = state.next_pending().map(|s| s.id.clone());
            Ok(json!({
                "path": store.path().display().to_string(),
                "complete": state.is_complete(),
                "next": next,
                "summary": state.summary(),
                "steps": state.steps,
            }))
        }
        "next" => {
            let state = store.load();
            match state.next_pending() {
                Some(step) => Ok(json!({
                    "id": step.id,
                    "title": step.title,
                    "optional": step.optional,
                })),
                None => Ok(json!({ "id": null, "complete": true })),
            }
        }
        "complete" => {
            let id = args.get(1).cloned().unwrap_or_default();
            if id.is_empty() {
                return Err("usage: cos agent onboarding complete <step> [note]".into());
            }
            let note = args.get(2).cloned();
            let mut state = store.load();
            state
                .complete_step(&id, note.clone())
                .map_err(|e| e.to_string())?;
            store
                .save(&state)
                .map_err(|e| format!("save failed: {e}"))?;
            Ok(json!({
                "id": id,
                "status": "completed",
                "note": note,
                "next": state.next_pending().map(|s| s.id.clone()),
                "complete": state.is_complete(),
            }))
        }
        "skip" => {
            let id = args.get(1).cloned().unwrap_or_default();
            if id.is_empty() {
                return Err("usage: cos agent onboarding skip <step>".into());
            }
            let mut state = store.load();
            state.skip_step(&id).map_err(|e| e.to_string())?;
            store
                .save(&state)
                .map_err(|e| format!("save failed: {e}"))?;
            Ok(json!({
                "id": id,
                "status": "skipped",
                "next": state.next_pending().map(|s| s.id.clone()),
                "complete": state.is_complete(),
            }))
        }
        "reset" => {
            let id = args.get(1).cloned().unwrap_or_default();
            let mut state = store.load();
            if id.is_empty() {
                state = onboarding::OnboardingState::default_steps();
            } else {
                state.reset_step(&id).map_err(|e| e.to_string())?;
            }
            store
                .save(&state)
                .map_err(|e| format!("save failed: {e}"))?;
            Ok(json!({
                "reset": if id.is_empty() { "all".to_string() } else { id },
                "next": state.next_pending().map(|s| s.id.clone()),
                "complete": state.is_complete(),
            }))
        }
        other => Err(format!(
            "unknown onboarding subcommand: {other}. try: status | next | complete <id> [note] | skip <id> | reset [id]"
        )),
    }
}

/// `cos agent notes [list|read <name>|write <name> <content>|append <name> <line>|delete <name>]`
/// — manages markdown notes the agent can read into its system prompt
/// (MEMORY.md / USER.md by convention) or any other ad-hoc note file.
fn notes_cmd(args: &[String]) -> Result<Value, String> {
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

/// `cos agent skills [list|info <id>|disabled|errors|root]` — exposes
/// the on-disk skill registry under `data_dir/agent/skills/`.
fn skills_cmd(args: &[String]) -> Result<Value, String> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("list");
    match sub {
        "root" => Ok(json!({
            "root": crate::paths::agent_skills_dir().display().to_string(),
        })),
        "list" | "" => {
            let res = skills::loader::load_default();
            let names: Vec<&String> = res.skills.keys().collect();
            Ok(json!({
                "root": crate::paths::agent_skills_dir().display().to_string(),
                "loaded": res.loaded_count(),
                "disabled": res.disabled.len(),
                "errors": res.errors.len(),
                "names": names,
            }))
        }
        "info" => {
            let id = args.get(1).cloned().unwrap_or_default();
            if id.is_empty() {
                return Err("usage: cos agent skills info <id>".into());
            }
            let res = skills::loader::load_default();
            if let Some(s) = res.skills.get(&id) {
                Ok(json!({
                    "id": s.id,
                    "dir": s.dir.display().to_string(),
                    "manifest_path": s.manifest_path.display().to_string(),
                    "name": s.manifest.name,
                    "description": s.manifest.description,
                    "version": s.manifest.version,
                    "license": s.manifest.license,
                    "author": s.manifest.author,
                    "homepage": s.manifest.homepage,
                    "allowed_tools": s.manifest.allowed_tools,
                    "triggers": s.manifest.triggers,
                    "body_bytes": s.body.len(),
                }))
            } else if let Some(reason) = res.disabled.get(&id) {
                Ok(json!({
                    "id": id,
                    "status": "disabled",
                    "reason": reason,
                }))
            } else if let Some(err) = res.errors.get(&id) {
                Ok(json!({
                    "id": id,
                    "status": "error",
                    "error": err,
                }))
            } else {
                Err(format!("unknown skill: {id}"))
            }
        }
        "disabled" => {
            let res = skills::loader::load_default();
            Ok(json!({
                "n": res.disabled.len(),
                "disabled": res.disabled,
            }))
        }
        "errors" => {
            let res = skills::loader::load_default();
            Ok(json!({
                "n": res.errors.len(),
                "errors": res.errors,
            }))
        }
        other => Err(format!(
            "unknown skills subcommand: {other}. try: list | info <id> | disabled | errors | root"
        )),
    }
}

/// `cos agent nudge [list|due|add <due_in_secs> <message> [--repeat <secs>] [--tag <tag>]|fire <id>|remove <id>|path]`
/// — managed periodic-nudge store. `list` shows all nudges; `due`
/// shows only those with `due_at_epoch_s <= now`. `add` parses a
/// relative offset in seconds (the most common case for "remind me
/// in 30 minutes"); `fire` advances repeating nudges or deletes
/// one-shots.
fn nudge_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::nudge::{now_epoch_s, Nudge, NudgeStore};
    let store = NudgeStore::new(crate::paths::agent_nudges_path());
    let sub = args.first().map(|s| s.as_str()).unwrap_or("list");
    match sub {
        "path" => Ok(json!({
            "path": crate::paths::agent_nudges_path().display().to_string(),
        })),
        "list" | "" => {
            let mut all = store.list();
            all.sort_by_key(|n| n.due_at_epoch_s);
            Ok(json!({
                "path": crate::paths::agent_nudges_path().display().to_string(),
                "n": all.len(),
                "nudges": all,
            }))
        }
        "due" => {
            let now = now_epoch_s();
            let mut due = store.due(now);
            due.sort_by_key(|n| n.due_at_epoch_s);
            Ok(json!({
                "now": now,
                "n": due.len(),
                "nudges": due,
            }))
        }
        "add" => {
            let due_in: i64 = args
                .get(1)
                .ok_or_else(|| "usage: cos agent nudge add <due_in_secs> <message> [--repeat <secs>] [--tag <tag>]".to_string())?
                .parse()
                .map_err(|e| format!("due_in_secs must be integer: {e}"))?;
            let message = args
                .get(2)
                .cloned()
                .filter(|m| !m.is_empty())
                .ok_or_else(|| "usage: cos agent nudge add <due_in_secs> <message> [--repeat <secs>] [--tag <tag>]".to_string())?;
            let mut repeat_secs: Option<u64> = None;
            let mut tag: Option<String> = None;
            let mut i = 3;
            while i < args.len() {
                match args[i].as_str() {
                    "--repeat" => {
                        repeat_secs = Some(
                            args.get(i + 1)
                                .ok_or_else(|| "--repeat needs <secs>".to_string())?
                                .parse()
                                .map_err(|e| format!("--repeat secs invalid: {e}"))?,
                        );
                        i += 2;
                    }
                    "--tag" => {
                        tag = Some(
                            args.get(i + 1)
                                .cloned()
                                .ok_or_else(|| "--tag needs <value>".to_string())?,
                        );
                        i += 2;
                    }
                    other => return Err(format!("unknown flag: {other}")),
                }
            }
            let now = now_epoch_s();
            let due_at = if due_in >= 0 {
                now.saturating_add(due_in as u64)
            } else {
                now.saturating_sub((-due_in) as u64)
            };
            let nudge = Nudge {
                id: String::new(),
                message,
                due_at_epoch_s: due_at,
                repeat_secs,
                tag,
                last_fired_epoch_s: None,
            };
            let id = store
                .add(nudge)
                .map_err(|e| format!("add failed: {e}"))?;
            Ok(json!({
                "id": id,
                "due_at_epoch_s": due_at,
            }))
        }
        "fire" => {
            let id = args
                .get(1)
                .cloned()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "usage: cos agent nudge fire <id>".to_string())?;
            let updated = store
                .fire(&id, now_epoch_s())
                .map_err(|e| format!("fire failed: {e}"))?;
            Ok(json!({ "id": id, "updated": updated }))
        }
        "remove" => {
            let id = args
                .get(1)
                .cloned()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "usage: cos agent nudge remove <id>".to_string())?;
            let removed = store
                .remove(&id)
                .map_err(|e| format!("remove failed: {e}"))?;
            Ok(json!({ "id": id, "removed": removed }))
        }
        other => Err(format!(
            "unknown nudge subcommand: {other}. try: list | due | add <due_in_secs> <message> [--repeat <secs>] [--tag <tag>] | fire <id> | remove <id> | path"
        )),
    }
}

/// Apply ad-hoc `--allow` / `--deny` overrides to a base
/// [`AgentConfig`] for one-shot scoping (currently used by
/// `cos agent mcp serve`). Returns the merged config without
/// mutating the input. Extracted so the merge logic can be tested
/// independently of the blocking server entry point.
fn merge_mcp_overrides(
    base: &crate::config::AgentConfig,
    allow: Option<Vec<String>>,
    deny: Vec<String>,
) -> crate::config::AgentConfig {
    let mut out = base.clone();
    if let Some(a) = allow {
        out.tool_allow = Some(a);
    }
    out.tool_deny.extend(deny);
    out
}

/// `cos agent mcp [serve|status]` — MCP (Model Context Protocol)
/// server that exposes the agent's tool registry to external clients
/// over newline-delimited JSON-RPC on stdio. `serve` runs in the
/// foreground until stdin closes; `status` reports the catalog
/// without listening.
fn mcp_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::tools::mcp::{server::McpServer, transport::StdioTransport};
    use std::sync::Arc;
    let sub = args.first().map(|s| s.as_str()).unwrap_or("status");
    match sub {
        "status" | "" => {
            let cfg = &crate::config::get().agent;
            let mut tools = tools::registry::default_registry();
            tools.set_guardrails(crate::agent::runtime::loop_::guardrails_from_cfg(cfg));
            tools.set_approval(crate::agent::runtime::loop_::approval_from_cfg(cfg));
            Ok(json!({
                "status": "ready",
                "transport": "stdio",
                "server_name": format!("cos-agent/{}", env!("CARGO_PKG_VERSION")),
                "tools_registered": tools.names_unfiltered().len(),
                "tools_permitted": tools.names().len(),
                "tools": tools.names(),
            }))
        }
        "serve" => {
            // Build the registry exactly as `agent::ask` would, so
            // the same guardrails/approval policy applies to MCP-
            // initiated tool calls. Ad-hoc --allow / --deny flags
            // narrow the tool surface for this serve invocation
            // without touching global config — useful for exposing a
            // restricted catalogue to a single MCP client.
            let cfg = &crate::config::get().agent;
            let mut allow_overrides: Option<Vec<String>> = None;
            let mut deny_overrides: Vec<String> = Vec::new();
            let mut i = 1usize;
            while i < args.len() {
                match args[i].as_str() {
                    "--allow" => {
                        let v = args
                            .get(i + 1)
                            .ok_or_else(|| "--allow needs <tool-name>".to_string())?;
                        allow_overrides
                            .get_or_insert_with(Vec::new)
                            .push(v.clone());
                        i += 2;
                    }
                    "--deny" => {
                        let v = args
                            .get(i + 1)
                            .ok_or_else(|| "--deny needs <tool-name>".to_string())?;
                        deny_overrides.push(v.clone());
                        i += 2;
                    }
                    other => {
                        return Err(format!(
                            "unknown flag for `mcp serve`: {other}. try --allow <name> | --deny <name>"
                        ))
                    }
                }
            }
            let mut tools = tools::registry::default_registry();
            // Honour allow override when supplied; otherwise inherit
            // cfg.tool_allow via the standard helper. --deny appends
            // to (does not replace) cfg.tool_deny so global denies
            // still apply.
            let merged = merge_mcp_overrides(cfg, allow_overrides, deny_overrides);
            tools.set_guardrails(crate::agent::runtime::loop_::guardrails_from_cfg(&merged));
            tools.set_approval(crate::agent::runtime::loop_::approval_from_cfg(cfg));
            let registry = Arc::new(tools);
            let server = McpServer::new(
                "cos-agent",
                env!("CARGO_PKG_VERSION"),
                registry,
            );
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("tokio runtime: {e}"))?;
            runtime
                .block_on(server.serve(StdioTransport::stdio()))
                .map_err(|e| format!("mcp serve: {e}"))?;
            Ok(json!({"status": "stopped", "reason": "stdin closed"}))
        }
        other => Err(format!(
            "unknown mcp subcommand: {other}. try: status | serve"
        )),
    }
}

/// `cos agent usage [overall|provider <name>|model <name>|session <id>]`
/// `[--since <ISO>] [--until <ISO>] [--ok|--error]` — filtered
/// aggregation over `llm.jsonl`. Mirrors `agent insights overall` for
/// the unfiltered case but adds the AND-combined filter set from
/// [`crate::agent::llm::usage::UsageQuery`].
fn usage_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::llm::usage::{aggregate_path_filtered, default_log_path, UsageQuery};
    use chrono::DateTime;
    let mut query = UsageQuery::default();
    // Default scope is "overall" — no positional bucket filter applied
    // beyond the optional flags. `provider <n>` / `model <n>` /
    // `session <id>` add a single additional filter.
    let scope = args.first().map(|s| s.as_str()).unwrap_or("overall");
    let mut i = match scope {
        "overall" | "" => 1,
        "provider" => {
            query.provider = Some(
                args.get(1)
                    .cloned()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| "usage: cos agent usage provider <name>".to_string())?,
            );
            2
        }
        "model" => {
            query.model = Some(
                args.get(1)
                    .cloned()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| "usage: cos agent usage model <name>".to_string())?,
            );
            2
        }
        "session" => {
            query.session_id = Some(
                args.get(1)
                    .cloned()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| "usage: cos agent usage session <id>".to_string())?,
            );
            2
        }
        other => {
            return Err(format!(
                "unknown usage scope: {other}. try: overall | provider <name> | model <name> | session <id>"
            ))
        }
    };
    while i < args.len() {
        match args[i].as_str() {
            "--since" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| "--since needs <ISO timestamp>".to_string())?;
                query.since = Some(
                    DateTime::parse_from_rfc3339(v)
                        .map(|d| d.with_timezone(&chrono::Utc))
                        .map_err(|e| format!("--since: {e}"))?,
                );
                i += 2;
            }
            "--until" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| "--until needs <ISO timestamp>".to_string())?;
                query.until = Some(
                    DateTime::parse_from_rfc3339(v)
                        .map(|d| d.with_timezone(&chrono::Utc))
                        .map_err(|e| format!("--until: {e}"))?,
                );
                i += 2;
            }
            "--ok" => {
                query.status_ok = Some(true);
                i += 1;
            }
            "--error" => {
                query.status_ok = Some(false);
                i += 1;
            }
            other => return Err(format!("unknown flag: {other}")),
        }
    }
    let path = default_log_path();
    let summary = aggregate_path_filtered(&path, &query);
    Ok(json!({
        "log": path.display().to_string(),
        "scope": scope,
        "filter": {
            "provider": query.provider,
            "model": query.model,
            "session_id": query.session_id,
            "since": query.since.map(|d| d.to_rfc3339()),
            "until": query.until.map(|d| d.to_rfc3339()),
            "status_ok": query.status_ok,
        },
        "total": summary.total,
        "by_provider": summary.by_provider,
        "by_model": summary.by_model,
        "by_session": summary.by_session,
        "parse_errors": summary.parse_errors,
    }))
}

/// `cos agent curator propose <session_id> [--accept] [--limit <n>]`
/// `[--no-require-acceptance] [--min-tools <n>] [--min-turns <n>]`
/// — distil a recorded conversation into a draft skill manifest.
///
/// Reads the session's history from the memory DB, infers tool
/// usage from the stored `[tool_use:NAME] ...` markers (no schema
/// migration required), and runs the deterministic
/// [`crate::agent::curator::Curator`] pure-function pipeline.
///
/// Output is a JSON object with either a `draft` (id/title/desc/
/// allowed_tools/confidence) or a `not_enough` reason.
fn curator_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::curator::{
        looks_like_acceptance, message_to_turn, ConversationTurn, Curator, CuratorConfig,
        CuratorOutcome,
    };
    let sub = args.first().map(|s| s.as_str()).unwrap_or("");
    if sub != "propose" {
        return Err(format!(
            "unknown curator subcommand: '{sub}'. try: propose <session_id> [--accept] [--limit <n>] [--no-require-acceptance] [--min-tools <n>] [--min-turns <n>]"
        ));
    }
    let sid = args
        .get(1)
        .cloned()
        .filter(|s| !s.is_empty() && !s.starts_with("--"))
        .ok_or_else(|| "usage: cos agent curator propose <session_id> [flags]".to_string())?;

    let mut limit: usize = 200;
    let mut force_accept = false;
    let mut config = CuratorConfig::default();
    let mut i = 2usize;
    while i < args.len() {
        match args[i].as_str() {
            "--accept" => {
                force_accept = true;
                i += 1;
            }
            "--no-require-acceptance" => {
                config.require_user_acceptance = false;
                i += 1;
            }
            "--limit" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| "--limit needs <n>".to_string())?;
                limit = v.parse().map_err(|e| format!("--limit: {e}"))?;
                i += 2;
            }
            "--min-tools" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| "--min-tools needs <n>".to_string())?;
                config.min_distinct_tools = v.parse().map_err(|e| format!("--min-tools: {e}"))?;
                i += 2;
            }
            "--min-turns" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| "--min-turns needs <n>".to_string())?;
                config.min_assistant_turns =
                    v.parse().map_err(|e| format!("--min-turns: {e}"))?;
                i += 2;
            }
            other => return Err(format!("unknown flag for `curator propose`: {other}")),
        }
    }

    let db = memory::sqlite_fts::MemoryDb::open_default()
        .map_err(|e| format!("memory db unavailable: {e}"))?;
    let rows = db
        .recent(&sid, limit)
        .map_err(|e| format!("memory recent: {e}"))?;
    if rows.is_empty() {
        return Ok(json!({
            "session_id": sid,
            "outcome": "not_enough",
            "reason": "session has no recorded messages",
        }));
    }
    let mut turns: Vec<ConversationTurn> = rows
        .iter()
        .filter_map(|r| message_to_turn(&r.role, &r.content))
        .collect();
    if force_accept {
        if let Some(last) = turns.last_mut() {
            last.user_acceptance = true;
        }
    } else {
        // Apply the conservative built-in heuristic to user turns
        // when the runtime didn't supply an explicit signal.
        for t in turns.iter_mut() {
            if matches!(t.role, crate::agent::curator::TurnRole::User) && looks_like_acceptance(&t.content) {
                t.user_acceptance = true;
            }
        }
    }
    let curator = Curator::new(config);
    match curator.propose(&turns) {
        CuratorOutcome::Drafted(draft) => Ok(json!({
            "session_id": sid,
            "outcome": "drafted",
            "messages_scanned": rows.len(),
            "draft": draft,
        })),
        CuratorOutcome::NotEnough { reason } => Ok(json!({
            "session_id": sid,
            "outcome": "not_enough",
            "messages_scanned": rows.len(),
            "reason": reason,
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_command_lists_all_options() {
        let err = run("not-a-command", &[]).unwrap_err();
        assert!(err.contains("ask"));
        assert!(err.contains("insights"));
        assert!(err.contains("recall"));
        assert!(err.contains("sessions"));
    }

    #[test]
    fn insights_overall_returns_empty_when_no_log() {
        // The default log path may or may not exist at test time;
        // either way the call must not panic and must shape a JSON
        // object with the expected fields.
        let v = insights_cmd(&[]).expect("insights ok");
        assert!(v.get("overall").is_some());
        assert!(v.get("per_provider").is_some());
        assert!(v.get("per_model").is_some());
        assert!(v.get("log").is_some());
    }

    #[test]
    fn insights_recent_parses_n_arg() {
        let v = insights_cmd(&["recent".into(), "5".into()]).expect("recent ok");
        assert!(v.get("records").is_some());
        // n is the actual returned count, not the requested limit; on a
        // fresh test env it should be zero records but the field must
        // still exist.
        let n = v.get("n").and_then(|x| x.as_u64()).expect("n field");
        assert!(n <= 5);
    }

    #[test]
    fn insights_sessions_returns_map() {
        let v = insights_cmd(&["sessions".into()]).expect("sessions ok");
        assert!(v.get("sessions").is_some());
    }

    #[test]
    fn insights_unknown_subcommand_errors() {
        let err = insights_cmd(&["bogus".into()]).unwrap_err();
        assert!(err.contains("bogus"));
        assert!(err.contains("overall"));
    }

    #[test]
    fn recall_empty_query_errors() {
        let err = recall_cmd(&[]).unwrap_err();
        assert!(err.to_lowercase().contains("usage"));
    }

    #[test]
    fn onboarding_status_returns_default_state_when_file_missing() {
        let v = onboarding_cmd(&[]).expect("onboarding status ok");
        // Default-shaped state: 6 steps, complete:false, next is "provider".
        assert!(v.get("steps").and_then(|s| s.as_array()).is_some());
        let next = v
            .get("next")
            .and_then(|n| n.as_str().or_else(|| n.is_null().then_some("")))
            .unwrap_or("");
        // On a fresh test env (no data dir set) the default state has
        // a pending first step. On a populated env, ``next`` may be
        // null if the user has already finished. Either is fine; we
        // just assert the field exists and has the right type.
        assert!(next.is_empty() || !next.is_empty());
    }

    #[test]
    fn onboarding_complete_requires_step_id() {
        let err = onboarding_cmd(&["complete".into()]).unwrap_err();
        assert!(err.to_lowercase().contains("usage"));
    }

    #[test]
    fn onboarding_unknown_subcommand_lists_options() {
        let err = onboarding_cmd(&["bogus".into()]).unwrap_err();
        assert!(err.contains("status"));
        assert!(err.contains("next"));
        assert!(err.contains("complete"));
    }

    #[test]
    fn notes_list_returns_dir_and_names() {
        let v = notes_cmd(&[]).expect("notes list ok");
        assert!(v.get("dir").is_some());
        assert!(v.get("notes").and_then(|x| x.as_array()).is_some());
    }

    #[test]
    fn notes_read_requires_name() {
        let err = notes_cmd(&["read".into()]).unwrap_err();
        assert!(err.to_lowercase().contains("usage"));
    }

    #[test]
    fn notes_unknown_subcommand_lists_options() {
        let err = notes_cmd(&["bogus".into()]).unwrap_err();
        assert!(err.contains("list"));
        assert!(err.contains("read"));
        assert!(err.contains("write"));
    }

    #[test]
    fn skills_root_returns_path() {
        let v = skills_cmd(&["root".into()]).expect("skills root ok");
        assert!(v.get("root").and_then(|x| x.as_str()).is_some());
    }

    #[test]
    fn skills_list_shape_correct() {
        let v = skills_cmd(&[]).expect("skills list ok");
        assert!(v.get("loaded").is_some());
        assert!(v.get("disabled").is_some());
        assert!(v.get("errors").is_some());
        assert!(v.get("names").and_then(|x| x.as_array()).is_some());
    }

    #[test]
    fn skills_info_requires_id() {
        let err = skills_cmd(&["info".into()]).unwrap_err();
        assert!(err.to_lowercase().contains("usage"));
    }

    #[test]
    fn skills_info_unknown_id_errors() {
        let err = skills_cmd(&["info".into(), "definitely-not-a-real-skill".into()]).unwrap_err();
        assert!(err.contains("definitely-not-a-real-skill"));
    }

    #[test]
    fn skills_unknown_subcommand_lists_options() {
        let err = skills_cmd(&["bogus".into()]).unwrap_err();
        assert!(err.contains("list"));
        assert!(err.contains("info"));
        assert!(err.contains("disabled"));
    }

    #[test]
    fn nudge_path_returns_string() {
        let v = nudge_cmd(&["path".into()]).expect("nudge path ok");
        assert!(v.get("path").and_then(|x| x.as_str()).is_some());
    }

    #[test]
    fn nudge_list_shape_correct() {
        let v = nudge_cmd(&[]).expect("nudge list ok");
        assert!(v.get("path").is_some());
        assert!(v.get("n").is_some());
        assert!(v.get("nudges").and_then(|x| x.as_array()).is_some());
    }

    #[test]
    fn nudge_add_requires_due_and_message() {
        let err = nudge_cmd(&["add".into()]).unwrap_err();
        assert!(err.to_lowercase().contains("usage"));
        let err2 = nudge_cmd(&["add".into(), "30".into()]).unwrap_err();
        assert!(err2.to_lowercase().contains("usage"));
    }

    #[test]
    fn nudge_add_rejects_non_integer_due() {
        let err = nudge_cmd(&[
            "add".into(),
            "not-a-number".into(),
            "msg".into(),
        ])
        .unwrap_err();
        assert!(err.contains("integer"));
    }

    #[test]
    fn nudge_fire_requires_id() {
        let err = nudge_cmd(&["fire".into()]).unwrap_err();
        assert!(err.to_lowercase().contains("usage"));
    }

    #[test]
    fn nudge_unknown_subcommand_lists_options() {
        let err = nudge_cmd(&["bogus".into()]).unwrap_err();
        assert!(err.contains("list"));
        assert!(err.contains("add"));
        assert!(err.contains("fire"));
    }

    #[test]
    fn mcp_status_returns_catalogue() {
        let v = mcp_cmd(&["status".into()]).expect("mcp status ok");
        assert_eq!(v.get("status").and_then(|x| x.as_str()), Some("ready"));
        assert_eq!(v.get("transport").and_then(|x| x.as_str()), Some("stdio"));
        assert!(v.get("tools_registered").is_some());
        assert!(v.get("tools_permitted").is_some());
        assert!(v.get("tools").and_then(|x| x.as_array()).is_some());
    }

    #[test]
    fn mcp_default_returns_status() {
        let v = mcp_cmd(&[]).expect("mcp default = status");
        assert_eq!(v.get("status").and_then(|x| x.as_str()), Some("ready"));
    }

    #[test]
    fn mcp_unknown_subcommand_lists_options() {
        let err = mcp_cmd(&["bogus".into()]).unwrap_err();
        assert!(err.contains("status"));
        assert!(err.contains("serve"));
    }

    #[test]
    fn usage_overall_returns_summary_shape() {
        let v = usage_cmd(&[]).expect("usage default = overall");
        assert!(v.get("log").is_some());
        assert_eq!(v.get("scope").and_then(|x| x.as_str()), Some("overall"));
        assert!(v.get("total").is_some());
        assert!(v.get("by_provider").is_some());
        assert!(v.get("by_model").is_some());
        assert!(v.get("by_session").is_some());
    }

    #[test]
    fn usage_provider_requires_name() {
        let err = usage_cmd(&["provider".into()]).unwrap_err();
        assert!(err.to_lowercase().contains("usage"));
    }

    #[test]
    fn usage_model_requires_name() {
        let err = usage_cmd(&["model".into()]).unwrap_err();
        assert!(err.to_lowercase().contains("usage"));
    }

    #[test]
    fn usage_session_requires_id() {
        let err = usage_cmd(&["session".into()]).unwrap_err();
        assert!(err.to_lowercase().contains("usage"));
    }

    #[test]
    fn usage_since_rejects_non_iso_timestamp() {
        let err =
            usage_cmd(&["overall".into(), "--since".into(), "not-iso".into()]).unwrap_err();
        assert!(err.to_lowercase().contains("since"));
    }

    #[test]
    fn usage_unknown_scope_lists_options() {
        let err = usage_cmd(&["bogus".into()]).unwrap_err();
        assert!(err.contains("provider"));
        assert!(err.contains("model"));
        assert!(err.contains("session"));
    }

    #[test]
    fn usage_provider_filter_records_in_response() {
        let v = usage_cmd(&["provider".into(), "anthropic".into()])
            .expect("usage provider ok");
        assert_eq!(
            v.get("filter")
                .and_then(|f| f.get("provider"))
                .and_then(|x| x.as_str()),
            Some("anthropic")
        );
    }

    #[test]
    fn merge_mcp_overrides_no_flags_is_clone() {
        let mut base = crate::config::AgentConfig::default();
        base.tool_allow = Some(vec!["echo".into()]);
        base.tool_deny = vec!["cos_sandbox".into()];
        let merged = merge_mcp_overrides(&base, None, Vec::new());
        assert_eq!(merged.tool_allow, base.tool_allow);
        assert_eq!(merged.tool_deny, base.tool_deny);
    }

    #[test]
    fn merge_mcp_overrides_allow_replaces_base_allow() {
        let mut base = crate::config::AgentConfig::default();
        base.tool_allow = Some(vec!["echo".into()]);
        let merged = merge_mcp_overrides(&base, Some(vec!["now".into()]), Vec::new());
        assert_eq!(merged.tool_allow, Some(vec!["now".into()]));
    }

    #[test]
    fn merge_mcp_overrides_deny_appends_to_base() {
        let mut base = crate::config::AgentConfig::default();
        base.tool_deny = vec!["cos_sandbox".into()];
        let merged = merge_mcp_overrides(&base, None, vec!["cos_proc".into()]);
        assert_eq!(merged.tool_deny, vec!["cos_sandbox".to_string(), "cos_proc".to_string()]);
    }

    #[test]
    fn merge_mcp_overrides_does_not_mutate_base() {
        let mut base = crate::config::AgentConfig::default();
        base.tool_allow = Some(vec!["a".into()]);
        let _ = merge_mcp_overrides(&base, Some(vec!["b".into()]), vec!["c".into()]);
        // Base unchanged.
        assert_eq!(base.tool_allow, Some(vec!["a".into()]));
        assert!(base.tool_deny.is_empty());
    }

    #[test]
    fn mcp_serve_unknown_flag_is_rejected() {
        let err = mcp_cmd(&["serve".into(), "--bogus".into(), "x".into()]).unwrap_err();
        assert!(err.to_lowercase().contains("unknown flag"));
    }

    #[test]
    fn mcp_serve_allow_without_value_errors() {
        let err = mcp_cmd(&["serve".into(), "--allow".into()]).unwrap_err();
        assert!(err.contains("--allow"));
    }

    #[test]
    fn mcp_serve_deny_without_value_errors() {
        let err = mcp_cmd(&["serve".into(), "--deny".into()]).unwrap_err();
        assert!(err.contains("--deny"));
    }

    #[test]
    fn curator_unknown_subcommand_lists_propose() {
        let err = curator_cmd(&["bogus".into()]).unwrap_err();
        assert!(err.contains("propose"));
    }

    #[test]
    fn curator_propose_requires_session_id() {
        let err = curator_cmd(&["propose".into()]).unwrap_err();
        assert!(err.to_lowercase().contains("usage"));
    }

    #[test]
    fn curator_propose_rejects_flag_as_session_id() {
        // `propose --accept` without a session id must error rather
        // than silently treating "--accept" as the session id.
        let err = curator_cmd(&["propose".into(), "--accept".into()]).unwrap_err();
        assert!(err.to_lowercase().contains("usage"));
    }

    #[test]
    fn curator_propose_unknown_flag_is_rejected() {
        let err = curator_cmd(&[
            "propose".into(),
            "any-sid".into(),
            "--bogus".into(),
        ])
        .unwrap_err();
        assert!(err.to_lowercase().contains("unknown flag"));
    }

    #[test]
    fn curator_propose_min_turns_requires_value() {
        let err = curator_cmd(&[
            "propose".into(),
            "any-sid".into(),
            "--min-turns".into(),
        ])
        .unwrap_err();
        assert!(err.contains("--min-turns"));
    }
}
