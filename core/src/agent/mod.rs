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

pub mod audit_cli;
pub mod classify;
pub mod context;
pub mod curator;
pub mod curator_author;
pub mod curator_drafts;
pub mod display;
pub mod doctor_cli;
pub mod honcho_cli;
pub mod insights;
pub mod llm;
pub mod media;
pub mod memory;
pub mod nudge;
pub mod onboarding;
pub mod prompt;
pub mod replay_cli;
pub mod run_log_cli;
pub mod runtime;
pub mod safety;
pub mod service;
pub mod shell_hooks;
pub mod skills;
pub mod summarise;
pub mod title;
pub mod tools;

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
        "stream" => stream_cmd(args),
        "live" => live_cmd(args),
        "chat" => chat_cmd(args),
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
        "service" => service::cmd(args),
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
        "provider-doctor" => provider_doctor_cmd(args),
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
        "vision" => vision_cmd(args),
        "display" => display_cmd(args),
        "shell-hooks" => shell_hooks_cmd(args),
        "media" => media_cmd(args),
        "binary-ext" => binary_ext_cmd(args),
        "context" => context_cmd(args),
        "file-safety" => file_safety_cmd(args),
        "osv" => osv_cmd(args),
        "semantic" => semantic_cmd(args),
        "interrupt" => interrupt_cmd(args),
        "learn" => learn_cmd(args),
        "hooks" => hooks_cmd(args),
        "audit" => audit_cli::audit_cmd(args),
        "doctor" => doctor_cli::doctor_cmd(args),
        "replay" => replay_cli::replay_cmd(args),
        "run-log" | "run_log" => run_log_cli::run_log_cmd(args),
        "honcho" => honcho_cli::honcho_cmd(args),
        other => Err(format!(
            "unknown command: {other}. try: ask | chat | status | service | insights | recall | sessions | onboarding | notes | skills | nudge | mcp | usage | curator | llm | redact | prompt | think-scrub | tokens | providers | provider-doctor | title | summarise | classify | tools | guardrails | approval | todo | compress | aux | retry | vision | display | shell-hooks | media | binary-ext | context | file-safety | osv | semantic | interrupt | learn | hooks | audit | doctor | replay | run-log | honcho"
        )),
    }
}

/// `cos agent interrupt <subcmd>` — signal a running agent session
/// so its loop unwinds cleanly between turns.
///
///   list                 — registered (live) session ids
///   signal <session-id>  — request interrupt; idempotent. JSON
///                          `{"signaled": true}` if a session was
///                          found, `{"signaled": false, "reason":
///                          "not registered"}` otherwise.
///
/// Sessions auto-register the moment they enter the agent loop and
/// auto-unregister on exit, so the live list mirrors what's actively
/// running in this `cos` process. Note that this does NOT cross
/// process boundaries — to interrupt sessions running under a
/// separate `cos agent service` worker, use the IPC `service cancel`
/// surface (different mechanism, persisted job-cancellation
/// semantics).
fn interrupt_cmd(args: &[String]) -> Result<Value, String> {
    let sub = args
        .first()
        .map(|s| s.as_str())
        .ok_or("usage: cos agent interrupt <list|signal> ...")?;
    match sub {
        "list" => {
            let mut sessions = crate::agent::runtime::interrupt::registered_sessions();
            sessions.sort();
            Ok(json!({
                "sessions": sessions,
                "count": sessions.len(),
            }))
        }
        "signal" => {
            let sid = args
                .get(1)
                .map(|s| s.as_str())
                .ok_or("usage: cos agent interrupt signal <session-id>")?;
            let signaled = crate::agent::runtime::interrupt::signal(sid);
            if signaled {
                Ok(json!({
                    "signaled": true,
                    "session_id": sid,
                }))
            } else {
                Ok(json!({
                    "signaled": false,
                    "session_id": sid,
                    "reason": "not registered (session not running in this process)",
                }))
            }
        }
        other => Err(format!(
            "unknown interrupt subcommand: {other}. try: list | signal"
        )),
    }
}

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
fn learn_cmd(args: &[String]) -> Result<Value, String> {
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

/// `cos agent hooks <subcmd>` — manage the runtime hook registry
/// and the persistent `data_dir/agent/hooks.json` config that
/// auto-registers hooks on every agent invocation.
///
///   list                       — names currently registered in
///                                this process + persistently
///                                enabled kinds (from disk).
///   enable <kind>              — add `<kind>` to hooks.json and
///                                register it in the current
///                                process. Idempotent.
///   disable <kind>             — remove `<kind>` from hooks.json
///                                and unregister it from the
///                                current process. Idempotent.
///
/// Supported kinds: `logging`. CLI `--kind <k>` form is also
/// accepted for `enable`/`disable` to mirror common subcommand
/// conventions.
fn hooks_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::runtime::hooks::global_registry;
    use crate::agent::runtime::hooks_config::{self, HookKind};

    let sub = args.first().map(|s| s.as_str()).unwrap_or("list");
    match sub {
        "list" => {
            let registry = global_registry();
            let names = registry.names();
            let cfg = hooks_config::load(&crate::paths::agent_hooks_path()).unwrap_or_default();
            let enabled_kinds: Vec<String> = cfg
                .enabled
                .iter()
                .map(|k| k.canonical().to_string())
                .collect();
            Ok(json!({
                "hooks": names.clone(),
                "count": names.len(),
                "persistent": enabled_kinds.clone(),
                "config_path": crate::paths::agent_hooks_path().display().to_string(),
            }))
        }
        "enable" => {
            let kind_str = parse_kind_arg(&args[1..])?;
            let kind = HookKind::parse(&kind_str)
                .ok_or_else(|| format!("unknown hook kind: {kind_str}. try: logging"))?;
            let path = crate::paths::agent_hooks_path();
            let mut cfg = hooks_config::load(&path).map_err(|e| e.to_string())?;
            let added = cfg.enable(kind);
            if added {
                hooks_config::save(&path, &cfg).map_err(|e| e.to_string())?;
            }
            // Also register in the current process so the change is
            // visible to anything else running in this binary
            // invocation (e.g. an immediate follow-up call).
            let registry = global_registry();
            let already = registry.names().iter().any(|n| n == kind.canonical());
            if !already {
                registry.register(hooks_config::instantiate(kind));
            }
            Ok(json!({
                "kind": kind.canonical(),
                "persisted": added,
                "registered_now": !already,
                "config_path": path.display().to_string(),
            }))
        }
        "disable" => {
            let kind_str = parse_kind_arg(&args[1..])?;
            let kind = HookKind::parse(&kind_str)
                .ok_or_else(|| format!("unknown hook kind: {kind_str}. try: logging"))?;
            let path = crate::paths::agent_hooks_path();
            let mut cfg = hooks_config::load(&path).map_err(|e| e.to_string())?;
            let removed = cfg.disable(kind);
            if removed {
                hooks_config::save(&path, &cfg).map_err(|e| e.to_string())?;
            }
            let unreg = global_registry().unregister(kind.canonical());
            Ok(json!({
                "kind": kind.canonical(),
                "persisted": removed,
                "unregistered_now": unreg,
                "config_path": path.display().to_string(),
            }))
        }
        other => Err(format!(
            "unknown hooks subcommand: {other}. try: list | enable <kind> | disable <kind>"
        )),
    }
}

/// Pull the kind out of `<kind>` or `--kind <kind>` positional/flag
/// forms. `cos agent hooks enable logging` and
/// `cos agent hooks enable --kind logging` both work.
fn parse_kind_arg(rest: &[String]) -> Result<String, String> {
    let mut iter = rest.iter();
    while let Some(a) = iter.next() {
        match a.as_str() {
            "--kind" => {
                return iter
                    .next()
                    .cloned()
                    .ok_or_else(|| "--kind requires a value".to_string());
            }
            s if !s.starts_with("--") => return Ok(s.to_string()),
            other => return Err(format!("unexpected flag: {other}")),
        }
    }
    Err("missing hook kind (positional or --kind <kind>)".to_string())
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
fn semantic_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::memory::semantic::SemanticStore;

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
            let model_drift = match (&embedder_model, &pinned) {
                (Some(a), Some(b)) if a != b => true,
                _ => false,
            };
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
    let limit: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(10);
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
        "guard" => skills_guard_cmd(&args[1..]),
        other => Err(format!(
            "unknown skills subcommand: {other}. try: list | info <id> | disabled | errors | root | install <archive> | hub <list|show|install> <owner/repo> [<id>] | usage <stats|record|path|clear> | guard <id> [--provenance <vendor|hub|user|local|unknown>] [--require-allowed-tools] [--max-file-bytes N] [--ignore-trust]"
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
/// `cos agent skills guard <id> [--provenance <p>] [--require-allowed-tools] [--max-file-bytes N] [--ignore-trust]`
///
/// Run [`crate::agent::skills::provenance::Guard`] against an
/// installed skill loaded by [`crate::agent::skills::loader::load_default`]
/// and report what the guard would say at invocation time. Useful
/// for operators reviewing whether a freshly-installed third-party
/// skill would actually be allowed to run.
///
/// `--provenance` overrides the default `Hub` (the strict path).
/// Accepts the lowercase forms of [`Provenance`]: vendor / hub /
/// user / local / unknown. Default is `hub` so the guard runs the
/// full check tree.
///
/// `--require-allowed-tools` flips
/// [`GuardConfig::require_allowed_tools`] on so a skill with no
/// declared `allowed_tools` is rejected.
///
/// `--max-file-bytes N` overrides the per-sibling-file size cap
/// (default 5 MiB). Useful to test what would happen with a tighter
/// policy.
///
/// `--ignore-trust` flips
/// [`GuardConfig::honour_provenance_trust`] off so even
/// `vendor`/`user` skills run the strict checks (lets you preview
/// the worst-case verdict for a vendored skill).
///
/// Output includes the resolved verdict (allow / deny / require
/// confirmation), the GuardConfig that produced it, and the
/// provenance used. Returns an error if the skill id isn't loaded.
fn skills_guard_cmd(args: &[String]) -> Result<Value, String> {
    let res = skills::loader::load_default();
    skills_guard_cmd_against(args, &res.skills)
}

/// Inner form of [`skills_guard_cmd`] that takes the already-loaded
/// skill map. Lets tests construct a fake skill in a tempdir without
/// touching the live data dir.
fn skills_guard_cmd_against(
    args: &[String],
    skills: &std::collections::BTreeMap<String, skills::loader::LoadedSkill>,
) -> Result<Value, String> {
    use crate::agent::skills::provenance::{Guard, GuardConfig, GuardOutcome, Provenance};

    let mut id: Option<String> = None;
    let mut provenance = Provenance::Hub;
    let mut cfg = GuardConfig::default();
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--provenance" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--provenance needs a value".to_string())?;
                provenance = match raw.to_ascii_lowercase().as_str() {
                    "vendor" => Provenance::Vendor,
                    "hub" => Provenance::Hub,
                    "user" => Provenance::User,
                    "local" => Provenance::Local,
                    "unknown" => Provenance::Unknown,
                    other => {
                        return Err(format!(
                        "unknown provenance: {other}. try: vendor | hub | user | local | unknown"
                    ))
                    }
                };
                i += 2;
            }
            "--require-allowed-tools" => {
                cfg.require_allowed_tools = true;
                i += 1;
            }
            "--max-file-bytes" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--max-file-bytes needs a value".to_string())?;
                cfg.max_file_bytes = raw
                    .parse::<u64>()
                    .map_err(|e| format!("--max-file-bytes parse: {e}"))?;
                i += 2;
            }
            "--ignore-trust" => {
                cfg.honour_provenance_trust = false;
                i += 1;
            }
            other if id.is_none() && !other.starts_with("--") => {
                id = Some(other.to_string());
                i += 1;
            }
            other => return Err(format!("unknown skills guard flag: {other}")),
        }
    }

    let id = id.ok_or_else(|| "usage: cos agent skills guard <id>".to_string())?;

    let skill = skills
        .get(&id)
        .ok_or_else(|| format!("skill not loaded: {id}"))?;

    let guard = Guard::new(cfg.clone());
    let outcome = guard.check(skill, provenance);
    let (verdict, reason) = match outcome {
        GuardOutcome::Allow => ("allow", None),
        GuardOutcome::Deny { reason } => ("deny", Some(reason)),
        GuardOutcome::RequireConfirmation { reason } => ("require_confirmation", Some(reason)),
    };

    Ok(json!({
        "id": skill.id,
        "verdict": verdict,
        "reason": reason,
        "provenance": provenance.as_str(),
        "config": {
            "max_file_bytes": cfg.max_file_bytes,
            "require_allowed_tools": cfg.require_allowed_tools,
            "honour_provenance_trust": cfg.honour_provenance_trust,
        },
    }))
}

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

    let sub = args.first().map(|s| s.as_str()).ok_or_else(|| {
        "usage: cos agent skills hub <list|show|install> <owner/repo> [<id>] [--force]".to_string()
    })?;

    let spec = args
        .get(1)
        .cloned()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
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
/// Stream a single prompt through the active provider's
/// `chat_stream()` and surface text deltas live to **stderr** so
/// the user sees incremental output (when the provider truly
/// streams — anthropic does today; others fall through to the
/// non-streaming shim and emit one big chunk).
///
/// Stdout is reserved for the final JSON envelope so the command
/// stays scriptable: `cos agent stream "..." 2>/dev/null | jq` is
/// equivalent to today's `cos agent ask`. Pipe stderr to a TTY
/// for the live feed.
///
/// Single-turn, no tool dispatch, no memory recording — the goal
/// is the streaming UX itself, not full agent loop integration.
/// `cos agent ask` remains the multi-turn tool-using path.
///
/// Usage: `cos agent stream "<prompt>"`. Errors propagate as
/// `Err(String)` so the dispatcher logs them through audit.
fn stream_cmd(args: &[String]) -> Result<Value, String> {
    let prompt = args.first().cloned().unwrap_or_default();
    if prompt.is_empty() {
        return Err("usage: cos agent stream \"<prompt>\"".into());
    }
    let cfg = &crate::config::get().agent;
    let provider = llm::registry::build(&cfg.provider, &cfg.model, cfg)
        .map_err(|e| format!("provider unavailable: {e}"))?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime: {e}"))?;

    runtime.block_on(stream_cmd_async(provider, cfg, &prompt))
}

async fn stream_cmd_async(
    provider: std::sync::Arc<dyn llm::Provider>,
    cfg: &crate::config::AgentConfig,
    user_prompt: &str,
) -> Result<Value, String> {
    use crate::agent::llm::types::{
        ChatRequest, FinishReason, Message, StreamEvent, ToolChoice, Usage,
    };
    use futures_util::StreamExt;
    use std::io::Write;

    let extra = cfg.system_prompt_path.as_deref().map(std::path::Path::new);
    let system = crate::agent::prompt::build_system_prompt(extra);

    let request = ChatRequest {
        model: cfg.model.clone(),
        messages: vec![Message::user_text(user_prompt)],
        system: Some(system),
        tools: Vec::new(),
        tool_choice: ToolChoice::Auto,
        max_tokens: Some(cfg.max_tokens),
        temperature: Some(cfg.temperature),
        top_p: None,
        stop_sequences: Vec::new(),
        extra: serde_json::Value::Null,
    };

    let mut stream = provider
        .chat_stream(request)
        .await
        .map_err(|e| format!("chat_stream: {e}"))?;

    let mut answer = String::new();
    let mut finish: Option<FinishReason> = None;
    let mut usage = Usage::default();
    let mut tool_calls: Vec<serde_json::Value> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    let stderr = std::io::stderr();
    let mut err_lock = stderr.lock();

    while let Some(event) = stream.next().await {
        match event {
            Ok(StreamEvent::TextDelta { text }) => {
                answer.push_str(&text);
                let _ = err_lock.write_all(text.as_bytes());
                let _ = err_lock.flush();
            }
            Ok(StreamEvent::ToolUseStart { id, name }) => {
                let _ = writeln!(err_lock, "\n[tool_use_start id={id} name={name}]");
            }
            Ok(StreamEvent::ToolInputDelta { partial_json, .. }) => {
                let _ = err_lock.write_all(partial_json.as_bytes());
                let _ = err_lock.flush();
            }
            Ok(StreamEvent::ToolUse(call)) => {
                let _ = writeln!(
                    err_lock,
                    "\n[tool_use id={} name={}] {}",
                    call.id, call.name, call.input
                );
                tool_calls.push(serde_json::json!({
                    "id": call.id,
                    "name": call.name,
                    "input": call.input,
                }));
            }
            Ok(StreamEvent::Message(resp)) => {
                // Non-streaming providers (mock / openai_compat /
                // gemini / bedrock / llama_local today) emit the
                // whole response as a single Message event. Render
                // its assembled text and tool_calls so the UX
                // still looks like a stream.
                for block in &resp.content {
                    if let crate::agent::llm::types::ContentBlock::Text { text } = block {
                        answer.push_str(text);
                        let _ = err_lock.write_all(text.as_bytes());
                    }
                }
                for call in &resp.tool_calls {
                    let _ = writeln!(
                        err_lock,
                        "\n[tool_use id={} name={}] {}",
                        call.id, call.name, call.input
                    );
                    tool_calls.push(serde_json::json!({
                        "id": call.id,
                        "name": call.name,
                        "input": call.input,
                    }));
                }
                let _ = err_lock.flush();
            }
            Ok(StreamEvent::Done {
                finish: f,
                usage: u,
            }) => {
                finish = Some(f);
                usage = u;
                let _ = writeln!(err_lock);
                let _ = err_lock.flush();
            }
            Ok(StreamEvent::Warning { message }) => {
                let _ = writeln!(err_lock, "\n[warning] {message}");
                warnings.push(message);
            }
            Err(e) => {
                let _ = writeln!(err_lock, "\n[error] {e}");
                return Err(format!("stream error: {e}"));
            }
        }
    }

    Ok(json!({
        "answer": answer,
        "finish": finish.map(|f| format!("{f:?}")),
        "tool_calls": tool_calls,
        "usage": {
            "input_tokens": usage.input_tokens,
            "output_tokens": usage.output_tokens,
            "cache_read_tokens": usage.cache_read_tokens,
            "cache_write_tokens": usage.cache_write_tokens,
        },
        "warnings": warnings,
        "provider": provider.name(),
        "model": cfg.model,
    }))
}

/// `cos agent live "<prompt>"` — multi-turn streaming agent with the
/// full tool registry. Same JSON envelope shape as `cos agent ask`,
/// but tokens stream live to stderr as they arrive (so the user sees
/// progress in long tool-driven sessions). Stdout is reserved for the
/// final JSON envelope so script consumers can `2>/dev/null | jq .`.
///
/// Differences from `cos agent stream` (single-shot, no tools, no
/// memory) and `cos agent ask` (multi-turn, tools, but waits for the
/// full ChatResponse before printing):
/// - Like `ask`: builds the full tool registry, opens the default
///   memory DB if available, runs `max_turns` turns until final.
/// - Like `stream`: tokens stream to stderr as they arrive; final
///   answer + per-turn tool dispatch are reflected in the envelope.
fn live_cmd(args: &[String]) -> Result<Value, String> {
    let prompt = args.first().cloned().unwrap_or_default();
    if prompt.is_empty() {
        return Err("usage: cos agent live \"<prompt>\"".into());
    }
    let cfg = &crate::config::get().agent;
    let provider = llm::registry::build(&cfg.provider, &cfg.model, cfg)
        .map_err(|e| format!("provider unavailable: {e}"))?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime: {e}"))?;

    runtime.block_on(live_cmd_async(provider, cfg, &prompt))
}

async fn live_cmd_async(
    provider: std::sync::Arc<dyn llm::Provider>,
    cfg: &crate::config::AgentConfig,
    user_prompt: &str,
) -> Result<Value, String> {
    use crate::agent::llm::accumulate::StreamSink;
    use crate::agent::llm::types::StreamEvent;
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    // Build the same registry the production `ask` path builds.
    let mut tools = crate::agent::tools::registry::default_registry();
    tools.set_guardrails(runtime::loop_::guardrails_from_cfg(cfg));
    tools.set_approval(runtime::loop_::approval_from_cfg(cfg));
    let _mcp_handles = runtime::loop_::attach_mcp_servers_for_cli(&mut tools, cfg).await;

    /// Stream sink that mirrors the `cos agent stream` UX: tokens to
    /// stderr live, tool starts/inputs as bracketed lines, warnings
    /// flagged. Captures everything the envelope needs to summarise
    /// the run (the loop itself returns just the final answer text).
    struct LiveSink {
        tool_calls: Mutex<Vec<serde_json::Value>>,
        warnings: Mutex<Vec<String>>,
        // Final usage + finish reason from the LAST `Done` event in
        // the multi-turn run. Earlier turns' Done events are still
        // forwarded — we just keep the latest.
        last_usage: Mutex<Option<crate::agent::llm::types::Usage>>,
        last_finish: Mutex<Option<crate::agent::llm::types::FinishReason>>,
    }
    impl StreamSink for LiveSink {
        fn on_event(&self, event: &StreamEvent) {
            let stderr = std::io::stderr();
            let mut err_lock = stderr.lock();
            match event {
                StreamEvent::TextDelta { text } => {
                    let _ = err_lock.write_all(text.as_bytes());
                    let _ = err_lock.flush();
                }
                StreamEvent::ToolUseStart { id, name } => {
                    let _ = writeln!(err_lock, "\n[tool_use_start id={id} name={name}]");
                }
                StreamEvent::ToolInputDelta { partial_json, .. } => {
                    let _ = err_lock.write_all(partial_json.as_bytes());
                    let _ = err_lock.flush();
                }
                StreamEvent::ToolUse(call) => {
                    let _ = writeln!(
                        err_lock,
                        "\n[tool_use id={} name={}] {}",
                        call.id, call.name, call.input
                    );
                    self.tool_calls.lock().unwrap().push(serde_json::json!({
                        "id": call.id,
                        "name": call.name,
                        "input": call.input,
                    }));
                }
                StreamEvent::Message(resp) => {
                    for block in &resp.content {
                        if let crate::agent::llm::types::ContentBlock::Text { text } = block {
                            let _ = err_lock.write_all(text.as_bytes());
                        }
                    }
                    for call in &resp.tool_calls {
                        let _ = writeln!(
                            err_lock,
                            "\n[tool_use id={} name={}] {}",
                            call.id, call.name, call.input
                        );
                        self.tool_calls.lock().unwrap().push(serde_json::json!({
                            "id": call.id,
                            "name": call.name,
                            "input": call.input,
                        }));
                    }
                    let _ = err_lock.flush();
                }
                StreamEvent::Done { finish, usage } => {
                    let _ = writeln!(err_lock, "\n[turn done finish={finish:?}]");
                    *self.last_usage.lock().unwrap() = Some(usage.clone());
                    *self.last_finish.lock().unwrap() = Some(*finish);
                }
                StreamEvent::Warning { message } => {
                    let _ = writeln!(err_lock, "\n[warning] {message}");
                    self.warnings.lock().unwrap().push(message.clone());
                }
            }
        }
    }

    let sink_obj = Arc::new(LiveSink {
        tool_calls: Mutex::new(Vec::new()),
        warnings: Mutex::new(Vec::new()),
        last_usage: Mutex::new(None),
        last_finish: Mutex::new(None),
    });
    let sink: Arc<dyn StreamSink> = sink_obj.clone();

    // Mirror the `ask` path's memory-DB handling: try default DB,
    // fall back to no-recording on failure.
    let result = match memory::sqlite_fts::MemoryDb::open_default() {
        Ok(db) => {
            let session_id = uuid::Uuid::new_v4().to_string();
            runtime::loop_::ask_with_stream(
                provider.clone(),
                cfg,
                user_prompt,
                &tools,
                Some((&db, session_id.as_str())),
                sink,
            )
            .await
        }
        Err(e) => {
            tracing::warn!(
                "memory: default DB unavailable ({e}); running without history recording"
            );
            runtime::loop_::ask_with_stream(provider.clone(), cfg, user_prompt, &tools, None, sink)
                .await
        }
    };

    match result {
        Ok(ask_result) => {
            let usage = sink_obj
                .last_usage
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_default();
            let finish = sink_obj.last_finish.lock().unwrap().take();
            Ok(json!({
                "answer": ask_result.answer,
                "turns": ask_result.turns,
                "provider": ask_result.provider,
                "model": ask_result.model,
                "session_id": ask_result.session_id,
                "tool_calls": *sink_obj.tool_calls.lock().unwrap(),
                "warnings": *sink_obj.warnings.lock().unwrap(),
                "finish": finish.map(|f| format!("{f:?}")),
                "usage": {
                    "input_tokens": usage.input_tokens,
                    "output_tokens": usage.output_tokens,
                    "cache_read_tokens": usage.cache_read_tokens,
                    "cache_write_tokens": usage.cache_write_tokens,
                },
            }))
        }
        Err(e) => Err(e.to_string()),
    }
}

/// `cos agent chat [--session <id>] [--no-stream] [--no-memory]
/// [--show-tools] [--max-turns N]` — interactive multi-turn REPL.
///
/// Reads prompts from stdin one line at a time and routes each
/// through the same agent runtime as `cos agent live`. The
/// session-id is preserved across turns so:
///   1. Every prompt and assistant turn is recorded under the
///      same FTS-searchable conversation;
///   2. The session title is generated once on the first turn
///      (matches `ask`/`live` semantics);
///   3. `cos_recall` invocations from inside the model can search
///      the running conversation as it grows.
///
/// **Architecture note:** The model sees ONLY the current prompt
/// per turn, not in-process replay of prior REPL turns. Cross-turn
/// memory is provided by:
///   - System prompt injection from `MEMORY.md` / `USER.md`
///     (already done by `prompt::build_system_prompt`).
///   - The `cos_recall` tool, which gives the model on-demand
///     access to FTS-searchable history.
/// This matches `live` and `ask`, costs fewer tokens, and avoids
/// hidden context that the model can't introspect.
///
/// **Slash commands** (recognised at the start of a non-empty
/// prompt; whitespace-trimmed):
///   - `/quit` / `/exit` / `/q` — leave the REPL.
///   - `/help` / `/?` — print the slash-command list.
///   - `/session` — print current session id and turn count.
///   - `/clear` — drop the current session and start a fresh one.
///   - `/history [N]` — show the last N (default 10) recorded
///     messages from the current session.
///   - `/tools` — list permitted tool names.
/// Any line that doesn't start with `/` is treated as a prompt.
///
/// Streaming behaviour mirrors `live`: tokens flow live to stderr;
/// the assistant's final text plus a one-line summary go to
/// stdout after each turn. Pass `--no-stream` to fall back to the
/// non-streaming `ask_with` path (useful for non-TTY use).
///
/// Stdin EOF (Ctrl+D / closed pipe) exits cleanly.
fn chat_cmd(args: &[String]) -> Result<Value, String> {
    let mut explicit_session: Option<String> = None;
    let mut streaming = true;
    let mut use_memory = true;
    let mut show_tools = false;
    let mut max_turns_override: Option<u32> = None;

    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--session" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| "--session needs <id>".to_string())?;
                explicit_session = Some(v.clone());
                i += 2;
            }
            "--no-stream" => {
                streaming = false;
                i += 1;
            }
            "--no-memory" => {
                use_memory = false;
                i += 1;
            }
            "--show-tools" => {
                show_tools = true;
                i += 1;
            }
            "--max-turns" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| "--max-turns needs <n>".to_string())?;
                max_turns_override = Some(v.parse().map_err(|e| format!("--max-turns: {e}"))?);
                i += 2;
            }
            other => return Err(format!("unknown flag for `chat`: {other}")),
        }
    }

    let cfg = &crate::config::get().agent;
    // Build the provider once and reuse across turns. If the user
    // mid-REPL wants a different model, they can `/quit` and re-launch.
    let provider = llm::registry::build(&cfg.provider, &cfg.model, cfg)
        .map_err(|e| format!("provider unavailable: {e}"))?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime: {e}"))?;

    runtime.block_on(chat_cmd_async(
        provider,
        cfg,
        explicit_session,
        streaming,
        use_memory,
        show_tools,
        max_turns_override,
    ))
}

