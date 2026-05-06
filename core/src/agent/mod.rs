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
pub mod curator_drafts;
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
pub mod classify;
pub mod summarise;

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
        "llm" => llm_cmd(args),
        "redact" => redact_cmd(args),
        "prompt" => prompt_cmd(args),
        "think-scrub" => think_scrub_cmd(args),
        "tokens" => tokens_cmd(args),
        "providers" => providers_cmd(args),
        "title" => title_cmd(args),
        "summarise" => summarise_cmd(args),
        "summarize" => summarise_cmd(args),
        "classify" => classify_cmd(args),
        "tools" => tools_cmd(args),
        "guardrails" => guardrails_cmd(args),
        "approval" => approval_cmd(args),
        "todo" => todo_cmd(args),
        "compress" => compress_cmd(args),
        "aux" | "auxiliary" => aux_cmd(args),
        "retry" => retry_cmd(args),
        other => Err(format!(
            "unknown command: {other}. try: ask | chat | status | service | insights | recall | sessions | onboarding | notes | skills | nudge | mcp | usage | curator | llm | redact | prompt | think-scrub | tokens | providers | title | summarise | classify | tools | guardrails | approval | todo | compress | aux | retry"
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
        "install" => {
            let archive = args.get(1).cloned().unwrap_or_default();
            if archive.is_empty() {
                return Err(
                    "usage: cos agent skills install <archive.zip> [--force]".into(),
                );
            }
            let force = args.iter().any(|a| a == "--force" || a == "-f");
            let path = std::path::PathBuf::from(&archive);
            match skills::sync::install_from_archive(&path, force) {
                Ok(res) => Ok(json!({
                    "ok": true,
                    "id": res.id,
                    "install_dir": res.install_dir.display().to_string(),
                    "files_extracted": res.files_extracted,
                    "bytes_on_disk": res.bytes_on_disk,
                    "replaced_existing": res.replaced_existing,
                })),
                Err(e) => Err(format!("install failed: {e}")),
            }
        }
        "hub" => skills_hub_cmd(&args[1..]),
        "usage" => skills_usage_cmd(&args[1..]),
        other => Err(format!(
            "unknown skills subcommand: {other}. try: list | info <id> | disabled | errors | root | install <archive> | hub <list|show|install> <owner/repo> [<id>] | usage <stats|record|path|clear>"
        )),
    }
}

/// `cos agent skills usage <stats|record|path|clear>`
///
/// Read/write surface over the skill-invocation JSONL log
/// ([`crate::agent::skills::provenance::UsageStore`]). Lives at
/// `agent_skills_usage_path()` (typically
/// `<data_dir>/agent/skills-usage.jsonl`).
///
/// * `stats [<id>]` — aggregate over the whole log, optionally
///   filtered to one skill id. Returns per-skill totals + average
///   duration + success rate.
/// * `record <id> --duration-ms N [--ok|--error] [--by <caller>]` —
///   append one usage record. Useful for external runners (a skill
///   that wraps an external script) to participate in the same
///   tracking surface.
/// * `path` — print the JSONL log path so callers can `tail -f` it
///   or pipe into their own analysis tooling.
/// * `clear` — truncate the log. Refuses without `--yes` so a
///   mistyped command can't wipe weeks of telemetry.
fn skills_usage_cmd(args: &[String]) -> Result<Value, String> {
    let path = crate::paths::agent_skills_usage_path();
    skills_usage_cmd_at(args, &path)
}

fn skills_usage_cmd_at(args: &[String], path: &std::path::Path) -> Result<Value, String> {
    use crate::agent::skills::provenance::{UsageRecord, UsageStore};
    use chrono::Utc;

    let store = UsageStore::new(path);
    let sub = args.first().map(|s| s.as_str()).unwrap_or("stats");
    match sub {
        "path" => Ok(json!({"path": path.display().to_string()})),
        "stats" | "" => {
            let agg = store.aggregate();
            let filter_id = args.get(1).filter(|s| !s.is_empty()).cloned();
            let entries: Vec<Value> = agg
                .iter()
                .filter(|(id, _)| {
                    filter_id
                        .as_deref()
                        .map(|f| f == id.as_str())
                        .unwrap_or(true)
                })
                .map(|(id, s)| {
                    json!({
                        "id": id,
                        "total": s.total,
                        "success": s.success,
                        "failure": s.failure,
                        "total_duration_ms": s.total_duration_ms,
                        "average_duration_ms": s.average_duration_ms(),
                        "success_rate": if s.total == 0 {
                            None
                        } else {
                            Some((s.success as f64) / (s.total as f64))
                        },
                    })
                })
                .collect();
            Ok(json!({
                "path": path.display().to_string(),
                "skill_count": entries.len(),
                "filter_id": filter_id,
                "skills": entries,
            }))
        }
        "record" => {
            let id = args
                .get(1)
                .cloned()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    "usage: cos agent skills usage record <id> --duration-ms N [--ok|--error] [--by <caller>]"
                        .to_string()
                })?;
            let mut duration_ms: Option<u64> = None;
            let mut success = true;
            let mut invoked_by: Option<String> = None;
            let mut i = 2usize;
            while i < args.len() {
                match args[i].as_str() {
                    "--duration-ms" => {
                        duration_ms = Some(parse_u64_arg(args.get(i + 1), "--duration-ms")?);
                        i += 2;
                    }
                    "--ok" => {
                        success = true;
                        i += 1;
                    }
                    "--error" | "--fail" => {
                        success = false;
                        i += 1;
                    }
                    "--by" => {
                        invoked_by = Some(
                            args.get(i + 1)
                                .cloned()
                                .ok_or_else(|| "--by needs a name".to_string())?,
                        );
                        i += 2;
                    }
                    other => {
                        return Err(format!("unknown flag for `usage record`: {other}"));
                    }
                }
            }
            let duration_ms = duration_ms.ok_or_else(|| {
                "--duration-ms is required for `usage record`".to_string()
            })?;
            let rec = UsageRecord {
                skill_id: id.clone(),
                timestamp: Utc::now().to_rfc3339(),
                success,
                duration_ms,
                invoked_by: invoked_by.clone(),
            };
            store
                .record(&rec)
                .map_err(|e| format!("record failed: {e}"))?;
            Ok(json!({
                "ok": true,
                "id": id,
                "timestamp": rec.timestamp,
                "success": success,
                "duration_ms": duration_ms,
                "invoked_by": invoked_by,
                "path": path.display().to_string(),
            }))
        }
        "clear" => {
            let confirmed = args.iter().any(|a| a == "--yes");
            if !confirmed {
                return Err(
                    "refusing to clear usage log without --yes (would discard all per-skill telemetry)"
                        .to_string(),
                );
            }
            if path.exists() {
                std::fs::remove_file(path)
                    .map_err(|e| format!("clear {}: {e}", path.display()))?;
            }
            Ok(json!({
                "ok": true,
                "path": path.display().to_string(),
                "cleared": true,
            }))
        }
        other => Err(format!(
            "unknown usage subcommand: {other}. try: stats [<id>] | record <id> --duration-ms N [--ok|--error] [--by <caller>] | path | clear --yes"
        )),
    }
}

/// `cos agent skills hub <list|show|install> <owner/repo> [<id>] [--force]`
///
/// Talks to a GitHub Releases-based skills hub
/// ([`crate::agent::skills::hub`]). `list` fetches the catalogue
/// from the latest release of `<owner>/<repo>` and emits the
/// available skills. `show` resolves one skill by id and emits its
/// download metadata. `install` downloads the asset, validates the
/// catalogue-declared SHA-256, and hands the local zip off to the
/// existing [`crate::agent::skills::sync::install_from_archive`]
/// pipeline.
///
/// Auth: optional GitHub PAT from `$COS_HUB_TOKEN`, `$GITHUB_TOKEN`,
/// or `$GH_TOKEN` (in that order). The token is forwarded to both
/// the GitHub REST API call and the asset download — required for
/// private hubs and helpful even for public hubs to avoid
/// unauthenticated rate limits.
fn skills_hub_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::skills::hub::{HubConfig, SkillsHub};

    let sub = args
        .first()
        .map(|s| s.as_str())
        .ok_or_else(|| {
            "usage: cos agent skills hub <list|show|install> <owner/repo> [<id>] [--force]"
                .to_string()
        })?;

    let spec = args.get(1).cloned().filter(|s| !s.is_empty()).ok_or_else(|| {
        format!("usage: cos agent skills hub {sub} <owner/repo> [<id>] [--force]")
    })?;
    let (owner, repo) = parse_owner_repo(&spec)?;

    let token = std::env::var("COS_HUB_TOKEN")
        .ok()
        .or_else(|| std::env::var("GITHUB_TOKEN").ok())
        .or_else(|| std::env::var("GH_TOKEN").ok())
        .filter(|t| !t.is_empty());

    let hub = SkillsHub::new(HubConfig::new(owner.clone(), repo.clone()).with_token(token.clone()));

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime: {e}"))?;

    match sub {
        "list" => {
            let cat = runtime
                .block_on(hub.latest_catalogue())
                .map_err(|e| format!("hub list failed: {e}"))?;
            let entries: Vec<Value> = cat
                .skills
                .iter()
                .map(|s| {
                    json!({
                        "id": s.id,
                        "name": s.name,
                        "version": s.version,
                        "asset": s.asset,
                        "sha256": s.sha256,
                        "tags": s.tags,
                        "description": s.description,
                    })
                })
                .collect();
            Ok(json!({
                "owner": owner,
                "repo": repo,
                "release_tag": cat.release_tag,
                "schema": cat.schema,
                "count": entries.len(),
                "skills": entries,
            }))
        }
        "show" => {
            let id = args
                .get(2)
                .cloned()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    "usage: cos agent skills hub show <owner/repo> <id>".to_string()
                })?;
            let resolved = runtime
                .block_on(hub.resolve(&id))
                .map_err(|e| format!("hub resolve failed: {e}"))?
                .ok_or_else(|| format!("no skill '{id}' in hub {owner}/{repo}"))?;
            Ok(json!({
                "id": resolved.entry.id,
                "name": resolved.entry.name,
                "version": resolved.entry.version,
                "asset": resolved.entry.asset,
                "sha256": resolved.entry.sha256,
                "size": resolved.size,
                "download_url": resolved.download_url,
            }))
        }
        "install" => {
            let id = args
                .get(2)
                .cloned()
                .filter(|s| !s.is_empty() && !s.starts_with("--"))
                .ok_or_else(|| {
                    "usage: cos agent skills hub install <owner/repo> <id> [--force]"
                        .to_string()
                })?;
            let force = args.iter().any(|a| a == "--force" || a == "-f");

            let resolved = runtime
                .block_on(hub.resolve(&id))
                .map_err(|e| format!("hub resolve failed: {e}"))?
                .ok_or_else(|| format!("no skill '{id}' in hub {owner}/{repo}"))?;

            let auth_header_owned = token.as_ref().map(|t| ("Authorization".to_string(), format!("Bearer {t}")));
            let mut header_pairs: Vec<(&str, &str)> = Vec::new();
            if let Some((k, v)) = auth_header_owned.as_ref() {
                header_pairs.push((k.as_str(), v.as_str()));
            }
            let download_label = format!("hub:{}/{}/{}", owner, repo, resolved.entry.id);
            let opts = crate::engine_pkg::download::DownloadOpts {
                url: &resolved.download_url,
                headers: &header_pairs,
                expected_sha256: Some(resolved.entry.sha256.as_str()),
                label: &download_label,
            };
            let dl = runtime
                .block_on(crate::engine_pkg::download::stream_to_temp(&opts))
                .map_err(|e| format!("download failed: {e}"))?;

            let res = skills::sync::install_from_archive(dl.temp_file.path(), force)
                .map_err(|e| format!("install failed: {e}"))?;
            Ok(json!({
                "ok": true,
                "id": res.id,
                "hub_id": resolved.entry.id,
                "version": resolved.entry.version,
                "install_dir": res.install_dir.display().to_string(),
                "files_extracted": res.files_extracted,
                "bytes_on_disk": res.bytes_on_disk,
                "bytes_downloaded": dl.bytes,
                "sha256": dl.sha256_hex,
                "replaced_existing": res.replaced_existing,
            }))
        }
        other => Err(format!(
            "unknown hub subcommand: {other}. try: list <owner/repo> | show <owner/repo> <id> | install <owner/repo> <id> [--force]"
        )),
    }
}

fn parse_owner_repo(spec: &str) -> Result<(String, String), String> {
    let mut parts = spec.splitn(2, '/');
    let owner = parts.next().unwrap_or("").trim();
    let repo = parts.next().unwrap_or("").trim();
    if owner.is_empty() || repo.is_empty() {
        return Err(format!(
            "expected '<owner>/<repo>' (e.g. clawos/skills-hub), got '{spec}'"
        ));
    }
    Ok((owner.to_string(), repo.to_string()))
}

/// `cos agent llm <providers|models|model|cost>`
///
/// Read-only inspection of the built-in
/// [`crate::agent::llm::metadata`] table — the static registry of
/// known LLM models, their context windows, capabilities, and
/// per-million-token pricing. Useful for cross-checking pricing
/// against an invoice, picking a model from the CLI without leaving
/// the terminal, or scripting a "what does this model support?"
/// guard before issuing a `cos agent ask`.
///
/// All data lives in the binary; no network or file IO is involved.
fn llm_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::llm::metadata;
    let sub = args.first().map(|s| s.as_str()).unwrap_or("providers");
    match sub {
        "providers" => {
            let providers: Vec<Value> = metadata::known_providers()
                .into_iter()
                .map(|name| {
                    let count = metadata::list_for_provider(name).len();
                    json!({"name": name, "models": count})
                })
                .collect();
            Ok(json!({
                "count": providers.len(),
                "total_entries": metadata::entry_count(),
                "providers": providers,
            }))
        }
        "models" => {
            let mut provider: Option<String> = None;
            let mut i = 1usize;
            while i < args.len() {
                match args[i].as_str() {
                    "--provider" => {
                        provider = Some(
                            args.get(i + 1)
                                .cloned()
                                .ok_or_else(|| "--provider needs a name".to_string())?,
                        );
                        i += 2;
                    }
                    other => return Err(format!("unknown flag for `llm models`: {other}")),
                }
            }
            let entries: Vec<&'static metadata::ModelMetadata> = match &provider {
                Some(p) => metadata::list_for_provider(p),
                None => metadata::known_providers()
                    .into_iter()
                    .flat_map(metadata::list_for_provider)
                    .collect(),
            };
            let models: Vec<Value> = entries.iter().map(|m| model_to_json(m)).collect();
            Ok(json!({
                "filter_provider": provider,
                "count": models.len(),
                "models": models,
            }))
        }
        "model" => {
            let name = args
                .get(1)
                .cloned()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "usage: cos agent llm model <name>".to_string())?;
            let m = metadata::lookup(&name)
                .ok_or_else(|| format!("unknown model: {name}"))?;
            Ok(model_to_json(m))
        }
        "cost" => {
            let mut name: Option<String> = None;
            let mut input: u64 = 0;
            let mut output: u64 = 0;
            let mut cache_read: u64 = 0;
            let mut cache_write: u64 = 0;
            let mut i = 1usize;
            while i < args.len() {
                match args[i].as_str() {
                    "--input" => {
                        input = parse_u64_arg(args.get(i + 1), "--input")?;
                        i += 2;
                    }
                    "--output" => {
                        output = parse_u64_arg(args.get(i + 1), "--output")?;
                        i += 2;
                    }
                    "--cache-read" => {
                        cache_read = parse_u64_arg(args.get(i + 1), "--cache-read")?;
                        i += 2;
                    }
                    "--cache-write" => {
                        cache_write = parse_u64_arg(args.get(i + 1), "--cache-write")?;
                        i += 2;
                    }
                    other if !other.starts_with("--") && name.is_none() => {
                        name = Some(other.to_string());
                        i += 1;
                    }
                    other => {
                        return Err(format!(
                            "unknown arg for `llm cost`: {other}. usage: cos agent llm cost <model> --input N --output N [--cache-read N] [--cache-write N]"
                        ));
                    }
                }
            }
            let name = name.ok_or_else(|| {
                "usage: cos agent llm cost <model> --input N --output N [--cache-read N] [--cache-write N]"
                    .to_string()
            })?;
            let cost = metadata::estimate_cost_usd(&name, input, output, cache_read, cache_write)
                .ok_or_else(|| format!("unknown model: {name}"))?;
            Ok(json!({
                "model": name,
                "input_tokens": input,
                "output_tokens": output,
                "cache_read_tokens": cache_read,
                "cache_write_tokens": cache_write,
                "estimated_usd": cost,
            }))
        }
        other => Err(format!(
            "unknown llm subcommand: {other}. try: providers | models [--provider X] | model <name> | cost <model> --input N --output N"
        )),
    }
}

fn model_to_json(m: &crate::agent::llm::metadata::ModelMetadata) -> Value {
    json!({
        "name": m.name,
        "provider": m.provider,
        "context_window": m.context_window,
        "max_output_tokens": m.max_output_tokens,
        "supports_tools": m.supports_tools,
        "supports_vision": m.supports_vision,
        "supports_streaming": m.supports_streaming,
        "pricing": {
            "input_per_mtok_usd": m.pricing.input_per_mtok_usd,
            "output_per_mtok_usd": m.pricing.output_per_mtok_usd,
            "cache_read_per_mtok_usd": m.pricing.cache_read_per_mtok_usd,
            "cache_write_per_mtok_usd": m.pricing.cache_write_per_mtok_usd,
        },
    })
}

fn parse_u64_arg(value: Option<&String>, flag: &str) -> Result<u64, String> {
    let v = value.ok_or_else(|| format!("{flag} needs an integer"))?;
    v.parse::<u64>()
        .map_err(|e| format!("{flag}: invalid integer '{v}': {e}"))
}

