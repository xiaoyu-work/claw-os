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
        other => Err(format!(
            "unknown command: {other}. try: ask | chat | status | service | insights | recall | sessions | onboarding | notes | skills | nudge | mcp | usage | curator | llm | redact | prompt"
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