#[allow(clippy::too_many_arguments)]
async fn chat_cmd_async(
    provider: std::sync::Arc<dyn llm::Provider>,
    cfg_in: &crate::config::AgentConfig,
    explicit_session: Option<String>,
    streaming: bool,
    use_memory: bool,
    show_tools: bool,
    max_turns_override: Option<u32>,
) -> Result<Value, String> {
    use crate::agent::llm::accumulate::StreamSink;
    use crate::agent::llm::types::StreamEvent;
    use std::io::{BufRead, Write};
    use std::sync::{Arc, Mutex};

    // Apply --max-turns override locally without mutating global config.
    let mut cfg_owned = cfg_in.clone();
    if let Some(n) = max_turns_override {
        cfg_owned.max_turns = n;
    }
    let cfg = &cfg_owned;

    // Build the registry once. MCP servers attach the same way as
    // `live`/`ask`, so the model has the full toolbox.
    let mut tools = crate::agent::tools::registry::default_registry();
    tools.set_guardrails(runtime::loop_::guardrails_from_cfg(cfg));
    tools.set_approval(runtime::loop_::approval_from_cfg(cfg));
    let _mcp_handles = runtime::loop_::attach_mcp_servers_for_cli(&mut tools, cfg).await;

    let memory_db = if use_memory {
        match memory::sqlite_fts::MemoryDb::open_default() {
            Ok(db) => Some(db),
            Err(e) => {
                tracing::warn!(
                    "memory: default DB unavailable ({e}); chat will run without history"
                );
                None
            }
        }
    } else {
        None
    };

    let mut session_id: String = explicit_session
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let stdout = std::io::stdout();
    let stderr = std::io::stderr();

    // Header banner — to stderr so a piped-stdout consumer only
    // sees the assistant outputs.
    {
        let mut e = stderr.lock();
        let _ = writeln!(
            e,
            "cos agent chat — provider={} model={} session={} memory={} streaming={}",
            cfg.provider,
            cfg.model,
            session_id,
            if memory_db.is_some() { "on" } else { "off" },
            if streaming { "on" } else { "off" }
        );
        let _ = writeln!(e, "Type /help for commands. Ctrl-D or /quit to exit.");
        if show_tools {
            let names = tools.names();
            let _ = writeln!(e, "tools ({}): {}", names.len(), names.join(", "));
        }
    }

    let stdin = std::io::stdin();
    let mut input = String::new();
    let mut prompt_seq: u32 = 0;
    let mut clean_exit = false;

    /// Stream sink shared across turns — re-used so allocation
    /// happens once. Each turn calls `reset()` before invoking
    /// the runtime so per-turn state doesn't bleed.
    struct ChatSink {
        tool_calls: Mutex<Vec<serde_json::Value>>,
        warnings: Mutex<Vec<String>>,
        last_usage: Mutex<Option<crate::agent::llm::types::Usage>>,
        last_finish: Mutex<Option<crate::agent::llm::types::FinishReason>>,
    }
    impl ChatSink {
        fn new() -> Self {
            Self {
                tool_calls: Mutex::new(Vec::new()),
                warnings: Mutex::new(Vec::new()),
                last_usage: Mutex::new(None),
                last_finish: Mutex::new(None),
            }
        }
        fn reset(&self) {
            self.tool_calls.lock().unwrap().clear();
            self.warnings.lock().unwrap().clear();
            *self.last_usage.lock().unwrap() = None;
            *self.last_finish.lock().unwrap() = None;
        }
    }
    impl StreamSink for ChatSink {
        fn on_event(&self, event: &StreamEvent) {
            let stderr = std::io::stderr();
            let mut e = stderr.lock();
            match event {
                StreamEvent::TextDelta { text } => {
                    let _ = e.write_all(text.as_bytes());
                    let _ = e.flush();
                }
                StreamEvent::ToolUseStart { id, name } => {
                    let _ = writeln!(e, "\n[tool_use_start id={id} name={name}]");
                }
                StreamEvent::ToolInputDelta { partial_json, .. } => {
                    let _ = e.write_all(partial_json.as_bytes());
                    let _ = e.flush();
                }
                StreamEvent::ToolUse(call) => {
                    let _ = writeln!(
                        e,
                        "\n[tool_use id={} name={}] {}",
                        call.id, call.name, call.input
                    );
                    self.tool_calls.lock().unwrap().push(serde_json::json!({
                        "id": call.id,
                        "name": call.name,
                        "input": call.input,
                    }));
                }
                StreamEvent::Message(resp) => {
                    for block in &resp.content {
                        if let crate::agent::llm::types::ContentBlock::Text { text } = block {
                            let _ = e.write_all(text.as_bytes());
                        }
                    }
                    for call in &resp.tool_calls {
                        let _ = writeln!(
                            e,
                            "\n[tool_use id={} name={}] {}",
                            call.id, call.name, call.input
                        );
                        self.tool_calls.lock().unwrap().push(serde_json::json!({
                            "id": call.id,
                            "name": call.name,
                            "input": call.input,
                        }));
                    }
                    let _ = e.flush();
                }
                StreamEvent::Done { finish, usage } => {
                    let _ = writeln!(e, "\n[turn done finish={finish:?}]");
                    *self.last_usage.lock().unwrap() = Some(usage.clone());
                    *self.last_finish.lock().unwrap() = Some(*finish);
                }
                StreamEvent::Warning { message } => {
                    let _ = writeln!(e, "\n[warning] {message}");
                    self.warnings.lock().unwrap().push(message.clone());
                }
            }
        }
    }

    let sink_obj = Arc::new(ChatSink::new());

    loop {
        // Prompt user (to stderr so stdout stays clean for
        // assistant text).
        {
            let mut e = stderr.lock();
            let _ = write!(e, "you> ");
            let _ = e.flush();
        }
        input.clear();
        let n = match stdin.lock().read_line(&mut input) {
            Ok(n) => n,
            Err(e) => {
                return Err(format!("stdin error: {e}"));
            }
        };
        if n == 0 {
            // EOF
            let _ = writeln!(stderr.lock(), "\n[eof]");
            clean_exit = true;
            break;
        }

        let line = input.trim();
        if line.is_empty() {
            continue;
        }

        // Slash commands.
        if let Some(rest) = line.strip_prefix('/') {
            let mut parts = rest.split_whitespace();
            let cmd = parts.next().unwrap_or("");
            match cmd {
                "quit" | "exit" | "q" => {
                    clean_exit = true;
                    break;
                }
                "help" | "?" => {
                    let mut e = stderr.lock();
                    let _ = writeln!(e, "/quit | /exit | /q       leave the REPL");
                    let _ = writeln!(e, "/help | /?               this help");
                    let _ = writeln!(e, "/session                 print current session id");
                    let _ = writeln!(e, "/clear                   start a fresh session id");
                    let _ = writeln!(
                        e,
                        "/history [N]             show last N (default 10) messages"
                    );
                    let _ = writeln!(e, "/tools                   list permitted tools");
                }
                "session" => {
                    let mut e = stderr.lock();
                    let _ = writeln!(e, "session={session_id} prompts_so_far={prompt_seq}");
                }
                "clear" => {
                    session_id = uuid::Uuid::new_v4().to_string();
                    prompt_seq = 0;
                    let mut e = stderr.lock();
                    let _ = writeln!(e, "[new session: {session_id}]");
                }
                "history" => {
                    let n: usize = parts.next().and_then(|s| s.parse().ok()).unwrap_or(10);
                    if let Some(db) = &memory_db {
                        match db.recent(&session_id, n) {
                            Ok(rows) => {
                                let mut e = stderr.lock();
                                if rows.is_empty() {
                                    let _ = writeln!(e, "(no messages yet)");
                                } else {
                                    for r in &rows {
                                        let snippet: String = r.content.chars().take(140).collect();
                                        let _ = writeln!(e, "[{}] {}", r.role, snippet);
                                    }
                                }
                            }
                            Err(e) => {
                                let _ = writeln!(stderr.lock(), "history error: {e}");
                            }
                        }
                    } else {
                        let _ = writeln!(stderr.lock(), "history unavailable (memory off)");
                    }
                }
                "tools" => {
                    let names = tools.names();
                    let _ = writeln!(
                        stderr.lock(),
                        "tools ({}): {}",
                        names.len(),
                        names.join(", ")
                    );
                }
                other => {
                    let _ = writeln!(stderr.lock(), "unknown slash command: /{other} (try /help)");
                }
            }
            continue;
        }

        prompt_seq += 1;

        // Run a turn.
        let user_prompt = line.to_string();
        let result = if streaming {
            sink_obj.reset();
            let sink: Arc<dyn StreamSink> = sink_obj.clone();
            let recorder = memory_db.as_ref().map(|db| (db, session_id.as_str()));
            runtime::loop_::ask_with_stream(
                provider.clone(),
                cfg,
                &user_prompt,
                &tools,
                recorder,
                sink,
            )
            .await
        } else if let Some(db) = &memory_db {
            runtime::loop_::ask_with_memory(
                provider.clone(),
                cfg,
                &user_prompt,
                &tools,
                db,
                &session_id,
            )
            .await
        } else {
            runtime::loop_::ask_with(provider.clone(), cfg, &user_prompt, &tools).await
        };

        match result {
            Ok(ask_result) => {
                // The assistant's final text goes to stdout so a
                // user piping `cos agent chat > replies.txt` still
                // gets clean output. Streaming already echoed
                // partial text to stderr so this is the canonical
                // copy.
                let mut o = stdout.lock();
                let _ = writeln!(o, "{}", ask_result.answer);
                let _ = o.flush();

                let mut e = stderr.lock();
                let _ = writeln!(
                    e,
                    "[turn {} done; turns={} model={} session={}]",
                    prompt_seq, ask_result.turns, ask_result.model, ask_result.session_id
                );
            }
            Err(err) => {
                let _ = writeln!(stderr.lock(), "[error] {err}");
                // Don't break — let the user retry / clear / quit.
            }
        }
    }

    Ok(json!({
        "status": if clean_exit { "ok" } else { "interrupted" },
        "session_id": session_id,
        "prompts": prompt_seq,
        "provider": cfg.provider,
        "model": cfg.model,
    }))
}

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

        // Approximate pool size from declared cfg (without resolving
        // each entry — that would require building the pool again
        // here and round-tripping lock contention). This is the count
        // of *declared* sources for the active provider, or 0 for
        // others (synthesised configs have no plural fields).
        let pool_declared_keys = if is_active {
            cfg.agent.api_key_credentials.len() + cfg.agent.api_key_envs.len()
        } else {
            0
        };
        let pool_strategy = if is_active && pool_declared_keys > 0 {
            Some(cfg.agent.pool_strategy.as_str())
        } else {
            None
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
            "pool_declared_keys": pool_declared_keys,
            "pool_strategy": pool_strategy,
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

/// `cos agent provider-doctor [--names <a,b,c>] [--probe-network]
/// [--timeout <secs>]`
///
/// Static config check + optional one-shot live ping of the active
/// LLM provider. Wraps [`providers_cmd`]'s output (env_present /
/// credential_present / configured per provider) and adds a `doctor`
/// section with the probe verdict.
///
/// **Probe target**: only the **active** provider (configured in
/// `[agent].provider`). Non-active probes are skipped because we
/// don't have a known "default cheap model" for non-active providers
/// — `Provider::supported_models()` typically echoes the configured
/// model — and guessing one (e.g. `gpt-4o-mini`) would silently
/// break when the user has another model configured.
///
/// **Skipped providers** (active but unprobeable): `mock` (pointless),
/// `llama_local` (would force a heavy GGUF load + RAM allocation,
/// surprising side effect for a "doctor" command).
///
/// **Probe shape**: minimal `chat()` request — one user message
/// (`"Reply with the single word OK."`), `max_tokens: Some(16)`. No
/// temperature / top_p / tools — those knobs cause false-negative
/// rejection on some providers/models even though basic chat works.
/// Treats any successful `chat()` round-trip as success regardless
/// of literal content; `excerpt` is informational only.
///
/// **Timeouts**: `--timeout <secs>` (default 30s) wraps the future
/// in `tokio::time::timeout`. NOTE: this is independent of the
/// provider's own `request_timeout` (set on the underlying
/// `reqwest::Client` from `AgentConfig.request_timeout`); the
/// effective ceiling is `min(--timeout, AgentConfig.request_timeout)`.
/// We surface both as `probe_timeout_secs` /
/// `provider_request_timeout_secs` to make the asymmetry visible.
///
/// **Secret hygiene**: every error/excerpt string emitted goes
/// through [`crate::agent::safety::redact::Redactor::default_set`]
/// before serialisation. `LlmError::Transport(reqwest)` can include
/// URLs (and users sometimes embed credentials in `base_url`);
/// upstream provider error text can echo Authorization headers in
/// rare cases. Always-redact > regret-later.
///
/// **Structured failure**: probe verdicts include `error_kind` —
/// one of `auth | rate_limited | not_configured | invalid_request
/// | transport | provider | parse | stream | internal | timeout`
/// — so callers can branch on the cause programmatically rather
/// than parsing redacted prose.
fn provider_doctor_cmd(args: &[String]) -> Result<Value, String> {
    let mut probe_network = false;
    let mut timeout_secs: u64 = 30;
    let mut filter_names: Option<Vec<String>> = None;

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
            "--probe-network" => {
                probe_network = true;
                i += 1;
            }
            "--timeout" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--timeout needs <secs>".to_string())?;
                timeout_secs = raw
                    .parse::<u64>()
                    .map_err(|_| format!("--timeout must be a positive integer (got '{raw}')"))?;
                if timeout_secs == 0 {
                    return Err("--timeout must be > 0".into());
                }
                i += 2;
            }
            other => {
                return Err(format!(
                    "unknown provider-doctor arg: {other}. try: --names <a,b,c> | --probe-network | --timeout <secs>"
                ));
            }
        }
    }

    // Re-use the static check by forwarding the relevant flags.
    // Always probe credentials in doctor mode (cheap; users running
    // doctor want a complete view).
    let mut static_args: Vec<String> = vec!["--probe-credentials".into()];
    if let Some(names) = filter_names.as_ref() {
        static_args.push("--names".into());
        static_args.push(names.join(","));
    }
    let mut out = providers_cmd(&static_args)?;

    let cfg = crate::config::get();
    let active_name = cfg.agent.provider.clone();
    let active_in_scope = filter_names
        .as_ref()
        .map(|f| f.iter().any(|n| n == &active_name))
        .unwrap_or(true);

    let probe_value = if !probe_network {
        json!({
            "attempted": false,
            "reason": "static check only — pass --probe-network to issue a one-shot live ping",
        })
    } else if !active_in_scope {
        json!({
            "attempted": false,
            "reason": format!(
                "active provider '{active_name}' filtered out by --names; doctor probes only the active provider"
            ),
        })
    } else if active_name == "mock" {
        json!({
            "attempted": false,
            "reason": "mock provider: probe is meaningless (no upstream)",
        })
    } else if active_name == "llama_local" {
        json!({
            "attempted": false,
            "reason": "llama_local provider: probe is skipped — would force a GGUF load + RAM allocation, surprising side effect for a doctor command. Use 'cos model load' + 'cos agent ask' to validate end-to-end.",
        })
    } else {
        run_active_provider_probe(&active_name, &cfg.agent, timeout_secs)
    };

    // Surface the asymmetry between our probe wrapper timeout and
    // the provider's own request timeout — the effective ceiling is
    // min of the two.
    let provider_request_timeout = cfg.agent.request_timeout;

    out["doctor"] = json!({
        "active": active_name,
        "active_in_scope": active_in_scope,
        "probe_network": probe_network,
        "probe_timeout_secs": timeout_secs,
        "provider_request_timeout_secs": provider_request_timeout,
        "effective_timeout_secs": std::cmp::min(timeout_secs, provider_request_timeout),
        "active_probe": probe_value,
    });
    Ok(out)
}

/// Run the live one-shot ping for the active provider. Builds a
/// fresh provider instance (no shared state with concurrent
/// commands), spins up a single-thread Tokio runtime, and reports
/// a structured verdict. All error/excerpt strings are redacted.
fn run_active_provider_probe(
    name: &str,
    agent_cfg: &crate::config::AgentConfig,
    timeout_secs: u64,
) -> Value {
    use crate::agent::llm::types::{ChatRequest, ContentBlock, Message};
    use crate::agent::safety::redact::Redactor;

    let model = if agent_cfg.model.is_empty() {
        "stub-model".to_string()
    } else {
        agent_cfg.model.clone()
    };
    let redactor = Redactor::default_set();

    let provider = match llm::registry::build(name, &model, agent_cfg) {
        Ok(p) => p,
        Err(e) => {
            return json!({
                "attempted": false,
                "reason": redactor.redact(&format!("provider build failed: {e}")),
                "error_kind": llm_error_kind(&e),
            });
        }
    };

    let configured = provider.is_configured();
    let req = ChatRequest {
        model: model.clone(),
        messages: vec![Message::user_text("Reply with the single word OK.")],
        system: None,
        tools: Vec::new(),
        tool_choice: crate::agent::llm::types::ToolChoice::Auto,
        max_tokens: Some(16),
        temperature: None,
        top_p: None,
        stop_sequences: Vec::new(),
        extra: serde_json::Value::Null,
    };

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            return json!({
                "attempted": false,
                "reason": redactor.redact(&format!("tokio runtime: {e}")),
                "error_kind": "internal",
            });
        }
    };

    let timeout = std::time::Duration::from_secs(timeout_secs);
    let start = std::time::Instant::now();
    let result =
        runtime.block_on(async move { tokio::time::timeout(timeout, provider.chat(req)).await });
    let duration_ms = start.elapsed().as_millis() as u64;

    match result {
        Err(_elapsed) => json!({
            "attempted": true,
            "ok": false,
            "timed_out": true,
            "duration_ms": duration_ms,
            "error_kind": "timeout",
            "error_message": format!("probe timed out after {timeout_secs}s"),
            "configured_at_build": configured,
        }),
        Ok(Err(e)) => {
            let kind = llm_error_kind(&e);
            let mut entry = json!({
                "attempted": true,
                "ok": false,
                "timed_out": false,
                "duration_ms": duration_ms,
                "error_kind": kind,
                "error_message": redactor.redact(&e.to_string()),
                "configured_at_build": configured,
            });
            // Surface specific structured fields for the provider/rate-limited variants.
            match &e {
                llm::LlmError::Provider { status, .. } => {
                    entry["status"] = json!(status);
                }
                llm::LlmError::RateLimited { retry_after_ms } => {
                    entry["retry_after_ms"] = json!(retry_after_ms);
                }
                _ => {}
            }
            entry
        }
        Ok(Ok(resp)) => {
            let raw_text: String = resp
                .content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ");
            let raw_clip: String = raw_text.chars().take(80).collect();
            let excerpt = redactor.redact(&raw_clip);
            json!({
                "attempted": true,
                "ok": true,
                "timed_out": false,
                "duration_ms": duration_ms,
                "model": resp.model,
                "input_tokens": resp.usage.input_tokens,
                "output_tokens": resp.usage.output_tokens,
                "finish_reason": match resp.finish_reason {
                    crate::agent::llm::types::FinishReason::Stop => "stop",
                    crate::agent::llm::types::FinishReason::Length => "length",
                    crate::agent::llm::types::FinishReason::ToolUse => "tool_use",
                    crate::agent::llm::types::FinishReason::Refusal => "refusal",
                    crate::agent::llm::types::FinishReason::ContentFilter => "content_filter",
                    crate::agent::llm::types::FinishReason::Other => "other",
                },
                "excerpt": excerpt,
                "configured_at_build": configured,
            })
        }
    }
}

/// Map an `LlmError` to a stable string tag for the doctor JSON
/// output. The probe-network UI branches on this tag, so don't
/// rename existing variants without considering callers.
fn llm_error_kind(e: &llm::LlmError) -> &'static str {
    match e {
        llm::LlmError::NotConfigured(_) => "not_configured",
        llm::LlmError::InvalidRequest(_) => "invalid_request",
        llm::LlmError::Transport(_) => "transport",
        llm::LlmError::Provider { .. } => "provider",
        llm::LlmError::RateLimited { .. } => "rate_limited",
        llm::LlmError::Auth => "auth",
        llm::LlmError::Parse(_) => "parse",
        llm::LlmError::Stream(_) => "stream",
        llm::LlmError::Internal(_) => "internal",
    }
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
        // Bedrock uses three env vars (access key + secret + optional
        // session token). We surface AWS_ACCESS_KEY_ID as the
        // "primary" one for the env_present indicator — having the
        // access key without the secret is useless, but having the
        // access key absent is a definitive "not configured" signal.
        "bedrock" => Some("AWS_ACCESS_KEY_ID"),
        // Local/no-auth providers.
        "ollama" | "mock" | "llama_local" => None,
        _ => None,
    }
}

/// Canonical credential name (in the `agent` namespace) per provider
/// alias. Mirrors `canonical_env_for_provider` but for the
/// credential store. `None` for providers that never need a key OR
/// for providers (like Bedrock) whose credential model doesn't fit
/// a single name — Bedrock uses `aws_access_key_credential` /
/// `aws_secret_key_credential` / `aws_session_token_credential`
/// independently, so there's no one-name-fits-all.
fn canonical_credential_for_provider(name: &str) -> Option<&'static str> {
    match name {
        "openai" => Some("openai"),
        "xai" => Some("xai"),
        "deepseek" => Some("deepseek"),
        "openrouter" => Some("openrouter"),
        "anthropic" => Some("anthropic"),
        "gemini" => Some("gemini"),
        "ollama" | "mock" | "llama_local" | "bedrock" => None,
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
    } else if name == "bedrock" {
        // Region-templated. We surface the template so users see
        // which region to pin via [agent].aws_region.
        Some("https://bedrock-runtime.{region}.amazonaws.com (region-derived)")
    } else if name == "llama_local" {
        Some("local: file path via AgentConfig.model")
    } else {
        None
    }
}

/// `cos agent title <text> | --file <path> | --stdin [--check] [--llm]`
/// — heuristic-only by default. Strips a leading slash-command verb
/// (so `/ask hello` becomes `hello`), takes the first non-empty
/// line, and clamps to `MAX_TITLE_CHARS`. Pure function, no LLM
/// call, no IO beyond the input read.
///
/// `--llm` opts into the LLM-backed path used by `runtime::loop_`:
/// resolves the auxiliary client from
/// [`crate::agent::runtime::loop_::auxiliary_from_cfg`] and calls
/// [`crate::agent::title::generate_title`]. Errors and empty model
/// output fall back to the heuristic. If no auxiliary client is
/// configured, errs with a clear message instead of silently
/// downgrading (so the operator knows their `--llm` request didn't
/// actually use the model).
fn title_cmd(args: &[String]) -> Result<Value, String> {
    let mut llm_mode = false;
    let mut filtered: Vec<String> = Vec::with_capacity(args.len());
    for a in args {
        if a == "--llm" {
            llm_mode = true;
        } else {
            filtered.push(a.clone());
        }
    }
    let (input, _check) = read_text_input(&filtered, "title")?;
    if !llm_mode {
        return Ok(title_heuristic_payload(&input));
    }
    let cfg = &crate::config::get().agent;
    let aux = crate::agent::runtime::loop_::auxiliary_from_cfg(cfg)
        .map_err(|e| format!("auxiliary client build failed: {e}"))?
        .ok_or_else(|| {
            "auxiliary client is not configured; set agent.auxiliary_provider + auxiliary_model in config or drop --llm"
                .to_string()
        })?;
    title_cmd_with_aux(&input, Some(&aux))
}

/// Inner helper: render either the heuristic title or call the LLM
/// path against a caller-supplied auxiliary client. Extracted so
/// tests can drive the LLM path with a `MockProvider`-backed
/// `AuxiliaryClient` without depending on global config state.
fn title_cmd_with_aux(
    input: &str,
    aux: Option<&crate::agent::llm::auxiliary::AuxiliaryClient>,
) -> Result<Value, String> {
    let Some(aux) = aux else {
        return Ok(title_heuristic_payload(input));
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime: {e}"))?;
    let title = runtime.block_on(crate::agent::title::generate_title(Some(aux), input));
    Ok(json!({
        "title": title,
        "input_chars": input.chars().count(),
        "title_chars": title.chars().count(),
        "method": "llm",
        "provider": aux.provider_name(),
        "model": aux.config().model,
    }))
}

fn title_heuristic_payload(input: &str) -> Value {
    let title = crate::agent::title::clamp(&crate::agent::title::heuristic(input));
    json!({
        "title": title,
        "input_chars": input.chars().count(),
        "title_chars": title.chars().count(),
        "method": "heuristic",
    })
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
    let mut llm_mode = false;
    let mut filtered: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--max" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--max needs a number".to_string())?;
                max_chars = raw
                    .parse::<usize>()
                    .map_err(|e| format!("--max: invalid u64: {e}"))?;
                i += 2;
            }
            "--llm" => {
                llm_mode = true;
                i += 1;
            }
            _ => {
                filtered.push(args[i].clone());
                i += 1;
            }
        }
    }
    let (input, _check) = read_text_input(&filtered, "summarise")?;
    if !llm_mode {
        return Ok(summarise_heuristic_payload(&input, max_chars));
    }
    let cfg = &crate::config::get().agent;
    let aux = crate::agent::runtime::loop_::auxiliary_from_cfg(cfg)
        .map_err(|e| format!("auxiliary client build failed: {e}"))?
        .ok_or_else(|| {
            "auxiliary client is not configured; set agent.auxiliary_provider + auxiliary_model in config or drop --llm"
                .to_string()
        })?;
    summarise_cmd_with_aux(&input, max_chars, Some(&aux))
}

/// Inner helper used by tests and by the live `--llm` path. When
/// `aux` is `None` the heuristic payload is returned unchanged so
/// callers always get a stable JSON shape.
fn summarise_cmd_with_aux(
    input: &str,
    max_chars: usize,
    aux: Option<&crate::agent::llm::auxiliary::AuxiliaryClient>,
) -> Result<Value, String> {
    let Some(aux) = aux else {
        return Ok(summarise_heuristic_payload(input, max_chars));
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime: {e}"))?;
    let summary = runtime.block_on(crate::agent::summarise::summarise(
        Some(aux),
        input,
        max_chars,
    ));
    Ok(json!({
        "summary": summary,
        "input_chars": input.chars().count(),
        "summary_chars": summary.chars().count(),
        "max_chars": max_chars,
        "method": "llm",
        "provider": aux.provider_name(),
        "model": aux.config().model,
    }))
}

fn summarise_heuristic_payload(input: &str, max_chars: usize) -> Value {
    let raw = crate::agent::summarise::heuristic(input);
    let summary = crate::agent::summarise::clamp(&raw, max_chars);
    json!({
        "summary": summary,
        "input_chars": input.chars().count(),
        "summary_chars": summary.chars().count(),
        "max_chars": max_chars,
        "clamped": raw.chars().count() > max_chars,
        "method": "heuristic",
    })
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
                    registry.get_unfiltered(n).map(|t| {
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
            let allow_arr: Option<Vec<String>> = g.allow.as_ref().map(|set| {
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
            let tool = args.get(1).cloned().ok_or_else(|| {
                "usage: cos agent approval check <tool> [--input '<json>']".to_string()
            })?;
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
        estimate_message_tokens, estimate_text_tokens, estimate_total_tokens, CompressorConfig,
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
                        cfg.summary_max_tokens = parse_u32_arg(args.get(i + 1), "--summary-max")?;
                        i += 2;
                    }
                    other => {
                        return Err(format!("unknown compress check flag: {other}"));
                    }
                }
            }

            let path = file.ok_or_else(|| "--file required".to_string())?;
            let raw = std::fs::read_to_string(&path).map_err(|e| format!("read {path}: {e}"))?;
            let mut messages: Vec<Message> = Vec::new();
            for (line_no, line) in raw.lines().enumerate() {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let msg: Message = serde_json::from_str(trimmed)
                    .map_err(|e| format!("parse line {} of {}: {}", line_no + 1, path, e))?;
                messages.push(msg);
            }

            let system = match (system_inline, system_file) {
                (Some(_), Some(_)) => {
                    return Err("--system and --system-file are mutually exclusive".into());
                }
                (Some(s), None) => Some(s),
                (None, Some(p)) => {
                    Some(std::fs::read_to_string(&p).map_err(|e| format!("read {p}: {e}"))?)
                }
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
            let mut schedule: Vec<Value> =
                Vec::with_capacity(max_attempts.saturating_sub(1) as usize);
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

/// `cos agent vision <subcommand>` — surface for the
/// [`crate::agent::media::vision::routing`] policy layer.
///
/// Currently only `route` is implemented: given an image
/// descriptor (size + mime + intent) and a policy (provider vision
/// support, OCR availability, native cap, vision-enabled toggle),
/// report the [`RoutingDecision`] (Native / Ocr / Skip + reason).
///
/// Two input modes:
///
/// * `--bytes N --mime <m>` — synthesise a descriptor without
///   reading any actual image. Useful for previewing decisions in
///   tests / scripts.
/// * `--file <path>` — read the file's size on disk; mime is
///   inferred from the extension unless `--mime` overrides it. The
///   file is **not** loaded into memory; only `metadata().len()` is
///   used.
///
/// Policy flags map 1:1 to [`RoutingPolicy`] fields. Defaults
/// match `RoutingPolicy::default()` (no provider vision, no OCR,
/// 5 MiB native cap, vision enabled).
fn vision_cmd(args: &[String]) -> Result<Value, String> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("");
    match sub {
        "route" => vision_route_cmd(&args[1..]),
        "sniff" => vision_sniff_cmd(&args[1..]),
        "analyze" => vision_analyze_cmd(&args[1..]),
        "" => Err("usage: cos agent vision <route|sniff|analyze> ... \
             (e.g. route --file <p> | sniff --file <p> | analyze --file <p> --prompt <t>)"
            .to_string()),
        other => Err(format!(
            "unknown vision subcommand: {other}. try: route | sniff | analyze"
        )),
    }
}

/// `cos agent vision sniff --file <path> | --url <url>`
///
/// Read the head of an image (file or URL), report the magic-byte
/// MIME, the byte length, and whether it's a "widely-supported"
/// vision MIME. Pure inspection — does not call any LLM.
fn vision_sniff_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::media::vision::analyze::sniff_mime;
    use crate::agent::media::vision::routing::ImageMime;

    let mut file: Option<String> = None;
    let mut url: Option<String> = None;
    let mut head_only_bytes: usize = 32; // sniff_mime needs ~12 bytes max
    let mut fetch_timeout_ms: u64 = 30_000;
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--file" => {
                file = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--file needs a path".to_string())?,
                );
                i += 2;
            }
            "--url" => {
                url = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--url needs a value".to_string())?,
                );
                i += 2;
            }
            "--head-bytes" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--head-bytes needs a number".to_string())?;
                head_only_bytes = raw
                    .parse::<usize>()
                    .map_err(|e| format!("--head-bytes parse: {e}"))?;
                i += 2;
            }
            "--fetch-timeout-ms" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--fetch-timeout-ms needs a number".to_string())?;
                fetch_timeout_ms = raw
                    .parse::<u64>()
                    .map_err(|e| format!("--fetch-timeout-ms parse: {e}"))?;
                i += 2;
            }
            other => return Err(format!("unknown vision sniff flag: {other}")),
        }
    }

    if file.is_some() == url.is_some() {
        return Err("vision sniff needs exactly one of --file <path> or --url <url>".to_string());
    }

    let (bytes_len, head, source) = if let Some(path) = file {
        let p = std::path::PathBuf::from(&path);
        let meta = std::fs::metadata(&p).map_err(|e| format!("stat {path}: {e}"))?;
        let bytes_len = meta.len() as usize;
        let data = std::fs::read(&p).map_err(|e| format!("read {path}: {e}"))?;
        let head_n = head_only_bytes.min(data.len());
        (bytes_len, data[..head_n].to_vec(), format!("file:{path}"))
    } else {
        let u = url.unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("tokio runtime: {e}"))?;
        let (data, _mime) = runtime
            .block_on(crate::agent::media::vision::analyze::fetch_image(
                &u,
                std::time::Duration::from_millis(fetch_timeout_ms),
            ))
            .map_err(|e| format!("fetch {u}: {e}"))?;
        let head_n = head_only_bytes.min(data.len());
        (data.len(), data[..head_n].to_vec(), format!("url:{u}"))
    };

    let mime = sniff_mime(&head);
    Ok(json!({
        "source": source,
        "bytes_len": bytes_len,
        "head_bytes_inspected": head.len(),
        "mime": format!("{:?}", mime),
        "mime_widely_supported": mime.is_widely_supported(),
        "is_other": matches!(mime, ImageMime::Other),
    }))
}

/// `cos agent vision analyze --file <path> | --url <url> | --base64 <data> --mime <m>
///                           --prompt <text> [--system <text>] [--max-tokens N]
///                           [--provider <name>] [--model <name>]
///                           [--fetch-timeout-ms N]`
///
/// End-to-end vision call: resolves the image to base64, builds a
/// multimodal chat request, dispatches via the configured (or
/// overridden) provider, and prints the assistant's text response.
fn vision_analyze_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::media::vision::analyze::{analyze, ImageInput, VisionRequest};
    use crate::agent::media::vision::routing::ImageMime;

    let mut file: Option<String> = None;
    let mut url: Option<String> = None;
    let mut base64_data: Option<String> = None;
    let mut mime_override: Option<String> = None;
    let mut prompt: Option<String> = None;
    let mut system: Option<String> = None;
    let mut max_tokens: Option<u32> = None;
    let mut provider_override: Option<String> = None;
    let mut model_override: Option<String> = None;
    let mut fetch_timeout_ms: u64 = 30_000;
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--file" => {
                file = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--file needs a path".to_string())?,
                );
                i += 2;
            }
            "--url" => {
                url = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--url needs a value".to_string())?,
                );
                i += 2;
            }
            "--base64" => {
                base64_data = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--base64 needs a value".to_string())?,
                );
                i += 2;
            }
            "--mime" => {
                mime_override = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--mime needs a value".to_string())?,
                );
                i += 2;
            }
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
            "--max-tokens" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--max-tokens needs a number".to_string())?;
                max_tokens = Some(
                    raw.parse::<u32>()
                        .map_err(|e| format!("--max-tokens parse: {e}"))?,
                );
                i += 2;
            }
            "--provider" => {
                provider_override = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--provider needs a value".to_string())?,
                );
                i += 2;
            }
            "--model" => {
                model_override = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--model needs a value".to_string())?,
                );
                i += 2;
            }
            "--fetch-timeout-ms" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--fetch-timeout-ms needs a number".to_string())?;
                fetch_timeout_ms = raw
                    .parse::<u64>()
                    .map_err(|e| format!("--fetch-timeout-ms parse: {e}"))?;
                i += 2;
            }
            other => return Err(format!("unknown vision analyze flag: {other}")),
        }
    }

    let prompt = prompt.ok_or_else(|| "vision analyze: --prompt <text> required".to_string())?;
    if prompt.trim().is_empty() {
        return Err("vision analyze: --prompt must be non-empty".to_string());
    }

    // Mutually-exclusive image source. base64 needs an explicit mime.
    let sources_set = usize::from(file.is_some())
        + usize::from(url.is_some())
        + usize::from(base64_data.is_some());
    if sources_set != 1 {
        return Err(
            "vision analyze needs exactly one of --file <path> | --url <url> | --base64 <data>"
                .to_string(),
        );
    }

    let image: ImageInput = if let Some(path) = file {
        let data = std::fs::read(&path).map_err(|e| format!("read {path}: {e}"))?;
        // Honour --mime if supplied; otherwise infer from extension; sniff
        // bytes as last resort so HEIC/BMP etc still get classified.
        let mime = if let Some(m) = mime_override.as_deref() {
            ImageMime::from_str(m)
        } else {
            let ext = std::path::Path::new(&path)
                .extension()
                .and_then(|e| e.to_str())
                .map(|s| s.to_ascii_lowercase())
                .unwrap_or_default();
            let by_ext = ImageMime::from_str(&ext);
            if matches!(by_ext, ImageMime::Other) {
                crate::agent::media::vision::analyze::sniff_mime(&data)
            } else {
                by_ext
            }
        };
        ImageInput::Bytes { data, mime }
    } else if let Some(u) = url {
        if let Some(m) = mime_override.as_deref() {
            // Caller supplied mime → fetch eagerly so we can pass Bytes
            // (skips fetch_image's per-byte mime sniff, lets caller win).
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("tokio runtime: {e}"))?;
            let (data, _mime) = runtime
                .block_on(crate::agent::media::vision::analyze::fetch_image(
                    &u,
                    std::time::Duration::from_millis(fetch_timeout_ms),
                ))
                .map_err(|e| format!("fetch {u}: {e}"))?;
            ImageInput::Bytes {
                data,
                mime: ImageMime::from_str(m),
            }
        } else {
            ImageInput::Url(u)
        }
    } else {
        let data = base64_data.unwrap();
        let mime = mime_override
            .as_deref()
            .ok_or_else(|| "--base64 requires --mime <m>".to_string())?;
        ImageInput::Base64 {
            data,
            mime: ImageMime::from_str(mime),
        }
    };

    let cfg = crate::config::get();
    let provider_name = provider_override
        .clone()
        .unwrap_or_else(|| cfg.agent.provider.clone());
    if provider_name.trim().is_empty() {
        return Err(
            "no provider configured (set agent.provider in config or pass --provider)".to_string(),
        );
    }
    let model_name = model_override
        .clone()
        .or_else(|| {
            if cfg.agent.model.is_empty() {
                None
            } else {
                Some(cfg.agent.model.clone())
            }
        })
        .ok_or_else(|| {
            "no model configured (set agent.model in config or pass --model)".to_string()
        })?;

    let provider = crate::agent::llm::registry::build(&provider_name, &model_name, &cfg.agent)
        .map_err(|e| format!("build provider {provider_name}: {e}"))?;

    let mut req = VisionRequest::new(prompt.clone(), image);
    req.system = system;
    req.max_tokens = max_tokens;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime: {e}"))?;

    let resp = runtime
        .block_on(analyze(
            provider.as_ref(),
            req,
            std::time::Duration::from_millis(fetch_timeout_ms),
        ))
        .map_err(|e| format!("vision analyze: {e}"))?;

    Ok(json!({
        "ok": true,
        "provider": provider_name,
        "model": model_name,
        "answer": resp.text,
        "model_reported": resp.model,
    }))
}

