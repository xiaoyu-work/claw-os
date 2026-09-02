use super::{curator_drafts, llm, memory};
use serde_json::{json, Value};

/// `cos agent curator propose <session_id> [--accept] [--limit <n>]`
/// `[--no-require-acceptance] [--min-tools <n>] [--min-turns <n>]`
/// `[--no-save]`
///
/// `cos agent curator drafts list [--status proposed|accepted|rejected]`
/// `cos agent curator drafts show <draft_id>`
/// `cos agent curator drafts accept <draft_id> [--note "<text>"]`
/// `cos agent curator drafts reject <draft_id> [--note "<text>"]`
/// `cos agent curator drafts delete <draft_id>`
pub(super) fn curator_cmd(args: &[String]) -> Result<Value, String> {
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
            let config = crate::config::current_snapshot();
            let cfg = &config.agent;
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
    let config = crate::config::current_snapshot();
    let cfg = &config.agent;
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
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/curator_commands.rs"
    ));
}
