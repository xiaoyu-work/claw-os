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
//! ├── memory/         sqlite_fts, semantic, curator
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
pub mod diagnose;
pub mod display;
pub mod doctor_cli;
pub mod insights;
pub mod lifecycle;
pub mod llm;
pub mod media;
pub mod memory;
pub mod nudge;
pub mod prompt;
pub mod replay_cli;
pub mod run_log_cli;
pub mod runtime;
pub mod safety;
pub mod service;
pub mod setup;
pub mod shell_hooks;
pub mod skills;
pub mod summarise;
pub mod title;
pub mod tools;
pub mod util;
pub mod web;

use serde_json::{json, Value};

use crate::apps;
use crate::clawd::agent_client;

/// Recover from a poisoned [`std::sync::Mutex`] by taking the inner
/// data. Poisoning means a previous holder panicked, but for the
/// `LiveSink` / `ChatSink` aggregators that's strictly informational
/// — none of the data they hold becomes corrupted by a panic in
/// another tool-call thread, so silently dropping the poison flag
/// keeps the rest of the run going instead of aborting it. Callers
/// that need to surface partial state to JSON / stderr would
/// otherwise inherit a cascade of `.unwrap()` panics.
#[inline]
fn mlock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn should_render_evidence_warning(
    stdout_is_tty: bool,
    status: &crate::agent::runtime::evidence::EvidenceStatus,
) -> bool {
    !stdout_is_tty
        && !matches!(
            status,
            crate::agent::runtime::evidence::EvidenceStatus::Verified
                | crate::agent::runtime::evidence::EvidenceStatus::NotRequired
        )
}

#[derive(Debug, Default)]
struct TerminalOutputState {
    line_open: bool,
}

impl TerminalOutputState {
    fn reset(&mut self) {
        self.line_open = false;
    }

    fn write_text(&mut self, out: &mut impl std::io::Write, text: &str) {
        if text.is_empty() {
            return;
        }
        let _ = out.write_all(text.as_bytes());
        self.line_open = !text.ends_with('\n');
    }

    fn write_line(&mut self, out: &mut impl std::io::Write, line: &str) {
        if self.line_open {
            let _ = writeln!(out);
        }
        let _ = writeln!(out, "{line}");
        self.line_open = false;
    }

    fn finish_line(&mut self, out: &mut impl std::io::Write) {
        if self.line_open {
            let _ = writeln!(out);
            self.line_open = false;
        }
    }
}

/// Dispatch a `cos agent <command>` invocation.
pub fn run(command: &str, args: &[String]) -> Result<Value, String> {
    match command {
        "ask" => {
            let mut full = false;
            let mut session_id: Option<String> = None;
            let mut timeout_ms: Option<u64> = None;
            let mut positional: Vec<String> = Vec::with_capacity(args.len());
            let mut i = 0usize;
            while i < args.len() {
                match args[i].as_str() {
                    // Opt-in to the full JSON envelope (provider, model,
                    // session_id, task_id, turns, …). Without this the
                    // command prints just the model's plain-text answer
                    // — that's the common case for humans and shell
                    // pipelines (`cos agent ask "..." | tee log`). Note
                    // the flag is `--full`, not `--json`: main.rs's
                    // `extract_format` already consumes `--json` /
                    // `--compact` / `--plain` / `--pretty` for the
                    // global output-format selector, so any per-command
                    // flag with those names would never reach this
                    // handler.
                    "--full" => {
                        full = true;
                        i += 1;
                    }
                    "--no-full" => {
                        full = false;
                        i += 1;
                    }
                    "--session" => {
                        let value = args
                            .get(i + 1)
                            .filter(|value| !value.trim().is_empty())
                            .ok_or_else(|| "--session needs a non-empty id".to_string())?;
                        session_id = Some(value.clone());
                        i += 2;
                    }
                    "--timeout-secs" => {
                        let value = args
                            .get(i + 1)
                            .ok_or_else(|| "--timeout-secs needs a positive integer".to_string())?;
                        let seconds = value
                            .parse::<u64>()
                            .map_err(|_| "--timeout-secs needs a positive integer".to_string())?;
                        if seconds == 0 {
                            return Err("--timeout-secs needs a positive integer".to_string());
                        }
                        timeout_ms = Some(
                            seconds
                                .checked_mul(1_000)
                                .ok_or_else(|| "--timeout-secs is too large".to_string())?,
                        );
                        i += 2;
                    }
                    other if other.starts_with("--") => {
                        return Err(format!(
                            "unknown ask flag: {other}. supported: --full | --no-full | --session <id> | --timeout-secs <n>"
                        ));
                    }
                    _ => {
                        positional.push(args[i].clone());
                        i += 1;
                    }
                }
            }
            let prompt = positional.first().cloned().unwrap_or_default();
            if prompt.is_empty() {
                return Err(
                    "usage: cos agent ask \"<prompt>\" [--full] [--session <id>] [--timeout-secs <n>]".into(),
                );
            }
            let envelope = match timeout_ms {
                Some(timeout_ms) => agent_client::ask_in_session_with_timeout(
                    &prompt,
                    session_id.as_deref(),
                    timeout_ms,
                )?,
                None => agent_client::ask_in_session(&prompt, session_id.as_deref())?,
            };
            if full {
                Ok(envelope)
            } else {
                // Default: write the model's answer as plain text and
                // return Value::Null so the router skips re-rendering
                // the envelope as JSON. If `answer` is missing or not a
                // string (shouldn't happen for `status=ok`) fall back
                // to printing the whole envelope so we never silently
                // drop the model's reply.
                match envelope.get("answer").and_then(|v| v.as_str()) {
                    Some(answer) => {
                        println!("{answer}");
                        Ok(Value::Null)
                    }
                    None => Ok(envelope),
                }
            }
        }
        "chat" => chat_cmd(args),
        "serve" => web::serve(args),
        "budget" => budget_cmd(args),
        "override" => override_cmd(args),
        "status" => {
            let cfg = &crate::config::get().agent;
            let daemon = agent_client::daemon_status()?;
            let ready = setup::is_ready(cfg);
            let key_source = match setup::resolved_key_source(cfg) {
                Ok(Some(source)) => source.to_json(),
                Ok(None) | Err(_) => Value::Null,
            };

            // Most-recent session (best-effort; never fails the call).
            let last_session = match memory::sqlite_fts::MemoryDb::open_default() {
                Ok(db) => match db.sessions(1) {
                    Ok(mut v) if !v.is_empty() => {
                        let s = v.remove(0);
                        json!({
                            "session_id": s.session_id,
                            "title": s.title,
                            "last_ts_ms": s.last_ts_ms,
                            "message_count": s.message_count,
                        })
                    }
                    _ => Value::Null,
                },
                Err(_) => Value::Null,
            };

            let (ready_ok, ready_reason, fix, readiness_error) = match ready {
                Ok(()) => (true, Value::Null, Value::Null, Value::Null),
                Err(reason_json) => {
                    let parsed: Value =
                        serde_json::from_str(&reason_json).unwrap_or_else(|_| json!(reason_json));
                    let err = parsed
                        .get("error")
                        .and_then(|v| v.as_str())
                        .map(|s| json!(s))
                        .unwrap_or(parsed.clone());
                    let fix = parsed
                        .get("fix")
                        .cloned()
                        .unwrap_or_else(|| json!("cos agent setup text"));
                    (false, err, fix, parsed)
                }
            };

            Ok(json!({
                "ready": ready_ok,
                "ready_reason": ready_reason,
                "readiness_error": readiness_error,
                "fix": fix,
                "provider": cfg.provider,
                "model": cfg.model,
                "key_source": key_source,
                "credential_pool": {
                    "declared": llm::credential_pool::Pool::is_declared(cfg),
                    "credential_names": cfg.api_key_credentials,
                    "environment_variables": cfg.api_key_envs,
                    "strategy": cfg.pool_strategy,
                    "cooldown_secs": cfg.pool_cooldown_secs,
                },
                "needs_credential": setup::provider_needs_credential(&cfg.provider),
                "config_path": setup::config_path().display().to_string(),
                "last_session": last_session,
                "daemon": daemon,
                "hint": "for the full provider/tools/skills/usage report, run `cos agent doctor`",
            }))
        }
        "service" => agent_client::service_cmd(args),
        "recall" => recall_cmd(args),
        "sessions" => sessions_cmd(args),
        "ls" => lifecycle::ls(args),
        "show" => lifecycle::show(args),
        "stop" => lifecycle::stop(args),
        "undo" => lifecycle::undo(args),
        "resume" => lifecycle::resume(args),
        "setup" => setup::run(args),
        "notes" => notes_cmd(args),
        "memory" => memory_cmd(args),
        "skills" => skills_cmd(args),
        "mcp" => mcp_cmd(args),
        "todo" => todo_cmd(args),
        "doctor" => doctor_cli::doctor_cmd(args),
        "diagnose" => diagnose::diagnose_cmd(args),
        "dev" => dev_dispatch(args),
        other => Err(format!(
            "unknown command: {other}. try: setup | ask | chat | serve | budget | override | status | sessions | recall | service | notes | memory | skills | todo | mcp | doctor | diagnose | dev | ls | show | stop | undo | resume"
        )),
    }
}