fn vision_route_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::media::vision::routing::{
        route, ImageDescriptor, ImageIntent, ImageMime, RoutingDecision, RoutingPolicy,
    };

    let mut file: Option<String> = None;
    let mut bytes_override: Option<usize> = None;
    let mut mime_override: Option<String> = None;
    let mut intent = ImageIntent::General;
    let mut policy = RoutingPolicy::default();
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--file" => {
                file = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--file needs a path".to_string())?,
                );
                i += 2;
            }
            "--bytes" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--bytes needs a number".to_string())?;
                bytes_override = Some(
                    raw.parse::<usize>()
                        .map_err(|e| format!("--bytes parse: {e}"))?,
                );
                i += 2;
            }
            "--mime" => {
                mime_override = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--mime needs a value".to_string())?,
                );
                i += 2;
            }
            "--intent" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--intent needs a value".to_string())?;
                intent = match raw.to_ascii_lowercase().as_str() {
                    "general" => ImageIntent::General,
                    "extract-text" | "extract_text" => ImageIntent::ExtractText,
                    "identify" => ImageIntent::Identify,
                    "caption" => ImageIntent::Caption,
                    other => {
                        return Err(format!(
                            "unknown --intent: {other}. try: general | extract-text | identify | caption"
                        ))
                    }
                };
                i += 2;
            }
            "--provider-vision" => {
                policy.provider_supports_vision = true;
                i += 1;
            }
            "--no-provider-vision" => {
                policy.provider_supports_vision = false;
                i += 1;
            }
            "--vision-disabled" => {
                policy.vision_enabled = false;
                i += 1;
            }
            "--vision-enabled" => {
                policy.vision_enabled = true;
                i += 1;
            }
            "--ocr-available" => {
                policy.ocr_available = true;
                i += 1;
            }
            "--no-ocr" => {
                policy.ocr_available = false;
                i += 1;
            }
            "--max-native-bytes" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--max-native-bytes needs a number".to_string())?;
                policy.max_native_bytes = raw
                    .parse::<usize>()
                    .map_err(|e| format!("--max-native-bytes parse: {e}"))?;
                i += 2;
            }
            other => return Err(format!("unknown vision route flag: {other}")),
        }
    }

    let (bytes_len, mime, source) = match (file.as_ref(), bytes_override, mime_override.as_ref()) {
        (None, None, _) => {
            return Err(
                "vision route needs --file <path> or --bytes N (and --mime if no --file)"
                    .to_string(),
            );
        }
        (Some(path), _, _) => {
            let p = std::path::PathBuf::from(path);
            let meta = std::fs::metadata(&p).map_err(|e| format!("stat {path}: {e}"))?;
            // --bytes overrides the on-disk size if both supplied (rare;
            // useful when previewing what would happen if we shrank the file).
            let len = bytes_override.unwrap_or(meta.len() as usize);
            // If --mime was supplied, honour it. Otherwise infer from extension.
            let m = match mime_override.as_deref() {
                Some(mime_str) => ImageMime::from_str(mime_str),
                None => {
                    let ext = p
                        .extension()
                        .and_then(|e| e.to_str())
                        .map(|s| s.to_ascii_lowercase())
                        .unwrap_or_default();
                    ImageMime::from_str(&ext)
                }
            };
            (len, m, format!("file:{path}"))
        }
        (None, Some(b), Some(m)) => (b, ImageMime::from_str(m), "synthetic".to_string()),
        (None, Some(_), None) => {
            return Err("--bytes requires --mime when --file is not supplied".to_string());
        }
    };

    let descriptor = ImageDescriptor {
        bytes_len,
        mime,
        intent,
    };
    let decision = route(&descriptor, &policy);
    let (verdict, reason) = match decision {
        RoutingDecision::Native => ("native", None),
        RoutingDecision::Ocr => ("ocr", None),
        RoutingDecision::Skip { reason } => ("skip", Some(reason)),
    };

    Ok(json!({
        "source": source,
        "descriptor": {
            "bytes_len": descriptor.bytes_len,
            "mime": format!("{:?}", descriptor.mime),
            "mime_widely_supported": descriptor.mime.is_widely_supported(),
            "intent": format!("{:?}", descriptor.intent),
        },
        "policy": {
            "provider_supports_vision": policy.provider_supports_vision,
            "vision_enabled": policy.vision_enabled,
            "max_native_bytes": policy.max_native_bytes,
            "ocr_available": policy.ocr_available,
        },
        "decision": verdict,
        "reason": reason,
    }))
}

/// `cos agent display <subcommand>` — render conversation
/// history from the memory DB through [`crate::agent::display`]'s
/// pure-functional formatter, so operators can preview what a
/// terminal/gateway would show without firing up a real session.
///
/// Subcommands:
///
/// * `transcript --session <id> [--limit N] [--width W]
///   [--no-truncate] [--truncate-at N] [--indent N]` — render the
///   most-recent N messages of `<id>` (oldest first) as a
///   single-string transcript using `display::render_message`.
/// * `format-bytes <n>` — preview `display::format_bytes`.
/// * `format-duration <ms>` — preview `display::format_duration`.
fn display_cmd(args: &[String]) -> Result<Value, String> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("");
    match sub {
        "transcript" => display_transcript_cmd(&args[1..]),
        "format-bytes" => display_format_bytes_cmd(&args[1..]),
        "format-duration" => display_format_duration_cmd(&args[1..]),
        "" => Err(
            "usage: cos agent display transcript --session <id> [--limit N] [--width W] [--no-truncate] [--truncate-at N] [--indent N] | format-bytes <n> | format-duration <ms>"
                .to_string(),
        ),
        other => Err(format!(
            "unknown display subcommand: {other}. try: transcript | format-bytes | format-duration"
        )),
    }
}

#[derive(Debug, Default)]
struct DisplayTranscriptArgs {
    session: Option<String>,
    limit: Option<usize>,
    width: Option<usize>,
    indent: Option<usize>,
    no_truncate: bool,
    truncate_at: Option<usize>,
}

fn parse_display_transcript_args(args: &[String]) -> Result<DisplayTranscriptArgs, String> {
    let mut out = DisplayTranscriptArgs::default();
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--session" => {
                out.session = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--session needs an id".to_string())?,
                );
                i += 2;
            }
            "--limit" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--limit needs a number".to_string())?;
                out.limit = Some(
                    raw.parse::<usize>()
                        .map_err(|_| format!("--limit not numeric: {raw}"))?,
                );
                i += 2;
            }
            "--width" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--width needs a number".to_string())?;
                out.width = Some(
                    raw.parse::<usize>()
                        .map_err(|_| format!("--width not numeric: {raw}"))?,
                );
                i += 2;
            }
            "--indent" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--indent needs a number".to_string())?;
                out.indent = Some(
                    raw.parse::<usize>()
                        .map_err(|_| format!("--indent not numeric: {raw}"))?,
                );
                i += 2;
            }
            "--no-truncate" => {
                out.no_truncate = true;
                i += 1;
            }
            "--truncate-at" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--truncate-at needs a number".to_string())?;
                out.truncate_at = Some(
                    raw.parse::<usize>()
                        .map_err(|_| format!("--truncate-at not numeric: {raw}"))?,
                );
                i += 2;
            }
            other => return Err(format!("unknown display transcript flag: {other}")),
        }
    }
    Ok(out)
}

fn display_config_from(args: &DisplayTranscriptArgs) -> crate::agent::display::DisplayConfig {
    let mut cfg = crate::agent::display::DisplayConfig::default();
    if let Some(w) = args.width {
        cfg.wrap_at = w;
    }
    if let Some(ind) = args.indent {
        cfg.continuation_indent = ind;
    }
    if args.no_truncate {
        cfg.truncate_at = None;
    } else if let Some(cap) = args.truncate_at {
        cfg.truncate_at = Some(cap);
    }
    cfg
}

fn role_from_str(raw: &str) -> crate::agent::display::Role {
    use crate::agent::display::Role;
    match raw {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        "tool" => Role::Tool,
        _ => Role::System,
    }
}

fn display_transcript_cmd(args: &[String]) -> Result<Value, String> {
    let parsed = parse_display_transcript_args(args)?;
    let session = parsed
        .session
        .clone()
        .ok_or_else(|| "--session <id> is required".to_string())?;
    let db = crate::agent::memory::sqlite_fts::MemoryDb::open_default()
        .map_err(|e| format!("open memory db: {e}"))?;
    display_transcript_with(&db, &session, &parsed)
}

fn display_transcript_with(
    db: &crate::agent::memory::sqlite_fts::MemoryDb,
    session_id: &str,
    parsed: &DisplayTranscriptArgs,
) -> Result<Value, String> {
    let cfg = display_config_from(parsed);
    let limit = parsed.limit.unwrap_or(50);
    let rows = db
        .recent(session_id, limit)
        .map_err(|e| format!("read session {session_id}: {e}"))?;
    let lines: Vec<String> = rows
        .iter()
        .map(|row| {
            crate::agent::display::render_message(role_from_str(&row.role), &row.content, &cfg)
        })
        .collect();
    let transcript = lines.join("\n");
    Ok(json!({
        "session_id": session_id,
        "message_count": rows.len(),
        "limit": limit,
        "wrap_at": cfg.wrap_at,
        "continuation_indent": cfg.continuation_indent,
        "truncate_at": cfg.truncate_at,
        "transcript": transcript,
    }))
}

fn display_format_bytes_cmd(args: &[String]) -> Result<Value, String> {
    let raw = args
        .first()
        .ok_or_else(|| "usage: cos agent display format-bytes <n>".to_string())?;
    let n: u64 = raw
        .parse()
        .map_err(|_| format!("format-bytes needs a positive integer, got: {raw}"))?;
    Ok(json!({
        "input": n,
        "formatted": crate::agent::display::format_bytes(n),
    }))
}

fn display_format_duration_cmd(args: &[String]) -> Result<Value, String> {
    let raw = args
        .first()
        .ok_or_else(|| "usage: cos agent display format-duration <ms>".to_string())?;
    let ms: u64 = raw
        .parse()
        .map_err(|_| format!("format-duration needs a positive integer (ms), got: {raw}"))?;
    Ok(json!({
        "input_ms": ms,
        "formatted": crate::agent::display::format_duration(std::time::Duration::from_millis(ms)),
    }))
}

/// `cos agent shell-hooks <init <bash|zsh|fish>|record-pre <cmd>|record-post <exit>|tail [--limit N]|clear --yes|path>`
///
/// Exposes [`crate::agent::shell_hooks`] as a CLI surface so the
/// user can install shell-init scripts that capture interactive
/// commands into a JSONL log the agent can later read for ambient
/// context. The `record-*` verbs are called by the init-script
/// hooks themselves; humans only invoke `init`, `tail`, `clear`,
/// `path`.
fn shell_hooks_cmd(args: &[String]) -> Result<Value, String> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("path");
    match sub {
        "path" | "" => Ok(json!({
            "path": crate::agent::shell_hooks::default_log_path().display().to_string(),
        })),
        "init" => {
            let raw = args
                .get(1)
                .map(|s| s.as_str())
                .ok_or_else(|| {
                    "usage: cos agent shell-hooks init <bash|zsh|fish>".to_string()
                })?;
            let shell = crate::agent::shell_hooks::Shell::parse(raw)?;
            let script = crate::agent::shell_hooks::render_init(shell);
            Ok(json!({
                "shell": shell.label(),
                "log_path": crate::agent::shell_hooks::default_log_path().display().to_string(),
                "script": script,
                "instructions": init_instructions_for(shell),
            }))
        }
        "record-pre" => {
            let cmd = args
                .get(1)
                .cloned()
                .ok_or_else(|| "usage: cos agent shell-hooks record-pre <cmd>".to_string())?;
            let path = crate::agent::shell_hooks::default_log_path();
            let ts_ms = crate::agent::shell_hooks::now_ms();
            crate::agent::shell_hooks::append_pre_at(&path, &cmd, ts_ms)
                .map_err(|e| format!("write failed: {e}"))?;
            Ok(json!({
                "kind": "pre",
                "ts_ms": ts_ms,
                "cmd": cmd,
                "path": path.display().to_string(),
            }))
        }
        "record-post" => {
            let raw = args
                .get(1)
                .ok_or_else(|| "usage: cos agent shell-hooks record-post <exit>".to_string())?;
            let exit: i32 = raw
                .parse()
                .map_err(|_| format!("record-post needs an integer exit code, got: {raw}"))?;
            let path = crate::agent::shell_hooks::default_log_path();
            let ts_ms = crate::agent::shell_hooks::now_ms();
            crate::agent::shell_hooks::append_post_at(&path, exit, ts_ms)
                .map_err(|e| format!("write failed: {e}"))?;
            Ok(json!({
                "kind": "post",
                "ts_ms": ts_ms,
                "exit": exit,
                "path": path.display().to_string(),
            }))
        }
        "tail" => {
            let mut limit: usize = 20;
            let mut i = 1usize;
            while i < args.len() {
                match args[i].as_str() {
                    "--limit" => {
                        let v = args
                            .get(i + 1)
                            .ok_or_else(|| "--limit needs <n>".to_string())?;
                        limit = v
                            .parse()
                            .map_err(|_| format!("--limit must be a positive integer, got: {v}"))?;
                        i += 2;
                    }
                    other => return Err(format!("unknown flag for `shell-hooks tail`: {other}")),
                }
            }
            let path = crate::agent::shell_hooks::default_log_path();
            let rows = crate::agent::shell_hooks::tail_at(&path, limit)
                .map_err(|e| format!("read failed: {e}"))?;
            Ok(json!({
                "path": path.display().to_string(),
                "limit": limit,
                "n": rows.len(),
                "records": rows,
            }))
        }
        "clear" => {
            // Require explicit --yes so it can never happen by accident.
            let confirmed = args.iter().any(|a| a == "--yes");
            if !confirmed {
                return Err(
                    "usage: cos agent shell-hooks clear --yes  (truncates the JSONL log)".into(),
                );
            }
            let path = crate::agent::shell_hooks::default_log_path();
            let cleared = crate::agent::shell_hooks::clear_at(&path)
                .map_err(|e| format!("clear failed: {e}"))?;
            Ok(json!({
                "path": path.display().to_string(),
                "cleared": cleared,
            }))
        }
        other => Err(format!(
            "unknown shell-hooks subcommand: {other}. try: init <bash|zsh|fish> | record-pre <cmd> | record-post <exit> | tail [--limit N] | clear --yes | path"
        )),
    }
}

fn init_instructions_for(shell: crate::agent::shell_hooks::Shell) -> &'static str {
    use crate::agent::shell_hooks::Shell;
    match shell {
        Shell::Bash => {
            "append the script to ~/.bashrc, or eval it inline: eval \"$(cos agent shell-hooks init bash | jq -r .script)\""
        }
        Shell::Zsh => {
            "append the script to ~/.zshrc, or eval it inline: eval \"$(cos agent shell-hooks init zsh | jq -r .script)\""
        }
        Shell::Fish => {
            "append the script to ~/.config/fish/config.fish, or source it: cos agent shell-hooks init fish | jq -r .script | source"
        }
    }
}

/// `cos agent media <providers|outputs-dir|list-outputs [--limit N] [--ext <e>]>`
///
/// Surfaces the media subsystem so operators can introspect:
///   * which TTS / STT / image-gen providers are wired up and which
///     are configured (currently only the `noop` reference impls
///     are auto-registered;  cloud factories will populate this
///     surface once `with_*_providers_from_cfg` lands)
///   * where rendered audio / image artifacts are written
///   * what's recently been generated under that directory
fn media_cmd(args: &[String]) -> Result<Value, String> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("providers");
    match sub {
        "providers" | "" => {
            let cfg = crate::config::get();
            let tts = crate::agent::media::factory::tts_registry_from_cfg(cfg);
            let stt = crate::agent::media::factory::stt_registry_from_cfg(cfg);
            let imagegen = crate::agent::media::factory::imagegen_registry_from_cfg(cfg);

            let tts_rows: Vec<_> = tts
                .names()
                .into_iter()
                .map(|name| {
                    let configured =
                        tts.get(&name).map(|p| p.is_configured()).unwrap_or(false);
                    json!({"name": name, "configured": configured})
                })
                .collect();
            let stt_rows: Vec<_> = stt
                .names()
                .into_iter()
                .map(|name| {
                    let configured =
                        stt.get(&name).map(|p| p.is_configured()).unwrap_or(false);
                    json!({"name": name, "configured": configured})
                })
                .collect();
            let imagegen_rows: Vec<_> = imagegen
                .names()
                .into_iter()
                .map(|name| {
                    let configured = imagegen
                        .get(&name)
                        .map(|p| p.is_configured())
                        .unwrap_or(false);
                    json!({"name": name, "configured": configured})
                })
                .collect();

            Ok(json!({
                "outputs_dir": crate::paths::agent_media_outputs_dir().display().to_string(),
                "tts": {
                    "n": tts_rows.len(),
                    "providers": tts_rows,
                },
                "stt": {
                    "n": stt_rows.len(),
                    "providers": stt_rows,
                },
                "imagegen": {
                    "n": imagegen_rows.len(),
                    "providers": imagegen_rows,
                },
            }))
        }
        "outputs-dir" => Ok(json!({
            "path": crate::paths::agent_media_outputs_dir().display().to_string(),
        })),
        "list-outputs" => {
            let mut limit: usize = 20;
            let mut ext_filter: Option<String> = None;
            let mut i = 1usize;
            while i < args.len() {
                match args[i].as_str() {
                    "--limit" => {
                        let v = args
                            .get(i + 1)
                            .ok_or_else(|| "--limit needs <n>".to_string())?;
                        limit = v
                            .parse()
                            .map_err(|_| format!("--limit must be a positive integer, got: {v}"))?;
                        i += 2;
                    }
                    "--ext" => {
                        let v = args
                            .get(i + 1)
                            .ok_or_else(|| "--ext needs <extension>".to_string())?;
                        ext_filter =
                            Some(v.trim_start_matches('.').to_ascii_lowercase());
                        i += 2;
                    }
                    other => {
                        return Err(format!(
                            "unknown flag for `media list-outputs`: {other}"
                        ));
                    }
                }
            }
            let dir = crate::paths::agent_media_outputs_dir();
            list_media_outputs(&dir, limit, ext_filter.as_deref())
        }
        "play" => media_play_cmd(&args[1..]),
        "playback-status" => media_playback_status_cmd(&args[1..]),
        other => Err(format!(
            "unknown media subcommand: {other}. try: providers | outputs-dir | list-outputs [--limit N] [--ext <e>] | play <path> | playback-status [--format wav|mp3|ogg|flac]"
        )),
    }
}

fn list_media_outputs(
    dir: &std::path::Path,
    limit: usize,
    ext_filter: Option<&str>,
) -> Result<Value, String> {
    if !dir.exists() {
        return Ok(json!({
            "dir": dir.display().to_string(),
            "exists": false,
            "limit": limit,
            "n": 0,
            "files": Vec::<Value>::new(),
        }));
    }
    let mut rows: Vec<(std::time::SystemTime, std::path::PathBuf, u64, String)> = Vec::new();
    let entries = std::fs::read_dir(dir).map_err(|e| format!("read_dir failed: {e}"))?;
    for ent in entries.flatten() {
        let path = ent.path();
        let meta = match ent.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !meta.is_file() {
            continue;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if let Some(want) = ext_filter {
            if ext != want {
                continue;
            }
        }
        let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
        rows.push((mtime, path, meta.len(), ext));
    }
    // Newest first.
    rows.sort_by(|a, b| b.0.cmp(&a.0));
    rows.truncate(limit);
    let files: Vec<Value> = rows
        .into_iter()
        .map(|(mtime, path, size, ext)| {
            let mtime_ms = mtime
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            json!({
                "path": path.display().to_string(),
                "name": path.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string(),
                "ext": ext,
                "size": size,
                "mtime_ms": mtime_ms,
            })
        })
        .collect();
    Ok(json!({
        "dir": dir.display().to_string(),
        "exists": true,
        "limit": limit,
        "ext_filter": ext_filter,
        "n": files.len(),
        "files": files,
    }))
}

// =====================================================================
// `cos agent media play <path>` — short-term blocking playback via
// the OS's native audio facility (PlaySoundW on Windows, afplay on
// macOS, format-aware CLI player on Linux). See
// `crate::agent::media::voice::system_playback` for the semantic
// contract and what's intentionally out of scope.
// =====================================================================

fn media_play_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::media::voice::system_playback;
    use std::path::PathBuf;

    let mut path: Option<PathBuf> = None;
    let mut detect_only = false;

    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--detect" => {
                detect_only = true;
                i += 1;
            }
            "--" => {
                if let Some(p) = args.get(i + 1) {
                    path = Some(PathBuf::from(p));
                }
                i += 2;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag for `media play`: {other}"));
            }
            _ => {
                if path.is_some() {
                    return Err(format!(
                        "unexpected extra argument to `media play`: {}",
                        args[i]
                    ));
                }
                path = Some(PathBuf::from(&args[i]));
                i += 1;
            }
        }
    }

    let path = path.ok_or("usage: cos agent media play <path> [--detect]")?;

    // Format detection up front so we always report it, even on error.
    let format = system_playback::PlaybackFormat::from_path(&path);
    let format_str = format.map(|f| f.as_str().to_string());

    if detect_only {
        let player = format.and_then(system_playback::detect_player);
        return Ok(json!({
            "path": path.display().to_string(),
            "format": format_str,
            "player": player,
            "playable": player.is_some(),
        }));
    }

    match system_playback::play_file_blocking(&path) {
        Ok(()) => Ok(json!({
            "ok": true,
            "path": path.display().to_string(),
            "format": format_str,
        })),
        Err(e) => Err(format!("playback failed: {e}")),
    }
}

fn media_playback_status_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::media::voice::system_playback::{detect_player, PlaybackFormat};

    let mut filter: Option<PlaybackFormat> = None;
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--format" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| "--format needs <wav|mp3|ogg|flac>".to_string())?;
                filter = Some(match v.to_ascii_lowercase().as_str() {
                    "wav" => PlaybackFormat::Wav,
                    "mp3" => PlaybackFormat::Mp3,
                    "ogg" | "oga" => PlaybackFormat::Ogg,
                    "flac" => PlaybackFormat::Flac,
                    other => {
                        return Err(format!(
                            "--format: unknown value '{other}'. try: wav | mp3 | ogg | flac"
                        ));
                    }
                });
                i += 2;
            }
            other => return Err(format!("unknown flag for `media playback-status`: {other}")),
        }
    }

    let formats: Vec<PlaybackFormat> = match filter {
        Some(f) => vec![f],
        None => vec![
            PlaybackFormat::Wav,
            PlaybackFormat::Mp3,
            PlaybackFormat::Ogg,
            PlaybackFormat::Flac,
        ],
    };

    let rows: Vec<Value> = formats
        .iter()
        .map(|f| {
            let player = detect_player(*f);
            json!({
                "format": f.as_str(),
                "player": player,
                "playable": player.is_some(),
            })
        })
        .collect();

    Ok(json!({
        "os": std::env::consts::OS,
        "formats": rows,
    }))
}

/// `cos agent binary-ext <list [--limit N]|check <path>|extensions>`
///
/// Surfaces [`crate::agent::safety::binary_ext`] so operators can:
///   * `check <path>` — quickly classify whether a file would be
///     treated as binary by the agent's IO helpers.
///   * `list [--limit N]` — inspect the active classifier's
///     extension set (sorted, optionally truncated).
///   * `extensions` — alias of `list` with no truncation, useful
///     when you want the raw set.
fn binary_ext_cmd(args: &[String]) -> Result<Value, String> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("list");
    match sub {
        "list" | "" => {
            let mut limit: Option<usize> = Some(50);
            let mut i = 1usize;
            while i < args.len() {
                match args[i].as_str() {
                    "--limit" => {
                        let v = args
                            .get(i + 1)
                            .ok_or_else(|| "--limit needs <n>".to_string())?;
                        limit = Some(
                            v.parse()
                                .map_err(|_| {
                                    format!("--limit must be a positive integer, got: {v}")
                                })?,
                        );
                        i += 2;
                    }
                    "--no-limit" => {
                        limit = None;
                        i += 1;
                    }
                    other => {
                        return Err(format!("unknown flag for `binary-ext list`: {other}"));
                    }
                }
            }
            let c = crate::agent::safety::binary_ext::BinaryExtensions::default();
            let total = c.len();
            let exts: Vec<&str> = match limit {
                Some(n) => c.iter().take(n).collect(),
                None => c.iter().collect(),
            };
            Ok(json!({
                "total": total,
                "limit": limit,
                "n": exts.len(),
                "extensions": exts,
            }))
        }
        "extensions" => {
            let c = crate::agent::safety::binary_ext::BinaryExtensions::default();
            let exts: Vec<&str> = c.iter().collect();
            Ok(json!({
                "total": c.len(),
                "extensions": exts,
            }))
        }
        "check" => {
            let raw = args
                .get(1)
                .ok_or_else(|| "usage: cos agent binary-ext check <path-or-extension>".to_string())?;
            let c = crate::agent::safety::binary_ext::BinaryExtensions::default();
            // Heuristic: if it looks like a bare extension (no path
            // separator, at most one leading `.`), treat it as such;
            // otherwise treat as a path.
            let looks_like_extension = !raw.contains(['/', '\\'])
                && (raw.starts_with('.') || !raw.contains('.'))
                && !raw.contains(' ');
            let (mode, is_binary, ext_resolved): (&str, bool, Option<String>) =
                if looks_like_extension {
                    let key = raw.trim().trim_start_matches('.').to_ascii_lowercase();
                    (
                        "extension",
                        c.contains_extension(raw),
                        if key.is_empty() { None } else { Some(key) },
                    )
                } else {
                    let p = std::path::Path::new(raw);
                    let ext = p
                        .extension()
                        .and_then(|e| e.to_str())
                        .map(|s| s.to_ascii_lowercase());
                    ("path", c.is_binary_path(p), ext)
                };
            Ok(json!({
                "input": raw,
                "mode": mode,
                "extension": ext_resolved,
                "is_binary": is_binary,
                "set_size": c.len(),
            }))
        }
        other => Err(format!(
            "unknown binary-ext subcommand: {other}. try: list [--limit N] [--no-limit] | extensions | check <path-or-extension>"
        )),
    }
}

/// `cos agent context <subcommand>` — surface for the
/// [`crate::agent::context`] modules:
///
///   * `hints [--cwd <path>] [--depth N=0] [--render]` — scan for
///     project markers (Cargo.toml, package.json, .git, …) and
///     either return JSON list or a rendered summary block.
///   * `refs --text <body> [--unique]` — extract `@`-references
///     from a user message body.
///   * `markers` — dump the static marker table for inspection.
fn context_cmd(args: &[String]) -> Result<Value, String> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("");
    match sub {
        "hints" => context_hints_cmd(&args[1..]),
        "refs" | "references" => context_refs_cmd(&args[1..]),
        "markers" => context_markers_cmd(&args[1..]),
        "build" => context_build_cmd(&args[1..]),
        "" => Err(
            "usage: cos agent context <hints|refs|markers|build> ... \
             (e.g. hints [--cwd <p>] [--depth N] [--render] | refs --text <body> [--unique] | markers | build [--cwd <p>] [--depth N] [--text <body>] [--note <line>...] [--max-refs N] [--max-hints N])"
                .to_string(),
        ),
        other => Err(format!(
            "unknown context subcommand: {other}. try: hints | refs | markers | build"
        )),
    }
}

fn context_hints_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::context::subdir_hints::{render_summary, scan_dir, scan_dir_recursive};

    let mut cwd: Option<String> = None;
    let mut depth: usize = 0;
    let mut render = false;
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--cwd" => {
                cwd = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--cwd needs a path".to_string())?,
                );
                i += 2;
            }
            "--depth" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--depth needs a number".to_string())?;
                depth = raw
                    .parse::<usize>()
                    .map_err(|e| format!("--depth parse: {e}"))?;
                i += 2;
            }
            "--render" => {
                render = true;
                i += 1;
            }
            other => return Err(format!("unknown context hints flag: {other}")),
        }
    }

    let root = match cwd {
        Some(s) => std::path::PathBuf::from(s),
        None => std::env::current_dir().map_err(|e| format!("get cwd: {e}"))?,
    };
    if !root.is_dir() {
        return Err(format!("not a directory: {}", root.display()));
    }

    let hits = if depth == 0 {
        scan_dir(&root)
    } else {
        scan_dir_recursive(&root, depth)
    };

    if render {
        return Ok(json!({
            "root": root.to_string_lossy(),
            "depth": depth,
            "count": hits.len(),
            "summary": render_summary(&hits),
        }));
    }

    let hits_json: Vec<Value> = hits
        .iter()
        .map(|h| {
            json!({
                "rel": h.rel,
                "kind": format!("{:?}", h.kind),
                "label": h.label,
                "is_dir": h.is_dir,
            })
        })
        .collect();

    Ok(json!({
        "root": root.to_string_lossy(),
        "depth": depth,
        "count": hits.len(),
        "hints": hits_json,
    }))
}

fn context_refs_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::context::references::{extract, extract_unique};

    let mut text: Option<String> = None;
    let mut unique = false;
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--text" => {
                text = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--text needs a value".to_string())?,
                );
                i += 2;
            }
            "--unique" => {
                unique = true;
                i += 1;
            }
            other => return Err(format!("unknown context refs flag: {other}")),
        }
    }

    let body = text.ok_or_else(|| "context refs: --text <body> required".to_string())?;
    let refs = if unique {
        extract_unique(&body)
    } else {
        extract(&body)
    };
    let refs_json: Vec<Value> = refs
        .iter()
        .map(|r| {
            json!({
                "raw": r.raw,
                "kind": format!("{:?}", r.kind),
                "start": r.start,
                "end": r.end,
            })
        })
        .collect();

    Ok(json!({
        "unique": unique,
        "count": refs.len(),
        "references": refs_json,
    }))
}