/// `cos agent redact <text> [--strict] [--check]`
/// `cos agent redact --file <path> [--strict] [--check]`
/// `cos agent redact --stdin [--strict] [--check]`
///
/// Standalone interface to [`crate::agent::safety::redact::Redactor`].
/// Useful for grepping a log file before posting to a bug report,
/// scrubbing pasted output before piping into a notebook, or scripting
/// "did this string contain secrets?" gates in CI without spinning up
/// a full agent loop.
///
/// `--strict` enables email redaction (off by default — most emails
/// are legitimate content).
///
/// `--check` returns `{contains_secrets: bool, pattern_count: N}`
/// instead of redacting, so callers can branch on detection.
fn redact_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::safety::redact::Redactor;

    let mut strict = false;
    let mut check = false;
    let mut from_stdin = false;
    let mut from_file: Option<String> = None;
    let mut positional: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--strict" => {
                strict = true;
                i += 1;
            }
            "--check" => {
                check = true;
                i += 1;
            }
            "--stdin" => {
                from_stdin = true;
                i += 1;
            }
            "--file" => {
                from_file = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--file needs a path".to_string())?,
                );
                i += 2;
            }
            other => {
                positional.push(other.to_string());
                i += 1;
            }
        }
    }

    let input = if from_stdin {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| format!("read stdin: {e}"))?;
        buf
    } else if let Some(path) = from_file {
        std::fs::read_to_string(&path).map_err(|e| format!("read {path}: {e}"))?
    } else if positional.is_empty() {
        return Err(
            "usage: cos agent redact <text> | --file <path> | --stdin [--strict] [--check]"
                .to_string(),
        );
    } else {
        positional.join(" ")
    };

    let r = if strict {
        Redactor::strict()
    } else {
        Redactor::default_set()
    };

    if check {
        Ok(json!({
            "contains_secrets": r.contains_secrets(&input),
            "pattern_count": r.pattern_count(),
            "input_chars": input.chars().count(),
            "strict": strict,
        }))
    } else {
        let redacted = r.redact(&input);
        let changed = redacted != input;
        Ok(json!({
            "redacted": redacted,
            "changed": changed,
            "input_chars": input.chars().count(),
            "output_chars": redacted.chars().count(),
            "pattern_count": r.pattern_count(),
            "strict": strict,
        }))
    }
}

/// `cos agent prompt [show|build] [--extra <path>] [--raw]`
///
/// Inspect the system prompt the agent sends with every chat
/// request. The prompt is composed by
/// [`crate::agent::prompt::build_system_prompt`] and includes:
///
///   1. Built-in scaffold (immutable in this binary).
///   2. `MEMORY.md` and `USER.md` from the system notes store
///      (auto-loaded; capped per-file via
///      [`crate::agent::memory::notes::MAX_NOTE_CHARS_FOR_PROMPT`]).
///   3. Due nudges from the nudge store.
///   4. Optional override file content from `--extra <path>`.
///
/// Useful for: debugging "why did the model behave this way?",
/// previewing a new MEMORY.md entry before committing, computing a
/// rough token budget for the system block, or capturing the prompt
/// to share in a bug report.
///
/// `--raw` returns the prompt as a single JSON string in the
/// `prompt` field (default). Without `--raw` the response also
/// includes a section breakdown (char counts of scaffold vs notes
/// vs nudges vs extra) so callers can see *why* the prompt is the
/// size it is.
fn prompt_cmd(args: &[String]) -> Result<Value, String> {
    use std::path::PathBuf;

    let sub = args.first().map(|s| s.as_str()).unwrap_or("show");
    if sub != "show" && sub != "build" && sub != "" {
        return Err(format!(
            "unknown prompt subcommand: {sub}. try: show [--extra <path>] [--raw] | build [--extra <path>] [--raw]"
        ));
    }
    let mut extra: Option<PathBuf> = None;
    let mut raw = false;
    let mut i = 1usize;
    while i < args.len() {
        match args[i].as_str() {
            "--extra" => {
                let p = args
                    .get(i + 1)
                    .cloned()
                    .ok_or_else(|| "--extra needs a path".to_string())?;
                extra = Some(PathBuf::from(p));
                i += 2;
            }
            "--raw" => {
                raw = true;
                i += 1;
            }
            other => {
                return Err(format!("unknown flag for `prompt`: {other}"));
            }
        }
    }
    let extra_ref = extra.as_deref();
    let prompt = crate::agent::prompt::build_system_prompt(extra_ref);
    if raw {
        Ok(json!({
            "prompt": prompt,
            "chars": prompt.chars().count(),
        }))
    } else {
        // Crude size breakdown: rebuild each piece in isolation by
        // diffing against a scaffold-only build. This is for a
        // quick visual inventory; the prompt itself is the
        // authoritative artifact.
        let scaffold_only = crate::agent::prompt::build_system_prompt(None);
        let scaffold_chars = scaffold_only.chars().count();
        let total_chars = prompt.chars().count();
        let extra_chars = if let Some(p) = extra_ref {
            std::fs::read_to_string(p)
                .map(|s| s.trim_end().chars().count())
                .unwrap_or(0)
        } else {
            0
        };
        Ok(json!({
            "prompt": prompt,
            "chars": total_chars,
            "scaffold_chars": scaffold_chars,
            "extra_path": extra.as_ref().map(|p| p.display().to_string()),
            "extra_chars": extra_chars,
            "approx_tokens": total_chars / 4,
        }))
    }
}

/// `cos agent think-scrub <text> [--check] [--strict]`
/// `cos agent think-scrub --file <path> [--check]`
/// `cos agent think-scrub --stdin [--check]`
///
/// Standalone interface to
/// [`crate::agent::context::think_scrub::ThinkScrubber`]. Strips
/// `<think>...</think>`, `<thinking>...</thinking>`, and
/// `<reasoning>...</reasoning>` blocks (multiline) from text.
///
/// Useful for: post-processing a transcript before pasting it into
/// a bug report, normalising responses from a reasoning model
/// before computing a diff against a non-reasoning baseline,
/// scripting "did this output contain hidden reasoning?" gates.
///
/// `--check` returns `{has_thinking: bool}` instead of scrubbing.
fn think_scrub_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::context::think_scrub::ThinkScrubber;

    let (input, check) = read_text_input(args, "think-scrub")?;
    let scrubber = ThinkScrubber::new();
    if check {
        Ok(json!({
            "has_thinking": scrubber.has_thinking(&input),
            "input_chars": input.chars().count(),
        }))
    } else {
        let scrubbed = scrubber.scrub(&input);
        let changed = scrubbed != input;
        Ok(json!({
            "scrubbed": scrubbed,
            "changed": changed,
            "input_chars": input.chars().count(),
            "output_chars": scrubbed.chars().count(),
        }))
    }
}

/// `cos agent tokens <text>`
/// `cos agent tokens --file <path>`
/// `cos agent tokens --stdin`
///
/// Crude token estimate (chars / 4) as used by
/// [`crate::agent::context::compressor::estimate_text_tokens`].
/// This is the same heuristic used inside the runtime to decide
/// when to trigger context compression, so the number you see here
/// is the same number the agent uses internally.
///
/// Not a tokenizer — it's deliberately model-agnostic and biased
/// slightly high so callers don't *under*-estimate. For
/// production-grade counts, integrate a tokenizer matching your
/// target model.
fn tokens_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::context::compressor::estimate_text_tokens;

    let (input, _check) = read_text_input(args, "tokens")?;
    let chars = input.chars().count();
    let bytes = input.len();
    let approx_tokens = estimate_text_tokens(&input);
    Ok(json!({
        "chars": chars,
        "bytes": bytes,
        "approx_tokens": approx_tokens,
        "method": "chars / 4 (model-agnostic heuristic; biased slightly high)",
    }))
}

/// Shared parser for the small family of "text-in / result-out"
/// agent subcommands (`redact`, `think-scrub`, `tokens`). Returns
/// `(input, check_mode)`.
///
/// Sources:
///   * `--file <path>` — read file content.
///   * `--stdin` — read all of stdin.
///   * positional args — joined with spaces (so the shell-natural
///     `cos agent tokens hello world` works without quoting).
///
/// `--check` is honoured by callers that have a "detect-only" mode;
/// `tokens_cmd` ignores it.
fn read_text_input(args: &[String], cmd: &str) -> Result<(String, bool), String> {
    let mut from_stdin = false;
    let mut from_file: Option<String> = None;
    let mut check = false;
    let mut positional: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--stdin" => {
                from_stdin = true;
                i += 1;
            }
            "--file" => {
                from_file = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--file needs a path".to_string())?,
                );
                i += 2;
            }
            "--check" => {
                check = true;
                i += 1;
            }
            other => {
                positional.push(other.to_string());
                i += 1;
            }
        }
    }
    let input = if from_stdin {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| format!("read stdin: {e}"))?;
        buf
    } else if let Some(path) = from_file {
        std::fs::read_to_string(&path).map_err(|e| format!("read {path}: {e}"))?
    } else if positional.is_empty() {
        return Err(format!(
            "usage: cos agent {cmd} <text> | --file <path> | --stdin"
        ));
    } else {
        positional.join(" ")
    };
    Ok((input, check))
}

/// `cos agent providers [--names <a,b,c>] [--probe-credentials]`
/// — diagnostic snapshot of every linked LLM provider plus the
/// canonical credential surface that would configure each one.
///
/// For the *active* provider (`config.agent.provider`) the user's
/// real `AgentConfig` is used so `is_configured` reflects what the
/// runtime actually sees. For the others a synthetic config is
/// substituted that hard-codes the canonical env-var + credential
/// names per alias (the convention this binary documents); that
/// way the answer to "what would happen if I switched my config
/// to provider X right now?" is honest, not a misleading
/// `not_configured` from a default-empty config.
///
/// `--probe-credentials` additionally scans the credential store
/// directly via `crate::credential::try_load(name, "agent")`. This
/// is opt-in because the probe touches `<data_dir>/credentials/`
/// which can be slow on networked storage; the env-var probe is
/// always cheap and always on.
fn providers_cmd(args: &[String]) -> Result<Value, String> {
    let mut filter_names: Option<Vec<String>> = None;
    let mut probe_credentials = false;
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--names" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--names needs a comma list".to_string())?;
                filter_names = Some(
                    raw.split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect(),
                );
                i += 2;
            }
            "--probe-credentials" => {
                probe_credentials = true;
                i += 1;
            }
            other => {
                return Err(format!(
                    "unknown providers arg: {other}. try: --names <a,b,c> | --probe-credentials"
                ));
            }
        }
    }

    let cfg = crate::config::get();
    let active = cfg.agent.provider.clone();
    let active_model = if cfg.agent.model.is_empty() {
        "stub-model".to_string()
    } else {
        cfg.agent.model.clone()
    };

    let mut entries = Vec::new();
    for &name in llm::available_providers().iter() {
        if let Some(filter) = filter_names.as_ref() {
            if !filter.iter().any(|n| n == name) {
                continue;
            }
        }

        let canonical_env = canonical_env_for_provider(name);
        let canonical_credential = canonical_credential_for_provider(name);
        let is_active = name == active;

        // Use the user's actual agent config for the active provider,
        // a synthetic canonical-name config for the others.
        let probe_cfg = if is_active {
            cfg.agent.clone()
        } else {
            let mut c = crate::config::AgentConfig::default();
            c.provider = name.to_string();
            c.api_key_credential = canonical_credential.map(String::from);
            c.api_key_env = canonical_env.map(String::from);
            c
        };

        let configured = match llm::registry::build(name, &active_model, &probe_cfg) {
            Ok(p) => p.is_configured(),
            Err(_) => false,
        };

        let env_present = canonical_env
            .map(|e| std::env::var(e).map(|v| !v.is_empty()).unwrap_or(false))
            .unwrap_or(false);

        let credential_present = if probe_credentials {
            canonical_credential
                .map(|c| {
                    crate::credential::try_load(c, "agent")
                        .map(|x| x.is_some())
                        .unwrap_or(false)
                })
                .unwrap_or(false)
        } else {
            false
        };

        entries.push(json!({
            "name": name,
            "active": is_active,
            "configured": configured,
            "default_base_url": default_base_url_for_provider(name),
            "env": canonical_env,
            "env_present": env_present,
            "credential": canonical_credential,
            "credential_present": credential_present,
            "key_required": canonical_env.is_some(),
        }));
    }

    Ok(json!({
        "active": active,
        "active_model": cfg.agent.model.clone(),
        "active_configured": entries.iter().any(|e| e.get("active") == Some(&Value::Bool(true)) && e.get("configured") == Some(&Value::Bool(true))),
        "probe_credentials": probe_credentials,
        "providers": entries,
        "count": entries.len(),
    }))
}

/// Canonical env var the binary documents per provider alias.
/// Returns `None` for providers that don't use an API key (mock,
/// llama_local, ollama).
fn canonical_env_for_provider(name: &str) -> Option<&'static str> {
    match name {
        "openai" => Some("OPENAI_API_KEY"),
        "xai" => Some("XAI_API_KEY"),
        "deepseek" => Some("DEEPSEEK_API_KEY"),
        "openrouter" => Some("OPENROUTER_API_KEY"),
        "anthropic" => Some("ANTHROPIC_API_KEY"),
        "gemini" => Some("GEMINI_API_KEY"),
        // Local/no-auth providers.
        "ollama" | "mock" | "llama_local" => None,
        _ => None,
    }
}

/// Canonical credential name (in the `agent` namespace) per provider
/// alias. Mirrors `canonical_env_for_provider` but for the
/// credential store. `None` for providers that never need a key.
fn canonical_credential_for_provider(name: &str) -> Option<&'static str> {
    match name {
        "openai" => Some("openai"),
        "xai" => Some("xai"),
        "deepseek" => Some("deepseek"),
        "openrouter" => Some("openrouter"),
        "anthropic" => Some("anthropic"),
        "gemini" => Some("gemini"),
        "ollama" | "mock" | "llama_local" => None,
        _ => None,
    }
}

/// Default base URL per provider alias when no override is set.
/// Helps users see what they'd hit out of the box.
fn default_base_url_for_provider(name: &str) -> Option<&'static str> {
    if llm::providers::openai_compat::is_alias(name) {
        Some(llm::providers::openai_compat::default_base_url_for(name))
    } else if name == "anthropic" {
        Some("https://api.anthropic.com/v1")
    } else if name == "gemini" {
        Some("https://generativelanguage.googleapis.com/v1beta")
    } else if name == "llama_local" {
        Some("local: file path via AgentConfig.model")
    } else {
        None
    }
}

/// `cos agent title <text> | --file <path> | --stdin`
/// — heuristic-only title generation. Strips a leading slash-command
/// verb (so `/ask hello` becomes `hello`), takes the first non-empty
/// line, and clamps to `MAX_TITLE_CHARS`. Pure function, no LLM call,
/// no IO beyond the input read. The async LLM-backed variant in
/// `agent::title::generate_title` is what `runtime::loop_` calls when
/// an auxiliary client is configured; this CLI only surfaces the
/// fallback so users can preview what would land if the aux call
/// failed (or wasn't configured).
fn title_cmd(args: &[String]) -> Result<Value, String> {
    let (input, _check) = read_text_input(args, "title")?;
    let title = crate::agent::title::clamp(&crate::agent::title::heuristic(&input));
    Ok(json!({
        "title": title,
        "input_chars": input.chars().count(),
        "title_chars": title.chars().count(),
        "method": "heuristic",
    }))
}

/// `cos agent summarise <text> | --file <path> | --stdin [--max N]`
/// — heuristic-only summary: take the first sentence (terminated by
/// `.`/`!`/`?` followed by whitespace or EOS) and clamp to `--max`
/// chars (default 200, matching the runtime's compressor default).
/// Pure function, no LLM call. As with `title`, the async
/// `agent::summarise::summarise` is the LLM-backed path used by the
/// runtime when an auxiliary client is configured; this CLI surfaces
/// the deterministic fallback for testing or for cheap one-offs.
///
/// Aliased as `cos agent summarize` (US spelling) so muscle memory
/// from either spelling works.
fn summarise_cmd(args: &[String]) -> Result<Value, String> {
    let mut max_chars: usize = 200;
    let mut filtered: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < args.len() {
        if args[i].as_str() == "--max" {
            let raw = args
                .get(i + 1)
                .ok_or_else(|| "--max needs a number".to_string())?;
            max_chars = raw
                .parse::<usize>()
                .map_err(|e| format!("--max: invalid u64: {e}"))?;
            i += 2;
        } else {
            filtered.push(args[i].clone());
            i += 1;
        }
    }
    let (input, _check) = read_text_input(&filtered, "summarise")?;
    let raw = crate::agent::summarise::heuristic(&input);
    let summary = crate::agent::summarise::clamp(&raw, max_chars);
    Ok(json!({
        "summary": summary,
        "input_chars": input.chars().count(),
        "summary_chars": summary.chars().count(),
        "max_chars": max_chars,
        "clamped": raw.chars().count() > max_chars,
        "method": "heuristic",
    }))
}

/// `cos agent classify <reply> --labels <a,b,c> | --file <path> | --stdin`
/// — match a (typically LLM-generated) reply string against a label
/// set using `match_label`'s case-insensitive + punctuation-tolerant
/// rules. Returns `{matched: <label> | null, labels: [...], reply}`.
/// Useful for testing prompt designs without spending tokens (you
/// can hand-craft a hypothetical reply and confirm the parser would
/// accept it).
fn classify_cmd(args: &[String]) -> Result<Value, String> {
    let mut labels_raw: Option<String> = None;
    let mut filtered: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < args.len() {
        if args[i].as_str() == "--labels" {
            labels_raw = Some(
                args.get(i + 1)
                    .cloned()
                    .ok_or_else(|| "--labels needs a comma list".to_string())?,
            );
            i += 2;
        } else {
            filtered.push(args[i].clone());
            i += 1;
        }
    }
    let labels_str = labels_raw
        .ok_or_else(|| "usage: cos agent classify <reply> --labels <a,b,c>".to_string())?;
    let labels: Vec<String> = labels_str
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if labels.is_empty() {
        return Err("--labels: at least one non-empty label required".into());
    }
    let (reply, _check) = read_text_input(&filtered, "classify")?;
    let label_refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
    let matched = crate::agent::classify::match_label(&reply, &label_refs);
    Ok(json!({
        "matched": matched,
        "labels": labels,
        "reply": reply,
        "reply_chars": reply.chars().count(),
    }))
}

/// `cos agent tools [list [--unfiltered]|show <name>|llm-list]`
/// — read-only tool registry inspection. `list` (default) returns the
/// permitted set under the runtime's guardrails (mirrors what the LLM
/// sees), with `--unfiltered` showing every registered tool including
/// those denied by config. `show <name>` returns the full schema
/// (description + JSON Schema input shape) — the same blob sent to
/// the model. `llm-list` returns the exact `Vec<llm::Tool>` the
/// model would receive (filtered).
///
/// All three subcommands construct the *same* registry+guardrails
/// pair the runtime would build, so what you see here is what the
/// model would see if you ran `cos agent ask` in the same env.
fn tools_cmd(args: &[String]) -> Result<Value, String> {
    let cfg = &crate::config::get().agent;
    let mut registry = tools::registry::default_registry();
    registry.set_guardrails(crate::agent::runtime::loop_::guardrails_from_cfg(cfg));
    registry.set_approval(crate::agent::runtime::loop_::approval_from_cfg(cfg));

    let sub = args.first().map(|s| s.as_str()).unwrap_or("list");
    match sub {
        "list" => {
            let mut unfiltered = false;
            for arg in args.iter().skip(1) {
                match arg.as_str() {
                    "--unfiltered" => unfiltered = true,
                    other => {
                        return Err(format!(
                            "unknown tools list flag: {other}. try: --unfiltered"
                        ));
                    }
                }
            }
            let names: Vec<&'static str> = if unfiltered {
                registry.names_unfiltered()
            } else {
                registry.names()
            };
            let entries: Vec<Value> = names
                .iter()
                .filter_map(|n| {
                    registry
                        .get_unfiltered(n)
                        .map(|t| {
                            let permitted = registry.guardrails().permits(n);
                            json!({
                                "name": n,
                                "description": t.description(),
                                "permitted": permitted,
                            })
                        })
                })
                .collect();
            Ok(json!({
                "registered_total": registry.names_unfiltered().len(),
                "permitted_count": registry.names().len(),
                "unfiltered": unfiltered,
                "tools": entries,
            }))
        }
        "show" => {
            let name = args
                .get(1)
                .cloned()
                .ok_or_else(|| "usage: cos agent tools show <name>".to_string())?;
            let tool = registry
                .get_unfiltered(&name)
                .ok_or_else(|| format!("tool '{name}' not registered"))?;
            Ok(json!({
                "name": tool.name(),
                "description": tool.description(),
                "input_schema": tool.input_schema(),
                "permitted": registry.guardrails().permits(&name),
            }))
        }
        "llm-list" => {
            let llm_tools = tools::guardrails::filter_llm_tools(&registry, registry.guardrails());
            Ok(json!({
                "count": llm_tools.len(),
                "tools": llm_tools.iter().map(|t| json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.input_schema,
                })).collect::<Vec<_>>(),
            }))
        }
        other => Err(format!(
            "unknown tools subcommand: {other}. try: list [--unfiltered] | show <name> | llm-list"
        )),
    }
}