/// `cos agent dev <subcmd>` — internal / power-user namespace.
///
/// These commands expose internal building blocks (token estimators,
/// classifiers, scrubbers, diagnostic dumps) that aren't part of the
/// primary user-facing agent surface but are still useful for
/// debugging, scripting, and downstream tooling.
fn dev_dispatch(args: &[String]) -> Result<Value, String> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("");
    let rest: Vec<String> = args.iter().skip(1).cloned().collect();
    match sub {
        "" | "list" | "--help" | "-h" => Ok(json!({
            "namespace": "cos agent dev",
            "summary": "Internal building blocks and power-user diagnostics. Not part of the stable user-facing surface.",
            "subcommands": [
                "insights", "usage", "audit", "replay", "run-log",
                "providers", "provider-doctor", "llm",
                "prompt", "tools", "guardrails", "approval",
                "redact", "think-scrub", "tokens", "title", "summarise", "classify",
                "display", "binary-ext", "file-safety", "context",
                "compress", "aux", "retry", "vision", "osv",
                "curator", "nudge", "shell-hooks", "media",
                "semantic", "interrupt", "learn", "hooks",
            ],
        })),
        "insights" => insights_cmd(&rest),
        "usage" => usage_cmd(&rest),
        "audit" => audit_cli::audit_cmd(&rest),
        "replay" => replay_cli::replay_cmd(&rest),
        "run-log" | "run_log" => run_log_cli::run_log_cmd(&rest),
        "providers" => providers_cmd(&rest),
        "provider-doctor" => provider_doctor_cmd(&rest),
        "llm" => llm_cmd(&rest),
        "prompt" => prompt_cmd(&rest),
        "tools" => tools_cmd(&rest),
        "guardrails" => guardrails_cmd(&rest),
        "approval" => approval_cmd(&rest),
        "redact" => redact_cmd(&rest),
        "think-scrub" => think_scrub_cmd(&rest),
        "tokens" => tokens_cmd(&rest),
        "title" => title_cmd(&rest),
        "summarise" | "summarize" => summarise_cmd(&rest),
        "classify" => classify_cmd(&rest),
        "display" => display_cmd(&rest),
        "binary-ext" => binary_ext_cmd(&rest),
        "file-safety" => file_safety_cmd(&rest),
        "context" => context_cmd(&rest),
        "compress" => compress_cmd(&rest),
        "aux" | "auxiliary" => aux_cmd(&rest),
        "retry" => retry_cmd(&rest),
        "vision" => vision_cmd(&rest),
        "osv" => osv_cmd(&rest),
        "curator" => curator_cmd(&rest),
        "nudge" => nudge_cmd(&rest),
        "shell-hooks" => shell_hooks_cmd(&rest),
        "media" => media_cmd(&rest),
        "semantic" => semantic_cmd(&rest),
        "interrupt" => interrupt_cmd(&rest),
        "learn" => learn_cmd(&rest),
        "hooks" => hooks_cmd(&rest),
        other => Err(format!(
            "unknown dev subcommand: {other}. run `cos agent dev` for the list."
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
    match iter.next().map(String::as_str) {
        Some("--kind") => iter
            .next()
            .cloned()
            .ok_or_else(|| "--kind requires a value".to_string()),
        Some(value) if !value.starts_with("--") => Ok(value.to_string()),
        Some(other) => Err(format!("unexpected flag: {other}")),
        None => Err("missing hook kind (positional or --kind <kind>)".to_string()),
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
fn semantic_cmd(args: &[String]) -> Result<Value, String> {
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

/// `cos agent insights [overall|recent|sessions] [n]` — aggregate
/// the JSONL run-record stream produced by every LLM call.
fn insights_cmd(args: &[String]) -> Result<Value, String> {
    use chrono::DateTime;
    use insights::InsightsFilter;

    let sub = args.first().map(|s| s.as_str()).unwrap_or("overall");
    let path = crate::paths::ai_run_log_path();

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

/// `cos agent memory [list|show|search|forget]` — user-facing
/// inspect/redact view of app-emitted memory rows. Apps push entries
/// in via the hidden `cos __memory remember` bridge under the
/// `memory.write` capability; this CLI surfaces what's been stored
/// and lets the user delete entries per row or per source.
fn memory_cmd(args: &[String]) -> Result<Value, String> {
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
            "user_root": crate::paths::agent_skills_dir().display().to_string(),
            "system_root": crate::paths::system_skills_dir().display().to_string(),
        })),
        "list" | "" => {
            let res = skills::loader::load_default();
            let names: Vec<&String> = res.skills.keys().collect();
            Ok(json!({
                "root": crate::paths::agent_skills_dir().display().to_string(),
                "user_root": crate::paths::agent_skills_dir().display().to_string(),
                "system_root": crate::paths::system_skills_dir().display().to_string(),
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
                    "source": s.origin.as_str(),
                    "version": s.manifest.version,
                    "license": s.manifest.license,
                    "author": s.manifest.author,
                    "homepage": s.manifest.homepage,
                    "allowed_tools": s.manifest.allowed_tools,
                    "triggers": s.manifest.triggers,
                    "body_bytes": s.body_bytes,
                    "disclosable": skills::disclosure::instruction_disclosable(s),
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
                resource_path: None,
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
            let m = metadata::lookup(&name).ok_or_else(|| format!("unknown model: {name}"))?;
            Ok(model_to_json(m))
        }
        other => Err(format!(
            "unknown llm subcommand: {other}. try: providers | models [--provider X] | model <name>"
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
/// Inspect the canonical system-prompt candidate frozen for a new session.
/// Existing sessions restore their persisted snapshot instead of rebuilding.
/// The candidate is composed by
/// [`crate::agent::prompt::build_system_prompt`] and includes:
///
///   1. Built-in scaffold (immutable in this binary).
///   2. Metadata-only installed Skill catalogue.
///   3. `MEMORY.md` and `USER.md` from the system notes store
///      (auto-loaded; capped per-file via
///      [`crate::agent::memory::notes::MAX_NOTE_CHARS_FOR_PROMPT`]).
///   4. Optional override file content from `--extra <path>`.
///
/// Useful for: debugging "why did the model behave this way?",
/// previewing a new MEMORY.md entry before committing, computing a
/// rough token budget for a new session, or capturing the candidate
/// to share in a bug report. Due nudges are reported separately because
/// they are request-local context, not part of the frozen prompt.
///
/// `--raw` returns the prompt as a single JSON string in the
/// `prompt` field (default). Without `--raw` the response also
/// includes a size breakdown and the currently due request-local context.
fn prompt_cmd(args: &[String]) -> Result<Value, String> {
    use std::path::PathBuf;

    let sub = args.first().map(|s| s.as_str()).unwrap_or("show");
    if sub != "show" && sub != "build" && !sub.is_empty() {
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
    let turn_context = crate::agent::prompt::build_turn_context_segments();
    let turn_context_chars: usize = turn_context
        .iter()
        .map(|segment| segment.content.chars().count())
        .sum();
    let turn_context_sources: Vec<&str> =
        turn_context.iter().map(|segment| segment.source).collect();
    if raw {
        Ok(json!({
            "prompt": prompt,
            "chars": prompt.chars().count(),
            "prompt_version": crate::agent::prompt::CANONICAL_PROMPT_VERSION,
            "scope": "new-session-candidate",
            "turn_context": turn_context.iter().map(|segment| json!({
                "source": segment.source,
                "content": segment.content,
            })).collect::<Vec<_>>(),
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
            "prompt_version": crate::agent::prompt::CANONICAL_PROMPT_VERSION,
            "scope": "new-session-candidate",
            "turn_context_chars": turn_context_chars,
            "turn_context_sources": turn_context_sources,
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
/// Internal: single-turn streaming helper. The async core that the
/// removed `cos agent stream` CLI used to call. Kept as a helper
/// so the streaming unit tests still exercise text accumulation,
/// tool-call surfacing, warnings, etc. on the no-tools / no-memory
/// path. Not reachable from any CLI today — `cos agent ask
/// --stream` uses `live_cmd_async` (the full agent loop) instead.
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
    let system = crate::agent::prompt::build_system_prompt_for(extra, Some(user_prompt));

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
            Ok(StreamEvent::Reasoning { .. }) => {}
            Ok(StreamEvent::ToolState { .. }) => {}
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

/// `cos agent chat [--session <id>] [--no-stream] [--no-memory]
/// [--show-tools] [--max-turns N]` — interactive multi-turn REPL.
///
/// Reads prompts from stdin one line at a time and routes each
/// through the same agent runtime as `cos agent live`. With memory
/// enabled, the session-id is preserved across turns so:
///   1. Every prompt and assistant turn is recorded under the
///      same FTS-searchable conversation;
///   2. The session title is generated once on the first turn
///      (matches `ask`/`live` semantics);
///   3. Recent turns are replayed directly into each model request,
///      so short follow-ups such as "1" retain conversational context;
///   4. `cos_recall` invocations from inside the model can search
///      the running conversation as it grows.
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
/// stdout after each turn. Pass `--no-stream` to use the equivalent
/// non-streaming continuation path (useful for non-TTY use).
///
/// Stdin EOF (Ctrl+D / closed pipe) exits cleanly.
///
/// ## What this is **not**
///
/// `cos agent chat` is the kernel Agent's own REPL — it is *not*
/// an App entry point. Installed Apps that want a one-shot LLM call
/// must use `cos ai chat --app <id>` instead. Passing `--app` to
/// `cos agent chat` is rejected; the App-gated path lives under
/// `cos ai chat` so the kernel Agent's CLI surface (memory, skills,
/// hooks, sessions, recall, …) is never exposed to third-party Apps.
fn chat_cmd(args: &[String]) -> Result<Value, String> {
    if args.iter().any(|a| a == "--app") {
        return Err(
            "`cos agent chat` is the kernel Agent's REPL and does not accept --app. \
             For one-shot App-gated calls use `cos ai chat --app <id> …` instead."
                .to_string(),
        );
    }

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
    setup::is_ready(cfg)?;
    // Build the provider once and reuse across turns. If the user
    // mid-REPL wants a different model, they can `/quit` and re-launch.
    let provider = crate::ai::gate::build_system_provider(cfg)
        .map_err(|e| format!("provider unavailable: {e}"))?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime: {e}"))?;
    let timeout = runtime::loop_::background_drain_timeout();
    runtime.block_on(async move {
        let outcome = chat_cmd_async(
            provider,
            cfg,
            explicit_session,
            streaming,
            use_memory,
            show_tools,
            max_turns_override,
        )
        .await;
        runtime::background::drain(timeout).await;
        outcome
    })
}

/// `cos agent budget` — inspect per-app AI spend.
///
/// Subcommands:
///   show <app>          Current period: used vs cap.
///   reset <app>         Roll over to next period (clears used).
///   history <app>       List past periods.
///
/// The system agent's usage is rolled up under the pseudo-app id
/// `system.agent`.
fn budget_cmd(args: &[String]) -> Result<Value, String> {
    use crate::ai::{budget, user_budget};

    let sub = args.first().map(String::as_str).unwrap_or("");
    match sub {
        "show" => {
            let app = args
                .get(1)
                .ok_or_else(|| "usage: cos agent budget show <app>".to_string())?;
            let store = budget::Store::open()?;
            let snap = store.current(app).map_err(|e| e.to_string())?;
            Ok(json!({
                "app": app,
                "period": snap.period,
                "units_used": snap.units_used,
            }))
        }
        "reset" => {
            let app = args
                .get(1)
                .ok_or_else(|| "usage: cos agent budget reset <app>".to_string())?;
            let store = budget::Store::open()?;
            store.reset(app).map_err(|e| e.to_string())?;
            Ok(json!({"app": app, "reset": true}))
        }
        "history" => {
            let app = args
                .get(1)
                .ok_or_else(|| "usage: cos agent budget history <app>".to_string())?;
            let store = budget::Store::open()?;
            let rows = store.history(app).map_err(|e| e.to_string())?;
            Ok(json!({"app": app, "history": rows}))
        }
        "user" => {
            // `cos agent budget user <show|path>` — inspect the per-user
            // aggregate cap. Writes go through the Cosmic Settings UI,
            // not the CLI; this is read-only.
            let user_sub = args.get(1).map(String::as_str).unwrap_or("show");
            match user_sub {
                "show" | "" => {
                    let cfg = user_budget::load()?;
                    let store = budget::Store::open()?;
                    let snap = store
                        .current(user_budget::USER_BUDGET_BUCKET)
                        .map_err(|e| e.to_string())?;
                    let cap = cfg.monthly_units;
                    let used = snap.units_used;
                    let available = if cap == 0 {
                        None
                    } else if used >= cap {
                        Some(0u64)
                    } else {
                        Some(cap - used)
                    };
                    Ok(json!({
                        "scope": "user",
                        "path": user_budget::config_path().display().to_string(),
                        "period": snap.period,
                        "units_used": used,
                        "units_cap": cap,
                        "unlimited": cap == 0,
                        "units_available": available,
                    }))
                }
                "path" => Ok(json!({
                    "scope": "user",
                    "path": user_budget::config_path().display().to_string(),
                })),
                other => Err(format!(
                    "unknown subcommand: cos agent budget user {other}. try: show | path"
                )),
            }
        }
        _ => Err("usage: cos agent budget <show|reset|history> <app>  |  \
             cos agent budget user <show|path>"
            .to_string()),
    }
}

/// `cos agent override <show|path|effective> <app>` — read-only
/// inspection of the per-user override file at
/// `$HOME/.config/cos/apps/<app>.json`. There is no `set` / `write`
/// subcommand by design: the Cosmic Settings UI is the sole writer.
fn override_cmd(args: &[String]) -> Result<Value, String> {
    use crate::ai::overrides;

    let sub = args.first().map(String::as_str).unwrap_or("");
    match sub {
        "show" => {
            let app = args
                .get(1)
                .ok_or_else(|| "usage: cos agent override show <app>".to_string())?;
            let ovr = overrides::load(app)?;
            Ok(json!({
                "app": app,
                "path": overrides::override_path(app).display().to_string(),
                "present": ovr.is_some(),
                "override": ovr,
            }))
        }
        "path" => {
            let app = args
                .get(1)
                .ok_or_else(|| "usage: cos agent override path <app>".to_string())?;
            Ok(json!({
                "app": app,
                "path": overrides::override_path(app).display().to_string(),
            }))
        }
        "effective" => {
            let app = args
                .get(1)
                .ok_or_else(|| "usage: cos agent override effective <app>".to_string())?;
            let apps_dir = std::env::var("COS_APPS_DIR")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|_| std::path::PathBuf::from("/usr/lib/cos/apps"));
            let installed = apps::discover(&apps_dir)
                .get(app)
                .cloned()
                .ok_or_else(|| format!("unknown app `{app}`"))?;
            let manifest_policy =
                installed.manifest.ai.as_ref().ok_or_else(|| {
                    format!("app `{app}` has no `ai` block — nothing to override")
                })?;
            let ovr = overrides::load(app)?;
            let disabled = ovr.as_ref().map(|o| o.disabled).unwrap_or(false);
            let effective = overrides::apply_to_policy(manifest_policy, ovr.as_ref());
            Ok(json!({
                "app": app,
                "disabled": disabled,
                "manifest": manifest_policy,
                "override": ovr,
                "effective": effective,
            }))
        }
        _ => Err("usage: cos agent override <show|path|effective> <app>".to_string()),
    }
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
    use std::collections::HashSet;
    use std::io::{BufRead, Write};
    use std::sync::{Arc, Mutex};

    // Apply --max-turns override locally without mutating global config.
    let mut cfg_owned = cfg_in.clone();
    if let Some(n) = max_turns_override {
        cfg_owned.max_turns = n;
    }
    let cfg = &cfg_owned;

    let mut session_id: String = explicit_session
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let mut exposure = crate::agent::tools::exposure::ToolExposureContext::from_current_session(
        Some(&session_id),
        None,
        crate::agent::tools::exposure::ExecutionHost::Direct,
        runtime::loop_::guardrails_from_cfg(cfg),
    )?;

    // Build the registry once. MCP servers attach the same way as
    // `live`/`ask`, so the model has the full toolbox.
    let mut tools = crate::agent::tools::registry::default_registry();
    tools.set_approval(runtime::loop_::approval_from_cfg(cfg));
    let _mcp_handles =
        runtime::loop_::attach_mcp_servers_for_cli(&mut tools, cfg, &mut exposure).await;

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

    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    // When the user is at an interactive terminal, the stream sink
    // already echoed the assistant's text to stderr (which is the
    // same terminal as stdout), so printing the assembled answer
    // again to stdout would duplicate it. Skip the second copy in
    // that case. When stdout is piped to a file or another command
    // we still want the canonical answer on stdout — the streaming
    // copy on stderr is the "progress" view there.
    let stdout_is_tty = std::io::IsTerminal::is_terminal(&std::io::stdout());

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
            let names = tools.names_for(&exposure);
            let _ = writeln!(e, "tools ({}): {}", names.len(), names.join(", "));
        }
    }

    let stdin = std::io::stdin();
    let mut input = Vec::new();
    let mut prompt_seq: u32 = 0;

    /// Stream sink shared across turns — re-used so allocation
    /// happens once. Each turn calls `reset()` before invoking
    /// the runtime so per-turn state doesn't bleed.
    ///
    /// `verbose_telemetry` controls the `[turn done finish=...]`
    /// telemetry line. We only want it when stdout is being piped
    /// somewhere (so a log consumer can see finish reasons); on an
    /// interactive terminal it's just noise after every reply.
    struct ChatSink {
        verbose_telemetry: bool,
        tool_calls: Mutex<Vec<serde_json::Value>>,
        announced_tools: Mutex<HashSet<String>>,
        warnings: Mutex<Vec<String>>,
        last_usage: Mutex<Option<crate::agent::llm::types::Usage>>,
        last_finish: Mutex<Option<crate::agent::llm::types::FinishReason>>,
        terminal: Arc<Mutex<TerminalOutputState>>,
        // Heartbeat keyed by tool_use id. Started when the runtime
        // dispatches a tool (via `ProgressSink::on_tool_start`),
        // cancelled when the result arrives. Without it the REPL
        // appeared frozen during slow filesystem walks — the user
        // saw the `[tool: name]` line and nothing else for 60s+.
        heartbeat: crate::agent::runtime::progress::Heartbeat,
    }
    impl ChatSink {
        fn new(verbose_telemetry: bool) -> Self {
            Self {
                verbose_telemetry,
                tool_calls: Mutex::new(Vec::new()),
                announced_tools: Mutex::new(HashSet::new()),
                warnings: Mutex::new(Vec::new()),
                last_usage: Mutex::new(None),
                last_finish: Mutex::new(None),
                terminal: Arc::new(Mutex::new(TerminalOutputState::default())),
                heartbeat: crate::agent::runtime::progress::Heartbeat::new(),
            }
        }
        fn reset(&self) {
            mlock(&self.tool_calls).clear();
            mlock(&self.announced_tools).clear();
            mlock(&self.warnings).clear();
            *mlock(&self.last_usage) = None;
            *mlock(&self.last_finish) = None;
            mlock(&self.terminal).reset();
        }

        fn announce_tool(&self, id: &str, name: &str, out: &mut impl Write) {
            let should_announce =
                id.is_empty() || mlock(&self.announced_tools).insert(id.to_string());
            if should_announce {
                mlock(&self.terminal).write_line(out, &format!("[tool: {name}]"));
            }
        }
    }
    impl StreamSink for ChatSink {
        fn on_event(&self, event: &StreamEvent) {
            let stderr = std::io::stderr();
            let mut e = stderr.lock();
            match event {
                StreamEvent::TextDelta { text } => {
                    mlock(&self.terminal).write_text(&mut e, text);
                    let _ = e.flush();
                }
                StreamEvent::ToolUseStart { id, name } => {
                    self.announce_tool(id, name, &mut e);
                }
                StreamEvent::ToolInputDelta { .. } => {}
                StreamEvent::ToolUse(call) => {
                    self.announce_tool(&call.id, &call.name, &mut e);
                    mlock(&self.tool_calls).push(serde_json::json!({
                        "id": call.id,
                        "name": call.name,
                    }));
                }
                StreamEvent::Reasoning { .. } => {}
                StreamEvent::ToolState { .. } => {}
                StreamEvent::Message(resp) => {
                    for block in &resp.content {
                        if let crate::agent::llm::types::ContentBlock::Text { text } = block {
                            mlock(&self.terminal).write_text(&mut e, text);
                        }
                    }
                    for call in &resp.tool_calls {
                        self.announce_tool(&call.id, &call.name, &mut e);
                        mlock(&self.tool_calls).push(serde_json::json!({
                            "id": call.id,
                            "name": call.name,
                        }));
                    }
                    let _ = e.flush();
                }
                StreamEvent::Done { finish, usage } => {
                    if self.verbose_telemetry {
                        mlock(&self.terminal)
                            .write_line(&mut e, &format!("[turn done finish={finish:?}]"));
                    } else {
                        mlock(&self.terminal).finish_line(&mut e);
                    }
                    *mlock(&self.last_usage) = Some(usage.clone());
                    *mlock(&self.last_finish) = Some(*finish);
                }
                StreamEvent::Warning { message } => {
                    mlock(&self.terminal)
                        .write_line(&mut e, &format!("[warning] {message}"));
                    mlock(&self.warnings).push(message.clone());
                }
            }
        }
    }

    impl crate::agent::runtime::progress::ProgressSink for ChatSink {
        fn on_tool_start(&self, id: &str, name: &str, _input: &serde_json::Value) {
            self.announce_tool(id, name, &mut std::io::stderr().lock());
            let terminal = Arc::clone(&self.terminal);
            self.heartbeat.start_with_callback(id, move |cancelled| {
                let mut terminal = mlock(&terminal);
                if cancelled.load(std::sync::atomic::Ordering::Acquire) {
                    return;
                }
                let mut stderr = std::io::stderr().lock();
                terminal.write_text(&mut stderr, ".");
                let _ = stderr.flush();
            });
        }

        fn on_tool_result(
            &self,
            id: &str,
            name: &str,
            ok: bool,
            _latency_ms: u64,
            _bytes_returned: usize,
            _content_preview: &str,
        ) {
            self.heartbeat.stop(id);
            {
                let mut stderr = std::io::stderr().lock();
                mlock(&self.terminal).finish_line(&mut stderr);
            }
            if !ok {
                mlock(&self.terminal).write_line(
                    &mut std::io::stderr().lock(),
                    &format!("[tool failed: {name}]"),
                );
            }
        }
    }

    let sink_obj = Arc::new(ChatSink::new(!stdout_is_tty));

    let clean_exit = loop {
        // Prompt user (to stderr so stdout stays clean for
        // assistant text).
        {
            let mut e = stderr.lock();
            let _ = write!(e, "you> ");
            let _ = e.flush();
        }
        input.clear();
        let n = match stdin.lock().read_until(b'\n', &mut input) {
            Ok(n) => n,
            Err(e) => {
                return Err(format!("stdin read error: {e}"));
            }
        };
        if n == 0 {
            // EOF
            let _ = writeln!(stderr.lock(), "\n[eof]");
            break true;
        }

        let decoded = String::from_utf8_lossy(&input);
        let had_invalid_utf8 = matches!(&decoded, std::borrow::Cow::Owned(_));
        if had_invalid_utf8 {
            tracing::debug!("chat input contained invalid UTF-8; invalid bytes were removed");
        }
        let line = decoded.trim();
        let repaired_command = had_invalid_utf8.then(|| line.replace('\u{FFFD}', ""));
        let command_line = repaired_command.as_deref().unwrap_or(line);
        if command_line.is_empty() {
            continue;
        }

        // Slash commands.
        if let Some(rest) = command_line.strip_prefix('/') {
            let mut parts = rest.split_whitespace();
            let cmd = parts.next().unwrap_or("");
            match cmd {
                "quit" | "exit" | "q" => {
                    break true;
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
                    exposure.set_conversation_session_id(session_id.clone());
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
                                        let content = memory::history::sanitize_stored_content(
                                            &r.role, &r.content,
                                        );
                                        let snippet: String = content.chars().take(140).collect();
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
                    let names = tools.names_for(&exposure);
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
            let progress: Arc<dyn crate::agent::runtime::progress::ProgressSink> = sink_obj.clone();
            if let Some(db) = &memory_db {
                runtime::loop_::ask_with_stream_continuation_exposure(
                    provider.clone(),
                    cfg,
                    &user_prompt,
                    &tools,
                    &exposure,
                    db,
                    &session_id,
                    100,
                    sink,
                    progress,
                )
                .await
            } else {
                runtime::loop_::ask_with_stream_exposure(
                    provider.clone(),
                    cfg,
                    &user_prompt,
                    &tools,
                    &exposure,
                    None,
                    sink,
                    progress,
                )
                .await
            }
        } else if let Some(db) = &memory_db {
            runtime::loop_::ask_with_memory_continuation_exposure(
                provider.clone(),
                cfg,
                &user_prompt,
                &tools,
                &exposure,
                db,
                &session_id,
                100,
            )
            .await
        } else {
            runtime::loop_::ask_with_exposure(
                provider.clone(),
                cfg,
                &user_prompt,
                &tools,
                &exposure,
            )
            .await
        };

        match result {
            Ok(ask_result) => {
                // Streaming sink echoes incremental text to stderr.
                // When stderr+stdout share a terminal (interactive
                // use), printing the final answer to stdout would
                // duplicate the response on screen. Only emit the
                // stdout copy when piping or when streaming is off
                // (i.e. the user hasn't seen the text yet).
                let print_final_to_stdout = !(streaming && stdout_is_tty);
                if print_final_to_stdout {
                    let mut o = stdout.lock();
                    let _ = writeln!(o, "{}", ask_result.answer);
                    let _ = o.flush();
                }

                // Per-turn telemetry footer (turn index, model,
                // session id) is debugging metadata. Keep it for
                // piped/logged runs but suppress on an interactive
                // terminal so the conversation reads cleanly.
                if !stdout_is_tty {
                    let mut e = stderr.lock();
                    let _ = writeln!(
                        e,
                        "[turn {} done; turns={} model={} session={}]",
                        prompt_seq, ask_result.turns, ask_result.model, ask_result.session_id
                    );
                }
                if should_render_evidence_warning(stdout_is_tty, &ask_result.evidence.status) {
                    let _ = writeln!(
                        stderr.lock(),
                        "[warning: response could not be fully verified]"
                    );
                } else if !matches!(
                    ask_result.evidence.status,
                    crate::agent::runtime::evidence::EvidenceStatus::Verified
                        | crate::agent::runtime::evidence::EvidenceStatus::NotRequired
                ) {
                    tracing::debug!(
                        status = ?ask_result.evidence.status,
                        warnings = ?ask_result.evidence.warnings,
                        "interactive response evidence was incomplete"
                    );
                }
                if ask_result
                    .fallback
                    .as_ref()
                    .is_some_and(|fallback| fallback.degraded)
                {
                    if let Some(fallback) = &ask_result.fallback {
                        let _ = writeln!(
                            stderr.lock(),
                            "[provider fallback: {}/{} -> {}/{}]",
                            fallback.primary_provider,
                            fallback.primary_model,
                            fallback.active_provider,
                            fallback.active_model
                        );
                    }
                }
            }
            Err(err) => {
                let _ = writeln!(stderr.lock(), "[error] {err}");
                // Don't break — let the user retry / clear / quit.
            }
        }
    };

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
            crate::config::AgentConfig {
                provider: name.to_string(),
                api_key_credential: canonical_credential.map(String::from),
                api_key_env: canonical_env.map(String::from),
                ..Default::default()
            }
        };

        let (configured, configuration_error) =
            provider_build_status(name, &active_model, &probe_cfg);

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
            "configuration_error": configuration_error,
        }));
    }

    let active_configured = entries.iter().any(|entry| {
        entry.get("active") == Some(&Value::Bool(true))
            && entry.get("configured") == Some(&Value::Bool(true))
    });
    let active_configuration_error = entries
        .iter()
        .find(|entry| entry.get("active") == Some(&Value::Bool(true)))
        .and_then(|entry| entry.get("configuration_error"))
        .cloned()
        .unwrap_or(Value::Null);

    Ok(json!({
        "active": active,
        "active_model": cfg.agent.model.clone(),
        "active_configured": active_configured,
        "active_configuration_error": active_configuration_error,
        "probe_credentials": probe_credentials,
        "providers": entries,
        "count": entries.len(),
    }))
}

fn provider_build_status(
    name: &str,
    model: &str,
    cfg: &crate::config::AgentConfig,
) -> (bool, Value) {
    match llm::registry::build(name, model, cfg) {
        Ok(provider) => (provider.is_configured(), Value::Null),
        Err(error) => (false, setup::provider_configuration_error(cfg, &error)),
    }
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
    } else if active_name.is_empty() {
        json!({
            "attempted": false,
            "reason": "no text-model provider configured — run `cos agent setup text` first (probe needs an active provider)",
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
    // NOTE: we deliberately do NOT wrap with `ai::gate::wrap_for_system`
    // here. The probe is an OS-internal diagnostic — it's the kernel
    // calling its own LLM stack to confirm the user's freshly-typed
    // configuration works. It is NOT an app-→AI call, so it should
    // not consume the system-agent budget bucket and it must not be
    // gated by the caps system (`cos agent setup` is typically run
    // from a user TTY with no upstream session, so requiring
    // `COS_SESSION` here would make the post-setup probe always fail
    // with "Permission denied (no active session)").

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
        llm::LlmError::CredentialStore { .. } => "credential_store",
        llm::LlmError::Parse(_) => "parse",
        llm::LlmError::Stream(_) => "stream",
        llm::LlmError::Internal(_) => "internal",
        // Added by HIGH-3/MEDIUM-12 fix: upstream returned a syntactically
        // malformed payload (bad JSON in SSE, oversized headers, etc.).
        // Distinct from `parse` (which we used for any decode failure)
        // because here the bug is on the provider's side, not in the
        // request we built.
        llm::LlmError::UpstreamMalformed(_) => "upstream_malformed",
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
    let registry = tools::registry::default_registry();
    let exposure = tools::exposure::ToolExposureContext::from_current_session(
        None,
        None,
        tools::exposure::ExecutionHost::Direct,
        crate::agent::runtime::loop_::guardrails_from_cfg(cfg),
    )
    .unwrap_or_else(|_| {
        tools::exposure::ToolExposureContext::isolated(
            crate::agent::runtime::loop_::guardrails_from_cfg(cfg),
        )
    });
    let mut registry = registry;
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
            let names: Vec<&str> = if unfiltered {
                registry.names_unfiltered()
            } else {
                registry.names_for(&exposure)
            };
            let entries: Vec<Value> = names
                .iter()
                .filter_map(|n| {
                    registry.descriptor_unfiltered(n).map(|descriptor| {
                        let decision = registry.exposure_decision(&exposure, n);
                        json!({
                            "name": n,
                            "description": descriptor.description,
                            "permitted": decision.is_visible(),
                            "hidden_reason": decision.reason(),
                        })
                    })
                })
                .collect();
            Ok(json!({
                "registered_total": registry.names_unfiltered().len(),
                "permitted_count": registry.names_for(&exposure).len(),
                "source": exposure.client().source.as_str(),
                "attended": exposure.client().attended,
                "local": exposure.client().local,
                "owner_uid": exposure.owner_uid(),
                "authority_session_id": exposure.authority_session_id(),
                "capability_generation": exposure.capability_generation(),
                "transports": exposure.transports().map(|transport| transport.as_str()).collect::<Vec<_>>(),
                "extensions": exposure.enabled_extensions().collect::<Vec<_>>(),
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
                .descriptor_unfiltered(&name)
                .ok_or_else(|| format!("tool '{name}' not registered"))?;
            let decision = registry.exposure_decision(&exposure, &name);
            Ok(json!({
                "name": tool.name,
                "description": tool.description,
                "input_schema": tool.input_schema,
                "permitted": decision.is_visible(),
                "hidden_reason": decision.reason(),
            }))
        }
        "llm-list" => {
            let llm_tools = registry.as_llm_tools_for(&exposure);
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
/// `~/.config/cos/config.json` is actually parsed the way you expect
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
    use crate::agent::tools::todo::{TodoItem, TodoStatus};

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
    let provider = crate::ai::gate::wrap_for_system(provider);

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
            let content = memory::history::sanitize_stored_content(&row.role, &row.content);
            crate::agent::display::render_message(role_from_str(&row.role), &content, &cfg)
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
    rows.sort_by_key(|row| std::cmp::Reverse(row.0));
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
    // Sequential per-package querying was the dominant cost here.
    // A typical Cargo.lock contains hundreds of crates; at ~300 ms
    // round-trip per OSV.dev call that's a 30+ s `cos agent osv
    // check`. Fan out to a small worker pool — capped at 8 to stay
    // under OSV.dev's polite-client guidance and to keep memory
    // bounded — and merge results in the original order so the
    // emitted JSON remains stable.
    use futures_util::stream::{self, StreamExt};
    const CONCURRENCY: usize = 8;
    let scored: Vec<(usize, Vec<crate::agent::safety::osv::OsvVulnerability>)> =
        rt.block_on(async {
            stream::iter(pkgs.iter().enumerate())
                .map(|(idx, pkg)| async move {
                    let vulns = crate::agent::safety::osv::query(pkg)
                        .await
                        .unwrap_or_else(|e| {
                            tracing::warn!(
                                "osv: {} {} {}: {}",
                                pkg.ecosystem,
                                pkg.name,
                                pkg.version,
                                e
                            );
                            Vec::new()
                        });
                    (idx, vulns)
                })
                .buffer_unordered(CONCURRENCY)
                .collect()
                .await
        });
    let mut by_idx: Vec<Vec<crate::agent::safety::osv::OsvVulnerability>> =
        (0..pkgs.len()).map(|_| Vec::new()).collect();
    for (idx, v) in scored {
        by_idx[idx] = v;
    }
    let mut total_vulns = 0u64;
    let mut results = Vec::new();
    for (pkg, vulns) in pkgs.iter().zip(by_idx) {
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
    if !out
        .tool_deny
        .iter()
        .any(|name| name == "cos_oauth_login")
    {
        out.tool_deny.push("cos_oauth_login".to_string());
    }
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
            tools.set_approval(crate::agent::runtime::loop_::approval_from_cfg(cfg));
            let exposure = tools::exposure::ToolExposureContext::from_current_session(
                None,
                None,
                tools::exposure::ExecutionHost::Direct,
                crate::agent::runtime::loop_::guardrails_from_cfg(cfg),
            )
            .unwrap_or_else(|_| {
                tools::exposure::ToolExposureContext::isolated(
                    crate::agent::runtime::loop_::guardrails_from_cfg(cfg),
                )
            });
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
                "tools_permitted": tools.names_for(&exposure).len(),
                "tools": tools.names_for(&exposure),
                "source": exposure.client().source.as_str(),
                "attended": exposure.client().attended,
                "local": exposure.client().local,
                "authority_session_id": exposure.authority_session_id(),
                "capability_generation": exposure.capability_generation(),
                "transports": exposure.transports().map(|transport| transport.as_str()).collect::<Vec<_>>(),
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
                        url: None,
                        bearer_env: None,
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
            tools.set_approval(crate::agent::runtime::loop_::approval_from_cfg(cfg));
            let exposure = tools::exposure::ToolExposureContext::from_current_session(
                None,
                None,
                tools::exposure::ExecutionHost::Direct,
                crate::agent::runtime::loop_::guardrails_from_cfg(&merged),
            )?
            .for_external_mcp();
            let registry = Arc::new(tools);
            let server = McpServer::new_with_context(
                "cos-agent",
                env!("CARGO_PKG_VERSION"),
                registry,
                exposure,
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

/// `cos agent usage [overall|provider <name>|model <name>|session <id>|app <id>|verb <name>]`
/// `[--since <ISO>] [--until <ISO>] [--ok|--error] [--app <id>] [--verb <name>]`
/// — filtered aggregation over `ai.jsonl`. Mirrors `agent insights
/// overall` for the unfiltered case but adds the AND-combined filter
/// set from [`crate::agent::llm::usage::UsageQuery`].
fn usage_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::llm::usage::{aggregate_path_filtered, default_log_path, UsageQuery};
    use chrono::DateTime;
    let mut query = UsageQuery::default();
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
        "app" => {
            query.app_id = Some(
                args.get(1)
                    .cloned()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| "usage: cos agent usage app <id>".to_string())?,
            );
            2
        }
        "verb" => {
            query.verb = Some(
                args.get(1)
                    .cloned()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| "usage: cos agent usage verb <name>".to_string())?,
            );
            2
        }
        other => {
            return Err(format!(
                "unknown usage scope: {other}. try: overall | provider <name> | model <name> | session <id> | app <id> | verb <name>"
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
            "--app" => {
                let v = args
                    .get(i + 1)
                    .cloned()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| "--app needs <id>".to_string())?;
                query.app_id = Some(v);
                i += 2;
            }
            "--verb" => {
                let v = args
                    .get(i + 1)
                    .cloned()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| "--verb needs <name>".to_string())?;
                query.verb = Some(v);
                i += 2;
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
            "app_id": query.app_id,
            "verb": query.verb,
            "since": query.since.map(|d| d.to_rfc3339()),
            "until": query.until.map(|d| d.to_rfc3339()),
            "status_ok": query.status_ok,
        },
        "total": summary.total,
        "by_provider": summary.by_provider,
        "by_model": summary.by_model,
        "by_session": summary.by_session,
        "by_app": summary.by_app,
        "by_verb": summary.by_verb,
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
        let mut primary_cfg = cfg.clone();
        primary_cfg.model = model.clone();
        let provider = crate::ai::gate::build_system_provider(&primary_cfg)
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
        let provider = crate::ai::gate::wrap_for_system(provider);
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

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/agent.rs"));
}