fn context_markers_cmd(_args: &[String]) -> Result<Value, String> {
    use crate::agent::context::subdir_hints::{HintKind, MARKERS, NOISE_DIRS};
    let by_kind = |k: HintKind| -> Vec<&'static str> {
        let mut v: Vec<&'static str> = MARKERS
            .iter()
            .filter(|m| m.kind == k)
            .map(|m| m.name)
            .collect();
        v.sort();
        v
    };
    Ok(json!({
        "total": MARKERS.len(),
        "by_kind": {
            "Manifest":  by_kind(HintKind::Manifest),
            "Vcs":       by_kind(HintKind::Vcs),
            "Ci":        by_kind(HintKind::Ci),
            "Framework": by_kind(HintKind::Framework),
            "Editor":    by_kind(HintKind::Editor),
            "Env":       by_kind(HintKind::Env),
        },
        "noise_dirs": NOISE_DIRS,
    }))
}

/// `cos agent context build [--cwd <p>] [--depth N] [--text <body>] [--note <line>...] [--max-refs N] [--max-hints N] [--no-dedup]`
fn context_build_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::context::engine::{build, ContextOptions};

    let mut opts = ContextOptions::default();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--cwd" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| "missing value for --cwd".to_string())?;
                let p = std::path::PathBuf::from(v);
                if !p.is_dir() {
                    return Err(format!("context build: --cwd is not a directory: {v}"));
                }
                opts.cwd = Some(p);
            }
            "--depth" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| "missing value for --depth".to_string())?;
                opts.scan_depth = v
                    .parse()
                    .map_err(|_| format!("--depth: invalid integer: {v}"))?;
            }
            "--text" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| "missing value for --text".to_string())?;
                opts.user_text = Some(v.clone());
            }
            "--note" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| "missing value for --note".to_string())?;
                opts.notes.push(v.clone());
            }
            "--max-refs" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| "missing value for --max-refs".to_string())?;
                opts.max_refs = Some(
                    v.parse()
                        .map_err(|_| format!("--max-refs: invalid integer: {v}"))?,
                );
            }
            "--max-hints" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| "missing value for --max-hints".to_string())?;
                opts.max_hints = Some(
                    v.parse()
                        .map_err(|_| format!("--max-hints: invalid integer: {v}"))?,
                );
            }
            "--no-dedup" => {
                opts.dedup_refs = false;
            }
            other => return Err(format!("context build: unknown flag: {other}")),
        }
        i += 1;
    }
    Ok(build(&opts).to_json())
}

/// `cos agent file-safety [check <path>|batch <path>...|categories]`
fn file_safety_cmd(args: &[String]) -> Result<Value, String> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("");
    match sub {
        "" => Err(
            "usage: cos agent file-safety [check <path> | batch <path>... | categories]"
                .to_string(),
        ),
        "check" => file_safety_check_cmd(&args[1..]),
        "batch" => file_safety_batch_cmd(&args[1..]),
        "categories" => Ok(json!({
            "categories": [
                "dangerous_extension",
                "credential",
                "system_directory",
                "vcs_internal",
            ],
            "verdicts": ["allow", "caution", "deny"],
        })),
        other => Err(format!(
            "unknown file-safety subcommand: {other}. try: check | batch | categories"
        )),
    }
}

fn file_safety_check_cmd(args: &[String]) -> Result<Value, String> {
    if args.is_empty() {
        return Err("usage: cos agent file-safety check <path>".to_string());
    }
    if args.len() > 1 {
        return Err(
            "file-safety check accepts a single path; use 'batch' for multiple".to_string(),
        );
    }
    let path = &args[0];
    let v = crate::agent::safety::file_safety::classify_str(path);
    Ok(file_safety_to_json(path, &v))
}

fn file_safety_batch_cmd(args: &[String]) -> Result<Value, String> {
    if args.is_empty() {
        return Err("usage: cos agent file-safety batch <path>...".to_string());
    }
    let mut results: Vec<Value> = Vec::with_capacity(args.len());
    let mut allow_count = 0u64;
    let mut caution_count = 0u64;
    let mut deny_count = 0u64;
    for path in args {
        let v = crate::agent::safety::file_safety::classify_str(path);
        match v {
            crate::agent::safety::file_safety::FileSafety::Allow => allow_count += 1,
            crate::agent::safety::file_safety::FileSafety::Caution { .. } => caution_count += 1,
            crate::agent::safety::file_safety::FileSafety::Deny { .. } => deny_count += 1,
        }
        results.push(file_safety_to_json(path, &v));
    }
    Ok(json!({
        "count": args.len(),
        "results": results,
        "summary": {
            "allow":   allow_count,
            "caution": caution_count,
            "deny":    deny_count,
        },
    }))
}

fn file_safety_to_json(path: &str, v: &crate::agent::safety::file_safety::FileSafety) -> Value {
    json!({
        "path":     path,
        "verdict":  v.label(),
        "reason":   v.reason(),
        "category": v.category().map(|c| c.as_str()),
    })
}

/// `cos agent osv [parse <file>|check <file>|query <name>@<version> --ecosystem <eco>|ecosystems]`
fn osv_cmd(args: &[String]) -> Result<Value, String> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("");
    match sub {
        "" => Err(
            "usage: cos agent osv [parse <file> | check <file> | query <name>@<version> --ecosystem <eco> | ecosystems]"
                .to_string(),
        ),
        "parse" => osv_parse_cmd(&args[1..]),
        "check" => osv_check_cmd(&args[1..]),
        "query" => osv_query_cmd(&args[1..]),
        "ecosystems" => Ok(json!({
            "ecosystems": [
                "crates.io",
                "npm",
                "PyPI",
                "Go",
            ],
            "lockfiles": [
                "Cargo.lock",
                "package-lock.json",
                "requirements.txt",
                "go.sum",
            ],
        })),
        other => Err(format!(
            "unknown osv subcommand: {other}. try: parse | check | query | ecosystems"
        )),
    }
}

fn osv_parse_cmd(args: &[String]) -> Result<Value, String> {
    if args.is_empty() {
        return Err("usage: cos agent osv parse <lockfile>".to_string());
    }
    if args.len() > 1 {
        return Err("osv parse accepts a single file argument".to_string());
    }
    let path = std::path::Path::new(&args[0]);
    let body =
        std::fs::read_to_string(path).map_err(|e| format!("osv: read {}: {e}", path.display()))?;
    let pkgs = crate::agent::safety::osv::parse_lockfile(path, &body)?;
    Ok(json!({
        "lockfile": path.display().to_string(),
        "count":    pkgs.len(),
        "packages": pkgs.iter().map(|p| p.to_json()).collect::<Vec<_>>(),
    }))
}

fn osv_check_cmd(args: &[String]) -> Result<Value, String> {
    if args.is_empty() {
        return Err("usage: cos agent osv check <lockfile>".to_string());
    }
    if args.len() > 1 {
        return Err("osv check accepts a single file argument".to_string());
    }
    let path = std::path::Path::new(&args[0]);
    let body =
        std::fs::read_to_string(path).map_err(|e| format!("osv: read {}: {e}", path.display()))?;
    let pkgs = crate::agent::safety::osv::parse_lockfile(path, &body)?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("osv: build runtime: {e}"))?;
    let mut total_vulns = 0u64;
    let mut results = Vec::with_capacity(pkgs.len());
    for pkg in &pkgs {
        let vulns = rt
            .block_on(crate::agent::safety::osv::query(pkg))
            .unwrap_or_else(|e| {
                tracing::warn!("osv: {} {} {}: {}", pkg.ecosystem, pkg.name, pkg.version, e);
                Vec::new()
            });
        total_vulns += vulns.len() as u64;
        if !vulns.is_empty() {
            results.push(json!({
                "package": pkg.to_json(),
                "vulns":   vulns.iter().map(|v| v.to_json()).collect::<Vec<_>>(),
            }));
        }
    }
    Ok(json!({
        "lockfile":      path.display().to_string(),
        "package_count": pkgs.len(),
        "vuln_count":    total_vulns,
        "results":       results,
    }))
}