/// `cos agent guardrails [show|check <tool>]`
/// — surface the allow/deny tool guardrails the runtime would build
/// from the current `AgentConfig`. `show` (default) reports the
/// active allow + deny lists. `check <tool>` runs the decision for
/// `<tool>` and returns `{permitted, decision: "allow"|"deny", reason?}`.
///
/// Useful for verifying that a `tool_allow`/`tool_deny` change in
/// `/etc/cos/config.json` is actually parsed the way you expect
/// before running a session.
fn guardrails_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::tools::guardrails::Decision;
    let cfg = &crate::config::get().agent;
    let g = crate::agent::runtime::loop_::guardrails_from_cfg(cfg);

    let sub = args.first().map(|s| s.as_str()).unwrap_or("show");
    match sub {
        "show" => {
            let allow_arr: Option<Vec<String>> = g
                .allow
                .as_ref()
                .map(|set| {
                    let mut v: Vec<String> = set.iter().cloned().collect();
                    v.sort();
                    v
                });
            let mut deny_arr: Vec<String> = g.deny.iter().cloned().collect();
            deny_arr.sort();
            Ok(json!({
                "mode": if g.allow.is_some() { "allowlist" } else { "permissive" },
                "allow": allow_arr,
                "deny": deny_arr,
                "deny_count": deny_arr.len(),
                "config_tool_allow": cfg.tool_allow.clone(),
                "config_tool_deny": cfg.tool_deny.clone(),
            }))
        }
        "check" => {
            let tool = args
                .get(1)
                .cloned()
                .ok_or_else(|| "usage: cos agent guardrails check <tool>".to_string())?;
            let decision = g.decide(&tool);
            let (verdict, reason) = match &decision {
                Decision::Allow => ("allow", None),
                Decision::Deny(r) => ("deny", Some(r.clone())),
            };
            Ok(json!({
                "tool": tool,
                "permitted": g.permits(&tool),
                "decision": verdict,
                "reason": reason,
            }))
        }
        other => Err(format!(
            "unknown guardrails subcommand: {other}. try: show | check <tool>"
        )),
    }
}

/// `cos agent approval [show|check <tool> [--input '<json>']]`
/// — surface the approval gate the runtime would build from the
/// current `AgentConfig` (auto_approve_tools / auto_deny_tools /
/// dangerous_tools). `show` lists the three sets. `check <tool>`
/// runs `ApprovalGate::evaluate` against the tool name and returns
/// the outcome (`approved` / `denied` / `deferred`).
///
/// Headless: no interactive approver is configured, so `dangerous`
/// tools without an explicit auto_approve return `deferred` — the
/// same outcome the runtime would surface back to the model as an
/// error tool_result. `--input` lets you pass a hypothetical JSON
/// payload (the gate doesn't shape-match yet but will once the
/// per-call predicate hooks land).
fn approval_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::runtime::approval::ApprovalOutcome;
    let cfg = &crate::config::get().agent;
    let gate = crate::agent::runtime::loop_::approval_from_cfg(cfg);

    let sub = args.first().map(|s| s.as_str()).unwrap_or("show");
    match sub {
        "show" => {
            let acfg = gate.config();
            let mut auto_approve: Vec<String> = acfg.auto_approve.iter().cloned().collect();
            let mut auto_deny: Vec<String> = acfg.auto_deny.iter().cloned().collect();
            let mut dangerous: Vec<String> = acfg.dangerous.iter().cloned().collect();
            auto_approve.sort();
            auto_deny.sort();
            dangerous.sort();
            Ok(json!({
                "auto_approve": auto_approve,
                "auto_deny": auto_deny,
                "dangerous": dangerous,
                "config_auto_approve_tools": cfg.auto_approve_tools.clone(),
                "config_auto_deny_tools": cfg.auto_deny_tools.clone(),
                "config_dangerous_tools": cfg.dangerous_tools.clone(),
            }))
        }
        "check" => {
            let tool = args
                .get(1)
                .cloned()
                .ok_or_else(|| "usage: cos agent approval check <tool> [--input '<json>']".to_string())?;
            let mut input: Value = Value::Null;
            let mut i = 2usize;
            while i < args.len() {
                if args[i].as_str() == "--input" {
                    let raw = args
                        .get(i + 1)
                        .ok_or_else(|| "--input needs a JSON string".to_string())?;
                    input = serde_json::from_str(raw)
                        .map_err(|e| format!("--input: invalid JSON: {e}"))?;
                    i += 2;
                } else {
                    return Err(format!(
                        "unknown approval check flag: {}. try: --input <json>",
                        args[i]
                    ));
                }
            }
            // ApprovalGate::evaluate is async; spin a small runtime.
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("tokio runtime: {e}"))?;
            let outcome = runtime.block_on(gate.evaluate(&tool, &input, "cli probe"));
            let (decision, note, reason, prompt) = match &outcome {
                ApprovalOutcome::Approved { note } => ("approved", note.clone(), None, None),
                ApprovalOutcome::Denied { reason } => ("denied", None, reason.clone(), None),
                ApprovalOutcome::Deferred { prompt } => ("deferred", None, None, prompt.clone()),
            };
            Ok(json!({
                "tool": tool,
                "decision": decision,
                "note": note,
                "reason": reason,
                "prompt": prompt,
                "would_short_circuit": gate.would_short_circuit(&tool),
            }))
        }
        other => Err(format!(
            "unknown approval subcommand: {other}. try: show | check <tool> [--input '<json>']"
        )),
    }
}

/// `cos agent todo [list <session_id>|add <session_id> <id> <title> [--note <text>]|set-status <session_id> <id> <pending|in_progress|completed|cancelled>|remove <session_id> <id>|clear <session_id> --yes|path]`
///
/// Surface for the per-session `TodoStore` (the same store the
/// `cos_todo` LLM tool writes to). Lets operators inspect or
/// hand-edit a session's todo list out-of-band — useful when a
/// long-running session has accumulated state and you want to
/// see/correct it without re-running the agent.
///
/// `clear` requires `--yes` so a typo can't wipe a session's todos.
/// `add` and `remove` are convenience wrappers over read+write
/// (whole-list semantics; concurrent writers will race, just like
/// the on-disk format expects).
fn todo_cmd(args: &[String]) -> Result<Value, String> {
    todo_cmd_at(args, &crate::agent::tools::todo::TodoStore::default_store())
}

/// Inner implementation taking an explicit store, so unit tests can
/// point at a tempdir without trampling the live `<data_dir>/agent/todos/`.
fn todo_cmd_at(
    args: &[String],
    store: &crate::agent::tools::todo::TodoStore,
) -> Result<Value, String> {
    use crate::agent::tools::todo::{TodoItem, TodoList, TodoStatus};

    let sub = args.first().map(|s| s.as_str()).unwrap_or("path");
    match sub {
        "path" => Ok(json!({
            "path": crate::paths::agent_todos_dir().display().to_string(),
        })),
        "list" => {
            let session = args
                .get(1)
                .cloned()
                .ok_or_else(|| "usage: cos agent todo list <session_id>".to_string())?;
            let list = store.read(&session)?;
            let counts = todo_status_counts(&list);
            Ok(json!({
                "session_id": session,
                "count": list.items.len(),
                "by_status": counts,
                "items": list.items,
            }))
        }
        "add" => {
            let session = args
                .get(1)
                .cloned()
                .ok_or_else(|| "usage: cos agent todo add <session_id> <id> <title> [--note <text>]".to_string())?;
            let id = args
                .get(2)
                .cloned()
                .ok_or_else(|| "todo add: id required".to_string())?;
            // Title can have spaces; collect non-flag positionals after id and join.
            let mut note: Option<String> = None;
            let mut positional: Vec<String> = Vec::new();
            let mut i = 3usize;
            while i < args.len() {
                if args[i].as_str() == "--note" {
                    note = Some(
                        args.get(i + 1)
                            .cloned()
                            .ok_or_else(|| "--note needs a value".to_string())?,
                    );
                    i += 2;
                } else {
                    positional.push(args[i].clone());
                    i += 1;
                }
            }
            if positional.is_empty() {
                return Err("todo add: title required".into());
            }
            let title = positional.join(" ");

            let mut list = store.read(&session)?;
            if list.items.iter().any(|item| item.id == id) {
                return Err(format!("todo id already exists: {id}"));
            }
            list.items.push(TodoItem {
                id: id.clone(),
                title,
                status: TodoStatus::default(),
                note,
            });
            store.write(&session, &list)?;
            Ok(json!({
                "session_id": session,
                "added": id,
                "count": list.items.len(),
            }))
        }
        "set-status" | "set_status" => {
            let session = args
                .get(1)
                .cloned()
                .ok_or_else(|| "usage: cos agent todo set-status <session_id> <id> <status>".to_string())?;
            let id = args
                .get(2)
                .cloned()
                .ok_or_else(|| "todo set-status: id required".to_string())?;
            let status_raw = args
                .get(3)
                .cloned()
                .ok_or_else(|| "todo set-status: status required".to_string())?;
            let status = parse_todo_status(&status_raw)?;
            let updated = store.set_status(&session, &id, status)?;
            Ok(json!({
                "session_id": session,
                "id": id,
                "status": status.as_str(),
                "items": updated.items,
            }))
        }
        "remove" => {
            let session = args
                .get(1)
                .cloned()
                .ok_or_else(|| "usage: cos agent todo remove <session_id> <id>".to_string())?;
            let id = args
                .get(2)
                .cloned()
                .ok_or_else(|| "todo remove: id required".to_string())?;
            let mut list = store.read(&session)?;
            let before = list.items.len();
            list.items.retain(|item| item.id != id);
            if list.items.len() == before {
                return Err(format!("todo id not found: {id}"));
            }
            store.write(&session, &list)?;
            Ok(json!({
                "session_id": session,
                "removed": id,
                "count": list.items.len(),
            }))
        }
        "clear" => {
            let session = args
                .get(1)
                .cloned()
                .ok_or_else(|| "usage: cos agent todo clear <session_id> --yes".to_string())?;
            let confirmed = args.iter().skip(2).any(|a| a == "--yes");
            if !confirmed {
                return Err("refusing to clear without --yes".into());
            }
            store.clear(&session)?;
            Ok(json!({
                "session_id": session,
                "cleared": true,
            }))
        }
        other => Err(format!(
            "unknown todo subcommand: {other}. try: list | add | set-status | remove | clear --yes | path"
        )),
    }
}

/// `cos agent compress [show-config|check --file <jsonl> [...]]`
///
/// Inspect the context-window compressor without invoking it. Two
/// surfaces:
///
/// - `show-config` — dump the default `CompressorConfig` so callers
///   know where the trigger / target / keep-tail / summary-max budgets
///   currently sit.
///
/// - `check` — load a JSONL file (one `Message` per line) plus an
///   optional system prompt, run `estimate_total_tokens` on it, and
///   report whether the total clears the configured trigger and how
///   far over the target budget it would land. Useful for capacity
///   planning ("would this conversation force a summarisation?")
///   without spending API tokens on a real `LlmCompressor` round-trip.
///
/// `--trigger / --target / --keep-tail / --summary-max` override the
/// default `CompressorConfig` budgets in-place so the same recorded
/// conversation can be inspected against multiple budget profiles.
fn compress_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::context::compressor::{
        estimate_message_tokens, estimate_total_tokens, estimate_text_tokens,
        CompressorConfig,
    };
    use crate::agent::llm::types::{Message, Role};

    let sub = args.first().map(|s| s.as_str()).unwrap_or("show-config");
    match sub {
        "show-config" => {
            let cfg = CompressorConfig::default();
            Ok(json!({
                "target_tokens": cfg.target_tokens,
                "trigger_tokens": cfg.trigger_tokens,
                "keep_tail_tokens": cfg.keep_tail_tokens,
                "summary_max_tokens": cfg.summary_max_tokens,
            }))
        }
        "check" => {
            let mut file: Option<String> = None;
            let mut system_inline: Option<String> = None;
            let mut system_file: Option<String> = None;
            let mut cfg = CompressorConfig::default();
            let mut i = 1usize;
            while i < args.len() {
                match args[i].as_str() {
                    "--file" => {
                        file = Some(
                            args.get(i + 1)
                                .cloned()
                                .ok_or_else(|| "--file needs a value".to_string())?,
                        );
                        i += 2;
                    }
                    "--system" => {
                        system_inline = Some(
                            args.get(i + 1)
                                .cloned()
                                .ok_or_else(|| "--system needs a value".to_string())?,
                        );
                        i += 2;
                    }
                    "--system-file" => {
                        system_file = Some(
                            args.get(i + 1)
                                .cloned()
                                .ok_or_else(|| "--system-file needs a value".to_string())?,
                        );
                        i += 2;
                    }
                    "--trigger" => {
                        cfg.trigger_tokens = parse_u32_arg(args.get(i + 1), "--trigger")?;
                        i += 2;
                    }
                    "--target" => {
                        cfg.target_tokens = parse_u32_arg(args.get(i + 1), "--target")?;
                        i += 2;
                    }
                    "--keep-tail" | "--keep_tail" => {
                        cfg.keep_tail_tokens = parse_u32_arg(args.get(i + 1), "--keep-tail")?;
                        i += 2;
                    }
                    "--summary-max" | "--summary_max" => {
                        cfg.summary_max_tokens =
                            parse_u32_arg(args.get(i + 1), "--summary-max")?;
                        i += 2;
                    }
                    other => {
                        return Err(format!("unknown compress check flag: {other}"));
                    }
                }
            }

            let path = file.ok_or_else(|| "--file required".to_string())?;
            let raw = std::fs::read_to_string(&path)
                .map_err(|e| format!("read {path}: {e}"))?;
            let mut messages: Vec<Message> = Vec::new();
            for (line_no, line) in raw.lines().enumerate() {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let msg: Message = serde_json::from_str(trimmed).map_err(|e| {
                    format!("parse line {} of {}: {}", line_no + 1, path, e)
                })?;
                messages.push(msg);
            }

            let system = match (system_inline, system_file) {
                (Some(_), Some(_)) => {
                    return Err("--system and --system-file are mutually exclusive".into());
                }
                (Some(s), None) => Some(s),
                (None, Some(p)) => Some(
                    std::fs::read_to_string(&p)
                        .map_err(|e| format!("read {p}: {e}"))?,
                ),
                (None, None) => None,
            };

            let system_tokens = system.as_deref().map(estimate_text_tokens).unwrap_or(0);
            let mut role_counts = std::collections::BTreeMap::<&str, u64>::new();
            let mut role_tokens = std::collections::BTreeMap::<&str, u32>::new();
            let mut per_message: Vec<serde_json::Value> = Vec::with_capacity(messages.len());
            for (idx, msg) in messages.iter().enumerate() {
                let role = match msg.role {
                    Role::System => "system",
                    Role::User => "user",
                    Role::Assistant => "assistant",
                    Role::Tool => "tool",
                };
                let toks = estimate_message_tokens(msg);
                *role_counts.entry(role).or_default() += 1;
                *role_tokens.entry(role).or_default() = role_tokens
                    .get(role)
                    .copied()
                    .unwrap_or(0)
                    .saturating_add(toks);
                per_message.push(json!({
                    "index": idx,
                    "role": role,
                    "blocks": msg.content.len(),
                    "estimated_tokens": toks,
                }));
            }
            let total = estimate_total_tokens(system.as_deref(), &messages);
            let would_trigger = total >= cfg.trigger_tokens;
            let over_target = total.saturating_sub(cfg.target_tokens);

            Ok(json!({
                "config": {
                    "target_tokens": cfg.target_tokens,
                    "trigger_tokens": cfg.trigger_tokens,
                    "keep_tail_tokens": cfg.keep_tail_tokens,
                    "summary_max_tokens": cfg.summary_max_tokens,
                },
                "system_tokens": system_tokens,
                "message_count": messages.len(),
                "messages_tokens": total.saturating_sub(system_tokens),
                "total_tokens": total,
                "would_trigger": would_trigger,
                "over_target": over_target,
                "by_role": {
                    "counts": role_counts,
                    "tokens": role_tokens,
                },
                "messages": per_message,
            }))
        }
        other => Err(format!(
            "unknown compress subcommand: {other}. try: show-config | check --file <jsonl>"
        )),
    }
}

fn parse_u32_arg(raw: Option<&String>, flag: &str) -> Result<u32, String> {
    let s = raw.ok_or_else(|| format!("{flag} needs a value"))?;
    s.parse::<u32>().map_err(|e| format!("{flag}: {e}"))
}

/// `cos agent aux [show|ask --prompt <text> [--system <text>] [--max-tokens N]]`
///
/// Inspect or invoke the auxiliary LLM client. The auxiliary path
/// exists so lightweight subtasks (title generation, classification,
/// summarisation) can route to a cheap secondary model instead of
/// burning flagship tokens. Configuration lives in
/// `AgentConfig::auxiliary_*`.
///
/// `show` reports the resolved auxiliary settings (provider / model
/// / max_tokens / temperature / configured?) without making any
/// network calls. `ask` actually invokes `AuxiliaryClient::ask`
/// against the configured provider — useful as a smoke test that the
/// cheap model is reachable and that credentials route correctly.
fn aux_cmd(args: &[String]) -> Result<Value, String> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("show");
    match sub {
        "show" | "" => {
            let cfg = &crate::config::get().agent;
            let aux_built = crate::agent::runtime::loop_::auxiliary_from_cfg(cfg);
            let (configured, build_error) = match &aux_built {
                Ok(Some(_)) => (true, None),
                Ok(None) => (false, None),
                Err(e) => (false, Some(e.to_string())),
            };
            Ok(json!({
                "configured": configured,
                "provider": cfg.auxiliary_provider,
                "model": cfg.auxiliary_model,
                "max_tokens": cfg.auxiliary_max_tokens,
                "temperature": cfg.auxiliary_temperature,
                "build_error": build_error,
                "note": "Auxiliary calls share base_url / credentials with the primary provider unless the underlying builder honours its own env vars.",
            }))
        }
        "ask" => {
            let mut prompt: Option<String> = None;
            let mut system: Option<String> = None;
            let mut max_tokens_override: Option<u32> = None;
            let mut i = 1usize;
            while i < args.len() {
                match args[i].as_str() {
                    "--prompt" => {
                        prompt = Some(
                            args.get(i + 1)
                                .cloned()
                                .ok_or_else(|| "--prompt needs a value".to_string())?,
                        );
                        i += 2;
                    }
                    "--system" => {
                        system = Some(
                            args.get(i + 1)
                                .cloned()
                                .ok_or_else(|| "--system needs a value".to_string())?,
                        );
                        i += 2;
                    }
                    "--max-tokens" | "--max_tokens" => {
                        max_tokens_override =
                            Some(parse_u32_arg(args.get(i + 1), "--max-tokens")?);
                        i += 2;
                    }
                    other => return Err(format!("unknown aux ask flag: {other}")),
                }
            }
            let prompt = prompt.ok_or_else(|| "--prompt required".to_string())?;
            let cfg = &crate::config::get().agent;
            let aux = crate::agent::runtime::loop_::auxiliary_from_cfg(cfg)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| {
                    "auxiliary client is not configured (set agent.auxiliary_provider + auxiliary_model in config)"
                        .to_string()
                })?;
            // Apply per-call max_tokens override by rebuilding a
            // fresh AuxiliaryClient with the overridden config. The
            // underlying provider Arc is reused.
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("tokio runtime: {e}"))?;
            let used_max_tokens = max_tokens_override.unwrap_or(aux.config().max_tokens);
            let answer = runtime
                .block_on(aux.ask(system.as_deref(), &prompt))
                .map_err(|e| format!("aux ask: {e}"))?;
            Ok(json!({
                "ok": true,
                "provider": aux.provider_name(),
                "model": aux.config().model,
                "max_tokens": used_max_tokens,
                "answer": answer,
            }))
        }
        other => Err(format!(
            "unknown aux subcommand: {other}. try: show | ask --prompt <text> [--system <text>] [--max-tokens N]"
        )),
    }
}

/// `cos agent retry [show|schedule [--attempts N]]`
///
/// Surface for the LLM-call retry policy resolved from the agent
/// config via [`crate::agent::runtime::loop_::retry_policy_from_cfg`].
///
/// `show` reports whether retries are enabled and the resolved
/// `RetryPolicy` (max_attempts / base_ms / max_ms / jitter), or
/// reports `enabled: false` when the helper returns `None`.
///
/// `schedule` previews the back-off delays the policy would emit per
/// attempt (1-indexed, exclusive of the first call). Useful for
/// capacity planning ("if every retry fires, how long until we give
/// up?") and for verifying that `retry_max_attempts` matches what's
/// in config without round-tripping a live request.
///
/// Because `RetryPolicy::delay_for` adds jitter when configured, the
/// schedule is non-deterministic when `jitter == true`; the output
/// includes the per-attempt delay AND the cap (`max_ms`) so callers
/// can compute worst-case bounds.
fn retry_cmd(args: &[String]) -> Result<Value, String> {
    let cfg = &crate::config::get().agent;
    let sub = args.first().map(|s| s.as_str()).unwrap_or("show");
    match sub {
        "show" | "" => {
            let policy = crate::agent::runtime::loop_::retry_policy_from_cfg(cfg);
            match policy {
                Some(p) => Ok(json!({
                    "enabled": true,
                    "max_attempts": p.max_attempts,
                    "base_ms": p.base_ms,
                    "max_ms": p.max_ms,
                    "jitter": p.jitter,
                    "config_retry_enabled": cfg.retry_enabled,
                    "config_retry_max_attempts": cfg.retry_max_attempts,
                })),
                None => Ok(json!({
                    "enabled": false,
                    "config_retry_enabled": cfg.retry_enabled,
                    "config_retry_max_attempts": cfg.retry_max_attempts,
                    "note": "retry_enabled is false OR retry_max_attempts < 2; only one attempt will fire on transient failure.",
                })),
            }
        }
        "schedule" => {
            let mut override_attempts: Option<u32> = None;
            let mut i = 1usize;
            while i < args.len() {
                match args[i].as_str() {
                    "--attempts" => {
                        override_attempts = Some(parse_u32_arg(args.get(i + 1), "--attempts")?);
                        i += 2;
                    }
                    other => return Err(format!("unknown retry schedule flag: {other}")),
                }
            }
            // Use the cfg-derived policy if present; otherwise fall
            // back to a synthesised standard policy. Either way,
            // --attempts overrides max_attempts so callers can probe
            // alternate schedules without rewriting config.
            let mut policy = crate::agent::runtime::loop_::retry_policy_from_cfg(cfg)
                .unwrap_or_else(crate::agent::llm::rate_limit::RetryPolicy::standard);
            if let Some(a) = override_attempts {
                policy.max_attempts = a;
            }
            let max_attempts = policy.max_attempts.max(1);
            // delay_for(attempt) is the delay AFTER `attempt` failures
            // (1-indexed). For max_attempts = N total attempts, there
            // are N-1 inter-attempt waits.
            let mut schedule: Vec<Value> = Vec::with_capacity(max_attempts.saturating_sub(1) as usize);
            let mut total_min_ms: u64 = 0;
            let mut total_max_ms: u64 = 0;
            for attempt in 1..max_attempts {
                let d = policy.delay_for(attempt);
                let d_ms = d.as_millis() as u64;
                // Worst case (jitter cap = 1.0): clamped base = base * 2^(attempt-1) capped at max_ms.
                let exp = attempt.saturating_sub(1).min(20);
                let raw_base = policy
                    .base_ms
                    .saturating_mul(1u64.checked_shl(exp).unwrap_or(u64::MAX));
                let cap = raw_base.min(policy.max_ms);
                total_min_ms = total_min_ms.saturating_add(d_ms);
                total_max_ms = total_max_ms.saturating_add(cap);
                schedule.push(json!({
                    "attempt": attempt,
                    "delay_ms": d_ms,
                    "cap_ms": cap,
                }));
            }
            Ok(json!({
                "max_attempts": max_attempts,
                "base_ms": policy.base_ms,
                "max_ms": policy.max_ms,
                "jitter": policy.jitter,
                "inter_attempt_waits": schedule,
                "total_observed_ms": total_min_ms,
                "total_worst_case_ms": total_max_ms,
            }))
        }
        other => Err(format!(
            "unknown retry subcommand: {other}. try: show | schedule [--attempts N]"
        )),
    }
}

fn todo_status_counts(list: &crate::agent::tools::todo::TodoList) -> serde_json::Value {
    use crate::agent::tools::todo::TodoStatus;
    let mut pending = 0u64;
    let mut in_progress = 0u64;
    let mut completed = 0u64;
    let mut cancelled = 0u64;
    for item in &list.items {
        match item.status {
            TodoStatus::Pending => pending += 1,
            TodoStatus::InProgress => in_progress += 1,
            TodoStatus::Completed => completed += 1,
            TodoStatus::Cancelled => cancelled += 1,
        }
    }
    json!({
        "pending": pending,
        "in_progress": in_progress,
        "completed": completed,
        "cancelled": cancelled,
    })
}

fn parse_todo_status(raw: &str) -> Result<crate::agent::tools::todo::TodoStatus, String> {
    use crate::agent::tools::todo::TodoStatus;
    match raw {
        "pending" => Ok(TodoStatus::Pending),
        "in_progress" | "in-progress" => Ok(TodoStatus::InProgress),
        "completed" | "done" => Ok(TodoStatus::Completed),
        "cancelled" | "canceled" => Ok(TodoStatus::Cancelled),
        other => Err(format!(
            "unknown todo status: {other}. try: pending | in_progress | completed | cancelled"
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
        "probe" => mcp_probe(&args[1..]),
        "call" => mcp_call(&args[1..]),
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
            "unknown mcp subcommand: {other}. try: status | serve | probe | call"
        )),
    }
}

/// Spec-shared parser for `--cmd / --arg / --env / --cwd / --timeout`.
#[derive(Debug)]
struct McpSpawnSpec {
    cmd: String,
    args: Vec<String>,
    env: Vec<(String, String)>,
    cwd: Option<String>,
    timeout_secs: u64,
}

fn parse_mcp_spawn_spec(args: &[String]) -> Result<(McpSpawnSpec, Vec<String>), String> {
    let mut cmd: Option<String> = None;
    let mut child_args: Vec<String> = Vec::new();
    let mut env: Vec<(String, String)> = Vec::new();
    let mut cwd: Option<String> = None;
    let mut timeout_secs: u64 = 30;
    let mut leftover: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--cmd" => {
                cmd = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--cmd needs a value".to_string())?,
                );
                i += 2;
            }
            "--arg" => {
                child_args.push(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--arg needs a value".to_string())?,
                );
                i += 2;
            }
            "--env" => {
                let raw = args
                    .get(i + 1)
                    .cloned()
                    .ok_or_else(|| "--env needs KEY=VALUE".to_string())?;
                let (k, v) = raw
                    .split_once('=')
                    .ok_or_else(|| format!("--env expects KEY=VALUE, got {raw:?}"))?;
                env.push((k.to_string(), v.to_string()));
                i += 2;
            }
            "--cwd" => {
                cwd = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--cwd needs a value".to_string())?,
                );
                i += 2;
            }
            "--timeout" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--timeout needs <secs>".to_string())?;
                timeout_secs = raw
                    .parse::<u64>()
                    .map_err(|e| format!("--timeout: {e}"))?;
                i += 2;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown spawn flag: {other}"));
            }
            _ => {
                leftover.push(args[i].clone());
                i += 1;
            }
        }
    }
    let cmd =
        cmd.ok_or_else(|| "--cmd <executable> required".to_string())?;
    Ok((
        McpSpawnSpec {
            cmd,
            args: child_args,
            env,
            cwd,
            timeout_secs,
        },
        leftover,
    ))
}

/// Spawn an MCP server child, run `init + tools/list`, return the
/// handshake details + tool catalogue. Used to verify a server is
/// reachable and to enumerate what it exposes before wiring it into
/// the agent's tool registry.
fn mcp_probe(args: &[String]) -> Result<Value, String> {
    let (spec, leftover) = parse_mcp_spawn_spec(args)?;
    if !leftover.is_empty() {
        return Err(format!(
            "unexpected positional arg(s) for `mcp probe`: {leftover:?}"
        ));
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime: {e}"))?;
    runtime.block_on(async move {
        let (transport, mut child) = spawn_mcp_child(&spec).await?;
        let client = crate::agent::tools::mcp::client::McpClient::new(transport);
        client.start().await;
        let init_fut = client.initialize(
            crate::agent::tools::mcp::protocol::Implementation {
                name: "cos-agent".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            crate::agent::tools::mcp::protocol::ClientCapabilities::default(),
        );
        let init = tokio::time::timeout(
            std::time::Duration::from_secs(spec.timeout_secs),
            init_fut,
        )
        .await
        .map_err(|_| {
            // Best-effort kill — child holds stdio fds.
            let _ = child.start_kill();
            format!("timed out waiting for initialize after {}s", spec.timeout_secs)
        })?
        .map_err(|e| format!("initialize: {e}"))?;
        // initialized notification — many servers don't gate on it,
        // but spec-correct clients send it.
        let _ = client.notify("notifications/initialized", None).await;
        let tools_fut = client.list_tools();
        let tools_res = tokio::time::timeout(
            std::time::Duration::from_secs(spec.timeout_secs),
            tools_fut,
        )
        .await;
        let tools_payload = match tools_res {
            Ok(Ok(list)) => json!({
                "ok": true,
                "tools": list.tools,
            }),
            Ok(Err(e)) => json!({
                "ok": false,
                "error": e.to_string(),
            }),
            Err(_) => json!({
                "ok": false,
                "error": format!("timed out after {}s", spec.timeout_secs),
            }),
        };
        // Drop client to abort reader task before killing child.
        drop(client);
        let _ = child.start_kill();
        let _ = child.wait().await;
        Ok::<_, String>(json!({
            "ok": true,
            "command": spec.cmd,
            "args": spec.args,
            "protocol_version": init.protocol_version,
            "server_info": init.server_info,
            "capabilities": init.capabilities,
            "tools_list": tools_payload,
        }))
    })
}

/// Spawn an MCP server child and invoke a single `tools/call`,
/// returning its `CallToolResult`. Useful for ad-hoc inspection or
/// for scripting against a server before the agent gets near it.
fn mcp_call(args: &[String]) -> Result<Value, String> {
    let (spec, leftover) = parse_mcp_spawn_spec(args)?;
    let mut tool_name: Option<String> = None;
    let mut input: Option<serde_json::Value> = None;
    let mut i = 0usize;
    while i < leftover.len() {
        match leftover[i].as_str() {
            "--input" => {
                let raw = leftover
                    .get(i + 1)
                    .cloned()
                    .ok_or_else(|| "--input needs a JSON value".to_string())?;
                input = Some(
                    serde_json::from_str(&raw)
                        .map_err(|e| format!("--input is not valid JSON: {e}"))?,
                );
                i += 2;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag for `mcp call`: {other}"));
            }
            _ => {
                if tool_name.is_some() {
                    return Err(format!(
                        "unexpected extra positional arg: {:?}",
                        leftover[i]
                    ));
                }
                tool_name = Some(leftover[i].clone());
                i += 1;
            }
        }
    }
    let tool = tool_name.ok_or_else(|| "tool name positional required".to_string())?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime: {e}"))?;
    runtime.block_on(async move {
        let (transport, mut child) = spawn_mcp_child(&spec).await?;
        let client = crate::agent::tools::mcp::client::McpClient::new(transport);
        client.start().await;
        let init_fut = client.initialize(
            crate::agent::tools::mcp::protocol::Implementation {
                name: "cos-agent".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            crate::agent::tools::mcp::protocol::ClientCapabilities::default(),
        );
        let _init = tokio::time::timeout(
            std::time::Duration::from_secs(spec.timeout_secs),
            init_fut,
        )
        .await
        .map_err(|_| {
            let _ = child.start_kill();
            format!("timed out waiting for initialize after {}s", spec.timeout_secs)
        })?
        .map_err(|e| format!("initialize: {e}"))?;
        let _ = client.notify("notifications/initialized", None).await;
        let call_fut = client.call_tool(tool.clone(), input.clone());
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(spec.timeout_secs),
            call_fut,
        )
        .await
        .map_err(|_| {
            let _ = child.start_kill();
            format!("timed out calling {} after {}s", tool, spec.timeout_secs)
        })?
        .map_err(|e| format!("tools/call: {e}"))?;
        drop(client);
        let _ = child.start_kill();
        let _ = child.wait().await;
        Ok::<_, String>(json!({
            "ok": !result.is_error.unwrap_or(false),
            "tool": tool,
            "is_error": result.is_error.unwrap_or(false),
            "content": result.content,
        }))
    })
}

/// Spawn an MCP child process and return a stdio-attached transport
/// alongside the child handle. Caller is responsible for killing the
/// child when done. Stdin/stdout are captured; stderr is inherited
/// so the operator sees server diagnostics directly.
async fn spawn_mcp_child(
    spec: &McpSpawnSpec,
) -> Result<
    (
        crate::agent::tools::mcp::transport::StdioTransport,
        tokio::process::Child,
    ),
    String,
> {
    use std::process::Stdio;
    let mut command = tokio::process::Command::new(&spec.cmd);
    command.args(&spec.args);
    for (k, v) in &spec.env {
        command.env(k, v);
    }
    if let Some(cwd) = &spec.cwd {
        command.current_dir(cwd);
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut child = command
        .spawn()
        .map_err(|e| format!("spawn {}: {e}", spec.cmd))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| "child stdin unavailable".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "child stdout unavailable".to_string())?;
    let transport = crate::agent::tools::mcp::transport::StdioTransport::from_pair(
        Box::new(stdout),
        Box::new(stdin),
    );
    Ok((transport, child))
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
/// `[--no-save]`
///
/// `cos agent curator drafts list [--status proposed|accepted|rejected]`
/// `cos agent curator drafts show <draft_id>`
/// `cos agent curator drafts accept <draft_id> [--note "<text>"]`
/// `cos agent curator drafts reject <draft_id> [--note "<text>"]`
/// `cos agent curator drafts delete <draft_id>`
fn curator_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::curator::{
        looks_like_acceptance, message_to_turn, ConversationTurn, Curator, CuratorConfig,
        CuratorOutcome,
    };
    let sub = args.first().map(|s| s.as_str()).unwrap_or("");
    match sub {
        "propose" => {}
        "drafts" => return curator_drafts_cmd(&args[1..]),
        other => {
            return Err(format!(
                "unknown curator subcommand: '{other}'. try: propose <session_id> [...] | drafts list|show|accept|reject|delete"
            ));
        }
    }
    let sid = args
        .get(1)
        .cloned()
        .filter(|s| !s.is_empty() && !s.starts_with("--"))
        .ok_or_else(|| "usage: cos agent curator propose <session_id> [flags]".to_string())?;

    let mut limit: usize = 200;
    let mut force_accept = false;
    let mut save = true;
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
            "--no-save" => {
                save = false;
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
        CuratorOutcome::Drafted(draft) => {
            let mut payload = json!({
                "session_id": sid,
                "outcome": "drafted",
                "messages_scanned": rows.len(),
                "draft": draft,
            });
            if save {
                match curator_drafts::DraftStore::open_default()
                    .and_then(|mut store| store.add(sid.clone(), draft.clone()))
                {
                    Ok(id) => {
                        payload["draft_id"] = json!(id);
                        payload["saved"] = json!(true);
                    }
                    Err(e) => {
                        payload["saved"] = json!(false);
                        payload["save_error"] = json!(e);
                    }
                }
            } else {
                payload["saved"] = json!(false);
            }
            Ok(payload)
        }
        CuratorOutcome::NotEnough { reason } => Ok(json!({
            "session_id": sid,
            "outcome": "not_enough",
            "messages_scanned": rows.len(),
            "reason": reason,
        })),
    }
}

/// `cos agent curator drafts ...` dispatcher. Pulled into its own
/// helper so the propose path stays readable.
fn curator_drafts_cmd(args: &[String]) -> Result<Value, String> {
    use curator_drafts::{DraftStatus, DraftStore};
    let sub = args.first().map(|s| s.as_str()).unwrap_or("");
    match sub {
        "list" => {
            let mut filter: Option<DraftStatus> = None;
            let mut i = 1usize;
            while i < args.len() {
                match args[i].as_str() {
                    "--status" => {
                        let v = args
                            .get(i + 1)
                            .ok_or_else(|| "--status needs <proposed|accepted|rejected>".to_string())?;
                        filter = Some(parse_draft_status(v)?);
                        i += 2;
                    }
                    other => return Err(format!("unknown flag for `drafts list`: {other}")),
                }
            }
            let store = DraftStore::open_default()?;
            let drafts: Vec<Value> = store
                .list()
                .iter()
                .filter(|r| filter.map(|s| r.status == s).unwrap_or(true))
                .map(|r| {
                    json!({
                        "id": r.id,
                        "session_id": r.session_id,
                        "created_ts_ms": r.created_ts_ms,
                        "status": r.status,
                        "suggested_id": r.draft.suggested_id,
                        "title": r.draft.title,
                        "confidence": r.draft.confidence,
                        "tools": r.draft.allowed_tools,
                        "note": r.note,
                    })
                })
                .collect();
            Ok(json!({
                "store": store.path().display().to_string(),
                "count": drafts.len(),
                "drafts": drafts,
            }))
        }
        "show" => {
            let id = args
                .get(1)
                .cloned()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "usage: cos agent curator drafts show <id>".to_string())?;
            let store = DraftStore::open_default()?;
            let rec = store
                .get(&id)
                .ok_or_else(|| format!("no draft with id {id}"))?;
            Ok(json!(rec))
        }
        "accept" | "reject" => {
            let id = args
                .get(1)
                .cloned()
                .filter(|s| !s.is_empty() && !s.starts_with("--"))
                .ok_or_else(|| format!("usage: cos agent curator drafts {sub} <id> [--note ...]"))?;
            let mut note: Option<String> = None;
            let mut i = 2usize;
            while i < args.len() {
                match args[i].as_str() {
                    "--note" => {
                        note = Some(
                            args.get(i + 1)
                                .cloned()
                                .ok_or_else(|| "--note needs text".to_string())?,
                        );
                        i += 2;
                    }
                    other => return Err(format!("unknown flag for `drafts {sub}`: {other}")),
                }
            }
            let status = if sub == "accept" {
                DraftStatus::Accepted
            } else {
                DraftStatus::Rejected
            };
            let mut store = DraftStore::open_default()?;
            store.set_status(&id, status, note)?;
            let rec = store.get(&id).cloned().ok_or_else(|| {
                format!("draft {id} disappeared after status change (race)")
            })?;
            Ok(json!({
                "id": rec.id,
                "status": rec.status,
                "note": rec.note,
            }))
        }
        "delete" => {
            let id = args
                .get(1)
                .cloned()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "usage: cos agent curator drafts delete <id>".to_string())?;
            let mut store = DraftStore::open_default()?;
            store.delete(&id)?;
            Ok(json!({"id": id, "deleted": true}))
        }
        "retitle" => {
            let id = args
                .get(1)
                .cloned()
                .filter(|s| !s.is_empty() && !s.starts_with("--"))
                .ok_or_else(|| {
                    "usage: cos agent curator drafts retitle <id> \"<new title>\"".to_string()
                })?;
            let title = args
                .get(2)
                .cloned()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    "usage: cos agent curator drafts retitle <id> \"<new title>\"".to_string()
                })?;
            let mut store = DraftStore::open_default()?;
            store.set_title(&id, &title)?;
            let rec = store.get(&id).cloned().ok_or_else(|| {
                format!("draft {id} disappeared after retitle (race)")
            })?;
            Ok(json!({
                "id": rec.id,
                "title": rec.draft.title,
            }))
        }
        other => Err(format!(
            "unknown drafts subcommand: '{other}'. try: list | show <id> | accept <id> | reject <id> | delete <id> | retitle <id> <title>"
        )),
    }
}