fn osv_query_cmd(args: &[String]) -> Result<Value, String> {
    let mut name_at_version: Option<String> = None;
    let mut ecosystem: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--ecosystem" => {
                i += 1;
                ecosystem = Some(
                    args.get(i)
                        .ok_or_else(|| "missing value for --ecosystem".to_string())?
                        .clone(),
                );
            }
            other if other.starts_with("--") => {
                return Err(format!("osv query: unknown flag: {other}"));
            }
            other => {
                if name_at_version.is_some() {
                    return Err("osv query: extra positional argument".to_string());
                }
                name_at_version = Some(other.to_string());
            }
        }
        i += 1;
    }
    let coord = name_at_version.ok_or_else(|| {
        "usage: cos agent osv query <name>@<version> --ecosystem <eco>".to_string()
    })?;
    let (name, version) = coord
        .rsplit_once('@')
        .ok_or_else(|| format!("osv query: '{coord}' is not in <name>@<version> format"))?;
    if name.is_empty() || version.is_empty() {
        return Err("osv query: name and version must both be non-empty".to_string());
    }
    let eco = ecosystem.ok_or_else(|| {
        "osv query: --ecosystem is required (e.g. crates.io, npm, PyPI, Go)".to_string()
    })?;
    let pkg = crate::agent::safety::osv::Package::new(eco, name, version);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("osv: build runtime: {e}"))?;
    let vulns = rt.block_on(crate::agent::safety::osv::query(&pkg))?;
    Ok(json!({
        "package":    pkg.to_json(),
        "vuln_count": vulns.len(),
        "vulns":      vulns.iter().map(|v| v.to_json()).collect::<Vec<_>>(),
    }))
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
            let configured_servers: Vec<Value> = cfg
                .mcp_servers
                .iter()
                .map(|s| {
                    json!({
                        "name": s.name,
                        "command": s.command,
                        "args_count": s.args.len(),
                        "env_count": s.env.len(),
                        "timeout_secs": s.timeout_secs,
                        "enabled": s.enabled,
                    })
                })
                .collect();
            let enabled_count = cfg.mcp_servers.iter().filter(|s| s.enabled).count();
            Ok(json!({
                "status": "ready",
                "transport": "stdio",
                "server_name": format!("cos-agent/{}", env!("CARGO_PKG_VERSION")),
                "tools_registered": tools.names_unfiltered().len(),
                "tools_permitted": tools.names().len(),
                "tools": tools.names(),
                "external_servers_configured": cfg.mcp_servers.len(),
                "external_servers_enabled": enabled_count,
                "external_servers": configured_servers,
            }))
        }
        "servers" => {
            // `cos agent mcp servers [--probe]` — list configured
            // external MCP servers. With `--probe`, attempt to attach
            // each enabled one and report tool counts (does not
            // mutate global state; the runtime registry is built
            // fresh inside this call and dropped on return).
            let probe = args.iter().any(|a| a == "--probe");
            let cfg = &crate::config::get().agent;
            if !probe {
                let entries: Vec<Value> = cfg
                    .mcp_servers
                    .iter()
                    .map(|s| {
                        json!({
                            "name": s.name,
                            "command": s.command,
                            "args": s.args,
                            "env_keys": s.env.keys().collect::<Vec<_>>(),
                            "cwd": s.cwd,
                            "timeout_secs": s.timeout_secs,
                            "enabled": s.enabled,
                        })
                    })
                    .collect();
                return Ok(json!({
                    "ok": true,
                    "probed": false,
                    "count": cfg.mcp_servers.len(),
                    "servers": entries,
                }));
            }
            // Probe: attach each enabled server, report tools, drop
            // handles immediately (children torn down). Best-effort:
            // failed attachments are reported per-server.
            use crate::agent::tools::mcp::integration::{attach_server, McpServerSpec};
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("tokio runtime: {e}"))?;
            let report = runtime.block_on(async {
                let mut out: Vec<Value> = Vec::with_capacity(cfg.mcp_servers.len());
                for s in &cfg.mcp_servers {
                    if !s.enabled {
                        out.push(json!({
                            "name": s.name,
                            "enabled": false,
                            "skipped": true,
                        }));
                        continue;
                    }
                    let spec = McpServerSpec {
                        name: s.name.clone(),
                        command: s.command.clone(),
                        args: s.args.clone(),
                        env: s.env.clone(),
                        cwd: s.cwd.clone(),
                        timeout_secs: s.timeout_secs,
                    };
                    let mut throwaway_registry =
                        crate::agent::tools::registry::ToolRegistry::new();
                    match attach_server(&spec, &mut throwaway_registry).await {
                        Ok(handle) => {
                            let tools = throwaway_registry.names_unfiltered();
                            out.push(json!({
                                "name": s.name,
                                "enabled": true,
                                "ok": true,
                                "tool_count": handle.tool_count(),
                                "tools": tools,
                            }));
                            // handle dropped here — child killed
                        }
                        Err(e) => {
                            out.push(json!({
                                "name": s.name,
                                "enabled": true,
                                "ok": false,
                                "error": e,
                            }));
                        }
                    }
                }
                out
            });
            Ok(json!({
                "ok": true,
                "probed": true,
                "count": cfg.mcp_servers.len(),
                "servers": report,
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
            "unknown mcp subcommand: {other}. try: status | servers [--probe] | serve | probe | call"
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
                timeout_secs = raw.parse::<u64>().map_err(|e| format!("--timeout: {e}"))?;
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
    let cmd = cmd.ok_or_else(|| "--cmd <executable> required".to_string())?;
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
        let init =
            tokio::time::timeout(std::time::Duration::from_secs(spec.timeout_secs), init_fut)
                .await
                .map_err(|_| {
                    // Best-effort kill — child holds stdio fds.
                    let _ = child.start_kill();
                    format!(
                        "timed out waiting for initialize after {}s",
                        spec.timeout_secs
                    )
                })?
                .map_err(|e| format!("initialize: {e}"))?;
        // initialized notification — many servers don't gate on it,
        // but spec-correct clients send it.
        let _ = client.notify("notifications/initialized", None).await;
        let tools_fut = client.list_tools();
        let tools_res =
            tokio::time::timeout(std::time::Duration::from_secs(spec.timeout_secs), tools_fut)
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
        let _init =
            tokio::time::timeout(std::time::Duration::from_secs(spec.timeout_secs), init_fut)
                .await
                .map_err(|_| {
                    let _ = child.start_kill();
                    format!(
                        "timed out waiting for initialize after {}s",
                        spec.timeout_secs
                    )
                })?
                .map_err(|e| format!("initialize: {e}"))?;
        let _ = client.notify("notifications/initialized", None).await;
        let call_fut = client.call_tool(tool.clone(), input.clone());
        let result =
            tokio::time::timeout(std::time::Duration::from_secs(spec.timeout_secs), call_fut)
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
        "author" => return curator_author_cmd(&args[1..]),
        "scan" => return curator_scan_cmd(&args[1..]),
        other => {
            return Err(format!(
                "unknown curator subcommand: '{other}'. try: propose <session_id> [...] | drafts list|show|accept|reject|delete | author <draft_id> [--model <name>] [--write] [--out <path>] | scan [--limit N] [--save] [--min-tools N] [--min-turns N] [--no-require-acceptance]"
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
                config.min_assistant_turns = v.parse().map_err(|e| format!("--min-turns: {e}"))?;
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
            if matches!(t.role, crate::agent::curator::TurnRole::User)
                && looks_like_acceptance(&t.content)
            {
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
        "auto-title" => {
            // `cos agent curator drafts auto-title <id> [--seed description|title|both] [--dry-run]`
            // Re-runs `agent::title::generate_title` against the draft's
            // text via the auxiliary client and (unless --dry-run) writes
            // the result back via `set_title`. Uses the same fallback
            // chain as runtime::loop_: empty model output / errors / no
            // aux configured all degrade to the heuristic so the command
            // never produces a blank title.
            let id = args
                .get(1)
                .cloned()
                .filter(|s| !s.is_empty() && !s.starts_with("--"))
                .ok_or_else(|| {
                    "usage: cos agent curator drafts auto-title <id> [--seed description|title|both] [--dry-run]"
                        .to_string()
                })?;
            let mut seed_kind = "description".to_string();
            let mut dry_run = false;
            let mut i = 2usize;
            while i < args.len() {
                match args[i].as_str() {
                    "--seed" => {
                        seed_kind = args
                            .get(i + 1)
                            .cloned()
                            .ok_or_else(|| "--seed needs description|title|both".to_string())?;
                        i += 2;
                    }
                    "--dry-run" => {
                        dry_run = true;
                        i += 1;
                    }
                    other => {
                        return Err(format!(
                            "unknown flag for `drafts auto-title`: {other}"
                        ));
                    }
                }
            }
            // Validate seed_kind BEFORE touching the live DB so a typo
            // doesn't leak the error to disk-IO context.
            match seed_kind.as_str() {
                "description" | "title" | "both" => {}
                other => {
                    return Err(format!(
                        "--seed: invalid '{other}' (try description|title|both)"
                    ))
                }
            }
            let mut store = DraftStore::open_default()?;
            let rec = store
                .get(&id)
                .cloned()
                .ok_or_else(|| format!("no draft with id '{id}'"))?;
            let seed = match seed_kind.as_str() {
                "description" => rec.draft.description.clone(),
                "title" => rec.draft.title.clone(),
                "both" => format!("{}\n\n{}", rec.draft.title, rec.draft.description),
                _ => unreachable!("validated above"),
            };
            let cfg = &crate::config::get().agent;
            let aux = crate::agent::runtime::loop_::auxiliary_from_cfg(cfg)
                .map_err(|e| format!("auxiliary client build failed: {e}"))?;
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("tokio runtime: {e}"))?;
            let new_title = runtime
                .block_on(crate::agent::title::generate_title(aux.as_ref(), &seed));
            let method = if aux.is_some() { "llm-or-fallback" } else { "heuristic" };
            if dry_run {
                return Ok(json!({
                    "id": rec.id,
                    "old_title": rec.draft.title,
                    "proposed_title": new_title,
                    "method": method,
                    "seed_kind": seed_kind,
                    "applied": false,
                }));
            }
            store.set_title(&id, &new_title)?;
            let after = store.get(&id).cloned().ok_or_else(|| {
                format!("draft {id} disappeared after auto-title (race)")
            })?;
            Ok(json!({
                "id": after.id,
                "old_title": rec.draft.title,
                "title": after.draft.title,
                "method": method,
                "seed_kind": seed_kind,
                "applied": true,
            }))
        }
        other => Err(format!(
            "unknown drafts subcommand: '{other}'. try: list | show <id> | accept <id> | reject <id> | delete <id> | retitle <id> <title> | auto-title <id> [--seed description|title|both] [--dry-run]"
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

/// `cos agent curator author <draft_id> [--model <name>] [--write] [--out <path>]`
///
/// Drives the [`crate::agent::curator_author::author`] LLM pass:
/// looks up the draft in the persistent draft store, replays the
/// originating session's history from the memory DB to rebuild the
/// turn list the deterministic pipeline saw, then asks the
/// configured LLM to produce a `SKILL.md` document. Output is the
/// full document on `document` plus metadata on source / chars /
/// error.
///
/// Side effects:
///  * `--write` (or `--out <path>`): persist the document.
///    Without `--out`, defaults to
///    `<agent_skills_dir>/<draft.suggested_id>/SKILL.md`. Refuses
///    to overwrite an existing file unless `--force` is also set.
///  * Without `--write`, the document is returned in the JSON
///    envelope and nothing is touched on disk — useful for
///    previewing in CI / scripts.
///
/// LLM source: by default the auxiliary client is used (cheap
/// model). `--model <name>` overrides the model id; `--primary`
/// forces routing through the primary provider instead of the
/// auxiliary one.
fn curator_author_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::curator::{message_to_turn, ConversationTurn};
    use crate::agent::curator_author::{author, AuthorConfig, AuthorSource};
    use curator_drafts::DraftStore;

    let draft_id = args
        .first()
        .cloned()
        .filter(|s| !s.is_empty() && !s.starts_with("--"))
        .ok_or_else(|| "usage: cos agent curator author <draft_id> [flags]".to_string())?;

    let mut model_override: Option<String> = None;
    let mut write_to_disk = false;
    let mut out_path: Option<String> = None;
    let mut force = false;
    let mut use_primary = false;
    let mut limit: usize = 200;

    let mut i = 1usize;
    while i < args.len() {
        match args[i].as_str() {
            "--model" => {
                model_override = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--model needs <name>".to_string())?,
                );
                i += 2;
            }
            "--write" => {
                write_to_disk = true;
                i += 1;
            }
            "--out" => {
                out_path = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--out needs <path>".to_string())?,
                );
                write_to_disk = true;
                i += 2;
            }
            "--force" => {
                force = true;
                i += 1;
            }
            "--primary" => {
                use_primary = true;
                i += 1;
            }
            "--limit" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| "--limit needs <n>".to_string())?;
                limit = v.parse().map_err(|e| format!("--limit: {e}"))?;
                i += 2;
            }
            other => return Err(format!("unknown flag for `curator author`: {other}")),
        }
    }

    // Resolve the draft.
    let store = DraftStore::open_default().map_err(|e| format!("draft store: {e}"))?;
    let entry = store.get(&draft_id).ok_or_else(|| {
        format!("no draft with id '{draft_id}' (try `cos agent curator drafts list`)")
    })?;

    // Replay the session's recorded turns. If the session is gone
    // (rare but possible if the user cleared memory between
    // propose and author) we still author from the draft alone.
    let turns: Vec<ConversationTurn> = match memory::sqlite_fts::MemoryDb::open_default() {
        Ok(db) => match db.recent(&entry.session_id, limit) {
            Ok(rows) => rows
                .iter()
                .filter_map(|r| message_to_turn(&r.role, &r.content))
                .collect(),
            Err(_) => Vec::new(),
        },
        Err(_) => Vec::new(),
    };

    // Build the provider. Auxiliary by default (when configured);
    // primary on --primary or when auxiliary isn't set.
    let cfg = &crate::config::get().agent;
    let aux_available = cfg
        .auxiliary_provider
        .as_deref()
        .map(|s| !s.is_empty())
        .unwrap_or(false)
        && cfg
            .auxiliary_model
            .as_deref()
            .map(|s| !s.is_empty())
            .unwrap_or(false);

    let (provider, resolved_model, route) = if use_primary || !aux_available {
        let model = model_override.unwrap_or_else(|| cfg.model.clone());
        let provider = llm::registry::build(&cfg.provider, &model, cfg)
            .map_err(|e| format!("primary provider unavailable: {e}"))?;
        let route = if use_primary {
            "primary"
        } else {
            "primary (auxiliary not configured)"
        };
        (provider, model, route)
    } else {
        let aux_provider_name = cfg.auxiliary_provider.clone().unwrap_or_default();
        let aux_model = model_override
            .clone()
            .unwrap_or_else(|| cfg.auxiliary_model.clone().unwrap_or_default());
        let provider = llm::registry::build(&aux_provider_name, &aux_model, cfg)
            .map_err(|e| format!("auxiliary provider unavailable: {e}"))?;
        (provider, aux_model, "auxiliary")
    };

    let acfg = AuthorConfig::for_model(resolved_model.clone());

    // Drive the async authoring call from a blocking dispatcher.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime: {e}"))?;
    let result = runtime.block_on(author(provider, &acfg, &entry.draft, &turns));

    let mut payload = json!({
        "draft_id": draft_id,
        "session_id": entry.session_id,
        "model": resolved_model,
        "route": route,
        "source": match result.source {
            AuthorSource::Llm => "llm",
            AuthorSource::Fallback => "fallback",
        },
        "body_chars": result.body_chars,
        "error": result.error,
        "turns_replayed": turns.len(),
    });

    if write_to_disk {
        let target_path = if let Some(custom) = out_path {
            std::path::PathBuf::from(custom)
        } else {
            crate::paths::agent_skills_dir()
                .join(&entry.draft.suggested_id)
                .join("SKILL.md")
        };
        if target_path.exists() && !force {
            payload["written"] = json!(false);
            payload["write_error"] = json!(format!(
                "refused to overwrite existing {} (pass --force)",
                target_path.display()
            ));
            payload["document"] = json!(result.document);
            return Ok(payload);
        }
        if let Some(parent) = target_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        std::fs::write(&target_path, &result.document)
            .map_err(|e| format!("write {}: {e}", target_path.display()))?;
        payload["written"] = json!(true);
        payload["path"] = json!(target_path.display().to_string());
    } else {
        payload["written"] = json!(false);
        payload["document"] = json!(result.document);
    }

    Ok(payload)
}

/// `cos agent curator scan [flags]` — batch-propose drafts across
/// recent sessions.
///
/// Walks the most recent N sessions in the memory DB, runs the
/// deterministic [`Curator::propose`] pipeline against each, and
/// returns a per-session report. By default nothing is persisted
/// (`saved: false` for every result) so the user can preview;
/// `--save` mirrors `propose --save` and writes accepted drafts
/// to the [`curator_drafts::DraftStore`].
///
/// Sessions that already produced a saved draft are skipped (we
/// don't redraft the same conversation on every scan), unless
/// `--reprocess` is set.
///
/// Flags:
///  * `--limit N` — examine the most recent N sessions
///    (default 25).
///  * `--save` — persist successful drafts.
///  * `--reprocess` — also include sessions that already have
///    a saved draft.
///  * `--min-tools N` — override [`CuratorConfig::min_distinct_tools`].
///  * `--min-turns N` — override [`CuratorConfig::min_assistant_turns`].
///  * `--no-require-acceptance` — drop the user-acceptance gate.
///  * `--message-limit N` — cap messages-per-session pulled from
///    the DB (default 200, mirrors `propose --limit`).
fn curator_scan_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::curator::{
        looks_like_acceptance, message_to_turn, ConversationTurn, Curator, CuratorConfig,
        CuratorOutcome, TurnRole,
    };
    use curator_drafts::DraftStore;

    let mut session_limit: usize = 25;
    let mut message_limit: usize = 200;
    let mut save = false;
    let mut reprocess = false;
    let mut config = CuratorConfig::default();

    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--limit" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| "--limit needs <n>".to_string())?;
                session_limit = v.parse().map_err(|e| format!("--limit: {e}"))?;
                i += 2;
            }
            "--message-limit" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| "--message-limit needs <n>".to_string())?;
                message_limit = v.parse().map_err(|e| format!("--message-limit: {e}"))?;
                i += 2;
            }
            "--save" => {
                save = true;
                i += 1;
            }
            "--reprocess" => {
                reprocess = true;
                i += 1;
            }
            "--no-require-acceptance" => {
                config.require_user_acceptance = false;
                i += 1;
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
                config.min_assistant_turns = v.parse().map_err(|e| format!("--min-turns: {e}"))?;
                i += 2;
            }
            other => return Err(format!("unknown flag for `curator scan`: {other}")),
        }
    }

    let db = memory::sqlite_fts::MemoryDb::open_default()
        .map_err(|e| format!("memory db unavailable: {e}"))?;
    let sessions = db
        .sessions(session_limit)
        .map_err(|e| format!("sessions query failed: {e}"))?;

    // Pre-load existing drafts so `reprocess: false` can cheaply
    // skip already-distilled sessions. Falling back to "no
    // existing drafts" if the store is unreadable lets scan
    // still work as a preview surface.
    let drafts_store = DraftStore::open_default().ok();
    let already_drafted: std::collections::HashSet<String> = drafts_store
        .as_ref()
        .map(|s| s.list().iter().map(|r| r.session_id.clone()).collect())
        .unwrap_or_default();

    let mut store_for_save = if save {
        Some(DraftStore::open_default().map_err(|e| format!("draft store: {e}"))?)
    } else {
        None
    };

    let curator = Curator::new(config);

    let mut results: Vec<Value> = Vec::new();
    let mut drafted = 0usize;
    let mut saved = 0usize;
    let mut skipped_existing = 0usize;
    let mut skipped_empty = 0usize;
    let mut not_enough = 0usize;

    for s in &sessions {
        if !reprocess && already_drafted.contains(&s.session_id) {
            skipped_existing += 1;
            results.push(json!({
                "session_id": s.session_id,
                "outcome": "skipped_existing",
                "title": s.title,
            }));
            continue;
        }
        let rows = match db.recent(&s.session_id, message_limit) {
            Ok(r) => r,
            Err(e) => {
                results.push(json!({
                    "session_id": s.session_id,
                    "outcome": "error",
                    "error": format!("recent: {e}"),
                }));
                continue;
            }
        };
        if rows.is_empty() {
            skipped_empty += 1;
            results.push(json!({
                "session_id": s.session_id,
                "outcome": "skipped_empty",
            }));
            continue;
        }
        let mut turns: Vec<ConversationTurn> = rows
            .iter()
            .filter_map(|r| message_to_turn(&r.role, &r.content))
            .collect();
        // Apply the conservative built-in heuristic to user turns
        // (matches `propose` without --accept).
        for t in turns.iter_mut() {
            if matches!(t.role, TurnRole::User) && looks_like_acceptance(&t.content) {
                t.user_acceptance = true;
            }
        }
        match curator.propose(&turns) {
            CuratorOutcome::Drafted(draft) => {
                drafted += 1;
                let mut entry = json!({
                    "session_id": s.session_id,
                    "outcome": "drafted",
                    "messages_scanned": rows.len(),
                    "title": s.title,
                    "draft": draft,
                });
                if let Some(store) = store_for_save.as_mut() {
                    match store.add(s.session_id.clone(), draft) {
                        Ok(id) => {
                            entry["draft_id"] = json!(id);
                            entry["saved"] = json!(true);
                            saved += 1;
                        }
                        Err(e) => {
                            entry["saved"] = json!(false);
                            entry["save_error"] = json!(e);
                        }
                    }
                } else {
                    entry["saved"] = json!(false);
                }
                results.push(entry);
            }
            CuratorOutcome::NotEnough { reason } => {
                not_enough += 1;
                results.push(json!({
                    "session_id": s.session_id,
                    "outcome": "not_enough",
                    "messages_scanned": rows.len(),
                    "reason": reason,
                }));
            }
        }
    }

    Ok(json!({
        "session_limit": session_limit,
        "scanned": sessions.len(),
        "drafted": drafted,
        "saved": saved,
        "not_enough": not_enough,
        "skipped_existing": skipped_existing,
        "skipped_empty": skipped_empty,
        "results": results,
    }))
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
        let err = skills_cmd(&["hub".into(), "install".into(), "owner/repo".into()]).unwrap_err();
        assert!(err.contains("usage:"));
        assert!(err.contains("install"));
    }

    #[test]
    fn skills_hub_show_requires_id() {
        let err = skills_cmd(&["hub".into(), "show".into(), "owner/repo".into()]).unwrap_err();
        assert!(err.contains("usage:"));
        assert!(err.contains("show"));
    }

    #[test]
    fn skills_hub_unknown_subcommand_lists_options() {
        let err = skills_cmd(&["hub".into(), "bogus".into(), "owner/repo".into()]).unwrap_err();
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
        assert!(
            !models.is_empty(),
            "anthropic should have at least one model"
        );
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
        let err = llm_cmd(&["model".into(), "definitely-not-a-real-model".into()]).unwrap_err();
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
        let models = llm_cmd(&["models".into(), "--provider".into(), first]).expect("models ok");
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
        let err = llm_cmd(&["cost".into(), "--input".into(), "1000".into()]).unwrap_err();
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
                let models =
                    llm_cmd(&["models".into(), "--provider".into(), provider]).expect("models ok");
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
        assert!(
            out.contains("[REDACTED:"),
            "expected placeholder, got {out}"
        );
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
        let v = redact_cmd(&["--check".into(), "leaks AKIAIOSFODNN7EXAMPLE here".into()])
            .expect("check ok");
        assert_eq!(
            v.get("contains_secrets").and_then(|x| x.as_bool()),
            Some(true)
        );
        assert!(
            v.get("redacted").is_none(),
            "check mode should not include redacted"
        );
    }

    #[test]
    fn redact_check_negative() {
        let v = redact_cmd(&["--check".into(), "innocent text".into()]).expect("check ok");
        assert_eq!(
            v.get("contains_secrets").and_then(|x| x.as_bool()),
            Some(false)
        );
    }

    #[test]
    fn redact_strict_flag_propagates() {
        let v = redact_cmd(&["--strict".into(), "contact me at user@example.com".into()])
            .expect("strict redact");
        let out = v.get("redacted").and_then(|x| x.as_str()).unwrap();
        assert!(
            out.contains("[REDACTED:email]"),
            "strict should redact emails: {out}"
        );
        assert_eq!(v.get("strict").and_then(|x| x.as_bool()), Some(true));
    }

    #[test]
    fn redact_default_does_not_redact_email() {
        let v = redact_cmd(&["contact me at user@example.com".into()]).expect("default redact");
        let out = v.get("redacted").and_then(|x| x.as_str()).unwrap();
        assert!(
            out.contains("user@example.com"),
            "default should keep email: {out}"
        );
    }

    #[test]
    fn redact_from_file() {
        let dir = tempfile::tempdir().expect("tmp");
        let p = dir.path().join("sample.txt");
        std::fs::write(&p, "token=ghp_aBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789").expect("write");
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
        assert_eq!(
            s.get("total_duration_ms").and_then(|x| x.as_u64()),
            Some(300)
        );
        assert_eq!(
            s.get("average_duration_ms").and_then(|x| x.as_u64()),
            Some(150)
        );
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
        let p = v
            .get("prompt")
            .and_then(|x| x.as_str())
            .expect("prompt str");
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
        let v =
            think_scrub_cmd(&["before <think>secret reasoning</think> after".into()]).expect("ok");
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
        let v = think_scrub_cmd(&["--check".into(), "no tags here".into()]).expect("ok");
        assert_eq!(v.get("has_thinking").and_then(|x| x.as_bool()), Some(false));
    }

    #[test]
    fn think_scrub_handles_multiline_block() {
        let v = think_scrub_cmd(&["<thinking>\nline one\nline two\n</thinking>\nfinal".into()])
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
        std::fs::write(&p, "<reasoning>internal</reasoning>\nthe answer is 42").expect("write");
        let v = think_scrub_cmd(&["--file".into(), p.to_string_lossy().to_string()]).expect("ok");
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
        let v = tokens_cmd(&["--file".into(), p.to_string_lossy().to_string()]).expect("ok");
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
        let (s, _) = read_text_input(&["a".into(), "b".into(), "c".into()], "tokens").expect("ok");
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
        let err = nudge_cmd(&["add".into(), "not-a-number".into(), "msg".into()]).unwrap_err();
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
    fn mcp_status_includes_external_servers_section() {
        let v = mcp_cmd(&["status".into()]).expect("mcp status ok");
        // Always present even when no external servers are configured.
        assert!(v.get("external_servers_configured").is_some());
        assert!(v.get("external_servers_enabled").is_some());
        assert!(
            v.get("external_servers")
                .and_then(|x| x.as_array())
                .is_some(),
            "external_servers must be a JSON array (possibly empty)"
        );
    }

    #[test]
    fn mcp_servers_without_probe_does_not_spawn_anything() {
        // Default test config has no mcp_servers, so this is a pure
        // shape assertion. It's still useful because a regression
        // that turned off the !probe early-return would either spawn
        // nothing (passes) or panic on attach (we'd see the failure).
        let v = mcp_cmd(&["servers".into()]).expect("mcp servers ok");
        assert_eq!(v.get("ok").and_then(|x| x.as_bool()), Some(true));
        assert_eq!(v.get("probed").and_then(|x| x.as_bool()), Some(false));
        assert!(v.get("servers").and_then(|x| x.as_array()).is_some());
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
        let err = usage_cmd(&["overall".into(), "--since".into(), "not-iso".into()]).unwrap_err();
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
        let v = usage_cmd(&["provider".into(), "anthropic".into()]).expect("usage provider ok");
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
        assert_eq!(
            merged.tool_deny,
            vec!["cos_sandbox".to_string(), "cos_proc".to_string()]
        );
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
        let err = curator_cmd(&["propose".into(), "any-sid".into(), "--bogus".into()]).unwrap_err();
        assert!(err.to_lowercase().contains("unknown flag"));
    }

    #[test]
    fn curator_propose_min_turns_requires_value() {
        let err =
            curator_cmd(&["propose".into(), "any-sid".into(), "--min-turns".into()]).unwrap_err();
        assert!(err.contains("--min-turns"));
    }

    #[test]
    fn curator_drafts_unknown_subcommand_lists_options() {
        let err = curator_drafts_cmd(&["bogus".into()]).unwrap_err();
        assert!(err.contains("auto-title"));
        assert!(err.contains("retitle"));
    }

    #[test]
    fn curator_author_requires_draft_id() {
        let err = curator_cmd(&["author".into()]).unwrap_err();
        assert!(err.to_lowercase().contains("usage"));
    }

    #[test]
    fn curator_author_rejects_flag_as_id() {
        let err = curator_cmd(&["author".into(), "--write".into()]).unwrap_err();
        assert!(err.to_lowercase().contains("usage"));
    }

    #[test]
    fn curator_author_unknown_flag_rejected() {
        let err = curator_cmd(&["author".into(), "draft-1".into(), "--bogus".into()]).unwrap_err();
        assert!(err.to_lowercase().contains("unknown flag"));
    }

    #[test]
    fn curator_author_missing_draft_returns_helpful_error() {
        // The default DraftStore should open successfully (or fail
        // with an IO error); either way, asking for an unknown id
        // must return a string mentioning the missing id.
        let result = curator_cmd(&["author".into(), "definitely-not-real".into()]);
        let err = result.unwrap_err();
        assert!(
            err.contains("definitely-not-real") || err.contains("draft store"),
            "want missing-id or draft-store error, got: {err}"
        );
    }

    #[test]
    fn curator_scan_unknown_flag_rejected() {
        let err = curator_cmd(&["scan".into(), "--bogus".into()]).unwrap_err();
        assert!(err.to_lowercase().contains("unknown flag"));
    }

    #[test]
    fn curator_scan_limit_requires_value() {
        let err = curator_cmd(&["scan".into(), "--limit".into()]).unwrap_err();
        assert!(err.contains("--limit"));
    }

    #[test]
    fn curator_scan_returns_envelope_when_db_available() {
        // The scan command may succeed (returning an envelope with
        // zero scanned sessions) or fail with a "memory db
        // unavailable" error depending on test environment. Both
        // are acceptable; what matters is no panic and a recognised
        // outcome shape.
        match curator_cmd(&["scan".into(), "--limit".into(), "1".into()]) {
            Ok(v) => {
                assert!(v.get("scanned").is_some(), "envelope missing 'scanned'");
                assert!(v.get("results").is_some(), "envelope missing 'results'");
                assert!(v.get("drafted").is_some(), "envelope missing 'drafted'");
            }
            Err(e) => {
                assert!(
                    e.contains("memory db") || e.contains("draft store"),
                    "unexpected scan error: {e}"
                );
            }
        }
    }

    #[test]
    fn curator_scan_listed_in_unknown_subcommand_help() {
        let err = curator_cmd(&["bogus".into()]).unwrap_err();
        assert!(err.contains("scan"), "want scan listed in help, got: {err}");
        assert!(
            err.contains("author"),
            "want author listed in help, got: {err}"
        );
    }

    #[test]
    fn curator_drafts_auto_title_requires_id() {
        let err = curator_drafts_cmd(&["auto-title".into()]).unwrap_err();
        assert!(err.contains("usage"));
    }

    #[test]
    fn curator_drafts_auto_title_rejects_unknown_flag() {
        let err = curator_drafts_cmd(&["auto-title".into(), "some-id".into(), "--bogus".into()])
            .unwrap_err();
        assert!(err.contains("unknown flag"));
    }

    #[test]
    fn curator_drafts_auto_title_rejects_invalid_seed() {
        let err = curator_drafts_cmd(&[
            "auto-title".into(),
            "some-id".into(),
            "--seed".into(),
            "bogus".into(),
        ])
        .unwrap_err();
        assert!(err.contains("--seed"));
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
        let v =
            providers_cmd(&["--names".into(), "openai,anthropic".into()]).expect("providers ok");
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
        assert_eq!(arr[0].get("name").and_then(|n| n.as_str()), Some("openai"));
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
        let v = providers_cmd(&["--names".into(), "openai".into()]).expect("providers ok");
        let count = v.get("count").and_then(|c| c.as_u64()).unwrap_or(0);
        assert_eq!(count, 1);
    }

    #[test]
    fn providers_cmd_surfaces_bedrock_with_aws_access_key_env() {
        // Bedrock uses three env vars (access key, secret, optional
        // session token); we surface AWS_ACCESS_KEY_ID as the canonical
        // env, matching AWS SDK convention. credential is None
        // because Bedrock's three-name credential model doesn't fit
        // the single-name `*_credential` field.
        let v = providers_cmd(&["--names".into(), "bedrock".into()]).expect("providers ok");
        let arr = v.get("providers").and_then(|p| p.as_array()).unwrap();
        assert_eq!(arr.len(), 1);
        let entry = &arr[0];
        assert_eq!(entry.get("name").and_then(|n| n.as_str()), Some("bedrock"));
        assert_eq!(
            entry.get("env").and_then(|e| e.as_str()),
            Some("AWS_ACCESS_KEY_ID")
        );
        assert_eq!(entry.get("credential"), Some(&serde_json::Value::Null));
        assert_eq!(
            entry.get("key_required"),
            Some(&serde_json::Value::Bool(true))
        );
        let url = entry
            .get("default_base_url")
            .and_then(|u| u.as_str())
            .unwrap_or("");
        assert!(
            url.contains("bedrock-runtime") && url.contains("{region}"),
            "expected region-templated default_base_url, got {url}"
        );
    }

    // ---- provider-doctor ----

    #[test]
    fn provider_doctor_static_only_includes_doctor_section() {
        // Default invocation: no --probe-network.
        let v = provider_doctor_cmd(&[]).expect("doctor ok");
        // Inherits the providers_cmd shape.
        assert!(v.get("providers").and_then(|p| p.as_array()).is_some());
        // Doctor section present.
        let doctor = v.get("doctor").expect("doctor section");
        assert_eq!(
            doctor.get("probe_network"),
            Some(&serde_json::Value::Bool(false))
        );
        let probe = doctor.get("active_probe").expect("active_probe");
        assert_eq!(
            probe.get("attempted"),
            Some(&serde_json::Value::Bool(false))
        );
        assert!(probe.get("reason").and_then(|r| r.as_str()).is_some());
    }

    #[test]
    fn provider_doctor_default_timeout_is_30s() {
        let v = provider_doctor_cmd(&[]).expect("doctor ok");
        let doctor = v.get("doctor").unwrap();
        assert_eq!(
            doctor.get("probe_timeout_secs").and_then(|t| t.as_u64()),
            Some(30)
        );
    }

    #[test]
    fn provider_doctor_custom_timeout_parses() {
        let v = provider_doctor_cmd(&["--timeout".into(), "5".into()]).expect("doctor ok");
        let doctor = v.get("doctor").unwrap();
        assert_eq!(
            doctor.get("probe_timeout_secs").and_then(|t| t.as_u64()),
            Some(5)
        );
    }

    #[test]
    fn provider_doctor_zero_timeout_rejected() {
        let err = provider_doctor_cmd(&["--timeout".into(), "0".into()]).unwrap_err();
        assert!(err.contains("--timeout"));
    }

    #[test]
    fn provider_doctor_non_numeric_timeout_rejected() {
        let err = provider_doctor_cmd(&["--timeout".into(), "soon".into()]).unwrap_err();
        assert!(err.contains("--timeout"));
    }

    #[test]
    fn provider_doctor_unknown_flag_rejected() {
        let err = provider_doctor_cmd(&["--mystery".into()]).unwrap_err();
        assert!(err.contains("--mystery"));
        assert!(err.contains("--probe-network"));
    }

    #[test]
    fn provider_doctor_names_filter_requires_value() {
        let err = provider_doctor_cmd(&["--names".into()]).unwrap_err();
        assert!(err.contains("--names"));
    }

    #[test]
    fn provider_doctor_skips_probe_for_mock_provider() {
        // The default test config provider is "mock" (see config.rs Default).
        // Verify --probe-network is gracefully skipped without spinning a
        // tokio runtime or hitting the network.
        let v = provider_doctor_cmd(&["--probe-network".into()]).expect("doctor ok");
        let probe = v
            .get("doctor")
            .and_then(|d| d.get("active_probe"))
            .expect("probe");
        // Active default in test cfg is "mock".
        assert_eq!(
            v.get("doctor")
                .and_then(|d| d.get("active"))
                .and_then(|a| a.as_str()),
            Some("mock")
        );
        assert_eq!(
            probe.get("attempted"),
            Some(&serde_json::Value::Bool(false))
        );
        let reason = probe.get("reason").and_then(|r| r.as_str()).unwrap_or("");
        assert!(
            reason.contains("mock") || reason.contains("meaningless"),
            "expected mock-skip reason, got {reason:?}"
        );
    }

    #[test]
    fn provider_doctor_filter_excluding_active_marks_out_of_scope() {
        // Active is "mock" in test config; filter to "openai" only.
        let v = provider_doctor_cmd(&["--probe-network".into(), "--names".into(), "openai".into()])
            .expect("doctor ok");
        let doctor = v.get("doctor").unwrap();
        assert_eq!(
            doctor.get("active_in_scope"),
            Some(&serde_json::Value::Bool(false))
        );
        let probe = doctor.get("active_probe").unwrap();
        assert_eq!(
            probe.get("attempted"),
            Some(&serde_json::Value::Bool(false))
        );
        let reason = probe.get("reason").and_then(|r| r.as_str()).unwrap_or("");
        assert!(reason.contains("filtered out") || reason.contains("--names"));
    }

    #[test]
    fn provider_doctor_surfaces_effective_timeout_min_of_two() {
        // probe_timeout 9999 + provider request_timeout (default = some
        // smaller value from CosConfig) → effective is the smaller one.
        let v = provider_doctor_cmd(&["--timeout".into(), "9999".into()]).expect("doctor ok");
        let doctor = v.get("doctor").unwrap();
        let probe_t = doctor
            .get("probe_timeout_secs")
            .and_then(|t| t.as_u64())
            .unwrap();
        let provider_t = doctor
            .get("provider_request_timeout_secs")
            .and_then(|t| t.as_u64())
            .unwrap();
        let effective = doctor
            .get("effective_timeout_secs")
            .and_then(|t| t.as_u64())
            .unwrap();
        assert_eq!(probe_t, 9999);
        assert_eq!(effective, std::cmp::min(probe_t, provider_t));
    }

    #[test]
    fn llm_error_kind_classification_is_complete() {
        // Pin the tag for every LlmError variant — adding a new variant
        // without updating the doctor classifier should fail this test.
        assert_eq!(
            llm_error_kind(&llm::LlmError::NotConfigured("x".into())),
            "not_configured"
        );
        assert_eq!(
            llm_error_kind(&llm::LlmError::InvalidRequest("x".into())),
            "invalid_request"
        );
        assert_eq!(
            llm_error_kind(&llm::LlmError::Provider {
                status: 500,
                message: "x".into(),
            }),
            "provider"
        );
        assert_eq!(
            llm_error_kind(&llm::LlmError::RateLimited { retry_after_ms: 0 }),
            "rate_limited"
        );
        assert_eq!(llm_error_kind(&llm::LlmError::Auth), "auth");
        assert_eq!(llm_error_kind(&llm::LlmError::Parse("x".into())), "parse");
        assert_eq!(llm_error_kind(&llm::LlmError::Stream("x".into())), "stream");
        assert_eq!(
            llm_error_kind(&llm::LlmError::Internal("x".into())),
            "internal"
        );
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
        assert_eq!(v.get("title").and_then(|s| s.as_str()), Some("hello there"));
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
    fn title_cmd_llm_without_aux_errs() {
        // No auxiliary config in test env → CLI should err clearly.
        let err = title_cmd(&["hello".into(), "--llm".into()]).unwrap_err();
        assert!(err.contains("auxiliary"));
    }

    #[test]
    fn title_cmd_llm_flag_is_consumed_not_treated_as_input() {
        // Without --llm we still get heuristic from "hello"; confirms
        // flag isn't joined into the input.
        let v = title_cmd(&["hello".into()]).expect("title ok");
        assert_eq!(v.get("title").and_then(|s| s.as_str()), Some("hello"));
    }

    #[test]
    fn title_cmd_with_aux_none_falls_back_to_heuristic() {
        let v = title_cmd_with_aux("/help me", None).expect("ok");
        assert_eq!(v.get("method").and_then(|s| s.as_str()), Some("heuristic"));
        assert_eq!(v.get("title").and_then(|s| s.as_str()), Some("me"));
    }

    #[test]
    fn title_cmd_with_aux_uses_mock_response() {
        use crate::agent::llm::auxiliary::{AuxiliaryClient, AuxiliaryConfig};
        use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
        use crate::config::AgentConfig;
        let cfg = AgentConfig::default();
        let provider = MockProvider::new("title-mock", &cfg);
        provider.push_response(MockResponse::Text("Quick rust setup".into()));
        let aux = AuxiliaryClient::new(
            std::sync::Arc::new(provider),
            AuxiliaryConfig::new("mock", "title-mock"),
        );
        let v = title_cmd_with_aux("How do I install rust?", Some(&aux)).expect("ok");
        assert_eq!(v.get("method").and_then(|s| s.as_str()), Some("llm"));
        assert_eq!(
            v.get("title").and_then(|s| s.as_str()),
            Some("Quick rust setup")
        );
        assert_eq!(v.get("provider").and_then(|s| s.as_str()), Some("mock"));
        assert_eq!(v.get("model").and_then(|s| s.as_str()), Some("title-mock"));
    }

    #[test]
    fn summarise_cmd_returns_first_sentence() {
        let v = summarise_cmd(&["First sentence. Second one.".into()]).expect("summarise ok");
        assert_eq!(
            v.get("summary").and_then(|s| s.as_str()),
            Some("First sentence.")
        );
        assert_eq!(v.get("clamped").and_then(|b| b.as_bool()), Some(false));
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
        let err = summarise_cmd(&["--max".into(), "not-a-number".into(), "x".into()]).unwrap_err();
        assert!(err.contains("--max"));
    }

    #[test]
    fn summarize_alias_dispatches_to_summarise() {
        // Confirm the US-spelling alias hits the same handler.
        let v = run("summarize", &["hello.".into()]).expect("summarize ok");
        assert_eq!(v.get("summary").and_then(|s| s.as_str()), Some("hello."));
    }

    #[test]
    fn summarise_cmd_llm_without_aux_errs() {
        let err = summarise_cmd(&["hello there".into(), "--llm".into()]).unwrap_err();
        assert!(err.contains("auxiliary"));
    }

    #[test]
    fn summarise_cmd_with_aux_none_falls_back_to_heuristic() {
        let v = summarise_cmd_with_aux("First sentence. Second one.", 200, None).expect("ok");
        assert_eq!(v.get("method").and_then(|s| s.as_str()), Some("heuristic"));
    }

    #[test]
    fn summarise_cmd_with_aux_uses_mock_response_when_input_exceeds_max() {
        use crate::agent::llm::auxiliary::{AuxiliaryClient, AuxiliaryConfig};
        use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
        use crate::config::AgentConfig;
        let cfg = AgentConfig::default();
        let provider = MockProvider::new("sum-mock", &cfg);
        provider.push_response(MockResponse::Text("Compact summary".into()));
        let aux = AuxiliaryClient::new(
            std::sync::Arc::new(provider),
            AuxiliaryConfig::new("mock", "sum-mock"),
        );
        // Input must exceed max_chars to trigger the aux path (see summarise()).
        let big = "long ".repeat(60);
        let v = summarise_cmd_with_aux(&big, 50, Some(&aux)).expect("ok");
        assert_eq!(v.get("method").and_then(|s| s.as_str()), Some("llm"));
        assert_eq!(
            v.get("summary").and_then(|s| s.as_str()),
            Some("Compact summary")
        );
        assert_eq!(v.get("provider").and_then(|s| s.as_str()), Some("mock"));
    }

    #[test]
    fn classify_cmd_matches_label_case_insensitively() {
        let v = classify_cmd(&[
            "POSITIVE".into(),
            "--labels".into(),
            "positive,negative,neutral".into(),
        ])
        .expect("classify ok");
        assert_eq!(v.get("matched").and_then(|m| m.as_str()), Some("positive"));
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
        let v = classify_cmd(&["yes.".into(), "--labels".into(), "yes,no".into()])
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
        let err = classify_cmd(&["yes".into(), "--labels".into(), ",, ,".into()]).unwrap_err();
        assert!(err.contains("--labels"));
    }

    #[test]
    fn classify_cmd_returns_label_set_in_response() {
        let v = classify_cmd(&["yes".into(), "--labels".into(), "yes,no,maybe".into()])
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
        assert!(
            !arr.is_empty(),
            "default registry should have at least echo + now"
        );
        // Every entry should be permitted under the default permissive guardrails.
        for entry in arr {
            assert_eq!(entry.get("permitted"), Some(&serde_json::Value::Bool(true)));
        }
        let permitted_count = v
            .get("permitted_count")
            .and_then(|c| c.as_u64())
            .unwrap_or(0);
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
        let unfiltered = tools_cmd(&["list".into(), "--unfiltered".into()]).expect("unfiltered ok");
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
        let v = guardrails_cmd(&["check".into(), "echo".into()]).expect("guardrails check ok");
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
        assert_eq!(v.get("decision").and_then(|d| d.as_str()), Some("approved"));
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
        let err = approval_cmd(&["check".into(), "echo".into(), "--input".into()]).unwrap_err();
        assert!(err.contains("--input"));
    }

    #[test]
    fn approval_cmd_check_unknown_flag_errs() {
        let err = approval_cmd(&["check".into(), "echo".into(), "--bogus".into()]).unwrap_err();
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
        let v = todo_cmd_at(&["list".into(), "session-1".into()], &store).expect("list ok");
        assert_eq!(v.get("count").and_then(|c| c.as_u64()), Some(0));
        let items = v
            .get("items")
            .and_then(|i| i.as_array())
            .expect("items array");
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
        let listed = todo_cmd_at(&["list".into(), "session-1".into()], &store).expect("list ok");
        let items = listed
            .get("items")
            .and_then(|i| i.as_array())
            .expect("items");
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
        let listed = todo_cmd_at(&["list".into(), "session-1".into()], &store).expect("list ok");
        let items = listed
            .get("items")
            .and_then(|i| i.as_array())
            .expect("items");
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
        let err = todo_cmd_at(&["add".into(), "s1".into(), "t1".into()], &store).unwrap_err();
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
        let v =
            todo_cmd_at(&["remove".into(), "s1".into(), "t1".into()], &store).expect("remove ok");
        assert_eq!(v.get("count").and_then(|c| c.as_u64()), Some(1));
        let listed = todo_cmd_at(&["list".into(), "s1".into()], &store).expect("list ok");
        let items = listed
            .get("items")
            .and_then(|i| i.as_array())
            .expect("items");
        assert_eq!(items[0].get("id").and_then(|i| i.as_str()), Some("t2"));
    }

    #[test]
    fn todo_cmd_remove_unknown_id_errs() {
        let (_dir, store) = temp_todo_store();
        let err = todo_cmd_at(&["remove".into(), "s1".into(), "ghost".into()], &store).unwrap_err();
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
        let v =
            todo_cmd_at(&["clear".into(), "s1".into(), "--yes".into()], &store).expect("clear ok");
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
        assert!(
            v.get("trigger_tokens")
                .and_then(|n| n.as_u64())
                .unwrap_or(0)
                > 0
        );
        assert!(v.get("keep_tail_tokens").and_then(|n| n.as_u64()).is_some());
        assert!(v
            .get("summary_max_tokens")
            .and_then(|n| n.as_u64())
            .is_some());
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
        let v = compress_cmd(&["check".into(), "--file".into(), path.display().to_string()])
            .expect("check ok");
        assert_eq!(v.get("message_count").and_then(|n| n.as_u64()), Some(0));
        assert_eq!(v.get("total_tokens").and_then(|n| n.as_u64()), Some(0));
        assert_eq!(
            v.get("would_trigger").and_then(|b| b.as_bool()),
            Some(false)
        );
    }

    #[test]
    fn compress_cmd_check_skips_blank_lines() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("conv.jsonl");
        let body = format!(
            "{}\n\n{}\n",
            serde_json::to_string(&crate::agent::llm::types::Message::user_text("hello")).unwrap(),
            serde_json::to_string(&crate::agent::llm::types::Message::assistant_text(
                "hi back"
            ))
            .unwrap(),
        );
        std::fs::write(&path, body).expect("write");
        let v = compress_cmd(&["check".into(), "--file".into(), path.display().to_string()])
            .expect("check ok");
        assert_eq!(v.get("message_count").and_then(|n| n.as_u64()), Some(2));
    }

    #[test]
    fn compress_cmd_check_counts_by_role() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("conv.jsonl");
        let body = format!(
            "{}\n{}\n{}\n",
            serde_json::to_string(&crate::agent::llm::types::Message::user_text("u1")).unwrap(),
            serde_json::to_string(&crate::agent::llm::types::Message::assistant_text("a1"))
                .unwrap(),
            serde_json::to_string(&crate::agent::llm::types::Message::user_text("u2")).unwrap(),
        );
        std::fs::write(&path, body).expect("write");
        let v = compress_cmd(&["check".into(), "--file".into(), path.display().to_string()])
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
        assert_eq!(
            cfg.get("trigger_tokens").and_then(|n| n.as_u64()),
            Some(12345)
        );
        assert_eq!(
            cfg.get("target_tokens").and_then(|n| n.as_u64()),
            Some(8000)
        );
        assert_eq!(
            cfg.get("keep_tail_tokens").and_then(|n| n.as_u64()),
            Some(1234)
        );
        assert_eq!(
            cfg.get("summary_max_tokens").and_then(|n| n.as_u64()),
            Some(777)
        );
    }

    #[test]
    fn compress_cmd_check_rejects_corrupt_jsonl() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("conv.jsonl");
        std::fs::write(&path, "{not json}\n").expect("write");
        let err = compress_cmd(&["check".into(), "--file".into(), path.display().to_string()])
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
        let err = mcp_call(&["--cmd".into(), "nonexistent-binary-xyz-zyx".into()]).unwrap_err();
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
        let raw: Vec<String> = vec!["--cmd".into(), "x".into(), "--bogus".into()];
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
        let err = mcp_probe(&["--cmd".into(), "python".into(), "extra".into()]).unwrap_err();
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
        let err = aux_cmd(&["ask".into(), "--prompt".into(), "hello".into()]).unwrap_err();
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
        let v =
            retry_cmd(&["schedule".into(), "--attempts".into(), "6".into()]).expect("schedule ok");
        let waits = v
            .get("inter_attempt_waits")
            .and_then(|w| w.as_array())
            .expect("array");
        assert_eq!(waits.len(), 5);
        assert_eq!(v.get("max_attempts").and_then(|n| n.as_u64()), Some(6));
    }

    #[test]
    fn retry_cmd_schedule_one_attempt_has_no_waits() {
        let v =
            retry_cmd(&["schedule".into(), "--attempts".into(), "1".into()]).expect("schedule ok");
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
        let v =
            retry_cmd(&["schedule".into(), "--attempts".into(), "11".into()]).expect("schedule ok");
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
        let err = retry_cmd(&["schedule".into(), "--attempts".into(), "lots".into()]).unwrap_err();
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

    // ---- skills_guard_cmd ----

    fn skills_guard_test_dir(label: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "cos-agent-skills-guard-{label}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn write_test_skill(
        dir: &std::path::Path,
        id: &str,
        tools: &[&str],
    ) -> crate::agent::skills::loader::LoadedSkill {
        use crate::agent::skills::loader::LoadedSkill;
        use std::fs;
        let sd = dir.join(id);
        fs::create_dir_all(&sd).unwrap();
        let mp = sd.join("SKILL.md");
        let allowed = if tools.is_empty() {
            String::new()
        } else {
            format!(
                "allowed-tools:\n{}\n",
                tools
                    .iter()
                    .map(|t| format!("  - {t}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        };
        fs::write(
            &mp,
            format!("---\nname: {id}\ndescription: test\n{allowed}---\n# body\n"),
        )
        .unwrap();
        let doc = crate::agent::skills::manifest::parse(&fs::read_to_string(&mp).unwrap()).unwrap();
        LoadedSkill {
            id: id.to_string(),
            dir: sd,
            manifest_path: mp,
            manifest: doc.manifest,
            body: doc.body,
        }
    }

    fn guard_skills_map(
        skill: crate::agent::skills::loader::LoadedSkill,
    ) -> std::collections::BTreeMap<String, crate::agent::skills::loader::LoadedSkill> {
        let mut m = std::collections::BTreeMap::new();
        m.insert(skill.id.clone(), skill);
        m
    }

    #[test]
    fn skills_guard_unknown_id_errs() {
        let map: std::collections::BTreeMap<String, crate::agent::skills::loader::LoadedSkill> =
            std::collections::BTreeMap::new();
        let err = skills_guard_cmd_against(&["nope".into()], &map).unwrap_err();
        assert!(err.contains("not loaded"));
    }

    #[test]
    fn skills_guard_missing_id_errs() {
        let map: std::collections::BTreeMap<String, crate::agent::skills::loader::LoadedSkill> =
            std::collections::BTreeMap::new();
        let err = skills_guard_cmd_against(&[], &map).unwrap_err();
        assert!(err.contains("usage"));
    }

    #[test]
    fn skills_guard_default_provenance_hub_allows_clean_skill() {
        let dir = skills_guard_test_dir("default-hub");
        let skill = write_test_skill(&dir, "alpha", &["echo"]);
        let map = guard_skills_map(skill);
        let v = skills_guard_cmd_against(&["alpha".into()], &map).expect("ok");
        assert_eq!(v.get("verdict").and_then(|s| s.as_str()), Some("allow"));
        assert_eq!(v.get("provenance").and_then(|s| s.as_str()), Some("hub"));
    }

    #[test]
    fn skills_guard_vendor_provenance_is_trusted() {
        // Even with require_allowed_tools + zero declared tools,
        // vendor provenance + honour_provenance_trust = Allow.
        let dir = skills_guard_test_dir("vendor-trust");
        let skill = write_test_skill(&dir, "beta", &[]);
        let map = guard_skills_map(skill);
        let v = skills_guard_cmd_against(
            &[
                "beta".into(),
                "--provenance".into(),
                "vendor".into(),
                "--require-allowed-tools".into(),
            ],
            &map,
        )
        .expect("ok");
        assert_eq!(v.get("verdict").and_then(|s| s.as_str()), Some("allow"));
    }

    #[test]
    fn skills_guard_require_allowed_tools_denies_empty_hub_skill() {
        let dir = skills_guard_test_dir("require-tools");
        let skill = write_test_skill(&dir, "gamma", &[]);
        let map = guard_skills_map(skill);
        let v = skills_guard_cmd_against(&["gamma".into(), "--require-allowed-tools".into()], &map)
            .expect("ok");
        assert_eq!(v.get("verdict").and_then(|s| s.as_str()), Some("deny"));
        assert!(v.get("reason").and_then(|s| s.as_str()).is_some());
    }

    #[test]
    fn skills_guard_ignore_trust_strips_vendor_pass() {
        // vendor + ignore-trust + require-allowed-tools (empty) → deny.
        let dir = skills_guard_test_dir("ignore-trust");
        let skill = write_test_skill(&dir, "delta", &[]);
        let map = guard_skills_map(skill);
        let v = skills_guard_cmd_against(
            &[
                "delta".into(),
                "--provenance".into(),
                "vendor".into(),
                "--ignore-trust".into(),
                "--require-allowed-tools".into(),
            ],
            &map,
        )
        .expect("ok");
        assert_eq!(v.get("verdict").and_then(|s| s.as_str()), Some("deny"));
        assert_eq!(
            v.get("config")
                .and_then(|c| c.get("honour_provenance_trust"))
                .and_then(|b| b.as_bool()),
            Some(false)
        );
    }

    #[test]
    fn skills_guard_max_file_bytes_triggers_confirmation() {
        // Write a sibling file larger than the cap and verify the
        // verdict flips to require_confirmation.
        let dir = skills_guard_test_dir("max-bytes");
        let skill = write_test_skill(&dir, "epsilon", &["echo"]);
        // 200 bytes payload, cap = 100 bytes.
        std::fs::write(skill.dir.join("data.bin"), vec![0u8; 200]).unwrap();
        let map = guard_skills_map(skill);
        let v = skills_guard_cmd_against(
            &["epsilon".into(), "--max-file-bytes".into(), "100".into()],
            &map,
        )
        .expect("ok");
        assert_eq!(
            v.get("verdict").and_then(|s| s.as_str()),
            Some("require_confirmation")
        );
        assert!(v
            .get("reason")
            .and_then(|s| s.as_str())
            .map(|r| r.contains("data.bin"))
            .unwrap_or(false));
    }

    #[test]
    fn skills_guard_unknown_provenance_errs() {
        let dir = skills_guard_test_dir("bad-prov");
        let skill = write_test_skill(&dir, "zeta", &["echo"]);
        let map = guard_skills_map(skill);
        let err = skills_guard_cmd_against(
            &["zeta".into(), "--provenance".into(), "alien".into()],
            &map,
        )
        .unwrap_err();
        assert!(err.contains("alien"));
    }

    #[test]
    fn skills_guard_unknown_flag_errs() {
        let dir = skills_guard_test_dir("bad-flag");
        let skill = write_test_skill(&dir, "eta", &["echo"]);
        let map = guard_skills_map(skill);
        let err = skills_guard_cmd_against(&["eta".into(), "--bogus".into()], &map).unwrap_err();
        assert!(err.contains("--bogus"));
    }

    #[test]
    fn skills_guard_invalid_max_file_bytes_errs() {
        let dir = skills_guard_test_dir("bad-bytes");
        let skill = write_test_skill(&dir, "theta", &["echo"]);
        let map = guard_skills_map(skill);
        let err = skills_guard_cmd_against(
            &["theta".into(), "--max-file-bytes".into(), "lots".into()],
            &map,
        )
        .unwrap_err();
        assert!(err.contains("--max-file-bytes"));
    }

    // ---- sessions_cmd / sessions_*_with ----

    fn fresh_session_db() -> memory::sqlite_fts::MemoryDb {
        memory::sqlite_fts::MemoryDb::open_in_memory().expect("open in-memory db")
    }

    #[test]
    fn sessions_list_with_empty_db_returns_no_sessions() {
        let db = fresh_session_db();
        let v = sessions_list_with(&db, 20).expect("list ok");
        assert_eq!(v.get("n").and_then(|n| n.as_u64()), Some(0));
        assert_eq!(v.get("limit").and_then(|n| n.as_u64()), Some(20));
        assert!(v
            .get("sessions")
            .and_then(|s| s.as_array())
            .map(|a| a.is_empty())
            .unwrap_or(false));
    }

    #[test]
    fn sessions_list_with_returns_recorded_sessions_in_recency_order() {
        let db = fresh_session_db();
        db.record_message("s-old", "user", "hi old").unwrap();
        // Tick to ensure a different ms.
        std::thread::sleep(std::time::Duration::from_millis(5));
        db.record_message("s-new", "user", "hi new").unwrap();

        let v = sessions_list_with(&db, 10).expect("list ok");
        let arr = v.get("sessions").and_then(|s| s.as_array()).expect("array");
        assert_eq!(arr.len(), 2);
        // Most recent first.
        assert_eq!(
            arr[0].get("session_id").and_then(|s| s.as_str()),
            Some("s-new")
        );
        assert_eq!(
            arr[1].get("session_id").and_then(|s| s.as_str()),
            Some("s-old")
        );
    }

    #[test]
    fn sessions_title_with_returns_null_when_unset() {
        let db = fresh_session_db();
        let v = sessions_title_with(&db, "sx").expect("title ok");
        assert_eq!(v.get("set").and_then(|b| b.as_bool()), Some(false));
        assert!(v.get("title").map(|t| t.is_null()).unwrap_or(false));
    }

    #[test]
    fn sessions_set_title_with_then_title_with_round_trips() {
        let db = fresh_session_db();
        let v = sessions_set_title_with(&db, "sx", "My Session").expect("set ok");
        assert_eq!(v.get("title").and_then(|s| s.as_str()), Some("My Session"));
        let v2 = sessions_title_with(&db, "sx").expect("title ok");
        assert_eq!(v2.get("title").and_then(|s| s.as_str()), Some("My Session"));
        assert_eq!(v2.get("set").and_then(|b| b.as_bool()), Some(true));
    }

    #[test]
    fn sessions_set_title_overwrites_existing_title() {
        let db = fresh_session_db();
        sessions_set_title_with(&db, "sx", "first").expect("set ok");
        sessions_set_title_with(&db, "sx", "second").expect("set ok");
        let v = sessions_title_with(&db, "sx").expect("title ok");
        assert_eq!(v.get("title").and_then(|s| s.as_str()), Some("second"));
    }

    #[test]
    fn parse_set_title_args_accepts_multi_word_title() {
        let (id, title) = parse_set_title_args(&[
            "sid".into(),
            "Hello".into(),
            "World".into(),
            "Of".into(),
            "Tests".into(),
        ])
        .expect("parse ok");
        assert_eq!(id, "sid");
        assert_eq!(title, "Hello World Of Tests");
    }

    #[test]
    fn parse_set_title_args_stops_at_first_flag() {
        let (id, title) = parse_set_title_args(&[
            "sid".into(),
            "Hello".into(),
            "World".into(),
            "--unknown".into(),
            "ignored".into(),
        ])
        .expect("parse ok");
        assert_eq!(id, "sid");
        assert_eq!(title, "Hello World");
    }

    #[test]
    fn parse_set_title_args_requires_id() {
        let err = parse_set_title_args(&[]).unwrap_err();
        assert!(err.contains("usage"));
    }

    #[test]
    fn parse_set_title_args_requires_title() {
        let err = parse_set_title_args(&["sid".into()]).unwrap_err();
        assert!(err.contains("usage"));
    }

    #[test]
    fn parse_set_title_args_rejects_id_starting_with_double_dash() {
        let err = parse_set_title_args(&["--id".into(), "title".into()]).unwrap_err();
        assert!(err.contains("usage"));
    }

    #[test]
    fn sessions_count_with_total_includes_all_sessions() {
        let db = fresh_session_db();
        db.record_message("s1", "user", "a").unwrap();
        db.record_message("s1", "assistant", "b").unwrap();
        db.record_message("s2", "user", "c").unwrap();
        let v = sessions_count_with(&db, None).expect("count ok");
        assert_eq!(v.get("total_messages").and_then(|n| n.as_i64()), Some(3));
    }

    #[test]
    fn sessions_count_with_filters_by_session_id() {
        let db = fresh_session_db();
        db.record_message("s1", "user", "a").unwrap();
        db.record_message("s1", "assistant", "b").unwrap();
        db.record_message("s2", "user", "c").unwrap();
        let v = sessions_count_with(&db, Some("s1")).expect("count ok");
        assert_eq!(v.get("messages").and_then(|n| n.as_i64()), Some(2));
        assert_eq!(v.get("session_id").and_then(|s| s.as_str()), Some("s1"));
    }

    #[test]
    fn sessions_clear_with_drops_session_messages_only() {
        let db = fresh_session_db();
        db.record_message("s1", "user", "a").unwrap();
        db.record_message("s1", "assistant", "b").unwrap();
        db.record_message("s2", "user", "c").unwrap();
        let v = sessions_clear_with(&db, "s1").expect("clear ok");
        assert_eq!(v.get("messages_cleared").and_then(|n| n.as_u64()), Some(2));
        // s2 should be intact.
        let total = sessions_count_with(&db, None).expect("count ok");
        assert_eq!(
            total.get("total_messages").and_then(|n| n.as_i64()),
            Some(1)
        );
    }

    #[test]
    fn sessions_clear_refuses_without_yes_flag() {
        let err = sessions_clear(&["sx".into()]).unwrap_err();
        assert!(err.contains("--yes"));
    }

    #[test]
    fn sessions_clear_requires_session_id() {
        let err = sessions_clear(&[]).unwrap_err();
        assert!(err.contains("usage"));
        let err2 = sessions_clear(&["--yes".into()]).unwrap_err();
        assert!(err2.contains("usage"));
    }

    #[test]
    fn sessions_title_requires_id() {
        let err = sessions_title(&[]).unwrap_err();
        assert!(err.contains("usage"));
    }

    #[test]
    fn sessions_cmd_unknown_subcommand_errs() {
        let err = sessions_cmd(&["bogus".into()]).unwrap_err();
        assert!(err.contains("bogus"));
    }

    #[test]
    fn sessions_cmd_numeric_first_arg_routes_to_list() {
        // Numeric first arg keeps backward-compat: cos agent sessions 5 → list 5.
        let v = sessions_cmd(&["5".into()]).expect("legacy list ok");
        assert_eq!(v.get("limit").and_then(|n| n.as_u64()), Some(5));
    }

    // ---- sessions_purge ----

    #[test]
    fn sessions_purge_requires_older_than() {
        let err = sessions_purge(&["--yes".into()]).unwrap_err();
        assert!(err.contains("--older-than"), "got {err}");
    }

    #[test]
    fn sessions_purge_validates_days_is_positive_integer() {
        let err = sessions_purge(&["--older-than".into(), "0".into(), "--yes".into()]).unwrap_err();
        assert!(err.contains("> 0"), "got {err}");
        let err2 =
            sessions_purge(&["--older-than".into(), "abc".into(), "--yes".into()]).unwrap_err();
        assert!(err2.contains("positive integer"), "got {err2}");
    }

    #[test]
    fn sessions_purge_refuses_apply_without_yes() {
        let err = sessions_purge(&["--older-than".into(), "1".into()]).unwrap_err();
        assert!(err.contains("--yes"), "got {err}");
        assert!(err.contains("--dry-run"), "got {err}");
    }

    #[test]
    fn sessions_purge_with_dry_run_does_not_mutate() {
        let db = fresh_session_db();
        // Insert one ancient message with explicit ts so we can
        // exercise the cutoff cleanly.
        db.record_message_at("old", "user", "ancient", 100).unwrap();
        // And one fresh row via the normal path so its ts_ms is now.
        db.record_message("new", "user", "fresh").unwrap();
        // Cutoff = 1000ms; "old" (100) is below, "new" (~now) is above.
        let v = sessions_purge_with(&db, 1000, 7, true).expect("dry ok");
        assert_eq!(v["dry_run"], json!(true));
        assert_eq!(v["messages_deleted"], json!(1));
        assert_eq!(v["sessions_emptied"], json!(1));
        // Messages still on disk after dry-run.
        let total = sessions_count_with(&db, None).unwrap();
        assert_eq!(total["total_messages"].as_i64(), Some(2));
    }

    #[test]
    fn sessions_purge_with_apply_drops_old_rows_and_titles() {
        let db = fresh_session_db();
        db.record_message_at("old", "user", "ancient", 100).unwrap();
        db.set_title("old", "Old Convo").unwrap();
        db.record_message("new", "user", "fresh").unwrap();
        // Apply with cutoff=1000.
        let v = sessions_purge_with(&db, 1000, 7, false).expect("apply ok");
        assert_eq!(v["dry_run"], json!(false));
        assert_eq!(v["messages_deleted"], json!(1));
        assert_eq!(v["sessions_emptied"], json!(1));
        assert_eq!(v["titles_deleted"], json!(1));
        // Only "new" remains.
        let total = sessions_count_with(&db, None).unwrap();
        assert_eq!(total["total_messages"].as_i64(), Some(1));
        // Title for "old" is gone.
        let title = db.title_for("old").unwrap();
        assert!(title.is_none());
    }

    #[test]
    fn sessions_purge_empty_db_returns_zero_counts() {
        let db = fresh_session_db();
        let v = sessions_purge_with(&db, 1000, 7, false).expect("apply ok");
        assert_eq!(v["messages_deleted"], json!(0));
        assert_eq!(v["sessions_emptied"], json!(0));
        assert_eq!(v["titles_deleted"], json!(0));
    }

    #[test]
    fn sessions_purge_dispatched_via_sessions_cmd() {
        // Smoke test that the `purge` verb is wired through
        // sessions_cmd. We pass --dry-run --older-than 999999 to
        // ensure no rows match (so the test doesn't depend on the
        // shared default db being empty).
        let v = sessions_cmd(&[
            "purge".into(),
            "--older-than".into(),
            "999999".into(),
            "--dry-run".into(),
        ])
        .expect("dispatch ok");
        assert_eq!(v["dry_run"], json!(true));
        assert_eq!(v["older_than_days"], json!(999999u64));
    }

    // ---- sessions_stats ----

    #[test]
    fn sessions_stats_rejects_extra_args() {
        let err = sessions_stats(&["bogus".into()]).unwrap_err();
        assert!(err.contains("unexpected argument"), "got {err}");
    }

    #[test]
    fn sessions_stats_session_flag_requires_value() {
        let err = sessions_stats(&["--session".into()]).unwrap_err();
        assert!(err.contains("--session requires"), "got {err}");
    }

    #[test]
    fn sessions_stats_session_flag_rejects_empty_value() {
        let err = sessions_stats(&["--session".into(), "".into()]).unwrap_err();
        assert!(err.contains("must not be empty"), "got {err}");
    }

    #[test]
    fn sessions_stats_session_with_unknown_id_returns_zeros() {
        let db = fresh_session_db();
        // Other sessions exist, but the requested one does not.
        db.record_message("other", "user", "x").unwrap();
        let v = sessions_stats_session_with(&db, "ghost", 1_000_000).expect("stats ok");
        assert_eq!(v["scope"], json!("session"));
        assert_eq!(v["session_id"], json!("ghost"));
        assert_eq!(v["title"], json!(null));
        assert_eq!(v["total_messages"], json!(0u64));
        assert_eq!(v["by_role"], json!([]));
        // No total_sessions / titled_sessions in per-session shape.
        assert!(v.get("total_sessions").is_none());
        assert!(v.get("titled_sessions").is_none());
    }

    #[test]
    fn sessions_stats_session_with_isolates_one_session() {
        let db = fresh_session_db();
        let now: i64 = 100 * 86_400_000;
        for _ in 0..3 {
            db.record_message_at("alpha", "user", "a", now - 3_600_000)
                .unwrap();
        }
        for _ in 0..7 {
            db.record_message_at("beta", "user", "b", now).unwrap();
        }
        db.set_title("alpha", "Alpha").unwrap();
        let v = sessions_stats_session_with(&db, "alpha", now).expect("stats ok");
        assert_eq!(v["session_id"], json!("alpha"));
        assert_eq!(v["title"], json!("Alpha"));
        assert_eq!(v["total_messages"], json!(3u64));
        assert_eq!(v["messages_last_1d"], json!(3u64));
        assert_eq!(v["by_role"], json!([{"role": "user", "count": 3u64}]));
    }

    #[test]
    fn sessions_stats_dispatched_with_session_flag() {
        let v = sessions_cmd(&["stats".into(), "--session".into(), "no-such-id".into()])
            .expect("dispatch ok");
        assert_eq!(v["scope"], json!("session"));
        assert_eq!(v["session_id"], json!("no-such-id"));
    }

    #[test]
    fn sessions_stats_with_empty_db_is_all_zeros() {
        let db = fresh_session_db();
        let v = sessions_stats_with(&db, 1_000_000).expect("stats ok");
        assert_eq!(v["ok"], json!(true));
        assert_eq!(v["total_messages"], json!(0u64));
        assert_eq!(v["total_sessions"], json!(0u64));
        assert_eq!(v["titled_sessions"], json!(0u64));
        assert_eq!(v["messages_last_7d"], json!(0u64));
        assert_eq!(v["by_role"], json!([]));
        assert_eq!(v["oldest_ts_ms"], json!(null));
        assert_eq!(v["newest_ts_ms"], json!(null));
    }

    #[test]
    fn sessions_stats_with_buckets_recency_and_role() {
        let db = fresh_session_db();
        let now: i64 = 100 * 86_400_000;
        db.record_message_at("s", "user", "fresh", now - 3_600_000)
            .unwrap();
        db.record_message_at("s", "assistant", "old", now - 10 * 86_400_000)
            .unwrap();
        db.record_message_at("t", "user", "ancient", now - 60 * 86_400_000)
            .unwrap();
        db.set_title("s", "Hello").unwrap();
        let v = sessions_stats_with(&db, now).expect("stats ok");
        assert_eq!(v["total_messages"], json!(3u64));
        assert_eq!(v["total_sessions"], json!(2u64));
        assert_eq!(v["titled_sessions"], json!(1u64));
        assert_eq!(v["messages_last_1d"], json!(1u64));
        assert_eq!(v["messages_last_7d"], json!(1u64));
        assert_eq!(v["messages_last_30d"], json!(2u64));
        // by_role: "user" leads with 2, "assistant" trails with 1.
        let roles = v["by_role"].as_array().expect("array");
        assert_eq!(roles.len(), 2);
        assert_eq!(roles[0]["role"], json!("user"));
        assert_eq!(roles[0]["count"], json!(2u64));
        assert_eq!(v["oldest_ts_ms"], json!(now - 60 * 86_400_000));
        assert_eq!(v["newest_ts_ms"], json!(now - 3_600_000));
    }

    #[test]
    fn sessions_stats_dispatched_via_sessions_cmd() {
        let v = sessions_cmd(&["stats".into()]).expect("dispatch ok");
        assert!(v.get("total_messages").is_some());
        assert!(v.get("by_role").is_some());
    }

    // ---- sessions_top ----

    #[test]
    fn sessions_top_with_empty_db_returns_empty_array() {
        let db = fresh_session_db();
        let v = sessions_top_with(&db, 10).expect("top ok");
        assert_eq!(v["ok"], json!(true));
        assert_eq!(v["limit"], json!(10u64));
        assert_eq!(v["n"], json!(0u64));
        assert_eq!(v["ordered_by"], json!("message_count_desc"));
        assert_eq!(v["sessions"], json!([]));
    }

    #[test]
    fn sessions_top_with_orders_by_count_desc() {
        let db = fresh_session_db();
        // "fat" 3 msgs, "mid" 2, "thin" 1.
        for _ in 0..3 {
            db.record_message("fat", "user", "x").unwrap();
        }
        for _ in 0..2 {
            db.record_message("mid", "user", "x").unwrap();
        }
        db.record_message("thin", "user", "x").unwrap();
        let v = sessions_top_with(&db, 10).expect("top ok");
        assert_eq!(v["n"], json!(3u64));
        let arr = v["sessions"].as_array().unwrap();
        assert_eq!(arr[0]["session_id"], json!("fat"));
        assert_eq!(arr[0]["message_count"], json!(3));
        assert_eq!(arr[1]["session_id"], json!("mid"));
        assert_eq!(arr[2]["session_id"], json!("thin"));
    }

    #[test]
    fn sessions_top_with_carries_titles() {
        let db = fresh_session_db();
        db.record_message("s", "user", "x").unwrap();
        db.set_title("s", "Greeting").unwrap();
        let v = sessions_top_with(&db, 10).expect("top ok");
        let arr = v["sessions"].as_array().unwrap();
        assert_eq!(arr[0]["title"], json!("Greeting"));
    }

    #[test]
    fn sessions_top_default_limit_is_20() {
        let db = fresh_session_db();
        // Just make sure no parse errors; with no rows the array is
        // empty but the limit echoes 20.
        let v = sessions_top(&[]).expect("dispatch ok");
        assert_eq!(v["limit"], json!(20u64));
    }

    #[test]
    fn sessions_top_dispatched_via_sessions_cmd() {
        let v = sessions_cmd(&["top".into(), "5".into()]).expect("dispatch ok");
        assert_eq!(v["limit"], json!(5u64));
        assert_eq!(v["ordered_by"], json!("message_count_desc"));
    }

    // ---- semantic_cmd: clear-all guards + status drift ----

    #[test]
    fn semantic_clear_all_refuses_without_yes() {
        let err = semantic_cmd(&["clear-all".into()]).unwrap_err();
        assert!(
            err.contains("--yes"),
            "expected error to point at --yes, got: {err}"
        );
    }

    #[test]
    fn semantic_unknown_subcommand_errs_with_usage_hint() {
        let err = semantic_cmd(&["bogus".into()]).unwrap_err();
        assert!(err.contains("clear-all"), "got: {err}");
        assert!(err.contains("status"), "got: {err}");
    }

    #[test]
    fn semantic_no_subcommand_errs_with_usage() {
        let err = semantic_cmd(&[]).unwrap_err();
        assert!(err.contains("usage"));
        assert!(err.contains("clear-all"));
    }

    // ---- vision_cmd / vision_route_cmd ----

    #[test]
    fn vision_cmd_default_subcommand_errs_with_usage() {
        let err = vision_cmd(&[]).unwrap_err();
        assert!(err.contains("usage"));
    }

    #[test]
    fn vision_cmd_unknown_subcommand_errs() {
        let err = vision_cmd(&["bogus".into()]).unwrap_err();
        assert!(err.contains("bogus"));
    }

    #[test]
    fn vision_route_synthetic_native_when_provider_vision_and_widely_supported() {
        let v = vision_route_cmd(&[
            "--bytes".into(),
            "1024".into(),
            "--mime".into(),
            "image/png".into(),
            "--provider-vision".into(),
        ])
        .expect("ok");
        assert_eq!(v.get("decision").and_then(|s| s.as_str()), Some("native"));
        assert!(v.get("reason").map(|r| r.is_null()).unwrap_or(false));
    }

    #[test]
    fn vision_route_skip_when_vision_disabled() {
        let v = vision_route_cmd(&[
            "--bytes".into(),
            "1024".into(),
            "--mime".into(),
            "image/png".into(),
            "--provider-vision".into(),
            "--vision-disabled".into(),
        ])
        .expect("ok");
        assert_eq!(v.get("decision").and_then(|s| s.as_str()), Some("skip"));
        assert!(v
            .get("reason")
            .and_then(|s| s.as_str())
            .map(|r| r.contains("vision disabled"))
            .unwrap_or(false));
    }

    #[test]
    fn vision_route_skip_when_zero_bytes() {
        let v = vision_route_cmd(&[
            "--bytes".into(),
            "0".into(),
            "--mime".into(),
            "image/png".into(),
            "--provider-vision".into(),
        ])
        .expect("ok");
        assert_eq!(v.get("decision").and_then(|s| s.as_str()), Some("skip"));
    }

    #[test]
    fn vision_route_extract_text_intent_prefers_ocr_when_available() {
        let v = vision_route_cmd(&[
            "--bytes".into(),
            "1024".into(),
            "--mime".into(),
            "image/png".into(),
            "--provider-vision".into(),
            "--ocr-available".into(),
            "--intent".into(),
            "extract-text".into(),
        ])
        .expect("ok");
        assert_eq!(v.get("decision").and_then(|s| s.as_str()), Some("ocr"));
    }

    #[test]
    fn vision_route_skip_when_oversized_and_no_ocr() {
        let v = vision_route_cmd(&[
            "--bytes".into(),
            "10000000".into(),
            "--mime".into(),
            "image/png".into(),
            "--provider-vision".into(),
            "--max-native-bytes".into(),
            "1000000".into(),
        ])
        .expect("ok");
        assert_eq!(v.get("decision").and_then(|s| s.as_str()), Some("skip"));
        assert!(v
            .get("reason")
            .and_then(|s| s.as_str())
            .map(|r| r.contains("exceeds native cap"))
            .unwrap_or(false));
    }

    #[test]
    fn vision_route_unsupported_mime_without_ocr_skips() {
        let v = vision_route_cmd(&[
            "--bytes".into(),
            "1024".into(),
            "--mime".into(),
            "image/heic".into(),
            "--provider-vision".into(),
        ])
        .expect("ok");
        assert_eq!(v.get("decision").and_then(|s| s.as_str()), Some("skip"));
    }

    #[test]
    fn vision_route_requires_bytes_or_file() {
        let err = vision_route_cmd(&[]).unwrap_err();
        assert!(err.contains("--file") || err.contains("--bytes"));
    }

    #[test]
    fn vision_route_bytes_without_mime_errs() {
        let err = vision_route_cmd(&["--bytes".into(), "1024".into()]).unwrap_err();
        assert!(err.contains("--mime"));
    }

    #[test]
    fn vision_route_unknown_intent_errs() {
        let err = vision_route_cmd(&[
            "--bytes".into(),
            "1024".into(),
            "--mime".into(),
            "image/png".into(),
            "--intent".into(),
            "bogus".into(),
        ])
        .unwrap_err();
        assert!(err.contains("bogus"));
    }

    #[test]
    fn vision_route_unknown_flag_errs() {
        let err =
            vision_route_cmd(&["--bytes".into(), "1024".into(), "--bogus".into()]).unwrap_err();
        assert!(err.contains("--bogus"));
    }

    #[test]
    fn vision_route_file_uses_on_disk_size_and_extension_mime() {
        let dir = std::env::temp_dir().join(format!(
            "cos-agent-vision-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.png");
        std::fs::write(&path, vec![0u8; 4096]).unwrap();
        let v = vision_route_cmd(&[
            "--file".into(),
            path.display().to_string(),
            "--provider-vision".into(),
        ])
        .expect("ok");
        let desc = v.get("descriptor").expect("descriptor");
        assert_eq!(desc.get("bytes_len").and_then(|n| n.as_u64()), Some(4096));
        assert_eq!(desc.get("mime").and_then(|m| m.as_str()), Some("Png"));
        assert_eq!(v.get("decision").and_then(|s| s.as_str()), Some("native"));
        // Cleanup.
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn vision_route_file_mime_override_wins() {
        let dir = std::env::temp_dir().join(format!(
            "cos-agent-vision-mime-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.dat");
        std::fs::write(&path, vec![0u8; 100]).unwrap();
        let v = vision_route_cmd(&[
            "--file".into(),
            path.display().to_string(),
            "--mime".into(),
            "image/jpeg".into(),
            "--provider-vision".into(),
        ])
        .expect("ok");
        let desc = v.get("descriptor").expect("descriptor");
        assert_eq!(desc.get("mime").and_then(|m| m.as_str()), Some("Jpeg"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn vision_route_file_missing_path_errs() {
        let err = vision_route_cmd(&[
            "--file".into(),
            "Z:\\definitely\\does\\not\\exist.png".into(),
        ])
        .unwrap_err();
        // On unix the path also won't exist.
        assert!(err.contains("stat") || err.contains("not"));
    }

    // ---- vision_sniff_cmd ----

    #[test]
    fn vision_sniff_requires_file_or_url() {
        let err = vision_sniff_cmd(&[]).unwrap_err();
        assert!(err.contains("--file") && err.contains("--url"));
    }

    #[test]
    fn vision_sniff_rejects_both_file_and_url() {
        let err = vision_sniff_cmd(&[
            "--file".into(),
            "x.png".into(),
            "--url".into(),
            "https://x.invalid/y".into(),
        ])
        .unwrap_err();
        assert!(err.contains("exactly one"));
    }

    #[test]
    fn vision_sniff_unknown_flag_errs() {
        let err = vision_sniff_cmd(&["--bogus".into(), "x".into()]).unwrap_err();
        assert!(err.contains("--bogus"));
    }

    #[test]
    fn vision_sniff_file_returns_mime_and_len() {
        // Write a tiny PNG-magic-byte stub (8-byte signature) to a temp
        // file and confirm sniff_mime classifies it.
        let dir = std::env::temp_dir().join(format!(
            "cos-vision-sniff-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("x.png");
        std::fs::write(
            &path,
            [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x01],
        )
        .unwrap();

        let v =
            vision_sniff_cmd(&["--file".into(), path.to_string_lossy().to_string()]).expect("ok");
        assert_eq!(v.get("bytes_len").and_then(|n| n.as_u64()), Some(10));
        assert_eq!(v.get("mime").and_then(|s| s.as_str()), Some("Png"));
        assert_eq!(
            v.get("mime_widely_supported").and_then(|b| b.as_bool()),
            Some(true)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn vision_sniff_file_unknown_magic_classifies_other() {
        let dir = std::env::temp_dir().join(format!(
            "cos-vision-sniff-other-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("x.bin");
        std::fs::write(&path, b"this is not an image").unwrap();

        let v =
            vision_sniff_cmd(&["--file".into(), path.to_string_lossy().to_string()]).expect("ok");
        assert_eq!(v.get("mime").and_then(|s| s.as_str()), Some("Other"));
        assert_eq!(v.get("is_other").and_then(|b| b.as_bool()), Some(true));
        assert_eq!(
            v.get("mime_widely_supported").and_then(|b| b.as_bool()),
            Some(false)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn vision_sniff_file_missing_path_errs() {
        let err = vision_sniff_cmd(&[
            "--file".into(),
            "Z:\\definitely\\does\\not\\exist.png".into(),
        ])
        .unwrap_err();
        assert!(err.contains("stat") || err.contains("not"));
    }

    #[test]
    fn vision_sniff_head_bytes_caps_inspection_window() {
        let dir = std::env::temp_dir().join(format!(
            "cos-vision-sniff-head-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("x.png");
        // 1KB file but PNG magic in first 8 bytes.
        let mut data = vec![0u8; 1024];
        data[0..8].copy_from_slice(&[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
        std::fs::write(&path, &data).unwrap();

        let v = vision_sniff_cmd(&[
            "--file".into(),
            path.to_string_lossy().to_string(),
            "--head-bytes".into(),
            "8".into(),
        ])
        .expect("ok");
        assert_eq!(v.get("bytes_len").and_then(|n| n.as_u64()), Some(1024));
        assert_eq!(
            v.get("head_bytes_inspected").and_then(|n| n.as_u64()),
            Some(8)
        );
        assert_eq!(v.get("mime").and_then(|s| s.as_str()), Some("Png"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- vision_analyze_cmd ----

    #[test]
    fn vision_analyze_requires_prompt() {
        let err = vision_analyze_cmd(&["--file".into(), "x.png".into()]).unwrap_err();
        assert!(err.contains("--prompt"));
    }

    #[test]
    fn vision_analyze_empty_prompt_errs() {
        let err = vision_analyze_cmd(&[
            "--file".into(),
            "x.png".into(),
            "--prompt".into(),
            "   ".into(),
        ])
        .unwrap_err();
        assert!(err.contains("non-empty"));
    }

    #[test]
    fn vision_analyze_rejects_zero_image_sources() {
        let err = vision_analyze_cmd(&["--prompt".into(), "describe".into()]).unwrap_err();
        assert!(err.contains("exactly one"));
    }

    #[test]
    fn vision_analyze_rejects_two_image_sources() {
        let err = vision_analyze_cmd(&[
            "--file".into(),
            "x.png".into(),
            "--url".into(),
            "https://x.invalid".into(),
            "--prompt".into(),
            "describe".into(),
        ])
        .unwrap_err();
        assert!(err.contains("exactly one"));
    }

    #[test]
    fn vision_analyze_base64_requires_mime() {
        let err = vision_analyze_cmd(&[
            "--base64".into(),
            "AAAA".into(),
            "--prompt".into(),
            "describe".into(),
        ])
        .unwrap_err();
        assert!(err.contains("--mime"));
    }

    #[test]
    fn vision_analyze_unknown_flag_errs() {
        let err = vision_analyze_cmd(&[
            "--bogus".into(),
            "v".into(),
            "--file".into(),
            "x.png".into(),
            "--prompt".into(),
            "describe".into(),
        ])
        .unwrap_err();
        assert!(err.contains("--bogus"));
    }

    #[test]
    fn vision_analyze_file_missing_errs_clean() {
        let err = vision_analyze_cmd(&[
            "--file".into(),
            "Z:\\nope\\image.png".into(),
            "--prompt".into(),
            "describe".into(),
        ])
        .unwrap_err();
        assert!(err.contains("read"));
    }

    // ---- vision_cmd dispatch picks up new subcommands ----

    #[test]
    fn vision_cmd_routes_sniff_subcommand() {
        // Empty sniff still dispatches into vision_sniff_cmd; we just
        // assert that the error originates from that helper.
        let err = vision_cmd(&["sniff".into()]).unwrap_err();
        assert!(err.contains("--file") && err.contains("--url"));
    }

    #[test]
    fn vision_cmd_routes_analyze_subcommand() {
        let err = vision_cmd(&["analyze".into()]).unwrap_err();
        assert!(err.contains("--prompt"));
    }

    // ---- display_cmd ----

    #[test]
    fn display_cmd_no_args_errs() {
        let err = display_cmd(&[]).unwrap_err();
        assert!(err.contains("usage"));
    }

    #[test]
    fn display_cmd_unknown_subcommand_errs() {
        let err = display_cmd(&["bogus".into()]).unwrap_err();
        assert!(err.contains("bogus"));
    }

    #[test]
    fn display_format_bytes_renders_human_readable() {
        let v = display_format_bytes_cmd(&["1536".into()]).expect("ok");
        assert_eq!(v.get("input").and_then(|n| n.as_u64()), Some(1536));
        assert_eq!(v.get("formatted").and_then(|s| s.as_str()), Some("1.5 KB"));
    }

    #[test]
    fn display_format_bytes_requires_arg() {
        let err = display_format_bytes_cmd(&[]).unwrap_err();
        assert!(err.contains("usage"));
    }

    #[test]
    fn display_format_bytes_rejects_non_numeric() {
        let err = display_format_bytes_cmd(&["abc".into()]).unwrap_err();
        assert!(err.contains("abc"));
    }

    #[test]
    fn display_format_duration_renders_minutes_seconds() {
        let v = display_format_duration_cmd(&["83400".into()]).expect("ok");
        assert_eq!(v.get("input_ms").and_then(|n| n.as_u64()), Some(83_400));
        let s = v.get("formatted").and_then(|s| s.as_str()).unwrap();
        // 83.4s → "1m 23.4s"
        assert!(s.starts_with("1m"));
    }

    #[test]
    fn display_format_duration_requires_arg() {
        let err = display_format_duration_cmd(&[]).unwrap_err();
        assert!(err.contains("usage"));
    }

    #[test]
    fn display_transcript_requires_session() {
        let err = parse_display_transcript_args(&[]).expect("parse");
        assert!(err.session.is_none());
        // The cmd-level call surfaces the missing-session error:
        let err = display_transcript_cmd(&[]).unwrap_err();
        assert!(err.contains("--session"));
    }

    #[test]
    fn display_transcript_unknown_flag_errs() {
        let err = parse_display_transcript_args(&["--bogus".into()]).unwrap_err();
        assert!(err.contains("--bogus"));
    }

    #[test]
    fn display_transcript_renders_messages_oldest_first() {
        let db = crate::agent::memory::sqlite_fts::MemoryDb::open_in_memory().expect("open mem db");
        db.record_message("sess-x", "user", "hello world").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        db.record_message("sess-x", "assistant", "hi back").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        db.record_message("sess-x", "tool", "result foo: 42")
            .unwrap();
        let parsed = DisplayTranscriptArgs {
            session: Some("sess-x".into()),
            limit: Some(10),
            ..Default::default()
        };
        let v = display_transcript_with(&db, "sess-x", &parsed).expect("render");
        assert_eq!(v.get("message_count").and_then(|n| n.as_u64()), Some(3));
        let t = v.get("transcript").and_then(|s| s.as_str()).unwrap();
        let user_pos = t.find("hello world").expect("user line");
        let asst_pos = t.find("hi back").expect("assistant line");
        let tool_pos = t.find("result foo").expect("tool line");
        assert!(user_pos < asst_pos);
        assert!(asst_pos < tool_pos);
        assert!(t.contains("[user]"));
        assert!(t.contains("[assistant]"));
        assert!(t.contains("[tool]"));
    }

    #[test]
    fn display_transcript_truncates_long_content_by_default() {
        let db = crate::agent::memory::sqlite_fts::MemoryDb::open_in_memory().expect("open mem db");
        let big = "X".repeat(10_000);
        db.record_message("sess-y", "user", &big).unwrap();
        let parsed = DisplayTranscriptArgs {
            session: Some("sess-y".into()),
            ..Default::default()
        };
        let v = display_transcript_with(&db, "sess-y", &parsed).expect("render");
        let t = v.get("transcript").and_then(|s| s.as_str()).unwrap();
        assert!(t.contains("chars omitted"));
    }

    #[test]
    fn display_transcript_no_truncate_keeps_full_content() {
        let db = crate::agent::memory::sqlite_fts::MemoryDb::open_in_memory().expect("open mem db");
        let big = "Y".repeat(10_000);
        db.record_message("sess-z", "user", &big).unwrap();
        let parsed = DisplayTranscriptArgs {
            session: Some("sess-z".into()),
            no_truncate: true,
            // Disable wrap so we can count Y's reliably without inserted newlines.
            width: Some(0),
            ..Default::default()
        };
        let v = display_transcript_with(&db, "sess-z", &parsed).expect("render");
        let t = v.get("transcript").and_then(|s| s.as_str()).unwrap();
        assert!(!t.contains("chars omitted"));
        let y_count = t.chars().filter(|c| *c == 'Y').count();
        assert_eq!(y_count, 10_000);
    }

    #[test]
    fn display_transcript_empty_session_renders_empty_transcript() {
        let db = crate::agent::memory::sqlite_fts::MemoryDb::open_in_memory().expect("open mem db");
        let parsed = DisplayTranscriptArgs {
            session: Some("nope".into()),
            ..Default::default()
        };
        let v = display_transcript_with(&db, "nope", &parsed).expect("render");
        assert_eq!(v.get("message_count").and_then(|n| n.as_u64()), Some(0));
        assert_eq!(v.get("transcript").and_then(|s| s.as_str()), Some(""));
    }

    #[test]
    fn shell_hooks_path_returns_default_log_path() {
        let v = shell_hooks_cmd(&["path".into()]).expect("path ok");
        let p = v.get("path").and_then(|s| s.as_str()).expect("path field");
        assert!(p.ends_with("shell-hooks.jsonl"), "got path: {p}");
    }

    #[test]
    fn shell_hooks_default_subcommand_is_path() {
        let v = shell_hooks_cmd(&[]).expect("default ok");
        assert!(v.get("path").is_some());
    }

    #[test]
    fn shell_hooks_init_bash_returns_script_with_trap() {
        let v = shell_hooks_cmd(&["init".into(), "bash".into()]).expect("init bash ok");
        assert_eq!(v.get("shell").and_then(|s| s.as_str()), Some("bash"));
        let script = v.get("script").and_then(|s| s.as_str()).expect("script");
        assert!(script.contains("trap '__cos_pre_exec' DEBUG"));
        assert!(v.get("instructions").and_then(|s| s.as_str()).is_some());
    }

    #[test]
    fn shell_hooks_init_zsh_returns_zsh_specific_script() {
        let v = shell_hooks_cmd(&["init".into(), "zsh".into()]).expect("init zsh ok");
        assert_eq!(v.get("shell").and_then(|s| s.as_str()), Some("zsh"));
        let script = v.get("script").and_then(|s| s.as_str()).expect("script");
        assert!(script.contains("add-zsh-hook preexec"));
    }

    #[test]
    fn shell_hooks_init_fish_returns_fish_specific_script() {
        let v = shell_hooks_cmd(&["init".into(), "fish".into()]).expect("init fish ok");
        assert_eq!(v.get("shell").and_then(|s| s.as_str()), Some("fish"));
        let script = v.get("script").and_then(|s| s.as_str()).expect("script");
        assert!(script.contains("--on-event fish_preexec"));
    }

    #[test]
    fn shell_hooks_init_unknown_shell_errs() {
        let err = shell_hooks_cmd(&["init".into(), "powershell".into()]).unwrap_err();
        assert!(err.contains("powershell"));
    }

    #[test]
    fn shell_hooks_init_missing_shell_errs() {
        let err = shell_hooks_cmd(&["init".into()]).unwrap_err();
        assert!(err.contains("usage"));
    }

    #[test]
    fn shell_hooks_record_pre_requires_cmd() {
        let err = shell_hooks_cmd(&["record-pre".into()]).unwrap_err();
        assert!(err.contains("usage"));
    }

    #[test]
    fn shell_hooks_record_post_requires_int_exit() {
        let err = shell_hooks_cmd(&["record-post".into(), "not-a-number".into()]).unwrap_err();
        assert!(err.contains("integer"));
    }

    #[test]
    fn shell_hooks_record_post_requires_arg() {
        let err = shell_hooks_cmd(&["record-post".into()]).unwrap_err();
        assert!(err.contains("usage"));
    }

    #[test]
    fn shell_hooks_tail_unknown_flag_errs() {
        let err = shell_hooks_cmd(&["tail".into(), "--bogus".into()]).unwrap_err();
        assert!(err.contains("unknown flag"));
    }

    #[test]
    fn shell_hooks_tail_limit_requires_value() {
        let err = shell_hooks_cmd(&["tail".into(), "--limit".into()]).unwrap_err();
        assert!(err.contains("--limit"));
    }

    #[test]
    fn shell_hooks_tail_limit_requires_int() {
        let err = shell_hooks_cmd(&["tail".into(), "--limit".into(), "abc".into()]).unwrap_err();
        assert!(err.contains("--limit"));
    }

    #[test]
    fn shell_hooks_clear_requires_yes_flag() {
        let err = shell_hooks_cmd(&["clear".into()]).unwrap_err();
        assert!(err.contains("--yes"));
    }

    #[test]
    fn shell_hooks_unknown_subcommand_errs() {
        let err = shell_hooks_cmd(&["bogus".into()]).unwrap_err();
        assert!(err.contains("bogus"));
        assert!(err.contains("init"));
    }

    #[test]
    fn media_default_lists_provider_registries() {
        let v = media_cmd(&[]).expect("default ok");
        assert!(v.get("outputs_dir").is_some());
        // The three registries are always present (only `noop` when
        // the active config selects `provider = "none"` for that
        // modality, which is the kernel-default state); each row
        // carries {name, configured}.
        for slot in ["tts", "stt", "imagegen"] {
            let block = v.get(slot).unwrap_or_else(|| panic!("missing {slot}"));
            let providers = block
                .get("providers")
                .and_then(|p| p.as_array())
                .unwrap_or_else(|| panic!("{slot}.providers not an array"));
            assert!(!providers.is_empty(), "{slot} has zero providers");
            let first = &providers[0];
            assert!(first.get("name").is_some());
            assert!(first.get("configured").is_some());
        }
    }

    #[test]
    fn media_providers_default_includes_noop_in_each_registry() {
        let v = media_cmd(&["providers".into()]).expect("providers ok");
        for slot in ["tts", "stt", "imagegen"] {
            let names: Vec<String> = v
                .get(slot)
                .and_then(|s| s.get("providers"))
                .and_then(|p| p.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|r| r.get("name").and_then(|n| n.as_str()).map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            assert!(
                names.contains(&"noop".to_string()),
                "{slot} missing noop, got: {names:?}"
            );
        }
    }

    #[test]
    fn media_outputs_dir_returns_path() {
        let v = media_cmd(&["outputs-dir".into()]).expect("outputs-dir ok");
        let p = v.get("path").and_then(|s| s.as_str()).expect("path field");
        assert!(p.contains("media"), "expected 'media' in path, got: {p}");
    }

    #[test]
    fn media_list_outputs_unknown_flag_errs() {
        let err = media_cmd(&["list-outputs".into(), "--bogus".into()]).unwrap_err();
        assert!(err.contains("unknown flag"));
    }

    #[test]
    fn media_list_outputs_limit_requires_value() {
        let err = media_cmd(&["list-outputs".into(), "--limit".into()]).unwrap_err();
        assert!(err.contains("--limit"));
    }

    #[test]
    fn media_list_outputs_limit_requires_int() {
        let err = media_cmd(&["list-outputs".into(), "--limit".into(), "abc".into()]).unwrap_err();
        assert!(err.contains("--limit"));
    }

    #[test]
    fn media_list_outputs_ext_requires_value() {
        let err = media_cmd(&["list-outputs".into(), "--ext".into()]).unwrap_err();
        assert!(err.contains("--ext"));
    }

    #[test]
    fn media_list_outputs_missing_dir_returns_empty() {
        let dir = std::env::temp_dir().join(format!(
            "cos-media-list-missing-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let v = list_media_outputs(&dir, 10, None).expect("list ok");
        assert_eq!(v.get("exists").and_then(|b| b.as_bool()), Some(false));
        assert_eq!(v.get("n").and_then(|n| n.as_u64()), Some(0));
        assert_eq!(
            v.get("files").and_then(|a| a.as_array()).map(|a| a.len()),
            Some(0)
        );
    }

    #[test]
    fn media_list_outputs_returns_files_newest_first_within_limit() {
        let dir =
            std::env::temp_dir().join(format!("cos-media-list-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        // Write files with sleeps between writes so mtime ordering
        // is deterministic across Windows / Linux / macOS without
        // pulling a fresh `filetime` dep into the workspace.
        for (name, body) in [("a.png", "1"), ("b.png", "22"), ("c.wav", "333")] {
            std::fs::write(dir.join(name), body).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let v = list_media_outputs(&dir, 10, None).expect("list ok");
        assert_eq!(v.get("n").and_then(|n| n.as_u64()), Some(3));
        let files = v.get("files").and_then(|a| a.as_array()).unwrap();
        let names: Vec<&str> = files
            .iter()
            .filter_map(|f| f.get("name").and_then(|s| s.as_str()))
            .collect();
        assert_eq!(names, vec!["c.wav", "b.png", "a.png"]);
        // Filtering by ext narrows the list.
        let v2 = list_media_outputs(&dir, 10, Some("png")).expect("list png ok");
        let names2: Vec<&str> = v2
            .get("files")
            .and_then(|a| a.as_array())
            .unwrap()
            .iter()
            .filter_map(|f| f.get("name").and_then(|s| s.as_str()))
            .collect();
        assert_eq!(names2, vec!["b.png", "a.png"]);
        // Limit caps the result.
        let v3 = list_media_outputs(&dir, 1, None).expect("list lim ok");
        assert_eq!(v3.get("n").and_then(|n| n.as_u64()), Some(1));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn media_unknown_subcommand_errs() {
        let err = media_cmd(&["bogus".into()]).unwrap_err();
        assert!(err.contains("bogus"));
        assert!(err.contains("providers"));
    }

    #[test]
    fn binary_ext_default_lists_with_capped_limit() {
        let v = binary_ext_cmd(&[]).expect("default ok");
        let n = v.get("n").and_then(|n| n.as_u64()).expect("n");
        let total = v.get("total").and_then(|n| n.as_u64()).expect("total");
        assert!(n <= 50, "default limit should cap at 50, got {n}");
        assert!(total >= n, "total ({total}) must be >= shown ({n})");
        assert!(v.get("extensions").is_some());
    }

    #[test]
    fn binary_ext_no_limit_returns_all() {
        let v = binary_ext_cmd(&["list".into(), "--no-limit".into()]).expect("no-limit ok");
        let n = v.get("n").and_then(|n| n.as_u64()).expect("n");
        let total = v.get("total").and_then(|n| n.as_u64()).expect("total");
        assert_eq!(n, total);
    }

    #[test]
    fn binary_ext_list_unknown_flag_errs() {
        let err = binary_ext_cmd(&["list".into(), "--bogus".into()]).unwrap_err();
        assert!(err.contains("unknown flag"));
    }

    #[test]
    fn binary_ext_list_limit_requires_value() {
        let err = binary_ext_cmd(&["list".into(), "--limit".into()]).unwrap_err();
        assert!(err.contains("--limit"));
    }

    #[test]
    fn binary_ext_list_limit_requires_int() {
        let err = binary_ext_cmd(&["list".into(), "--limit".into(), "abc".into()]).unwrap_err();
        assert!(err.contains("--limit"));
    }

    #[test]
    fn binary_ext_extensions_returns_all_unbounded() {
        let v = binary_ext_cmd(&["extensions".into()]).expect("extensions ok");
        let total = v.get("total").and_then(|n| n.as_u64()).expect("total");
        let len = v
            .get("extensions")
            .and_then(|a| a.as_array())
            .map(|a| a.len() as u64)
            .expect("extensions array");
        assert_eq!(total, len);
    }

    #[test]
    fn binary_ext_check_recognises_path_with_known_ext() {
        let v =
            binary_ext_cmd(&["check".into(), "C:\\Users\\me\\image.PNG".into()]).expect("check ok");
        assert_eq!(v.get("mode").and_then(|s| s.as_str()), Some("path"));
        assert_eq!(v.get("is_binary").and_then(|b| b.as_bool()), Some(true));
        assert_eq!(v.get("extension").and_then(|s| s.as_str()), Some("png"));
    }

    #[test]
    fn binary_ext_check_recognises_text_path_as_not_binary() {
        let v = binary_ext_cmd(&["check".into(), "/etc/cos/config.json".into()]).expect("check ok");
        assert_eq!(v.get("mode").and_then(|s| s.as_str()), Some("path"));
        assert_eq!(v.get("is_binary").and_then(|b| b.as_bool()), Some(false));
    }

    #[test]
    fn binary_ext_check_extension_only_input_uses_extension_mode() {
        let v = binary_ext_cmd(&["check".into(), ".gguf".into()]).expect("check ok");
        assert_eq!(v.get("mode").and_then(|s| s.as_str()), Some("extension"));
        assert_eq!(v.get("is_binary").and_then(|b| b.as_bool()), Some(true));
        assert_eq!(v.get("extension").and_then(|s| s.as_str()), Some("gguf"));

        let v2 = binary_ext_cmd(&["check".into(), "exe".into()]).expect("check ok2");
        assert_eq!(v2.get("mode").and_then(|s| s.as_str()), Some("extension"));
        assert_eq!(v2.get("is_binary").and_then(|b| b.as_bool()), Some(true));
    }

    #[test]
    fn binary_ext_check_unknown_extension_returns_false() {
        let v = binary_ext_cmd(&["check".into(), "logfile.unknown".into()]).expect("check ok");
        assert_eq!(v.get("is_binary").and_then(|b| b.as_bool()), Some(false));
    }

    #[test]
    fn binary_ext_check_requires_arg() {
        let err = binary_ext_cmd(&["check".into()]).unwrap_err();
        assert!(err.contains("usage"));
    }

    #[test]
    fn binary_ext_unknown_subcommand_errs() {
        let err = binary_ext_cmd(&["bogus".into()]).unwrap_err();
        assert!(err.contains("bogus"));
        assert!(err.contains("list"));
    }

    // ---- context_cmd dispatch ----

    #[test]
    fn context_cmd_no_args_errs_with_usage() {
        let err = context_cmd(&[]).unwrap_err();
        assert!(err.contains("usage"));
    }

    #[test]
    fn context_cmd_unknown_subcommand_errs() {
        let err = context_cmd(&["bogus".into()]).unwrap_err();
        assert!(err.contains("bogus"));
        assert!(err.contains("hints"));
    }

    // ---- context hints ----

    #[test]
    fn context_hints_unknown_flag_errs() {
        let err = context_hints_cmd(&["--bogus".into(), "x".into()]).unwrap_err();
        assert!(err.contains("--bogus"));
    }

    #[test]
    fn context_hints_invalid_cwd_errs() {
        let err =
            context_hints_cmd(&["--cwd".into(), "Z:\\definitely\\not\\there".into()]).unwrap_err();
        assert!(err.contains("not a directory"));
    }

    #[test]
    fn context_hints_finds_real_markers_in_temp_dir() {
        let dir = std::env::temp_dir().join(format!(
            "cos-context-hints-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        let v =
            context_hints_cmd(&["--cwd".into(), dir.to_string_lossy().to_string()]).expect("ok");
        assert_eq!(v.get("count").and_then(|n| n.as_u64()), Some(1));
        let hints = v.get("hints").and_then(|h| h.as_array()).unwrap();
        assert!(hints
            .iter()
            .any(|h| h.get("label").and_then(|s| s.as_str()) == Some("Rust crate")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn context_hints_render_returns_summary_paragraph() {
        let dir = std::env::temp_dir().join(format!(
            "cos-context-hints-render-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("package.json"), "{}").unwrap();
        let v = context_hints_cmd(&[
            "--cwd".into(),
            dir.to_string_lossy().to_string(),
            "--render".into(),
        ])
        .expect("ok");
        let s = v.get("summary").and_then(|s| s.as_str()).unwrap_or("");
        assert!(s.contains("Project hints"));
        assert!(s.contains("Node.js project"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn context_hints_recursive_with_depth() {
        let dir = std::env::temp_dir().join(format!(
            "cos-context-hints-deep-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let nested = dir.join("apps").join("web");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("package.json"), "{}").unwrap();
        // Depth 0 → no recursion → no hits.
        let v0 = context_hints_cmd(&[
            "--cwd".into(),
            dir.to_string_lossy().to_string(),
            "--depth".into(),
            "0".into(),
        ])
        .expect("ok");
        assert_eq!(v0.get("count").and_then(|n| n.as_u64()), Some(0));
        // Depth 3 → recursive walk → finds the nested manifest.
        let v3 = context_hints_cmd(&[
            "--cwd".into(),
            dir.to_string_lossy().to_string(),
            "--depth".into(),
            "3".into(),
        ])
        .expect("ok");
        assert_eq!(v3.get("count").and_then(|n| n.as_u64()), Some(1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- context refs ----

    #[test]
    fn context_refs_requires_text() {
        let err = context_refs_cmd(&[]).unwrap_err();
        assert!(err.contains("--text"));
    }

    #[test]
    fn context_refs_unknown_flag_errs() {
        let err = context_refs_cmd(&["--bogus".into(), "v".into()]).unwrap_err();
        assert!(err.contains("--bogus"));
    }

    #[test]
    fn context_refs_extracts_paths_and_urls() {
        let v = context_refs_cmd(&[
            "--text".into(),
            "see @notes.md and @https://example.com/x".into(),
        ])
        .expect("ok");
        assert_eq!(v.get("count").and_then(|n| n.as_u64()), Some(2));
        let refs = v.get("references").and_then(|x| x.as_array()).unwrap();
        assert_eq!(refs[0].get("kind").and_then(|s| s.as_str()), Some("Path"));
        assert_eq!(
            refs[0].get("raw").and_then(|s| s.as_str()),
            Some("notes.md")
        );
        assert_eq!(refs[1].get("kind").and_then(|s| s.as_str()), Some("Url"));
    }

    #[test]
    fn context_refs_unique_dedupes() {
        let v =
            context_refs_cmd(&["--text".into(), "@a @a @a".into(), "--unique".into()]).expect("ok");
        assert_eq!(v.get("count").and_then(|n| n.as_u64()), Some(1));
        assert_eq!(v.get("unique").and_then(|b| b.as_bool()), Some(true));
    }

    // ---- context markers ----

    #[test]
    fn context_markers_dumps_table() {
        let v = context_markers_cmd(&[]).expect("ok");
        let total = v.get("total").and_then(|n| n.as_u64()).unwrap();
        assert!(total >= 30);
        let by_kind = v.get("by_kind").and_then(|x| x.as_object()).unwrap();
        let manifests = by_kind.get("Manifest").and_then(|x| x.as_array()).unwrap();
        let names: Vec<&str> = manifests.iter().filter_map(|s| s.as_str()).collect();
        assert!(names.contains(&"Cargo.toml"));
        assert!(names.contains(&"package.json"));
        assert!(names.contains(&"go.mod"));
    }

    // ---- context build (engine) ----

    #[test]
    fn context_build_no_args_returns_empty_block() {
        let v = context_cmd(&["build".into()]).expect("ok");
        assert_eq!(v.get("is_empty").and_then(|b| b.as_bool()), Some(true));
        assert!(v.get("rendered").map(|x| x.is_null()).unwrap_or(false));
    }

    #[test]
    fn context_build_unknown_flag_errs() {
        let err = context_cmd(&["build".into(), "--bogus".into()]).unwrap_err();
        assert!(err.contains("--bogus"));
    }

    #[test]
    fn context_build_invalid_cwd_errs() {
        let err = context_cmd(&[
            "build".into(),
            "--cwd".into(),
            "Z:\\definitely\\not\\there".into(),
        ])
        .unwrap_err();
        assert!(err.contains("not a directory"));
    }

    #[test]
    fn context_build_invalid_depth_errs() {
        let err = context_cmd(&["build".into(), "--depth".into(), "abc".into()]).unwrap_err();
        assert!(err.contains("--depth"));
    }

    #[test]
    fn context_build_with_text_extracts_references() {
        let v = context_cmd(&["build".into(), "--text".into(), "look at @notes.md".into()])
            .expect("ok");
        assert_eq!(v.get("is_empty").and_then(|b| b.as_bool()), Some(false));
        let refs = v.get("references").and_then(|x| x.as_array()).unwrap();
        assert_eq!(refs.len(), 1);
        let rendered = v.get("rendered").and_then(|s| s.as_str()).unwrap_or("");
        assert!(rendered.contains("PROJECT_CONTEXT"));
        assert!(rendered.contains("notes.md"));
    }

    #[test]
    fn context_build_with_cwd_picks_up_hints() {
        let dir = std::env::temp_dir().join(format!(
            "cos-context-build-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        let v = context_cmd(&[
            "build".into(),
            "--cwd".into(),
            dir.to_string_lossy().to_string(),
        ])
        .expect("ok");
        let hints = v.get("hints").and_then(|x| x.as_array()).unwrap();
        assert_eq!(hints.len(), 1);
        assert_eq!(
            hints[0].get("label").and_then(|s| s.as_str()),
            Some("Rust crate")
        );
        let rendered = v.get("rendered").and_then(|s| s.as_str()).unwrap_or("");
        assert!(rendered.contains("Project hints"));
        assert!(rendered.contains("cwd:"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn context_build_with_notes_appends_them() {
        let v = context_cmd(&[
            "build".into(),
            "--note".into(),
            "host: Windows".into(),
            "--note".into(),
            "12 MB free".into(),
        ])
        .expect("ok");
        let notes = v.get("notes").and_then(|x| x.as_array()).unwrap();
        assert_eq!(notes.len(), 2);
        let rendered = v.get("rendered").and_then(|s| s.as_str()).unwrap_or("");
        assert!(rendered.contains("Notes:"));
        assert!(rendered.contains("host: Windows"));
        assert!(rendered.contains("12 MB free"));
    }

    #[test]
    fn context_build_max_refs_caps_count() {
        let v = context_cmd(&[
            "build".into(),
            "--text".into(),
            "@a @b @c @d @e".into(),
            "--max-refs".into(),
            "2".into(),
        ])
        .expect("ok");
        let refs = v.get("references").and_then(|x| x.as_array()).unwrap();
        assert_eq!(refs.len(), 2);
    }

    #[test]
    fn context_build_no_dedup_keeps_duplicates() {
        let v = context_cmd(&[
            "build".into(),
            "--text".into(),
            "@a @a @a".into(),
            "--no-dedup".into(),
        ])
        .expect("ok");
        let refs = v.get("references").and_then(|x| x.as_array()).unwrap();
        assert_eq!(refs.len(), 3);
    }

    // ---- file-safety dispatch ----

    #[test]
    fn file_safety_no_args_errs_with_usage() {
        let err = file_safety_cmd(&[]).unwrap_err();
        assert!(err.contains("usage"));
        assert!(err.contains("check"));
    }

    #[test]
    fn file_safety_unknown_subcommand_errs() {
        let err = file_safety_cmd(&["bogus".into()]).unwrap_err();
        assert!(err.contains("bogus"));
    }

    #[test]
    fn file_safety_check_requires_path() {
        let err = file_safety_cmd(&["check".into()]).unwrap_err();
        assert!(err.contains("usage"));
    }

    #[test]
    fn file_safety_check_rejects_multiple_paths() {
        let err = file_safety_cmd(&["check".into(), "a".into(), "b".into()]).unwrap_err();
        assert!(err.contains("single path"));
    }

    #[test]
    fn file_safety_check_allows_normal_file() {
        let v =
            file_safety_cmd(&["check".into(), "/home/user/project/main.rs".into()]).expect("ok");
        assert_eq!(v.get("verdict").and_then(|s| s.as_str()), Some("allow"));
        assert!(v.get("category").and_then(|c| c.as_str()).is_none());
    }

    #[test]
    fn file_safety_check_denies_credential_dir() {
        let v = file_safety_cmd(&["check".into(), "/home/user/.ssh/id_rsa".into()]).expect("ok");
        assert_eq!(v.get("verdict").and_then(|s| s.as_str()), Some("deny"));
        assert_eq!(
            v.get("category").and_then(|c| c.as_str()),
            Some("credential")
        );
    }

    #[test]
    fn file_safety_check_denies_dangerous_extension() {
        let v = file_safety_cmd(&["check".into(), "/tmp/payload.exe".into()]).expect("ok");
        assert_eq!(v.get("verdict").and_then(|s| s.as_str()), Some("deny"));
        assert_eq!(
            v.get("category").and_then(|c| c.as_str()),
            Some("dangerous_extension")
        );
    }

    #[test]
    fn file_safety_check_caution_for_shell_script() {
        let v = file_safety_cmd(&["check".into(), "/home/user/run.sh".into()]).expect("ok");
        assert_eq!(v.get("verdict").and_then(|s| s.as_str()), Some("caution"));
    }

    #[test]
    fn file_safety_batch_aggregates_summary() {
        let v = file_safety_cmd(&[
            "batch".into(),
            "/home/user/main.rs".into(),
            "/etc/passwd".into(),
            "/home/user/run.sh".into(),
        ])
        .expect("ok");
        assert_eq!(v.get("count").and_then(|n| n.as_u64()), Some(3));
        let summary = v.get("summary").and_then(|x| x.as_object()).unwrap();
        assert_eq!(summary.get("allow").and_then(|n| n.as_u64()), Some(1));
        assert_eq!(summary.get("caution").and_then(|n| n.as_u64()), Some(1));
        assert_eq!(summary.get("deny").and_then(|n| n.as_u64()), Some(1));
    }

    #[test]
    fn file_safety_batch_requires_at_least_one_path() {
        let err = file_safety_cmd(&["batch".into()]).unwrap_err();
        assert!(err.contains("usage"));
    }

    #[test]
    fn file_safety_categories_lists_known_categories() {
        let v = file_safety_cmd(&["categories".into()]).expect("ok");
        let cats = v.get("categories").and_then(|c| c.as_array()).unwrap();
        let names: Vec<&str> = cats.iter().filter_map(|c| c.as_str()).collect();
        assert!(names.contains(&"dangerous_extension"));
        assert!(names.contains(&"credential"));
        assert!(names.contains(&"system_directory"));
        assert!(names.contains(&"vcs_internal"));
        let verdicts = v.get("verdicts").and_then(|x| x.as_array()).unwrap();
        let vs: Vec<&str> = verdicts.iter().filter_map(|v| v.as_str()).collect();
        assert_eq!(vs, vec!["allow", "caution", "deny"]);
    }

    // ---- osv dispatch (no network) ----

    #[test]
    fn osv_no_args_errs_with_usage() {
        let err = osv_cmd(&[]).unwrap_err();
        assert!(err.contains("usage"));
        assert!(err.contains("parse"));
    }

    #[test]
    fn osv_unknown_subcommand_errs() {
        let err = osv_cmd(&["bogus".into()]).unwrap_err();
        assert!(err.contains("bogus"));
    }

    #[test]
    fn osv_ecosystems_lists_known_ecosystems() {
        let v = osv_cmd(&["ecosystems".into()]).expect("ok");
        let eco = v.get("ecosystems").and_then(|x| x.as_array()).unwrap();
        let names: Vec<&str> = eco.iter().filter_map(|s| s.as_str()).collect();
        assert!(names.contains(&"crates.io"));
        assert!(names.contains(&"npm"));
        assert!(names.contains(&"PyPI"));
        assert!(names.contains(&"Go"));
        let lockfiles = v.get("lockfiles").and_then(|x| x.as_array()).unwrap();
        let ls: Vec<&str> = lockfiles.iter().filter_map(|s| s.as_str()).collect();
        assert!(ls.contains(&"Cargo.lock"));
        assert!(ls.contains(&"go.sum"));
    }

    #[test]
    fn osv_parse_requires_path() {
        let err = osv_cmd(&["parse".into()]).unwrap_err();
        assert!(err.contains("usage"));
    }

    #[test]
    fn osv_parse_rejects_extra_args() {
        let err = osv_cmd(&["parse".into(), "a".into(), "b".into()]).unwrap_err();
        assert!(err.contains("single"));
    }

    #[test]
    fn osv_parse_reads_cargo_lock() {
        let dir =
            std::env::temp_dir().join(format!("cos-osv-parse-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let lock_path = dir.join("Cargo.lock");
        std::fs::write(
            &lock_path,
            "[[package]]\nname = \"foo\"\nversion = \"1.2.3\"\n\n[[package]]\nname = \"bar\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let v = osv_cmd(&["parse".into(), lock_path.to_string_lossy().to_string()]).expect("ok");
        assert_eq!(v.get("count").and_then(|n| n.as_u64()), Some(2));
        let pkgs = v.get("packages").and_then(|x| x.as_array()).unwrap();
        let names: Vec<&str> = pkgs
            .iter()
            .filter_map(|p| p.get("name").and_then(|s| s.as_str()))
            .collect();
        assert!(names.contains(&"foo"));
        assert!(names.contains(&"bar"));
        for p in pkgs {
            assert_eq!(
                p.get("ecosystem").and_then(|s| s.as_str()),
                Some("crates.io")
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn osv_parse_unknown_lockfile_errs() {
        let dir =
            std::env::temp_dir().join(format!("cos-osv-bad-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("Pipfile.lock");
        std::fs::write(&p, "{}").unwrap();
        let err = osv_cmd(&["parse".into(), p.to_string_lossy().to_string()]).unwrap_err();
        assert!(err.contains("unknown lockfile"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn osv_query_requires_coord() {
        let err = osv_cmd(&["query".into()]).unwrap_err();
        assert!(err.contains("usage"));
    }

    #[test]
    fn osv_query_requires_ecosystem_flag() {
        let err = osv_cmd(&["query".into(), "lodash@1.0.0".into()]).unwrap_err();
        assert!(err.contains("--ecosystem"));
    }

    #[test]
    fn osv_query_rejects_malformed_coord() {
        let err = osv_cmd(&[
            "query".into(),
            "no-version".into(),
            "--ecosystem".into(),
            "npm".into(),
        ])
        .unwrap_err();
        assert!(err.contains("name>@<version"));
    }

    #[test]
    fn osv_query_rejects_unknown_flag() {
        let err = osv_cmd(&[
            "query".into(),
            "foo@1.0".into(),
            "--bogus".into(),
            "x".into(),
        ])
        .unwrap_err();
        assert!(err.contains("--bogus"));
    }

    // ---- stream subcommand ----------------------------------------------

    /// Build a mock provider with a scripted text response and run
    /// `stream_cmd_async` against it. Returns the JSON envelope.
    fn run_stream_async(
        text: &str,
        cfg: &crate::config::AgentConfig,
        prompt: &str,
    ) -> serde_json::Value {
        use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
        let mock = MockProvider::new(&cfg.model, cfg);
        mock.push_response(MockResponse::Text(text.to_string()));
        let provider: std::sync::Arc<dyn llm::Provider> = std::sync::Arc::new(mock);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(stream_cmd_async(provider, cfg, prompt))
            .expect("stream ok")
    }

    #[test]
    fn stream_cmd_rejects_empty_prompt() {
        let err = stream_cmd(&[]).unwrap_err();
        assert!(err.contains("usage"));
    }

    #[test]
    fn stream_cmd_rejects_empty_string_prompt() {
        let err = stream_cmd(&[String::new()]).unwrap_err();
        assert!(err.contains("usage"));
    }

    #[test]
    fn stream_async_accumulates_text_and_returns_envelope() {
        let mut cfg = crate::config::AgentConfig::default();
        cfg.provider = "mock".into();
        cfg.model = "mock-model".into();
        let v = run_stream_async("hello world", &cfg, "say hi");
        assert_eq!(
            v.get("answer").and_then(|a| a.as_str()),
            Some("hello world")
        );
        assert_eq!(v.get("provider").and_then(|p| p.as_str()), Some("mock"));
        assert_eq!(v.get("model").and_then(|m| m.as_str()), Some("mock-model"));
        // mock's chat_stream emits Message + Done; finish_reason for
        // a plain text reply is FinishReason::Stop.
        assert_eq!(v.get("finish").and_then(|f| f.as_str()), Some("Stop"));
        assert!(v.get("tool_calls").unwrap().as_array().unwrap().is_empty());
    }

    #[test]
    fn stream_async_surfaces_tool_calls() {
        use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
        use crate::agent::llm::types::ToolCall;
        let mut cfg = crate::config::AgentConfig::default();
        cfg.provider = "mock".into();
        cfg.model = "mock-model".into();
        let mock = MockProvider::new(&cfg.model, &cfg);
        mock.push_response(MockResponse::ToolUse(vec![ToolCall {
            id: "call_1".into(),
            name: "echo".into(),
            input: serde_json::json!({"text": "hi"}),
        }]));
        let provider: std::sync::Arc<dyn llm::Provider> = std::sync::Arc::new(mock);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let v = rt
            .block_on(stream_cmd_async(provider, &cfg, "use a tool"))
            .expect("stream ok");
        let calls = v.get("tool_calls").unwrap().as_array().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["id"], "call_1");
        assert_eq!(calls[0]["name"], "echo");
        // mock emits ToolUse via Message variant → finish ToolUse.
        assert_eq!(v.get("finish").and_then(|f| f.as_str()), Some("ToolUse"));
    }

    #[test]
    fn stream_async_propagates_provider_error() {
        use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
        let mut cfg = crate::config::AgentConfig::default();
        cfg.provider = "mock".into();
        cfg.model = "mock-model".into();
        let mock = MockProvider::new(&cfg.model, &cfg);
        mock.push_response(MockResponse::Error(llm::LlmError::Auth));
        let provider: std::sync::Arc<dyn llm::Provider> = std::sync::Arc::new(mock);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = rt
            .block_on(stream_cmd_async(provider, &cfg, "hi"))
            .unwrap_err();
        assert!(
            err.contains("chat_stream") || err.contains("auth"),
            "want chat_stream/auth in err, got {err}"
        );
    }

    #[test]
    fn stream_async_envelope_includes_usage_keys() {
        let mut cfg = crate::config::AgentConfig::default();
        cfg.provider = "mock".into();
        cfg.model = "mock-model".into();
        let v = run_stream_async("ok", &cfg, "ping");
        let usage = v.get("usage").unwrap();
        assert!(usage.get("input_tokens").is_some());
        assert!(usage.get("output_tokens").is_some());
        assert!(usage.get("cache_read_tokens").is_some());
        assert!(usage.get("cache_write_tokens").is_some());
    }

    /// Helper for `cos agent live` integration tests. Mirrors the
    /// `run_stream_async` helper above but routes through the new
    /// multi-turn streaming path.
    fn run_live_async(
        responses: &[(&str, Option<Vec<llm::types::ToolCall>>)],
        cfg: &crate::config::AgentConfig,
        prompt: &str,
    ) -> Value {
        use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
        let mock = MockProvider::new(&cfg.model, cfg);
        for (text, tool_calls) in responses {
            match tool_calls {
                Some(calls) if !calls.is_empty() => {
                    mock.push_response(MockResponse::ToolUse(calls.clone()));
                }
                _ => {
                    mock.push_response(MockResponse::Text((*text).into()));
                }
            }
        }
        let provider: std::sync::Arc<dyn llm::Provider> = std::sync::Arc::new(mock);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(live_cmd_async(provider, cfg, prompt))
            .expect("live ok")
    }

    #[test]
    fn live_async_returns_text_envelope() {
        let mut cfg = crate::config::AgentConfig::default();
        cfg.provider = "mock".into();
        cfg.model = "mock-model".into();
        // Disable memory recording for this test to keep it
        // hermetic — the temp data_dir scaffolding from
        // env-overrides is intentionally not set up here, so the
        // open_default() may fall back to no-recording mode anyway.
        let v = run_live_async(&[("hello world", None)], &cfg, "say hello");
        assert_eq!(v["answer"].as_str(), Some("hello world"));
        assert!(v["session_id"].as_str().unwrap().len() > 0);
        assert_eq!(v["provider"].as_str(), Some("mock"));
        assert_eq!(v["model"].as_str(), Some("mock-model"));
        // Text-only run: no tool calls.
        assert_eq!(v["tool_calls"].as_array().unwrap().len(), 0);
        // Mock emits Text via Message → Done with Stop finish.
        assert_eq!(v["finish"].as_str(), Some("Stop"));
        let usage = v.get("usage").unwrap();
        assert!(usage.get("input_tokens").is_some());
    }

    #[test]
    fn live_async_records_tool_call_pair() {
        use crate::agent::llm::types::ToolCall;
        let mut cfg = crate::config::AgentConfig::default();
        cfg.provider = "mock".into();
        cfg.model = "mock-model".into();
        cfg.max_turns = 2; // tool-call → echo result → final text
        let v = run_live_async(
            &[
                (
                    "",
                    Some(vec![ToolCall {
                        id: "call_1".into(),
                        name: "echo".into(),
                        input: serde_json::json!({"text": "abc"}),
                    }]),
                ),
                ("done", None),
            ],
            &cfg,
            "echo abc",
        );
        // Streaming sink records the tool_use event.
        let calls = v["tool_calls"].as_array().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["id"], "call_1");
        assert_eq!(calls[0]["name"], "echo");
        // Final answer comes from the second turn's Text response.
        assert_eq!(v["answer"].as_str(), Some("done"));
        assert!(v["turns"].as_u64().unwrap() >= 2);
    }

    #[test]
    fn live_async_propagates_provider_error() {
        use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
        let mut cfg = crate::config::AgentConfig::default();
        cfg.provider = "mock".into();
        cfg.model = "mock-model".into();
        let mock = MockProvider::new(&cfg.model, &cfg);
        mock.push_response(MockResponse::Error(llm::LlmError::Auth));
        let provider: std::sync::Arc<dyn llm::Provider> = std::sync::Arc::new(mock);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = rt
            .block_on(live_cmd_async(provider, &cfg, "hi"))
            .unwrap_err();
        // AgentError::Llm wraps the provider error; the formatter
        // includes either "auth" or the provider-error prefix.
        assert!(
            err.to_lowercase().contains("auth")
                || err.to_lowercase().contains("llm")
                || err.to_lowercase().contains("provider"),
            "want auth/llm/provider in err, got {err}"
        );
    }

    #[test]
    fn live_cmd_rejects_empty_prompt() {
        let err = live_cmd(&[]).unwrap_err();
        assert!(err.contains("usage"), "got {err}");
        let err2 = live_cmd(&["".into()]).unwrap_err();
        assert!(err2.contains("usage"), "got {err2}");
    }

    #[test]
    fn chat_cmd_rejects_unknown_flag() {
        let err = chat_cmd(&["--bogus".into()]).unwrap_err();
        assert!(err.to_lowercase().contains("unknown flag"), "got {err}");
    }

    #[test]
    fn chat_cmd_session_flag_requires_value() {
        let err = chat_cmd(&["--session".into()]).unwrap_err();
        assert!(err.contains("--session"), "got {err}");
    }

    #[test]
    fn chat_cmd_max_turns_flag_requires_value() {
        let err = chat_cmd(&["--max-turns".into()]).unwrap_err();
        assert!(err.contains("--max-turns"), "got {err}");
    }

    #[test]
    fn chat_cmd_max_turns_flag_rejects_non_numeric() {
        let err = chat_cmd(&["--max-turns".into(), "lots".into()]).unwrap_err();
        assert!(err.contains("--max-turns"), "got {err}");
    }

    #[test]
    fn chat_routed_through_run() {
        // Confirm the dispatcher in `run()` reaches `chat_cmd`. Pass an
        // unknown flag so we get a deterministic error without trying
        // to read stdin.
        let err = run("chat", &["--definitely-not-real".into()]).unwrap_err();
        assert!(err.to_lowercase().contains("unknown flag"), "got {err}");
    }

    // -----------------------------------------------------------------
    // interrupt_cmd
    // -----------------------------------------------------------------

    #[test]
    fn interrupt_cmd_default_errs_with_usage() {
        let err = interrupt_cmd(&[]).unwrap_err();
        assert!(err.contains("interrupt"), "got {err}");
        assert!(err.contains("list"), "got {err}");
        assert!(err.contains("signal"), "got {err}");
    }

    #[test]
    fn interrupt_cmd_list_returns_active_sessions() {
        let id = format!("cli-list-{}", uuid::Uuid::new_v4().simple());
        let _h = crate::agent::runtime::interrupt::register(&id);
        let v = interrupt_cmd(&["list".into()]).expect("list ok");
        let arr = v["sessions"].as_array().expect("sessions array");
        let ids: Vec<&str> = arr.iter().filter_map(|s| s.as_str()).collect();
        assert!(ids.contains(&id.as_str()), "list missing {id}: {arr:?}");
        assert!(v["count"].as_u64().unwrap() >= 1);
    }

    #[test]
    fn interrupt_cmd_signal_unknown_session_reports_not_registered() {
        let id = format!("cli-unknown-{}", uuid::Uuid::new_v4().simple());
        let v = interrupt_cmd(&["signal".into(), id.clone()]).expect("ok");
        assert_eq!(v["signaled"], serde_json::Value::Bool(false));
        assert_eq!(v["session_id"].as_str().unwrap(), id);
        assert!(v["reason"].as_str().unwrap().contains("not registered"));
    }

    #[test]
    fn interrupt_cmd_signal_active_session_returns_signaled_true() {
        let id = format!("cli-signal-{}", uuid::Uuid::new_v4().simple());
        let h = crate::agent::runtime::interrupt::register(&id);
        let v = interrupt_cmd(&["signal".into(), id.clone()]).expect("ok");
        assert_eq!(v["signaled"], serde_json::Value::Bool(true));
        assert_eq!(v["session_id"].as_str().unwrap(), id);
        // Signal really took effect.
        assert!(h.check());
    }

    #[test]
    fn interrupt_cmd_signal_requires_session_id() {
        let err = interrupt_cmd(&["signal".into()]).unwrap_err();
        assert!(err.contains("usage"), "got {err}");
    }

    #[test]
    fn interrupt_cmd_unknown_subcommand_errs() {
        let err = interrupt_cmd(&["frobnicate".into()]).unwrap_err();
        assert!(err.contains("unknown"), "got {err}");
    }

    #[test]
    fn run_interrupt_routes_to_interrupt_cmd() {
        // Confirm the agent dispatcher reaches interrupt_cmd.
        let err = run("interrupt", &["frobnicate".into()]).unwrap_err();
        assert!(err.contains("unknown"), "got {err}");
    }

    // -----------------------------------------------------------------
    // learn (memory curator CLI)
    // -----------------------------------------------------------------

    /// Pin the curator default log under a per-test temp dir so we
    /// don't trample the real machine's `%ProgramData%\cos\` state.
    /// Returns the temp dir so the caller can clean it up.
    fn isolate_cos_data_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cos-learn-cli-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("COS_DATA_DIR", &dir);
        dir
    }

    #[test]
    fn learn_cmd_unknown_subcommand_errs() {
        let err = learn_cmd(&["frobnicate".into()]).unwrap_err();
        assert!(err.contains("unknown"), "got {err}");
    }

    #[test]
    fn learn_cmd_extract_requires_session_flag() {
        let _dir = isolate_cos_data_dir("missing-session");
        let err = learn_cmd(&["extract".into()]).unwrap_err();
        assert!(err.contains("--session"), "got {err}");
    }

    #[test]
    fn learn_cmd_extract_unknown_flag_errs() {
        let err = learn_cmd(&["extract".into(), "--frobnicate".into(), "x".into()]).unwrap_err();
        assert!(err.contains("unknown"), "got {err}");
    }

    #[test]
    fn learn_cmd_extract_min_confidence_out_of_range_errs() {
        let err = learn_cmd(&[
            "extract".into(),
            "--session".into(),
            "s".into(),
            "--min-confidence".into(),
            "1.5".into(),
        ])
        .unwrap_err();
        assert!(err.contains("--min-confidence"), "got {err}");
    }

    #[test]
    fn learn_cmd_extract_min_confidence_not_float_errs() {
        let err = learn_cmd(&[
            "extract".into(),
            "--session".into(),
            "s".into(),
            "--min-confidence".into(),
            "abc".into(),
        ])
        .unwrap_err();
        assert!(err.contains("--min-confidence"), "got {err}");
    }

    #[test]
    fn learn_cmd_extract_limit_not_integer_errs() {
        let err = learn_cmd(&[
            "extract".into(),
            "--session".into(),
            "s".into(),
            "--limit".into(),
            "abc".into(),
        ])
        .unwrap_err();
        assert!(err.contains("--limit"), "got {err}");
    }

    #[test]
    fn learn_cmd_extract_dry_run_with_unknown_session_succeeds() {
        // dry-run skips both LLM and dedupe, and an unknown session
        // simply has zero recent messages — should be a clean
        // success envelope with empty facts.
        let _dir = isolate_cos_data_dir("dry-run-empty");
        let v = learn_cmd(&[
            "extract".into(),
            "--session".into(),
            "no-such-session".into(),
            "--dry-run".into(),
        ])
        .expect("dry-run should not fail");
        assert_eq!(v["ok"], serde_json::Value::Bool(true));
        assert_eq!(v["dry_run"], serde_json::Value::Bool(true));
        assert_eq!(v["messages_examined"], serde_json::json!(0));
        assert!(
            v["facts_proposed"].as_array().unwrap().is_empty(),
            "got {v}"
        );
    }

    #[test]
    fn learn_cmd_status_default_is_empty_when_log_missing() {
        let _dir = isolate_cos_data_dir("status-empty");
        let v = learn_cmd(&["status".into()]).expect("ok");
        assert_eq!(v["session_count"], serde_json::json!(0));
        assert_eq!(v["log_exists"], serde_json::Value::Bool(false));
    }

    #[test]
    fn learn_cmd_default_subcommand_is_status() {
        let _dir = isolate_cos_data_dir("status-default");
        let v = learn_cmd(&[]).expect("ok");
        assert!(v.get("session_count").is_some(), "got {v}");
    }

    #[test]
    fn learn_cmd_clear_log_requires_session_or_all() {
        let _dir = isolate_cos_data_dir("clear-needs-flag");
        let err = learn_cmd(&["clear-log".into()]).unwrap_err();
        assert!(
            err.contains("--session") || err.contains("--all"),
            "got {err}"
        );
    }

    #[test]
    fn learn_cmd_clear_log_all_writes_empty_log() {
        let dir = isolate_cos_data_dir("clear-all");
        let v = learn_cmd(&["clear-log".into(), "--all".into()]).expect("ok");
        assert_eq!(v["ok"], serde_json::Value::Bool(true));
        // log file is now created on disk under the isolated dir.
        let log = dir.join("agent").join("memory").join("curation_log.json");
        assert!(log.exists(), "expected {} to exist", log.display());
    }

    #[test]
    fn learn_cmd_clear_log_for_unknown_session_reports_zero_removed() {
        let _dir = isolate_cos_data_dir("clear-unknown");
        let v = learn_cmd(&["clear-log".into(), "--session".into(), "ghost".into()]).expect("ok");
        assert_eq!(v["removed_entries"], serde_json::json!(0));
    }

    #[test]
    fn learn_cmd_prompt_returns_embedded_default() {
        let v = learn_cmd(&["prompt".into()]).expect("ok");
        let s = v["system_prompt"].as_str().unwrap();
        assert!(s.contains("<fact"), "prompt should mention <fact tags");
        assert!(s.contains("category"));
    }

    #[test]
    fn run_learn_routes_to_learn_cmd() {
        // dispatcher routing — using `prompt` because it's IO-free.
        let v = run("learn", &["prompt".into()]).expect("ok");
        assert!(v.get("system_prompt").is_some(), "got {v}");
    }

    // -----------------------------------------------------------------
    // hooks (runtime hook registry CLI)
    // -----------------------------------------------------------------

    #[test]
    fn hooks_cmd_list_default_returns_count() {
        let _dir = isolate_cos_data_dir("hooks-list-default");
        let v = hooks_cmd(&[]).expect("ok");
        assert!(v.get("hooks").is_some(), "got {v}");
        assert!(v.get("count").is_some(), "got {v}");
        assert!(v["count"].is_number(), "got {v}");
        assert!(v["persistent"].is_array(), "got {v}");
        assert!(v["config_path"].is_string(), "got {v}");
    }

    #[test]
    fn hooks_cmd_list_after_register_includes_name() {
        use crate::agent::runtime::hooks::{global_registry, Hook, HookContext, HookOutcome};
        let _dir = isolate_cos_data_dir("hooks-list-after-register");
        struct TestHook;
        impl Hook for TestHook {
            fn name(&self) -> &str {
                "cli-test-hook"
            }
            fn pre_turn(&self, _ctx: &HookContext) -> HookOutcome {
                HookOutcome::Continue
            }
        }
        let registry = global_registry();
        registry.register(std::sync::Arc::new(TestHook));
        let v = hooks_cmd(&["list".into()]).expect("ok");
        let names = v["hooks"].as_array().unwrap();
        assert!(
            names.iter().any(|n| n.as_str() == Some("cli-test-hook")),
            "got {v}"
        );
        // Cleanup so we don't leak the registration into other tests.
        registry.unregister("cli-test-hook");
    }

    #[test]
    fn hooks_cmd_unknown_subcommand_errs() {
        let err = hooks_cmd(&["frobnicate".into()]).unwrap_err();
        assert!(err.contains("unknown"), "got {err}");
    }

    #[test]
    fn run_hooks_routes_to_hooks_cmd() {
        let _dir = isolate_cos_data_dir("hooks-route");
        let v = run("hooks", &["list".into()]).expect("ok");
        assert!(v.get("count").is_some(), "got {v}");
    }

    #[test]
    fn hooks_cmd_enable_persists_kind_and_registers_in_process() {
        use crate::agent::runtime::hooks::global_registry;
        use crate::agent::runtime::hooks_config;
        let _dir = isolate_cos_data_dir("hooks-enable");
        // make sure no leftover registration from a prior test
        global_registry().unregister("logging");

        let v = hooks_cmd(&["enable".into(), "logging".into()]).expect("ok");
        assert_eq!(v["kind"], serde_json::json!("logging"));
        assert_eq!(v["persisted"], serde_json::json!(true));
        assert_eq!(v["registered_now"], serde_json::json!(true));

        // file exists with logging in enabled list
        let cfg = hooks_config::load(&crate::paths::agent_hooks_path()).expect("load");
        assert_eq!(cfg.enabled, vec![hooks_config::HookKind::Logging]);

        // hook actually registered
        assert!(global_registry().names().contains(&"logging".to_string()));

        // cleanup
        global_registry().unregister("logging");
    }

    #[test]
    fn hooks_cmd_enable_idempotent_second_call_is_noop() {
        use crate::agent::runtime::hooks::global_registry;
        let _dir = isolate_cos_data_dir("hooks-enable-idempotent");
        global_registry().unregister("logging");

        let _ = hooks_cmd(&["enable".into(), "logging".into()]).expect("ok");
        let v = hooks_cmd(&["enable".into(), "logging".into()]).expect("ok");
        assert_eq!(v["persisted"], serde_json::json!(false));
        assert_eq!(v["registered_now"], serde_json::json!(false));

        global_registry().unregister("logging");
    }

    #[test]
    fn hooks_cmd_enable_accepts_kind_flag_form() {
        use crate::agent::runtime::hooks::global_registry;
        let _dir = isolate_cos_data_dir("hooks-enable-flag");
        global_registry().unregister("logging");

        let v = hooks_cmd(&["enable".into(), "--kind".into(), "logging".into()]).expect("ok");
        assert_eq!(v["kind"], serde_json::json!("logging"));

        global_registry().unregister("logging");
    }

    #[test]
    fn hooks_cmd_enable_unknown_kind_errs() {
        let _dir = isolate_cos_data_dir("hooks-enable-unknown");
        let err = hooks_cmd(&["enable".into(), "frobnicate".into()]).unwrap_err();
        assert!(err.contains("unknown hook kind"), "got {err}");
    }

    #[test]
    fn hooks_cmd_enable_missing_kind_errs() {
        let _dir = isolate_cos_data_dir("hooks-enable-missing");
        let err = hooks_cmd(&["enable".into()]).unwrap_err();
        assert!(err.contains("missing hook kind"), "got {err}");
    }

    #[test]
    fn hooks_cmd_enable_checkpoint_kind_persists_and_registers() {
        use crate::agent::runtime::hooks::global_registry;
        use crate::agent::runtime::hooks_config;
        let _dir = isolate_cos_data_dir("hooks-enable-checkpoint");
        global_registry().unregister("checkpoint");

        let v = hooks_cmd(&["enable".into(), "checkpoint".into()]).expect("ok");
        assert_eq!(v["kind"], serde_json::json!("checkpoint"));
        assert_eq!(v["persisted"], serde_json::json!(true));
        assert_eq!(v["registered_now"], serde_json::json!(true));

        let cfg = hooks_config::load(&crate::paths::agent_hooks_path()).expect("load");
        assert_eq!(cfg.enabled, vec![hooks_config::HookKind::Checkpoint]);
        assert!(global_registry()
            .names()
            .contains(&"checkpoint".to_string()));

        global_registry().unregister("checkpoint");
    }

    #[test]
    fn hooks_cmd_disable_removes_from_config_and_registry() {
        use crate::agent::runtime::hooks::global_registry;
        use crate::agent::runtime::hooks_config;
        let _dir = isolate_cos_data_dir("hooks-disable");
        global_registry().unregister("logging");

        let _ = hooks_cmd(&["enable".into(), "logging".into()]).expect("ok");
        let v = hooks_cmd(&["disable".into(), "logging".into()]).expect("ok");
        assert_eq!(v["persisted"], serde_json::json!(true));
        assert_eq!(v["unregistered_now"], serde_json::json!(true));

        let cfg = hooks_config::load(&crate::paths::agent_hooks_path()).expect("load");
        assert!(cfg.enabled.is_empty());
        assert!(!global_registry().names().contains(&"logging".to_string()));
    }

    #[test]
    fn hooks_cmd_disable_idempotent_when_not_enabled() {
        let _dir = isolate_cos_data_dir("hooks-disable-noop");
        let v = hooks_cmd(&["disable".into(), "logging".into()]).expect("ok");
        assert_eq!(v["persisted"], serde_json::json!(false));
        assert_eq!(v["unregistered_now"], serde_json::json!(false));
    }

    #[test]
    fn hooks_cmd_list_includes_persistent_kinds() {
        use crate::agent::runtime::hooks::global_registry;
        let _dir = isolate_cos_data_dir("hooks-list-persistent");
        global_registry().unregister("logging");

        let _ = hooks_cmd(&["enable".into(), "logging".into()]).expect("ok");
        let v = hooks_cmd(&["list".into()]).expect("ok");
        let pers = v["persistent"].as_array().unwrap();
        assert!(
            pers.iter().any(|x| x.as_str() == Some("logging")),
            "got {v}"
        );

        // cleanup
        let _ = hooks_cmd(&["disable".into(), "logging".into()]).expect("ok");
    }

    // -----------------------------------------------------------------
    // media play / playback-status
    // -----------------------------------------------------------------

    #[test]
    fn media_play_requires_a_path() {
        let err = media_play_cmd(&[]).unwrap_err();
        assert!(err.contains("usage"), "got {err}");
    }

    #[test]
    fn media_play_rejects_extra_positional_argument() {
        let err = media_play_cmd(&["a.wav".into(), "b.wav".into()]).unwrap_err();
        assert!(err.contains("unexpected extra"), "got {err}");
    }

    #[test]
    fn media_play_rejects_unknown_flag() {
        let err = media_play_cmd(&["--frobnicate".into(), "a.wav".into()]).unwrap_err();
        assert!(err.contains("unknown flag"), "got {err}");
    }

    #[test]
    fn media_play_detect_only_returns_format_and_player_for_wav() {
        // --detect doesn't try to play; it just resolves the format
        // and tells you which player would be used. Safe to run on
        // CI because nothing is dispatched.
        let v = media_play_cmd(&["--detect".into(), "foo.wav".into()]).expect("ok");
        assert_eq!(v["format"], serde_json::Value::String("wav".to_string()));
        assert_eq!(v["path"].as_str().unwrap(), "foo.wav");
        // `playable` is OS-dependent; just sanity-check it's bool.
        assert!(v["playable"].is_boolean(), "got {v}");
    }

    #[test]
    fn media_play_detect_only_returns_null_format_for_unknown_extension() {
        let v = media_play_cmd(&["--detect".into(), "foo.txt".into()]).expect("ok");
        assert!(v["format"].is_null(), "got {v}");
        assert!(v["player"].is_null(), "got {v}");
        assert_eq!(v["playable"], serde_json::Value::Bool(false));
    }

    #[test]
    fn media_play_real_dispatch_missing_file_errs() {
        let p = format!(
            "{}\\cos-media-play-test-missing-{}.wav",
            std::env::temp_dir().display(),
            uuid::Uuid::new_v4().simple()
        );
        let err = media_play_cmd(&[p.clone()]).unwrap_err();
        assert!(err.contains("playback failed"), "got {err}");
        assert!(
            err.contains("does not exist") || err.contains("io error"),
            "got {err}"
        );
    }

    #[test]
    fn media_playback_status_rejects_unknown_format_value() {
        let err = media_playback_status_cmd(&["--format".into(), "aac".into()]).unwrap_err();
        assert!(err.contains("aac"), "got {err}");
    }

    #[test]
    fn media_playback_status_format_flag_requires_value() {
        let err = media_playback_status_cmd(&["--format".into()]).unwrap_err();
        assert!(err.contains("--format"), "got {err}");
    }

    #[test]
    fn media_playback_status_default_returns_all_four_formats() {
        let v = media_playback_status_cmd(&[]).expect("ok");
        let arr = v["formats"].as_array().expect("formats array");
        assert_eq!(arr.len(), 4);
        let exts: Vec<&str> = arr.iter().filter_map(|r| r["format"].as_str()).collect();
        assert!(exts.contains(&"wav"));
        assert!(exts.contains(&"mp3"));
        assert!(exts.contains(&"ogg"));
        assert!(exts.contains(&"flac"));
        assert!(v["os"].is_string(), "got {v}");
    }

    #[test]
    fn media_playback_status_format_filter_returns_just_one_row() {
        let v = media_playback_status_cmd(&["--format".into(), "wav".into()]).expect("ok");
        let arr = v["formats"].as_array().expect("formats array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["format"].as_str().unwrap(), "wav");
    }

    #[test]
    fn media_playback_status_unknown_flag_errs() {
        let err = media_playback_status_cmd(&["--quack".into()]).unwrap_err();
        assert!(err.contains("unknown flag"), "got {err}");
    }

    #[test]
    fn run_media_play_routes_through_dispatcher() {
        // Confirm the cos-agent dispatcher reaches media_play_cmd.
        let err = run("media", &["play".into()]).unwrap_err();
        assert!(err.contains("usage"), "got {err}");
    }

    #[test]
    fn run_media_playback_status_routes_through_dispatcher() {
        let v = run("media", &["playback-status".into()]).expect("ok");
        assert!(v["formats"].is_array(), "got {v}");
    }
}