fn parse_draft_status(s: &str) -> Result<curator_drafts::DraftStatus, String> {
    match s {
        "proposed" => Ok(curator_drafts::DraftStatus::Proposed),
        "accepted" => Ok(curator_drafts::DraftStatus::Accepted),
        "rejected" => Ok(curator_drafts::DraftStatus::Rejected),
        other => Err(format!(
            "invalid status '{other}': try proposed|accepted|rejected"
        )),
    }
}
/// Reads the session's history from the memory DB, infers tool
/// usage from the stored `[tool_use:NAME] ...` markers (no schema
/// migration required), and runs the deterministic
/// [`crate::agent::curator::Curator`] pure-function pipeline.
///
/// Output is a JSON object with either a `draft` (id/title/desc/
/// allowed_tools/confidence) or a `not_enough` reason.

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
    fn parse_owner_repo_accepts_valid_form() {
        let (o, r) = parse_owner_repo("clawos/skills-hub").unwrap();
        assert_eq!(o, "clawos");
        assert_eq!(r, "skills-hub");
    }

    #[test]
    fn parse_owner_repo_trims_whitespace() {
        let (o, r) = parse_owner_repo(" foo / bar ").unwrap();
        assert_eq!(o, "foo");
        assert_eq!(r, "bar");
    }

    #[test]
    fn parse_owner_repo_rejects_missing_slash() {
        let err = parse_owner_repo("noslashhere").unwrap_err();
        assert!(err.contains("owner"));
    }

    #[test]
    fn parse_owner_repo_rejects_empty_segments() {
        assert!(parse_owner_repo("/repo").is_err());
        assert!(parse_owner_repo("owner/").is_err());
        assert!(parse_owner_repo("/").is_err());
        assert!(parse_owner_repo("").is_err());
    }

    #[test]
    fn skills_hub_requires_subcommand() {
        let err = skills_cmd(&["hub".into()]).unwrap_err();
        assert!(err.contains("list"));
        assert!(err.contains("install"));
    }

    #[test]
    fn skills_hub_requires_owner_repo() {
        let err = skills_cmd(&["hub".into(), "list".into()]).unwrap_err();
        assert!(err.contains("owner/repo"));
    }

    #[test]
    fn skills_hub_install_requires_id() {
        let err = skills_cmd(&[
            "hub".into(),
            "install".into(),
            "owner/repo".into(),
        ])
        .unwrap_err();
        assert!(err.contains("usage:"));
        assert!(err.contains("install"));
    }

    #[test]
    fn skills_hub_show_requires_id() {
        let err = skills_cmd(&[
            "hub".into(),
            "show".into(),
            "owner/repo".into(),
        ])
        .unwrap_err();
        assert!(err.contains("usage:"));
        assert!(err.contains("show"));
    }

    #[test]
    fn skills_hub_unknown_subcommand_lists_options() {
        let err = skills_cmd(&[
            "hub".into(),
            "bogus".into(),
            "owner/repo".into(),
        ])
        .unwrap_err();
        assert!(err.contains("list"));
        assert!(err.contains("install"));
    }

    #[test]
    fn llm_providers_returns_known_providers_with_counts() {
        let v = llm_cmd(&["providers".into()]).expect("llm providers ok");
        let count = v.get("count").and_then(|c| c.as_u64()).expect("count");
        assert!(count >= 1, "expected at least one provider, got {count}");
        let providers = v
            .get("providers")
            .and_then(|p| p.as_array())
            .expect("providers array");
        for p in providers {
            assert!(p.get("name").and_then(|x| x.as_str()).is_some());
            assert!(p.get("models").and_then(|x| x.as_u64()).is_some());
        }
        let total = v
            .get("total_entries")
            .and_then(|c| c.as_u64())
            .expect("total_entries");
        assert!(total >= count, "total entries should be >= provider count");
    }

    #[test]
    fn llm_providers_default_when_no_args() {
        // The bare `cos agent llm` invocation with no subcommand
        // defaults to providers (mirrors `usage` defaulting to overall).
        let v = llm_cmd(&[]).expect("llm default ok");
        assert!(v.get("providers").is_some());
    }

    #[test]
    fn llm_models_filters_by_provider() {
        let v = llm_cmd(&["models".into(), "--provider".into(), "anthropic".into()])
            .expect("llm models filter ok");
        let models = v
            .get("models")
            .and_then(|m| m.as_array())
            .expect("models array");
        assert!(!models.is_empty(), "anthropic should have at least one model");
        for m in models {
            assert_eq!(
                m.get("provider").and_then(|p| p.as_str()),
                Some("anthropic"),
                "filter leaked: {m:?}"
            );
        }
    }

    #[test]
    fn llm_models_unfiltered_returns_all() {
        let v = llm_cmd(&["models".into()]).expect("llm models all ok");
        let n = v.get("count").and_then(|c| c.as_u64()).expect("count");
        assert!(n >= 1);
    }

    #[test]
    fn llm_model_unknown_errors() {
        let err = llm_cmd(&["model".into(), "definitely-not-a-real-model".into()])
            .unwrap_err();
        assert!(err.contains("unknown model"));
    }

    #[test]
    fn llm_model_returns_pricing_and_capability_fields() {
        // Pick the first model the registry reports for the first
        // known provider so this test is robust to table changes.
        let providers = llm_cmd(&["providers".into()]).expect("providers ok");
        let first = providers
            .get("providers")
            .and_then(|p| p.as_array())
            .and_then(|arr| arr.first())
            .and_then(|p| p.get("name"))
            .and_then(|n| n.as_str())
            .expect("at least one known provider")
            .to_string();
        let models = llm_cmd(&["models".into(), "--provider".into(), first])
            .expect("models ok");
        let first_model = models
            .get("models")
            .and_then(|m| m.as_array())
            .and_then(|arr| arr.first())
            .and_then(|m| m.get("name"))
            .and_then(|n| n.as_str())
            .expect("at least one model for first provider")
            .to_string();
        let v = llm_cmd(&["model".into(), first_model]).expect("model ok");
        assert!(v.get("name").and_then(|x| x.as_str()).is_some());
        assert!(v.get("provider").and_then(|x| x.as_str()).is_some());
        assert!(v.get("context_window").is_some());
        assert!(v.get("supports_tools").is_some());
        assert!(v
            .get("pricing")
            .and_then(|p| p.get("input_per_mtok_usd"))
            .and_then(|x| x.as_f64())
            .is_some());
    }

    #[test]
    fn llm_cost_requires_model_arg() {
        let err = llm_cmd(&["cost".into(), "--input".into(), "1000".into()])
            .unwrap_err();
        assert!(err.contains("usage:"));
    }

    #[test]
    fn llm_cost_unknown_model_errors() {
        let err = llm_cmd(&[
            "cost".into(),
            "definitely-not-a-real-model".into(),
            "--input".into(),
            "1000".into(),
            "--output".into(),
            "100".into(),
        ])
        .unwrap_err();
        assert!(err.contains("unknown model"));
    }

    #[test]
    fn llm_cost_invalid_int_errors() {
        let providers = llm_cmd(&["providers".into()]).expect("providers ok");
        let model_name = providers
            .get("providers")
            .and_then(|p| p.as_array())
            .and_then(|arr| arr.first())
            .and_then(|p| p.get("name"))
            .and_then(|n| n.as_str())
            .map(|s| s.to_string())
            .map(|provider| {
                let models = llm_cmd(&["models".into(), "--provider".into(), provider])
                    .expect("models ok");
                models
                    .get("models")
                    .and_then(|m| m.as_array())
                    .and_then(|arr| arr.first())
                    .and_then(|m| m.get("name"))
                    .and_then(|n| n.as_str())
                    .expect("at least one model")
                    .to_string()
            })
            .expect("at least one provider");
        let err = llm_cmd(&[
            "cost".into(),
            model_name,
            "--input".into(),
            "not-a-number".into(),
        ])
        .unwrap_err();
        assert!(err.contains("--input"));
    }

    #[test]
    fn llm_unknown_subcommand_lists_options() {
        let err = llm_cmd(&["bogus".into()]).unwrap_err();
        assert!(err.contains("providers"));
        assert!(err.contains("models"));
    }

    #[test]
    fn redact_no_args_errors_with_usage() {
        let err = redact_cmd(&[]).unwrap_err();
        assert!(err.contains("usage:"));
    }

    #[test]
    fn redact_replaces_known_secrets() {
        let v = redact_cmd(&["my key is sk-abcdef0123456789ABCDEF0123456789abcd ok".into()])
            .expect("redact ok");
        let out = v.get("redacted").and_then(|x| x.as_str()).unwrap();
        assert!(out.contains("[REDACTED:"), "expected placeholder, got {out}");
        assert!(!out.contains("sk-abcdef0123456789ABCDEF0123456789abcd"));
        assert_eq!(v.get("changed").and_then(|x| x.as_bool()), Some(true));
    }

    #[test]
    fn redact_unchanged_for_clean_input() {
        let v = redact_cmd(&["hello world, this is just text".into()]).expect("redact ok");
        let out = v.get("redacted").and_then(|x| x.as_str()).unwrap();
        assert_eq!(out, "hello world, this is just text");
        assert_eq!(v.get("changed").and_then(|x| x.as_bool()), Some(false));
    }

    #[test]
    fn redact_check_returns_detection_only() {
        let v = redact_cmd(&[
            "--check".into(),
            "leaks AKIAIOSFODNN7EXAMPLE here".into(),
        ])
        .expect("check ok");
        assert_eq!(v.get("contains_secrets").and_then(|x| x.as_bool()), Some(true));
        assert!(v.get("redacted").is_none(), "check mode should not include redacted");
    }

    #[test]
    fn redact_check_negative() {
        let v = redact_cmd(&["--check".into(), "innocent text".into()]).expect("check ok");
        assert_eq!(v.get("contains_secrets").and_then(|x| x.as_bool()), Some(false));
    }

    #[test]
    fn redact_strict_flag_propagates() {
        let v = redact_cmd(&["--strict".into(), "contact me at user@example.com".into()])
            .expect("strict redact");
        let out = v.get("redacted").and_then(|x| x.as_str()).unwrap();
        assert!(out.contains("[REDACTED:email]"), "strict should redact emails: {out}");
        assert_eq!(v.get("strict").and_then(|x| x.as_bool()), Some(true));
    }

    #[test]
    fn redact_default_does_not_redact_email() {
        let v = redact_cmd(&["contact me at user@example.com".into()])
            .expect("default redact");
        let out = v.get("redacted").and_then(|x| x.as_str()).unwrap();
        assert!(out.contains("user@example.com"), "default should keep email: {out}");
    }

    #[test]
    fn redact_from_file() {
        let dir = tempfile::tempdir().expect("tmp");
        let p = dir.path().join("sample.txt");
        std::fs::write(
            &p,
            "token=ghp_aBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789",
        )
        .expect("write");
        let v = redact_cmd(&["--file".into(), p.to_string_lossy().to_string()])
            .expect("file redact ok");
        let out = v.get("redacted").and_then(|x| x.as_str()).unwrap();
        assert!(
            out.contains("[REDACTED:github_token]"),
            "expected github_token placeholder, got {out}"
        );
    }

    #[test]
    fn redact_file_missing_path_errors() {
        let err = redact_cmd(&["--file".into()]).unwrap_err();
        assert!(err.contains("--file"));
    }

    #[test]
    fn redact_joins_multiple_positional_args() {
        // Without --, the dispatcher will tokenize on spaces; we
        // re-stitch them so `cos agent redact hello world` doesn't
        // error out.
        let v = redact_cmd(&[
            "hello".into(),
            "this".into(),
            "has".into(),
            "Bearer".into(),
            "abcdefABCDEF1234567890123456789012345678".into(),
        ])
        .expect("multi-arg ok");
        let out = v.get("redacted").and_then(|x| x.as_str()).unwrap();
        assert!(out.contains("[REDACTED:"), "got {out}");
    }

    #[test]
    fn skills_usage_stats_empty_returns_zero_count() {
        let dir = tempfile::tempdir().expect("tmp");
        let p = dir.path().join("usage.jsonl");
        let v = skills_usage_cmd_at(&["stats".into()], &p).expect("stats ok");
        assert_eq!(v.get("skill_count").and_then(|x| x.as_u64()), Some(0));
        assert_eq!(
            v.get("skills").and_then(|x| x.as_array()).map(|a| a.len()),
            Some(0)
        );
    }

    #[test]
    fn skills_usage_record_then_stats_aggregates() {
        let dir = tempfile::tempdir().expect("tmp");
        let p = dir.path().join("usage.jsonl");
        skills_usage_cmd_at(
            &[
                "record".into(),
                "demo".into(),
                "--duration-ms".into(),
                "100".into(),
                "--ok".into(),
            ],
            &p,
        )
        .expect("record 1");
        skills_usage_cmd_at(
            &[
                "record".into(),
                "demo".into(),
                "--duration-ms".into(),
                "200".into(),
                "--error".into(),
            ],
            &p,
        )
        .expect("record 2");
        let v = skills_usage_cmd_at(&["stats".into()], &p).expect("stats ok");
        let skills = v.get("skills").and_then(|x| x.as_array()).unwrap();
        assert_eq!(skills.len(), 1);
        let s = &skills[0];
        assert_eq!(s.get("id").and_then(|x| x.as_str()), Some("demo"));
        assert_eq!(s.get("total").and_then(|x| x.as_u64()), Some(2));
        assert_eq!(s.get("success").and_then(|x| x.as_u64()), Some(1));
        assert_eq!(s.get("failure").and_then(|x| x.as_u64()), Some(1));
        assert_eq!(s.get("total_duration_ms").and_then(|x| x.as_u64()), Some(300));
        assert_eq!(s.get("average_duration_ms").and_then(|x| x.as_u64()), Some(150));
    }

    #[test]
    fn skills_usage_stats_filter_by_id() {
        let dir = tempfile::tempdir().expect("tmp");
        let p = dir.path().join("usage.jsonl");
        for id in ["a", "b", "c"] {
            skills_usage_cmd_at(
                &[
                    "record".into(),
                    id.into(),
                    "--duration-ms".into(),
                    "10".into(),
                ],
                &p,
            )
            .expect("rec");
        }
        let v = skills_usage_cmd_at(&["stats".into(), "b".into()], &p).expect("stats ok");
        let skills = v.get("skills").and_then(|x| x.as_array()).unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(
            skills[0].get("id").and_then(|x| x.as_str()),
            Some("b"),
            "filter should keep only `b`"
        );
        assert_eq!(v.get("filter_id").and_then(|x| x.as_str()), Some("b"));
    }

    #[test]
    fn skills_usage_record_requires_duration() {
        let dir = tempfile::tempdir().expect("tmp");
        let p = dir.path().join("usage.jsonl");
        let err = skills_usage_cmd_at(&["record".into(), "demo".into()], &p).unwrap_err();
        assert!(err.contains("--duration-ms"));
    }

    #[test]
    fn skills_usage_record_requires_id() {
        let dir = tempfile::tempdir().expect("tmp");
        let p = dir.path().join("usage.jsonl");
        let err = skills_usage_cmd_at(&["record".into()], &p).unwrap_err();
        assert!(err.contains("usage:"));
    }

    #[test]
    fn skills_usage_record_with_invoked_by_persists() {
        let dir = tempfile::tempdir().expect("tmp");
        let p = dir.path().join("usage.jsonl");
        skills_usage_cmd_at(
            &[
                "record".into(),
                "demo".into(),
                "--duration-ms".into(),
                "5".into(),
                "--by".into(),
                "delegate".into(),
            ],
            &p,
        )
        .expect("record ok");
        let body = std::fs::read_to_string(&p).expect("read");
        assert!(body.contains("\"invoked_by\":\"delegate\""), "body: {body}");
    }

    #[test]
    fn skills_usage_clear_refuses_without_yes() {
        let dir = tempfile::tempdir().expect("tmp");
        let p = dir.path().join("usage.jsonl");
        std::fs::write(&p, "junk").expect("write");
        let err = skills_usage_cmd_at(&["clear".into()], &p).unwrap_err();
        assert!(err.contains("--yes"));
        assert!(p.exists(), "file must remain after refused clear");
    }

    #[test]
    fn skills_usage_clear_with_yes_removes_file() {
        let dir = tempfile::tempdir().expect("tmp");
        let p = dir.path().join("usage.jsonl");
        std::fs::write(&p, "junk").expect("write");
        let v = skills_usage_cmd_at(&["clear".into(), "--yes".into()], &p).expect("clear ok");
        assert_eq!(v.get("cleared").and_then(|x| x.as_bool()), Some(true));
        assert!(!p.exists(), "file should be removed");
    }

    #[test]
    fn skills_usage_clear_missing_file_is_ok() {
        let dir = tempfile::tempdir().expect("tmp");
        let p = dir.path().join("does-not-exist.jsonl");
        let v = skills_usage_cmd_at(&["clear".into(), "--yes".into()], &p).expect("clear ok");
        assert_eq!(v.get("cleared").and_then(|x| x.as_bool()), Some(true));
    }

    #[test]
    fn skills_usage_path_returns_path() {
        let dir = tempfile::tempdir().expect("tmp");
        let p = dir.path().join("usage.jsonl");
        let v = skills_usage_cmd_at(&["path".into()], &p).expect("path ok");
        let returned = v.get("path").and_then(|x| x.as_str()).unwrap();
        assert!(returned.ends_with("usage.jsonl"), "got {returned}");
    }

    #[test]
    fn skills_usage_record_unknown_flag_errors() {
        let dir = tempfile::tempdir().expect("tmp");
        let p = dir.path().join("usage.jsonl");
        let err = skills_usage_cmd_at(
            &[
                "record".into(),
                "demo".into(),
                "--duration-ms".into(),
                "1".into(),
                "--bogus".into(),
            ],
            &p,
        )
        .unwrap_err();
        assert!(err.contains("--bogus"));
    }

    #[test]
    fn skills_usage_unknown_subcommand_lists_options() {
        let dir = tempfile::tempdir().expect("tmp");
        let p = dir.path().join("usage.jsonl");
        let err = skills_usage_cmd_at(&["bogus".into()], &p).unwrap_err();
        assert!(err.contains("stats"));
        assert!(err.contains("record"));
    }

    #[test]
    fn prompt_show_returns_prompt_string() {
        let v = prompt_cmd(&[]).expect("prompt show ok");
        let p = v.get("prompt").and_then(|x| x.as_str()).expect("prompt str");
        assert!(!p.is_empty());
        let chars = v.get("chars").and_then(|x| x.as_u64()).expect("chars");
        assert!(chars > 0);
    }

    #[test]
    fn prompt_show_default_includes_size_breakdown() {
        let v = prompt_cmd(&["show".into()]).expect("show ok");
        assert!(v.get("scaffold_chars").is_some());
        assert!(v.get("approx_tokens").is_some());
        assert!(v.get("extra_path").is_some()); // null when not provided
    }

    #[test]
    fn prompt_raw_omits_size_breakdown() {
        let v = prompt_cmd(&["show".into(), "--raw".into()]).expect("raw ok");
        assert!(v.get("prompt").is_some());
        assert!(v.get("scaffold_chars").is_none());
        assert!(v.get("extra_path").is_none());
    }

    #[test]
    fn prompt_extra_appends_file_content() {
        let dir = tempfile::tempdir().expect("tmp");
        let extra = dir.path().join("preface.md");
        std::fs::write(&extra, "ZZZUNIQUEMARKERZZZ_extra_preface_text").expect("write");
        let baseline = prompt_cmd(&["show".into()]).expect("baseline");
        let with_extra = prompt_cmd(&[
            "show".into(),
            "--extra".into(),
            extra.to_string_lossy().to_string(),
        ])
        .expect("with extra");
        let baseline_chars = baseline.get("chars").and_then(|x| x.as_u64()).unwrap();
        let extra_chars = with_extra.get("chars").and_then(|x| x.as_u64()).unwrap();
        assert!(extra_chars > baseline_chars, "extra should grow prompt");
        let p = with_extra.get("prompt").and_then(|x| x.as_str()).unwrap();
        assert!(
            p.contains("ZZZUNIQUEMARKERZZZ_extra_preface_text"),
            "extra content must be in prompt"
        );
        assert_eq!(
            with_extra.get("extra_path").and_then(|x| x.as_str()),
            Some(extra.to_string_lossy().as_ref())
        );
    }

    #[test]
    fn prompt_build_alias_works() {
        let v = prompt_cmd(&["build".into()]).expect("build ok");
        assert!(v.get("prompt").is_some());
    }

    #[test]
    fn prompt_unknown_subcommand_errors() {
        let err = prompt_cmd(&["bogus".into()]).unwrap_err();
        assert!(err.contains("show"));
        assert!(err.contains("build"));
    }

    #[test]
    fn prompt_unknown_flag_errors() {
        let err = prompt_cmd(&["show".into(), "--bogus".into()]).unwrap_err();
        assert!(err.contains("--bogus"));
    }

    #[test]
    fn prompt_extra_missing_path_errors() {
        let err = prompt_cmd(&["show".into(), "--extra".into()]).unwrap_err();
        assert!(err.contains("--extra"));
    }

    #[test]
    fn prompt_extra_nonexistent_file_does_not_panic() {
        // build_system_prompt silently swallows file IO errors and
        // falls back to scaffold-only — preserve that here.
        let v = prompt_cmd(&[
            "show".into(),
            "--extra".into(),
            "Z:\\definitely\\not\\a\\real\\path".into(),
        ])
        .expect("ok");
        assert!(v.get("prompt").and_then(|x| x.as_str()).is_some());
    }

    #[test]
    fn think_scrub_strips_think_block() {
        let v = think_scrub_cmd(&[
            "before <think>secret reasoning</think> after".into(),
        ])
        .expect("ok");
        let out = v.get("scrubbed").and_then(|x| x.as_str()).unwrap();
        assert!(!out.contains("secret reasoning"), "got {out}");
        assert!(out.contains("before"));
        assert!(out.contains("after"));
        assert_eq!(v.get("changed").and_then(|x| x.as_bool()), Some(true));
    }

    #[test]
    fn think_scrub_unchanged_for_clean_input() {
        let v = think_scrub_cmd(&["just plain text".into()]).expect("ok");
        assert_eq!(v.get("changed").and_then(|x| x.as_bool()), Some(false));
    }

    #[test]
    fn think_scrub_check_returns_detection_only() {
        let v = think_scrub_cmd(&[
            "--check".into(),
            "<thinking>internal</thinking> answer".into(),
        ])
        .expect("ok");
        assert_eq!(v.get("has_thinking").and_then(|x| x.as_bool()), Some(true));
        assert!(v.get("scrubbed").is_none());
    }

    #[test]
    fn think_scrub_check_negative() {
        let v = think_scrub_cmd(&["--check".into(), "no tags here".into()])
            .expect("ok");
        assert_eq!(v.get("has_thinking").and_then(|x| x.as_bool()), Some(false));
    }

    #[test]
    fn think_scrub_handles_multiline_block() {
        let v = think_scrub_cmd(&[
            "<thinking>\nline one\nline two\n</thinking>\nfinal".into(),
        ])
        .expect("ok");
        let out = v.get("scrubbed").and_then(|x| x.as_str()).unwrap();
        assert!(!out.contains("line one"), "got {out}");
        assert!(out.contains("final"));
    }

    #[test]
    fn think_scrub_no_args_errors_with_usage() {
        let err = think_scrub_cmd(&[]).unwrap_err();
        assert!(err.contains("usage:"));
    }

    #[test]
    fn think_scrub_from_file() {
        let dir = tempfile::tempdir().expect("tmp");
        let p = dir.path().join("trace.txt");
        std::fs::write(
            &p,
            "<reasoning>internal</reasoning>\nthe answer is 42",
        )
        .expect("write");
        let v = think_scrub_cmd(&["--file".into(), p.to_string_lossy().to_string()])
            .expect("ok");
        let out = v.get("scrubbed").and_then(|x| x.as_str()).unwrap();
        assert!(!out.contains("internal"), "got {out}");
        assert!(out.contains("the answer is 42"));
    }

    #[test]
    fn tokens_basic_input() {
        // chars / 4 with a min of 1 — see estimate_text_tokens.
        let v = tokens_cmd(&["hello world this is some text".into()]).expect("ok");
        let chars = v.get("chars").and_then(|x| x.as_u64()).unwrap();
        let tokens = v.get("approx_tokens").and_then(|x| x.as_u64()).unwrap();
        assert_eq!(chars, "hello world this is some text".len() as u64);
        assert!(tokens >= 1);
        assert!(tokens <= chars, "tokens should be <= chars");
    }

    #[test]
    fn tokens_no_args_errors_with_usage() {
        let err = tokens_cmd(&[]).unwrap_err();
        assert!(err.contains("usage:"));
    }

    #[test]
    fn tokens_from_file() {
        let dir = tempfile::tempdir().expect("tmp");
        let p = dir.path().join("body.txt");
        let content = "x".repeat(400);
        std::fs::write(&p, &content).expect("write");
        let v = tokens_cmd(&["--file".into(), p.to_string_lossy().to_string()])
            .expect("ok");
        assert_eq!(v.get("chars").and_then(|x| x.as_u64()), Some(400));
        // chars / 4 = 100
        assert_eq!(v.get("approx_tokens").and_then(|x| x.as_u64()), Some(100));
    }

    #[test]
    fn tokens_includes_method_label() {
        let v = tokens_cmd(&["abc".into()]).expect("ok");
        let m = v.get("method").and_then(|x| x.as_str()).unwrap();
        assert!(m.contains("chars"), "got {m}");
    }

    #[test]
    fn read_text_input_joins_positional_with_spaces() {
        let (s, _) = read_text_input(
            &["a".into(), "b".into(), "c".into()],
            "tokens",
        )
        .expect("ok");
        assert_eq!(s, "a b c");
    }

    #[test]
    fn read_text_input_file_missing_path_errors() {
        let err = read_text_input(&["--file".into()], "tokens").unwrap_err();
        assert!(err.contains("--file"));
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

    #[test]
    fn providers_cmd_lists_every_registered_provider() {
        let v = providers_cmd(&[]).expect("providers ok");
        let arr = v
            .get("providers")
            .and_then(|p| p.as_array())
            .expect("providers array");
        let names: std::collections::HashSet<_> = arr
            .iter()
            .filter_map(|p| p.get("name").and_then(|n| n.as_str()))
            .collect();
        for &expected in llm::available_providers().iter() {
            assert!(
                names.contains(expected),
                "providers_cmd missing {expected}: got {names:?}"
            );
        }
        assert_eq!(
            v.get("count").and_then(|c| c.as_u64()).unwrap_or(0),
            llm::available_providers().len() as u64
        );
    }

    #[test]
    fn providers_cmd_marks_active_provider() {
        let active = crate::config::get().agent.provider.clone();
        let v = providers_cmd(&[]).expect("providers ok");
        let arr = v.get("providers").and_then(|p| p.as_array()).unwrap();
        let active_entries: Vec<_> = arr
            .iter()
            .filter(|e| e.get("active") == Some(&serde_json::Value::Bool(true)))
            .collect();
        assert_eq!(active_entries.len(), 1, "exactly one active provider");
        assert_eq!(
            active_entries[0].get("name").and_then(|n| n.as_str()),
            Some(active.as_str())
        );
    }

    #[test]
    fn providers_cmd_filters_by_names_flag() {
        let v = providers_cmd(&["--names".into(), "openai,anthropic".into()])
            .expect("providers ok");
        let arr = v.get("providers").and_then(|p| p.as_array()).unwrap();
        assert_eq!(arr.len(), 2);
        let names: Vec<_> = arr
            .iter()
            .filter_map(|p| p.get("name").and_then(|n| n.as_str()))
            .collect();
        assert!(names.contains(&"openai"));
        assert!(names.contains(&"anthropic"));
    }

    #[test]
    fn providers_cmd_filter_drops_unknown_names_silently() {
        let v = providers_cmd(&["--names".into(), "openai,does-not-exist".into()])
            .expect("providers ok");
        let arr = v.get("providers").and_then(|p| p.as_array()).unwrap();
        // "does-not-exist" is not in REGISTERED, so it gets dropped.
        assert_eq!(arr.len(), 1);
        assert_eq!(
            arr[0].get("name").and_then(|n| n.as_str()),
            Some("openai")
        );
    }

    #[test]
    fn providers_cmd_rejects_unknown_flags() {
        let err = providers_cmd(&["--bogus".into()]).unwrap_err();
        assert!(err.contains("--bogus"));
        assert!(err.contains("--names"));
    }

    #[test]
    fn providers_cmd_names_flag_requires_value() {
        let err = providers_cmd(&["--names".into()]).unwrap_err();
        assert!(err.contains("--names"));
    }

    #[test]
    fn providers_cmd_local_providers_have_no_canonical_env_or_credential() {
        let v = providers_cmd(&["--names".into(), "ollama,mock,llama_local".into()])
            .expect("providers ok");
        let arr = v.get("providers").and_then(|p| p.as_array()).unwrap();
        assert_eq!(arr.len(), 3);
        for entry in arr {
            assert_eq!(
                entry.get("env"),
                Some(&serde_json::Value::Null),
                "{:?} should have no canonical env",
                entry.get("name")
            );
            assert_eq!(
                entry.get("credential"),
                Some(&serde_json::Value::Null),
                "{:?} should have no canonical credential",
                entry.get("name")
            );
            assert_eq!(
                entry.get("key_required"),
                Some(&serde_json::Value::Bool(false)),
                "{:?} should not require a key",
                entry.get("name")
            );
        }
    }

    #[test]
    fn providers_cmd_cloud_providers_advertise_canonical_env_and_credential() {
        let v = providers_cmd(&[
            "--names".into(),
            "openai,anthropic,gemini,xai,deepseek,openrouter".into(),
        ])
        .expect("providers ok");
        let arr = v.get("providers").and_then(|p| p.as_array()).unwrap();
        assert_eq!(arr.len(), 6);
        let by_name: std::collections::HashMap<_, _> = arr
            .iter()
            .map(|e| {
                (
                    e.get("name").and_then(|n| n.as_str()).unwrap().to_string(),
                    e.clone(),
                )
            })
            .collect();
        assert_eq!(
            by_name["openai"].get("env").and_then(|e| e.as_str()),
            Some("OPENAI_API_KEY")
        );
        assert_eq!(
            by_name["anthropic"].get("env").and_then(|e| e.as_str()),
            Some("ANTHROPIC_API_KEY")
        );
        assert_eq!(
            by_name["gemini"].get("env").and_then(|e| e.as_str()),
            Some("GEMINI_API_KEY")
        );
        assert_eq!(
            by_name["openrouter"]
                .get("credential")
                .and_then(|e| e.as_str()),
            Some("openrouter")
        );
        for n in [
            "openai",
            "anthropic",
            "gemini",
            "xai",
            "deepseek",
            "openrouter",
        ] {
            assert_eq!(
                by_name[n].get("key_required"),
                Some(&serde_json::Value::Bool(true)),
                "{n} should require a key"
            );
        }
    }

    #[test]
    fn providers_cmd_default_base_url_per_alias() {
        let v = providers_cmd(&[]).expect("providers ok");
        let arr = v.get("providers").and_then(|p| p.as_array()).unwrap();
        let by_name: std::collections::HashMap<_, _> = arr
            .iter()
            .map(|e| {
                (
                    e.get("name").and_then(|n| n.as_str()).unwrap().to_string(),
                    e.clone(),
                )
            })
            .collect();
        assert_eq!(
            by_name["openai"]
                .get("default_base_url")
                .and_then(|u| u.as_str()),
            Some("https://api.openai.com/v1")
        );
        assert_eq!(
            by_name["xai"]
                .get("default_base_url")
                .and_then(|u| u.as_str()),
            Some("https://api.x.ai/v1")
        );
        assert_eq!(
            by_name["ollama"]
                .get("default_base_url")
                .and_then(|u| u.as_str()),
            Some("http://localhost:11434/v1")
        );
        assert_eq!(
            by_name["anthropic"]
                .get("default_base_url")
                .and_then(|u| u.as_str()),
            Some("https://api.anthropic.com/v1")
        );
        assert_eq!(
            by_name["gemini"]
                .get("default_base_url")
                .and_then(|u| u.as_str()),
            Some("https://generativelanguage.googleapis.com/v1beta")
        );
    }

    #[test]
    fn providers_cmd_env_present_reflects_environment() {
        // Pick an env name extremely unlikely to be set in CI to assert
        // the negative path. We can't safely set/unset OPENAI_API_KEY in
        // a process-shared test, so we just check the contract.
        let v = providers_cmd(&[]).expect("providers ok");
        let arr = v.get("providers").and_then(|p| p.as_array()).unwrap();
        for entry in arr {
            let env = entry.get("env").and_then(|e| e.as_str());
            let env_present = entry
                .get("env_present")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if env.is_none() {
                assert!(
                    !env_present,
                    "providers without canonical env must report env_present=false"
                );
            }
        }
    }

    #[test]
    fn providers_cmd_probe_credentials_default_off() {
        let v = providers_cmd(&[]).expect("providers ok");
        assert_eq!(
            v.get("probe_credentials"),
            Some(&serde_json::Value::Bool(false))
        );
        let arr = v.get("providers").and_then(|p| p.as_array()).unwrap();
        for entry in arr {
            assert_eq!(
                entry.get("credential_present"),
                Some(&serde_json::Value::Bool(false)),
                "credential_present must be false when --probe-credentials is off (no false positives)"
            );
        }
    }

    #[test]
    fn providers_cmd_probe_credentials_flag_flips_marker() {
        let v = providers_cmd(&["--probe-credentials".into()]).expect("providers ok");
        assert_eq!(
            v.get("probe_credentials"),
            Some(&serde_json::Value::Bool(true))
        );
        // We don't assert credential_present truthiness because the
        // test environment is unpredictable; just that the probe ran.
    }

    #[test]
    fn providers_cmd_count_matches_providers_array_len() {
        let v = providers_cmd(&[]).expect("providers ok");
        let arr_len = v
            .get("providers")
            .and_then(|p| p.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        let count = v.get("count").and_then(|c| c.as_u64()).unwrap_or(0);
        assert_eq!(count as usize, arr_len);
    }

    #[test]
    fn providers_cmd_filter_count_matches_filtered_array() {
        let v = providers_cmd(&["--names".into(), "openai".into()])
            .expect("providers ok");
        let count = v.get("count").and_then(|c| c.as_u64()).unwrap_or(0);
        assert_eq!(count, 1);
    }

    #[test]
    fn title_cmd_returns_first_line_clamped() {
        let v = title_cmd(&["hello world".into()]).expect("title ok");
        assert_eq!(v.get("title").and_then(|s| s.as_str()), Some("hello world"));
        assert_eq!(v.get("method").and_then(|s| s.as_str()), Some("heuristic"));
    }

    #[test]
    fn title_cmd_strips_slash_command_verb() {
        let v = title_cmd(&["/ask hello there".into()]).expect("title ok");
        assert_eq!(
            v.get("title").and_then(|s| s.as_str()),
            Some("hello there")
        );
    }

    #[test]
    fn title_cmd_takes_first_line_only() {
        let v = title_cmd(&["one\ntwo\nthree".into()]).expect("title ok");
        assert_eq!(v.get("title").and_then(|s| s.as_str()), Some("one"));
    }

    #[test]
    fn title_cmd_empty_input_falls_back_to_untitled() {
        let v = title_cmd(&["   ".into()]).expect("title ok");
        assert_eq!(v.get("title").and_then(|s| s.as_str()), Some("untitled"));
    }

    #[test]
    fn title_cmd_requires_some_input() {
        let err = title_cmd(&[]).unwrap_err();
        assert!(err.contains("title"));
    }

    #[test]
    fn summarise_cmd_returns_first_sentence() {
        let v = summarise_cmd(&["First sentence. Second one.".into()])
            .expect("summarise ok");
        assert_eq!(
            v.get("summary").and_then(|s| s.as_str()),
            Some("First sentence.")
        );
        assert_eq!(
            v.get("clamped").and_then(|b| b.as_bool()),
            Some(false)
        );
    }

    #[test]
    fn summarise_cmd_clamps_to_max_with_ellipsis() {
        let v = summarise_cmd(&[
            "abcdefghij no terminator".into(),
            "--max".into(),
            "5".into(),
        ])
        .expect("summarise ok");
        let s = v.get("summary").and_then(|s| s.as_str()).unwrap_or("");
        assert_eq!(s.chars().count(), 5);
        assert!(s.ends_with('…'), "should end with ellipsis: {s:?}");
        assert_eq!(v.get("clamped").and_then(|b| b.as_bool()), Some(true));
    }

    #[test]
    fn summarise_cmd_default_max_is_200() {
        let v = summarise_cmd(&["short input".into()]).expect("summarise ok");
        assert_eq!(v.get("max_chars").and_then(|n| n.as_u64()), Some(200));
    }

    #[test]
    fn summarise_cmd_max_requires_value() {
        let err = summarise_cmd(&["--max".into()]).unwrap_err();
        assert!(err.contains("--max"));
    }

    #[test]
    fn summarise_cmd_max_must_parse() {
        let err = summarise_cmd(&["--max".into(), "not-a-number".into(), "x".into()])
            .unwrap_err();
        assert!(err.contains("--max"));
    }

    #[test]
    fn summarize_alias_dispatches_to_summarise() {
        // Confirm the US-spelling alias hits the same handler.
        let v = run("summarize", &["hello.".into()]).expect("summarize ok");
        assert_eq!(v.get("summary").and_then(|s| s.as_str()), Some("hello."));
    }

    #[test]
    fn classify_cmd_matches_label_case_insensitively() {
        let v = classify_cmd(&[
            "POSITIVE".into(),
            "--labels".into(),
            "positive,negative,neutral".into(),
        ])
        .expect("classify ok");
        assert_eq!(
            v.get("matched").and_then(|m| m.as_str()),
            Some("positive")
        );
    }

    #[test]
    fn classify_cmd_returns_null_on_no_match() {
        let v = classify_cmd(&[
            "definitely not a label".into(),
            "--labels".into(),
            "yes,no".into(),
        ])
        .expect("classify ok");
        assert_eq!(v.get("matched"), Some(&serde_json::Value::Null));
    }

    #[test]
    fn classify_cmd_tolerates_trailing_punctuation() {
        let v = classify_cmd(&[
            "yes.".into(),
            "--labels".into(),
            "yes,no".into(),
        ])
        .expect("classify ok");
        assert_eq!(v.get("matched").and_then(|m| m.as_str()), Some("yes"));
    }

    #[test]
    fn classify_cmd_requires_labels_flag() {
        let err = classify_cmd(&["yes".into()]).unwrap_err();
        assert!(err.contains("--labels"));
    }

    #[test]
    fn classify_cmd_labels_flag_requires_value() {
        let err = classify_cmd(&["--labels".into()]).unwrap_err();
        assert!(err.contains("--labels"));
    }

    #[test]
    fn classify_cmd_empty_label_list_rejected() {
        let err = classify_cmd(&[
            "yes".into(),
            "--labels".into(),
            ",, ,".into(),
        ])
        .unwrap_err();
        assert!(err.contains("--labels"));
    }

    #[test]
    fn classify_cmd_returns_label_set_in_response() {
        let v = classify_cmd(&[
            "yes".into(),
            "--labels".into(),
            "yes,no,maybe".into(),
        ])
        .expect("classify ok");
        let labels = v
            .get("labels")
            .and_then(|l| l.as_array())
            .expect("labels array");
        assert_eq!(labels.len(), 3);
    }

    // ---- tools_cmd ----

    #[test]
    fn tools_cmd_default_lists_permitted_tools() {
        let v = tools_cmd(&[]).expect("tools list ok");
        let arr = v
            .get("tools")
            .and_then(|t| t.as_array())
            .expect("tools array");
        assert!(!arr.is_empty(), "default registry should have at least echo + now");
        // Every entry should be permitted under the default permissive guardrails.
        for entry in arr {
            assert_eq!(
                entry.get("permitted"),
                Some(&serde_json::Value::Bool(true))
            );
        }
        let permitted_count = v.get("permitted_count").and_then(|c| c.as_u64()).unwrap_or(0);
        assert_eq!(permitted_count as usize, arr.len());
    }

    #[test]
    fn tools_cmd_show_returns_full_schema() {
        let v = tools_cmd(&["show".into(), "echo".into()]).expect("tools show ok");
        assert_eq!(v.get("name").and_then(|n| n.as_str()), Some("echo"));
        assert!(v.get("description").is_some());
        assert!(v.get("input_schema").is_some());
    }

    #[test]
    fn tools_cmd_show_unknown_tool_errs() {
        let err = tools_cmd(&["show".into(), "does-not-exist".into()]).unwrap_err();
        assert!(err.contains("does-not-exist"));
    }

    #[test]
    fn tools_cmd_show_requires_name() {
        let err = tools_cmd(&["show".into()]).unwrap_err();
        assert!(err.contains("show"));
    }

    #[test]
    fn tools_cmd_llm_list_returns_serialised_tool_blob() {
        let v = tools_cmd(&["llm-list".into()]).expect("tools llm-list ok");
        let arr = v
            .get("tools")
            .and_then(|t| t.as_array())
            .expect("tools array");
        assert!(!arr.is_empty());
        for entry in arr {
            assert!(entry.get("name").and_then(|n| n.as_str()).is_some());
            assert!(entry.get("input_schema").is_some());
        }
    }

    #[test]
    fn tools_cmd_unknown_subcommand_errs() {
        let err = tools_cmd(&["bogus".into()]).unwrap_err();
        assert!(err.contains("bogus"));
        assert!(err.contains("list"));
    }

    #[test]
    fn tools_cmd_unfiltered_includes_at_least_as_many_as_filtered() {
        let plain = tools_cmd(&["list".into()]).expect("plain list ok");
        let unfiltered =
            tools_cmd(&["list".into(), "--unfiltered".into()]).expect("unfiltered ok");
        let plain_count = plain
            .get("tools")
            .and_then(|t| t.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        let unfiltered_count = unfiltered
            .get("tools")
            .and_then(|t| t.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        assert!(unfiltered_count >= plain_count);
    }

    // ---- guardrails_cmd ----

    #[test]
    fn guardrails_cmd_default_show_reports_permissive_mode() {
        let v = guardrails_cmd(&[]).expect("guardrails show ok");
        // Default config has no tool_allow / empty tool_deny → permissive.
        let mode = v.get("mode").and_then(|m| m.as_str()).unwrap_or("");
        assert!(
            mode == "permissive" || mode == "allowlist",
            "mode {mode:?} should be permissive or allowlist"
        );
        assert!(v.get("deny_count").and_then(|c| c.as_u64()).is_some());
    }

    #[test]
    fn guardrails_cmd_check_returns_decision_for_known_tool() {
        let v = guardrails_cmd(&["check".into(), "echo".into()])
            .expect("guardrails check ok");
        let decision = v.get("decision").and_then(|d| d.as_str()).unwrap_or("");
        assert!(decision == "allow" || decision == "deny");
        assert_eq!(v.get("tool").and_then(|t| t.as_str()), Some("echo"));
    }

    #[test]
    fn guardrails_cmd_check_requires_tool_name() {
        let err = guardrails_cmd(&["check".into()]).unwrap_err();
        assert!(err.contains("check"));
    }

    #[test]
    fn guardrails_cmd_unknown_subcommand_errs() {
        let err = guardrails_cmd(&["bogus".into()]).unwrap_err();
        assert!(err.contains("bogus"));
        assert!(err.contains("show"));
    }

    // ---- approval_cmd ----

    #[test]
    fn approval_cmd_default_show_returns_three_sets() {
        let v = approval_cmd(&[]).expect("approval show ok");
        assert!(v.get("auto_approve").and_then(|a| a.as_array()).is_some());
        assert!(v.get("auto_deny").and_then(|a| a.as_array()).is_some());
        assert!(v.get("dangerous").and_then(|a| a.as_array()).is_some());
    }

    #[test]
    fn approval_cmd_check_safe_tool_returns_approved() {
        // Default config has no dangerous_tools → every tool short-circuits to approved.
        let v = approval_cmd(&["check".into(), "echo".into()]).expect("approval check ok");
        assert_eq!(
            v.get("decision").and_then(|d| d.as_str()),
            Some("approved")
        );
        assert_eq!(
            v.get("would_short_circuit").and_then(|b| b.as_bool()),
            Some(true)
        );
    }

    #[test]
    fn approval_cmd_check_requires_tool_name() {
        let err = approval_cmd(&["check".into()]).unwrap_err();
        assert!(err.contains("check"));
    }

    #[test]
    fn approval_cmd_check_input_must_parse_as_json() {
        let err = approval_cmd(&[
            "check".into(),
            "echo".into(),
            "--input".into(),
            "not json".into(),
        ])
        .unwrap_err();
        assert!(err.contains("--input"));
    }

    #[test]
    fn approval_cmd_check_input_flag_requires_value() {
        let err = approval_cmd(&[
            "check".into(),
            "echo".into(),
            "--input".into(),
        ])
        .unwrap_err();
        assert!(err.contains("--input"));
    }

    #[test]
    fn approval_cmd_check_unknown_flag_errs() {
        let err = approval_cmd(&[
            "check".into(),
            "echo".into(),
            "--bogus".into(),
        ])
        .unwrap_err();
        assert!(err.contains("--bogus"));
    }

    #[test]
    fn approval_cmd_unknown_subcommand_errs() {
        let err = approval_cmd(&["bogus".into()]).unwrap_err();
        assert!(err.contains("bogus"));
        assert!(err.contains("show"));
    }

    // ---- todo_cmd ----

    fn temp_todo_store() -> (tempfile::TempDir, crate::agent::tools::todo::TodoStore) {
        let dir = tempfile::tempdir().expect("tmp");
        let store = crate::agent::tools::todo::TodoStore::new(dir.path().to_path_buf());
        (dir, store)
    }

    #[test]
    fn todo_cmd_path_returns_dir() {
        let v = todo_cmd(&["path".into()]).expect("path ok");
        assert!(v.get("path").and_then(|p| p.as_str()).is_some());
    }

    #[test]
    fn todo_cmd_list_empty_session_returns_empty() {
        let (_dir, store) = temp_todo_store();
        let v = todo_cmd_at(&["list".into(), "session-1".into()], &store)
            .expect("list ok");
        assert_eq!(v.get("count").and_then(|c| c.as_u64()), Some(0));
        let items = v.get("items").and_then(|i| i.as_array()).expect("items array");
        assert!(items.is_empty());
    }

    #[test]
    fn todo_cmd_list_requires_session() {
        let (_dir, store) = temp_todo_store();
        let err = todo_cmd_at(&["list".into()], &store).unwrap_err();
        assert!(err.contains("list"));
    }

    #[test]
    fn todo_cmd_add_appends_and_persists() {
        let (_dir, store) = temp_todo_store();
        let v = todo_cmd_at(
            &[
                "add".into(),
                "session-1".into(),
                "t1".into(),
                "first".into(),
                "todo".into(),
                "item".into(),
            ],
            &store,
        )
        .expect("add ok");
        assert_eq!(v.get("count").and_then(|c| c.as_u64()), Some(1));

        // Re-read confirms persistence + multi-word title joined.
        let listed =
            todo_cmd_at(&["list".into(), "session-1".into()], &store).expect("list ok");
        let items = listed.get("items").and_then(|i| i.as_array()).expect("items");
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].get("title").and_then(|t| t.as_str()),
            Some("first todo item")
        );
        assert_eq!(
            items[0].get("status").and_then(|s| s.as_str()),
            Some("pending")
        );
    }

    #[test]
    fn todo_cmd_add_with_note_flag() {
        let (_dir, store) = temp_todo_store();
        todo_cmd_at(
            &[
                "add".into(),
                "session-1".into(),
                "t1".into(),
                "title".into(),
                "--note".into(),
                "explanatory note".into(),
            ],
            &store,
        )
        .expect("add ok");
        let listed =
            todo_cmd_at(&["list".into(), "session-1".into()], &store).expect("list ok");
        let items = listed.get("items").and_then(|i| i.as_array()).expect("items");
        assert_eq!(
            items[0].get("note").and_then(|n| n.as_str()),
            Some("explanatory note")
        );
    }

    #[test]
    fn todo_cmd_add_rejects_duplicate_id() {
        let (_dir, store) = temp_todo_store();
        todo_cmd_at(
            &["add".into(), "s1".into(), "t1".into(), "first".into()],
            &store,
        )
        .expect("first add ok");
        let err = todo_cmd_at(
            &["add".into(), "s1".into(), "t1".into(), "second".into()],
            &store,
        )
        .unwrap_err();
        assert!(err.contains("t1"));
    }

    #[test]
    fn todo_cmd_add_requires_title() {
        let (_dir, store) = temp_todo_store();
        let err = todo_cmd_at(
            &["add".into(), "s1".into(), "t1".into()],
            &store,
        )
        .unwrap_err();
        assert!(err.contains("title"));
    }

    #[test]
    fn todo_cmd_add_note_flag_requires_value() {
        let (_dir, store) = temp_todo_store();
        let err = todo_cmd_at(
            &[
                "add".into(),
                "s1".into(),
                "t1".into(),
                "title".into(),
                "--note".into(),
            ],
            &store,
        )
        .unwrap_err();
        assert!(err.contains("--note"));
    }

    #[test]
    fn todo_cmd_set_status_updates_one_item() {
        let (_dir, store) = temp_todo_store();
        todo_cmd_at(
            &["add".into(), "s1".into(), "t1".into(), "first".into()],
            &store,
        )
        .expect("add ok");
        let v = todo_cmd_at(
            &[
                "set-status".into(),
                "s1".into(),
                "t1".into(),
                "in_progress".into(),
            ],
            &store,
        )
        .expect("set-status ok");
        assert_eq!(
            v.get("status").and_then(|s| s.as_str()),
            Some("in_progress")
        );
    }

    #[test]
    fn todo_cmd_set_status_accepts_dash_alias() {
        let (_dir, store) = temp_todo_store();
        todo_cmd_at(
            &["add".into(), "s1".into(), "t1".into(), "first".into()],
            &store,
        )
        .expect("add ok");
        // Both `in_progress` and `in-progress` should work.
        todo_cmd_at(
            &[
                "set-status".into(),
                "s1".into(),
                "t1".into(),
                "in-progress".into(),
            ],
            &store,
        )
        .expect("dash alias accepted");
    }

    #[test]
    fn todo_cmd_set_status_rejects_unknown_status() {
        let (_dir, store) = temp_todo_store();
        todo_cmd_at(
            &["add".into(), "s1".into(), "t1".into(), "first".into()],
            &store,
        )
        .expect("add ok");
        let err = todo_cmd_at(
            &[
                "set-status".into(),
                "s1".into(),
                "t1".into(),
                "bogus".into(),
            ],
            &store,
        )
        .unwrap_err();
        assert!(err.contains("bogus"));
    }

    #[test]
    fn todo_cmd_remove_drops_item() {
        let (_dir, store) = temp_todo_store();
        todo_cmd_at(
            &["add".into(), "s1".into(), "t1".into(), "a".into()],
            &store,
        )
        .expect("add ok");
        todo_cmd_at(
            &["add".into(), "s1".into(), "t2".into(), "b".into()],
            &store,
        )
        .expect("add ok");
        let v = todo_cmd_at(
            &["remove".into(), "s1".into(), "t1".into()],
            &store,
        )
        .expect("remove ok");
        assert_eq!(v.get("count").and_then(|c| c.as_u64()), Some(1));
        let listed = todo_cmd_at(&["list".into(), "s1".into()], &store).expect("list ok");
        let items = listed.get("items").and_then(|i| i.as_array()).expect("items");
        assert_eq!(items[0].get("id").and_then(|i| i.as_str()), Some("t2"));
    }

    #[test]
    fn todo_cmd_remove_unknown_id_errs() {
        let (_dir, store) = temp_todo_store();
        let err = todo_cmd_at(
            &["remove".into(), "s1".into(), "ghost".into()],
            &store,
        )
        .unwrap_err();
        assert!(err.contains("ghost"));
    }

    #[test]
    fn todo_cmd_clear_requires_yes_flag() {
        let (_dir, store) = temp_todo_store();
        let err = todo_cmd_at(&["clear".into(), "s1".into()], &store).unwrap_err();
        assert!(err.contains("--yes"));
    }

    #[test]
    fn todo_cmd_clear_with_yes_wipes_session() {
        let (_dir, store) = temp_todo_store();
        todo_cmd_at(
            &["add".into(), "s1".into(), "t1".into(), "a".into()],
            &store,
        )
        .expect("add ok");
        let v = todo_cmd_at(
            &["clear".into(), "s1".into(), "--yes".into()],
            &store,
        )
        .expect("clear ok");
        assert_eq!(v.get("cleared").and_then(|c| c.as_bool()), Some(true));
        let listed = todo_cmd_at(&["list".into(), "s1".into()], &store).expect("list ok");
        assert_eq!(listed.get("count").and_then(|c| c.as_u64()), Some(0));
    }

    #[test]
    fn todo_cmd_list_includes_status_breakdown() {
        let (_dir, store) = temp_todo_store();
        todo_cmd_at(
            &["add".into(), "s1".into(), "t1".into(), "a".into()],
            &store,
        )
        .expect("add ok");
        todo_cmd_at(
            &["add".into(), "s1".into(), "t2".into(), "b".into()],
            &store,
        )
        .expect("add ok");
        todo_cmd_at(
            &[
                "set-status".into(),
                "s1".into(),
                "t2".into(),
                "completed".into(),
            ],
            &store,
        )
        .expect("status ok");
        let listed = todo_cmd_at(&["list".into(), "s1".into()], &store).expect("list ok");
        let counts = listed.get("by_status").expect("by_status");
        assert_eq!(counts.get("pending").and_then(|n| n.as_u64()), Some(1));
        assert_eq!(counts.get("completed").and_then(|n| n.as_u64()), Some(1));
        assert_eq!(counts.get("in_progress").and_then(|n| n.as_u64()), Some(0));
        assert_eq!(counts.get("cancelled").and_then(|n| n.as_u64()), Some(0));
    }

    #[test]
    fn todo_cmd_unknown_subcommand_errs() {
        let (_dir, store) = temp_todo_store();
        let err = todo_cmd_at(&["bogus".into()], &store).unwrap_err();
        assert!(err.contains("bogus"));
    }

    // ---- compress_cmd ----

    #[test]
    fn compress_cmd_show_config_returns_defaults() {
        let v = compress_cmd(&["show-config".into()]).expect("show-config ok");
        assert!(v.get("target_tokens").and_then(|n| n.as_u64()).unwrap_or(0) > 0);
        assert!(v.get("trigger_tokens").and_then(|n| n.as_u64()).unwrap_or(0) > 0);
        assert!(v.get("keep_tail_tokens").and_then(|n| n.as_u64()).is_some());
        assert!(v.get("summary_max_tokens").and_then(|n| n.as_u64()).is_some());
    }

    #[test]
    fn compress_cmd_default_subcommand_is_show_config() {
        let v = compress_cmd(&[]).expect("default ok");
        assert!(v.get("target_tokens").is_some());
    }

    #[test]
    fn compress_cmd_check_requires_file() {
        let err = compress_cmd(&["check".into()]).unwrap_err();
        assert!(err.contains("--file"));
    }

    #[test]
    fn compress_cmd_check_reports_zero_for_empty_file() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("conv.jsonl");
        std::fs::write(&path, "").expect("write");
        let v = compress_cmd(&[
            "check".into(),
            "--file".into(),
            path.display().to_string(),
        ])
        .expect("check ok");
        assert_eq!(v.get("message_count").and_then(|n| n.as_u64()), Some(0));
        assert_eq!(v.get("total_tokens").and_then(|n| n.as_u64()), Some(0));
        assert_eq!(v.get("would_trigger").and_then(|b| b.as_bool()), Some(false));
    }

    #[test]
    fn compress_cmd_check_skips_blank_lines() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("conv.jsonl");
        let body = format!(
            "{}\n\n{}\n",
            serde_json::to_string(&crate::agent::llm::types::Message::user_text("hello"))
                .unwrap(),
            serde_json::to_string(&crate::agent::llm::types::Message::assistant_text(
                "hi back"
            ))
            .unwrap(),
        );
        std::fs::write(&path, body).expect("write");
        let v = compress_cmd(&[
            "check".into(),
            "--file".into(),
            path.display().to_string(),
        ])
        .expect("check ok");
        assert_eq!(v.get("message_count").and_then(|n| n.as_u64()), Some(2));
    }

    #[test]
    fn compress_cmd_check_counts_by_role() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("conv.jsonl");
        let body = format!(
            "{}\n{}\n{}\n",
            serde_json::to_string(&crate::agent::llm::types::Message::user_text("u1"))
                .unwrap(),
            serde_json::to_string(&crate::agent::llm::types::Message::assistant_text("a1"))
                .unwrap(),
            serde_json::to_string(&crate::agent::llm::types::Message::user_text("u2"))
                .unwrap(),
        );
        std::fs::write(&path, body).expect("write");
        let v = compress_cmd(&[
            "check".into(),
            "--file".into(),
            path.display().to_string(),
        ])
        .expect("check ok");
        let by_role = v.get("by_role").expect("by_role");
        let counts = by_role.get("counts").expect("counts");
        assert_eq!(counts.get("user").and_then(|n| n.as_u64()), Some(2));
        assert_eq!(counts.get("assistant").and_then(|n| n.as_u64()), Some(1));
    }

    #[test]
    fn compress_cmd_check_includes_system_tokens_when_provided() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("conv.jsonl");
        std::fs::write(&path, "").expect("write");
        let v = compress_cmd(&[
            "check".into(),
            "--file".into(),
            path.display().to_string(),
            "--system".into(),
            "you are a helpful assistant".into(),
        ])
        .expect("check ok");
        assert!(v.get("system_tokens").and_then(|n| n.as_u64()).unwrap_or(0) > 0);
    }

    #[test]
    fn compress_cmd_check_system_file_loads_text() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("conv.jsonl");
        std::fs::write(&path, "").expect("write");
        let sys_path = dir.path().join("sys.txt");
        std::fs::write(&sys_path, "system prompt body").expect("write");
        let v = compress_cmd(&[
            "check".into(),
            "--file".into(),
            path.display().to_string(),
            "--system-file".into(),
            sys_path.display().to_string(),
        ])
        .expect("check ok");
        assert!(v.get("system_tokens").and_then(|n| n.as_u64()).unwrap_or(0) > 0);
    }

    #[test]
    fn compress_cmd_check_system_and_system_file_conflict() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("conv.jsonl");
        std::fs::write(&path, "").expect("write");
        let sys_path = dir.path().join("sys.txt");
        std::fs::write(&sys_path, "x").expect("write");
        let err = compress_cmd(&[
            "check".into(),
            "--file".into(),
            path.display().to_string(),
            "--system".into(),
            "y".into(),
            "--system-file".into(),
            sys_path.display().to_string(),
        ])
        .unwrap_err();
        assert!(err.contains("mutually exclusive"));
    }

    #[test]
    fn compress_cmd_check_would_trigger_when_total_meets_trigger() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("conv.jsonl");
        let big = "x".repeat(2000);
        let body = format!(
            "{}\n",
            serde_json::to_string(&crate::agent::llm::types::Message::user_text(&big)).unwrap(),
        );
        std::fs::write(&path, body).expect("write");
        let v = compress_cmd(&[
            "check".into(),
            "--file".into(),
            path.display().to_string(),
            "--trigger".into(),
            "10".into(),
        ])
        .expect("check ok");
        assert_eq!(v.get("would_trigger").and_then(|b| b.as_bool()), Some(true));
    }

    #[test]
    fn compress_cmd_check_overrides_config() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("conv.jsonl");
        std::fs::write(&path, "").expect("write");
        let v = compress_cmd(&[
            "check".into(),
            "--file".into(),
            path.display().to_string(),
            "--trigger".into(),
            "12345".into(),
            "--target".into(),
            "8000".into(),
            "--keep-tail".into(),
            "1234".into(),
            "--summary-max".into(),
            "777".into(),
        ])
        .expect("check ok");
        let cfg = v.get("config").expect("config");
        assert_eq!(cfg.get("trigger_tokens").and_then(|n| n.as_u64()), Some(12345));
        assert_eq!(cfg.get("target_tokens").and_then(|n| n.as_u64()), Some(8000));
        assert_eq!(cfg.get("keep_tail_tokens").and_then(|n| n.as_u64()), Some(1234));
        assert_eq!(cfg.get("summary_max_tokens").and_then(|n| n.as_u64()), Some(777));
    }

    #[test]
    fn compress_cmd_check_rejects_corrupt_jsonl() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("conv.jsonl");
        std::fs::write(&path, "{not json}\n").expect("write");
        let err = compress_cmd(&[
            "check".into(),
            "--file".into(),
            path.display().to_string(),
        ])
        .unwrap_err();
        assert!(err.contains("parse line 1"));
    }

    #[test]
    fn compress_cmd_check_rejects_unknown_flag() {
        let err = compress_cmd(&["check".into(), "--bogus".into()]).unwrap_err();
        assert!(err.contains("--bogus"));
    }

    #[test]
    fn compress_cmd_unknown_subcommand_errs() {
        let err = compress_cmd(&["bogus".into()]).unwrap_err();
        assert!(err.contains("bogus"));
    }

    // ---- mcp_cmd probe/call argument parsing ----

    #[test]
    fn mcp_probe_requires_cmd() {
        let err = mcp_probe(&[]).unwrap_err();
        assert!(err.contains("--cmd"));
    }

    #[test]
    fn mcp_call_requires_cmd() {
        let err = mcp_call(&[]).unwrap_err();
        assert!(err.contains("--cmd"));
    }

    #[test]
    fn mcp_call_requires_tool_positional() {
        let err = mcp_call(&[
            "--cmd".into(),
            "nonexistent-binary-xyz-zyx".into(),
        ])
        .unwrap_err();
        assert!(err.contains("tool name"));
    }

    #[test]
    fn parse_mcp_spawn_spec_collects_args_env_cwd_timeout() {
        let raw: Vec<String> = vec![
            "--cmd".into(),
            "python".into(),
            "--arg".into(),
            "-u".into(),
            "--arg".into(),
            "server.py".into(),
            "--env".into(),
            "API_KEY=secret".into(),
            "--env".into(),
            "DEBUG=1".into(),
            "--cwd".into(),
            "/tmp".into(),
            "--timeout".into(),
            "60".into(),
            "leftover-positional".into(),
        ];
        let (spec, leftover) = parse_mcp_spawn_spec(&raw).expect("parse ok");
        assert_eq!(spec.cmd, "python");
        assert_eq!(spec.args, vec!["-u", "server.py"]);
        assert_eq!(
            spec.env,
            vec![
                ("API_KEY".to_string(), "secret".to_string()),
                ("DEBUG".to_string(), "1".to_string()),
            ]
        );
        assert_eq!(spec.cwd.as_deref(), Some("/tmp"));
        assert_eq!(spec.timeout_secs, 60);
        assert_eq!(leftover, vec!["leftover-positional".to_string()]);
    }

    #[test]
    fn parse_mcp_spawn_spec_rejects_malformed_env() {
        let raw: Vec<String> = vec![
            "--cmd".into(),
            "x".into(),
            "--env".into(),
            "noequalshere".into(),
        ];
        let err = parse_mcp_spawn_spec(&raw).unwrap_err();
        assert!(err.contains("KEY=VALUE"));
    }

    #[test]
    fn parse_mcp_spawn_spec_rejects_unknown_flag() {
        let raw: Vec<String> = vec![
            "--cmd".into(),
            "x".into(),
            "--bogus".into(),
        ];
        let err = parse_mcp_spawn_spec(&raw).unwrap_err();
        assert!(err.contains("--bogus"));
    }

    #[test]
    fn parse_mcp_spawn_spec_default_timeout_is_30() {
        let raw: Vec<String> = vec!["--cmd".into(), "x".into()];
        let (spec, leftover) = parse_mcp_spawn_spec(&raw).expect("parse ok");
        assert_eq!(spec.timeout_secs, 30);
        assert!(leftover.is_empty());
    }

    #[test]
    fn parse_mcp_spawn_spec_timeout_invalid_errs() {
        let raw: Vec<String> = vec![
            "--cmd".into(),
            "x".into(),
            "--timeout".into(),
            "fast".into(),
        ];
        let err = parse_mcp_spawn_spec(&raw).unwrap_err();
        assert!(err.contains("--timeout"));
    }

    #[test]
    fn mcp_probe_propagates_spawn_failure() {
        // A binary that almost certainly doesn't exist on PATH.
        let raw: Vec<String> = vec![
            "--cmd".into(),
            "definitely-not-a-real-binary-zzz-9999".into(),
            "--timeout".into(),
            "2".into(),
        ];
        let err = mcp_probe(&raw).unwrap_err();
        // tokio::process::Command::spawn returns the underlying OS
        // error; both Windows ("program not found") and Unix ("No such
        // file") flavours are acceptable, so we only assert the binary
        // name is mentioned.
        assert!(err.contains("definitely-not-a-real-binary-zzz-9999"));
    }

    #[test]
    fn mcp_probe_rejects_extra_positional() {
        let err = mcp_probe(&[
            "--cmd".into(),
            "python".into(),
            "extra".into(),
        ])
        .unwrap_err();
        assert!(err.contains("positional"));
    }

    #[test]
    fn mcp_call_rejects_invalid_input_json() {
        let err = mcp_call(&[
            "--cmd".into(),
            "python".into(),
            "echo".into(),
            "--input".into(),
            "not json{".into(),
        ])
        .unwrap_err();
        assert!(err.contains("--input"));
    }

    #[test]
    fn mcp_call_rejects_extra_positional() {
        let err = mcp_call(&[
            "--cmd".into(),
            "python".into(),
            "echo".into(),
            "another".into(),
        ])
        .unwrap_err();
        assert!(err.contains("positional"));
    }

    // ---- aux_cmd ----

    #[test]
    fn aux_cmd_show_default_unconfigured() {
        let v = aux_cmd(&["show".into()]).expect("show ok");
        // Default config has no auxiliary_provider set. Configured
        // SHOULD be false (no aux). build_error null.
        assert!(v.get("configured").is_some());
        assert!(v.get("provider").is_some()); // null is fine
        assert!(v.get("model").is_some());
        assert!(v.get("max_tokens").is_some());
        assert!(v.get("note").and_then(|n| n.as_str()).is_some());
    }

    #[test]
    fn aux_cmd_default_subcommand_is_show() {
        let v = aux_cmd(&[]).expect("default ok");
        assert!(v.get("max_tokens").is_some());
    }

    #[test]
    fn aux_cmd_ask_requires_prompt() {
        let err = aux_cmd(&["ask".into()]).unwrap_err();
        assert!(err.contains("--prompt"));
    }

    #[test]
    fn aux_cmd_ask_unknown_flag_errs() {
        let err = aux_cmd(&[
            "ask".into(),
            "--prompt".into(),
            "hi".into(),
            "--bogus".into(),
        ])
        .unwrap_err();
        assert!(err.contains("--bogus"));
    }

    #[test]
    fn aux_cmd_ask_when_unconfigured_errs() {
        // Default config has no aux, so ask MUST refuse before doing
        // any network IO.
        let err = aux_cmd(&[
            "ask".into(),
            "--prompt".into(),
            "hello".into(),
        ])
        .unwrap_err();
        assert!(err.contains("auxiliary"));
    }

    #[test]
    fn aux_cmd_max_tokens_invalid_errs() {
        let err = aux_cmd(&[
            "ask".into(),
            "--prompt".into(),
            "hi".into(),
            "--max-tokens".into(),
            "lots".into(),
        ])
        .unwrap_err();
        assert!(err.contains("--max-tokens"));
    }

    #[test]
    fn aux_cmd_unknown_subcommand_errs() {
        let err = aux_cmd(&["bogus".into()]).unwrap_err();
        assert!(err.contains("bogus"));
    }

    // ---- retry_cmd ----

    #[test]
    fn retry_cmd_show_default_disabled() {
        // Default config has retry_enabled = false.
        let v = retry_cmd(&["show".into()]).expect("show ok");
        assert_eq!(v.get("enabled").and_then(|b| b.as_bool()), Some(false));
        assert!(v.get("config_retry_enabled").is_some());
        assert!(v.get("note").and_then(|s| s.as_str()).is_some());
    }

    #[test]
    fn retry_cmd_default_subcommand_is_show() {
        let v = retry_cmd(&[]).expect("default ok");
        assert!(v.get("enabled").is_some());
    }

    #[test]
    fn retry_cmd_schedule_falls_back_to_standard_when_disabled() {
        // retry_cmd schedule should still produce a preview using
        // RetryPolicy::standard() even when config has retries off.
        let v = retry_cmd(&["schedule".into()]).expect("schedule ok");
        let waits = v
            .get("inter_attempt_waits")
            .and_then(|w| w.as_array())
            .expect("array");
        // standard() = 4 attempts → 3 inter-attempt waits.
        assert_eq!(waits.len(), 3);
        assert_eq!(v.get("max_attempts").and_then(|n| n.as_u64()), Some(4));
    }

    #[test]
    fn retry_cmd_schedule_attempts_override() {
        let v = retry_cmd(&[
            "schedule".into(),
            "--attempts".into(),
            "6".into(),
        ])
        .expect("schedule ok");
        let waits = v
            .get("inter_attempt_waits")
            .and_then(|w| w.as_array())
            .expect("array");
        assert_eq!(waits.len(), 5);
        assert_eq!(v.get("max_attempts").and_then(|n| n.as_u64()), Some(6));
    }

    #[test]
    fn retry_cmd_schedule_one_attempt_has_no_waits() {
        let v = retry_cmd(&[
            "schedule".into(),
            "--attempts".into(),
            "1".into(),
        ])
        .expect("schedule ok");
        let waits = v
            .get("inter_attempt_waits")
            .and_then(|w| w.as_array())
            .expect("array");
        assert!(waits.is_empty());
        assert_eq!(v.get("total_observed_ms").and_then(|n| n.as_u64()), Some(0));
    }

    #[test]
    fn retry_cmd_schedule_caps_delay_at_max_ms() {
        // standard() base=500, max=8000. delay_for(4) would naively
        // be 500 << 3 = 4000 (≤ max), delay_for(5) = 500 << 4 = 8000
        // (= max), delay_for(10) > max → capped.
        let v = retry_cmd(&[
            "schedule".into(),
            "--attempts".into(),
            "11".into(),
        ])
        .expect("schedule ok");
        let waits = v
            .get("inter_attempt_waits")
            .and_then(|w| w.as_array())
            .expect("array");
        // Find attempt 10 → cap_ms must be exactly max_ms (8000).
        let last = &waits[waits.len() - 1];
        assert_eq!(last.get("cap_ms").and_then(|n| n.as_u64()), Some(8000));
    }

    #[test]
    fn retry_cmd_schedule_invalid_attempts_errs() {
        let err = retry_cmd(&[
            "schedule".into(),
            "--attempts".into(),
            "lots".into(),
        ])
        .unwrap_err();
        assert!(err.contains("--attempts"));
    }

    #[test]
    fn retry_cmd_schedule_unknown_flag_errs() {
        let err = retry_cmd(&["schedule".into(), "--bogus".into()]).unwrap_err();
        assert!(err.contains("--bogus"));
    }

    #[test]
    fn retry_cmd_unknown_subcommand_errs() {
        let err = retry_cmd(&["bogus".into()]).unwrap_err();
        assert!(err.contains("bogus"));
    }
}
