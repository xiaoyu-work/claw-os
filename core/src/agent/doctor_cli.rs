//! `cos agent doctor` — single-shot holistic self-check.
//!
//! Aggregates many small checks into one pass/warn/fail summary so
//! operators can answer "is my agent stack healthy?" in one
//! command. By default every check is cheap (file stats, in-memory
//! counts, JSONL scans of the local run log); no network calls.
//! Pass `--probe-network` to also issue a one-shot live ping to
//! the active provider.
//!
//! Doctor folds in the data exposed by these (now dev-only)
//! commands so users don't have to chase them down individually:
//!
//! - `cos agent dev providers`         → per-provider config matrix
//! - `cos agent dev provider-doctor`   → live ping (with `--probe-network`)
//! - `cos agent dev usage`             → token totals
//! - `cos agent dev insights`          → run-log overall summary
//! - `cos agent dev audit summary`     → audit log by-kind summary
//!
//! Flags:
//! - `--quick` skips all log scans (audit, run_log, usage, insights)
//!   and never issues network probes. Cheapest possible check.
//! - `--probe-network` issues a single live `chat()` request to the
//!   active provider (skipped for `mock`, `llama_local`).
//! - `--probe-timeout <secs>` (default 30) bounds the network probe.
//!
//! Output shape (always JSON, machine-friendly):
//!
//! ```json
//! {
//!   "status": "ok" | "warn" | "fail",
//!   "summary": {"ok": N, "warn": N, "fail": N},
//!   "flags": {"quick": bool, "probe_network": bool, ...},
//!   "checks": {
//!     "provider":  { "status": "ok", "active": "...", "matrix": [...], "network_probe": {...} },
//!     "engines":   { "status": "warn", ... },
//!     "memory":    { "status": "ok",   ... },
//!     "audit":     { "status": "ok", "summary": {...}, ... },
//!     "run_log":   { "status": "ok",   ... },
//!     "usage":     { "status": "ok", "total": {...} },
//!     "insights":  { "status": "ok", "overall": {...} },
//!     "skills":    { "status": "ok",   ... },
//!     "hooks":     { "status": "ok",   ... }
//!   }
//! }
//! ```
//!
//! Severity rollup:
//! - `fail`: at least one check is `fail` (e.g., the configured
//!   provider isn't even registered, or `--probe-network` failed)
//! - `warn`: no fails but at least one `warn` (e.g., no engines
//!   linked, semantic store disabled, no recent activity)
//! - `ok`:   everything green
//!
//! `warn` is the default for "configured-but-empty" cases — those
//! are normal on a fresh install and shouldn't trip alerting.

use std::path::Path;

use serde_json::{json, Value};

use crate::agent::audit_cli;
use crate::agent::llm;
use crate::agent::memory::sqlite_fts::MemoryDb;
use crate::agent::runtime::hooks::global_registry;
use crate::agent::runtime::hooks_config;
use crate::agent::skills;
use crate::agent::tools;
use crate::config;
use crate::model::engines::engines_linked;
use crate::paths;

/// Top-level dispatcher.
///
/// Flags:
/// - `--quick` skips audit/run_log/usage/insights scans and the
///   network probe. Cheapest possible run.
/// - `--probe-network` issues a one-shot live ping to the active
///   provider (skipped for mock / llama_local).
/// - `--probe-timeout <secs>` (default 30) bounds the network probe.
pub fn doctor_cmd(args: &[String]) -> Result<Value, String> {
    let mut quick = false;
    let mut probe_network = false;
    let mut probe_timeout_secs: u64 = 30;

    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--quick" => {
                quick = true;
                i += 1;
            }
            "--probe-network" => {
                probe_network = true;
                i += 1;
            }
            "--probe-timeout" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| "--probe-timeout needs <secs>".to_string())?;
                probe_timeout_secs = v
                    .parse::<u64>()
                    .map_err(|_| format!("--probe-timeout must be a positive integer (got '{v}')"))?;
                if probe_timeout_secs == 0 {
                    return Err("--probe-timeout must be > 0".into());
                }
                i += 2;
            }
            other => {
                return Err(format!(
                    "unknown doctor flag: {other}. try: --quick | --probe-network | --probe-timeout <secs>"
                ));
            }
        }
    }
    // --quick implicitly disables --probe-network (no network in
    // quick mode, period).
    let effective_probe_network = probe_network && !quick;

    let provider = check_provider(effective_probe_network, probe_timeout_secs);
    let engines = check_engines();
    let memory = check_memory();
    let audit = if quick {
        json!({"status": "skipped", "reason": "--quick"})
    } else {
        check_audit(&paths::agent_audit_log_path())
    };
    let run_log = if quick {
        json!({"status": "skipped", "reason": "--quick"})
    } else {
        check_log_file(&paths::ai_run_log_path(), "run_log")
    };
    let usage = if quick {
        json!({"status": "skipped", "reason": "--quick"})
    } else {
        check_usage()
    };
    let insights = if quick {
        json!({"status": "skipped", "reason": "--quick"})
    } else {
        check_insights()
    };
    let skills = check_skills();
    let hooks = check_hooks();
    let media = check_media_modalities();

    let checks = json!({
        "provider": provider,
        "engines": engines,
        "memory": memory,
        "audit": audit,
        "run_log": run_log,
        "usage": usage,
        "insights": insights,
        "skills": skills,
        "hooks": hooks,
        "media": media,
    });

    let mut ok_n = 0u32;
    let mut warn_n = 0u32;
    let mut fail_n = 0u32;
    for (_k, v) in checks.as_object().unwrap() {
        match v.get("status").and_then(|s| s.as_str()).unwrap_or("ok") {
            "ok" => ok_n += 1,
            "warn" => warn_n += 1,
            "fail" => fail_n += 1,
            _ => {} // "skipped" doesn't count
        }
    }

    let overall = if fail_n > 0 {
        "fail"
    } else if warn_n > 0 {
        "warn"
    } else {
        "ok"
    };

    Ok(json!({
        "status": overall,
        "summary": { "ok": ok_n, "warn": warn_n, "fail": fail_n },
        "flags": {
            "quick": quick,
            "probe_network": effective_probe_network,
            "probe_network_requested": probe_network,
            "probe_timeout_secs": probe_timeout_secs,
        },
        "checks": checks,
    }))
}

// ---------------------------------------------------------------------------
// Subchecks. Each returns a JSON object with at least
// `{"status": "ok"|"warn"|"fail"|"skipped", ...}`.
// ---------------------------------------------------------------------------

fn check_provider(probe_network: bool, probe_timeout_secs: u64) -> Value {
    let cfg = &config::get().agent;
    let registered = llm::registry::is_registered(&cfg.provider);
    let available = llm::available_providers();

    // Reuse the dev-only `providers` / `provider-doctor` commands so
    // doctor and the focused dev commands always agree on shape.
    // --probe-credentials gives us the encrypted-store presence
    // signal in addition to env_present, so the report can
    // distinguish "key in env" from "key in keychain".
    let probe_args: Vec<String> = if probe_network {
        vec!["--probe-network".into(), "--timeout".into(), probe_timeout_secs.to_string()]
    } else {
        Vec::new()
    };
    let raw = super::provider_doctor_cmd(&probe_args).unwrap_or_else(|e| {
        json!({"error": e})
    });

    let matrix = raw.get("providers").cloned().unwrap_or(Value::Array(vec![]));
    let active_configured = raw
        .get("active_configured")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let configuration_error = raw
        .get("active_configuration_error")
        .cloned()
        .unwrap_or(Value::Null);
    // Only surface `network_probe` when the caller actually asked
    // for one; otherwise `provider_doctor_cmd` returns a "static
    // only" stub that's just noise here (the `flags` block already
    // tells the caller whether a probe was attempted).
    let network_probe = if probe_network {
        raw.get("doctor")
            .and_then(|d| d.get("active_probe"))
            .cloned()
    } else {
        None
    };

    let probe_failed = network_probe
        .as_ref()
        .and_then(|p| p.get("ok"))
        .and_then(|v| v.as_bool())
        .map(|ok| !ok)
        .unwrap_or(false);
    let probe_attempted = network_probe
        .as_ref()
        .and_then(|p| p.get("attempted"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let status = if !registered {
        "fail"
    } else if !active_configured {
        // Active provider registered but no key in env nor keychain
        // — `ask` would fail at request time. Treat as fail so
        // `doctor` exit code can gate scripts.
        "fail"
    } else if probe_attempted && probe_failed {
        "fail"
    } else {
        "ok"
    };

    let mut out = json!({
        "status": status,
        "active": cfg.provider,
        "registered": registered,
        "available": available,
        "model": cfg.model,
        "max_turns": cfg.max_turns,
        "configured": active_configured,
        "configuration_error": configuration_error,
        "matrix": matrix,
    });
    if let Some(p) = network_probe {
        out["network_probe"] = p;
    }
    out
}

fn check_engines() -> Value {
    let linked = engines_linked();
    // No engines linked is a warn (works for cloud-only setups but
    // local LLM/embedding will be unavailable).
    let status = if linked.is_empty() { "warn" } else { "ok" };
    json!({
        "status": status,
        "linked": linked,
        "count": linked.len(),
    })
}

/// One-line readiness summary per media modality. Each entry is
/// `{provider, ready, reason}`. Overall status is `warn` if any
/// configured modality (provider != none) fails the credential
/// check, otherwise `ok`.
fn check_media_modalities() -> Value {
    use crate::agent::setup::Modality;
    let mut entries = serde_json::Map::new();
    let mut warn = false;
    for m in [Modality::Tts, Modality::Stt, Modality::ImageGen, Modality::Embed] {
        let snap = crate::agent::setup::status_for(m);
        let provider = snap.get("provider").and_then(|v| v.as_str()).unwrap_or("none");
        let ready = snap.get("ready").and_then(|v| v.as_bool()).unwrap_or(false);
        let reason = snap.get("reason").cloned().unwrap_or(Value::Null);
        // `embed.provider=auto` means "use the bundled local embedding
        // stack when available". When it is missing (for example Linux
        // arm64 images), setup/status reports the modality as
        // unconfigured so the user can choose a provider explicitly.
        let state = if matches!(m, Modality::Embed) && provider == "local" {
            if ready {
                "system-local"
            } else {
                warn = true;
                "configured-but-not-ready"
            }
        } else if provider == "none" || provider.is_empty() {
            "unconfigured"
        } else if ready {
            "ready"
        } else {
            warn = true;
            "configured-but-not-ready"
        };
        entries.insert(
            m.name().into(),
            json!({
                "state": state,
                "provider": provider,
                "ready": ready,
                "reason": reason,
            }),
        );
    }
    let status = if warn { "warn" } else { "ok" };
    json!({
        "status": status,
        "hint": "configure with `cos agent setup [tts|stt|imagegen|embed]` or `cos agent setup all`",
        "modalities": entries,
    })
}

fn check_memory() -> Value {
    let now_ms: i64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let memory_db = match MemoryDb::open_default() {
        Ok(db) => {
            let total = db.count_total().unwrap_or(0);
            // Best-effort enrichment with the same surface
            // ``cos agent sessions stats`` exposes — if stats fails
            // for any reason we still report the basic count. This
            // lets a single ``cos agent doctor`` answer "how much
            // history have I accumulated, and is it worth purging?"
            // without forcing the user to run a second command.
            let stats_block = match db.stats(now_ms) {
                Ok(s) => json!({
                    "total_sessions": s.total_sessions as u64,
                    "titled_sessions": s.titled_sessions as u64,
                    "messages_last_1d": s.messages_last_1d as u64,
                    "messages_last_7d": s.messages_last_7d as u64,
                    "messages_last_30d": s.messages_last_30d as u64,
                    "oldest_ts_ms": s.oldest_ts_ms,
                    "newest_ts_ms": s.newest_ts_ms,
                }),
                Err(_) => Value::Null,
            };
            json!({
                "status": "ok",
                "path": paths::agent_memory_db_path().display().to_string(),
                "total_messages": total,
                "stats": stats_block,
            })
        }
        Err(e) => {
            let err_str = e.to_string();
            let path = paths::agent_memory_db_path();
            let parent = path.parent().map(|p| p.display().to_string()).unwrap_or_default();
            let fix = if err_str.to_lowercase().contains("permission denied")
                || err_str.to_lowercase().contains("eperm")
            {
                Some(format!(
                    "ensure `{parent}` is writable by the user running cos, e.g.: sudo install -d -m 0755 -o $USER {parent}"
                ))
            } else if err_str.to_lowercase().contains("no such file")
                || err_str.to_lowercase().contains("enoent")
            {
                Some(format!(
                    "create the parent dir: sudo install -d -m 0755 -o $USER {parent}"
                ))
            } else {
                None
            };
            let mut entry = json!({
                "status": "fail",
                "path": path.display().to_string(),
                "error": err_str,
            });
            if let Some(f) = fix {
                entry["fix"] = json!(f);
            }
            entry
        }
    };

    // Semantic store is opt-in (needs an embedder configured). When
    // it returns Ok(None), report disabled rather than failed.
    use crate::agent::memory::semantic::{SemanticStore, SemanticStoreExt};
    let semantic = match SemanticStore::open_default() {
        Ok(Some(s)) => json!({
            "status": "ok",
            "path": paths::agent_semantic_db_path().display().to_string(),
            "row_count": s.count(None).unwrap_or(0),
        }),
        Ok(None) => json!({
            "status": "warn",
            "configured": false,
            "reason": "no embedder configured",
            "fix": "set `agent.semantic_embedder` in ~/.config/cos/config.json (or COS_CONFIG_PATH) — semantic memory is opt-in",
        }),
        Err(e) => json!({
            "status": "fail",
            "error": e.to_string(),
        }),
    };

    let notes_dir = paths::agent_notes_dir();
    let notes = if notes_dir.exists() {
        let count = std::fs::read_dir(&notes_dir)
            .map(|d| d.filter_map(Result::ok).count())
            .unwrap_or(0);
        json!({"status": "ok", "path": notes_dir.display().to_string(), "files": count})
    } else {
        json!({"status": "ok", "path": notes_dir.display().to_string(), "files": 0})
    };

    // Memory DB itself is a hard requirement, so its status drives
    // the rollup. Semantic warn shouldn't bubble to fail.
    let parent_status = memory_db
        .get("status")
        .and_then(|s| s.as_str())
        .unwrap_or("ok");
    let semantic_status = semantic
        .get("status")
        .and_then(|s| s.as_str())
        .unwrap_or("ok");
    let combined = match (parent_status, semantic_status) {
        ("fail", _) | (_, "fail") => "fail",
        ("warn", _) | (_, "warn") => "warn",
        _ => "ok",
    };
    json!({
        "status": combined,
        "memory_db": memory_db,
        "semantic": semantic,
        "notes": notes,
    })
}

fn check_log_file(path: &Path, label: &str) -> Value {
    if !path.exists() {
        // Demoted from "warn" to "ok": an empty/absent log on a fresh
        // install just means the user hasn't run agent commands yet,
        // which isn't actionable and was the most common false-alarm
        // in `cos agent doctor` output.
        return json!({
            "status": "ok",
            "label": label,
            "path": path.display().to_string(),
            "lines": 0,
            "bytes": 0,
            "note": "log file not yet created (no agent activity recorded yet)",
        });
    }
    // Stream the line count instead of slurping the whole log.
    // The audit / run log can grow to hundreds of MB on long-lived
    // installs; a `read_to_string` here is enough to OOM a small
    // container running `cos agent doctor`.
    let lines = match std::fs::File::open(path) {
        Ok(f) => {
            use std::io::BufRead;
            std::io::BufReader::new(f).lines().count() as u64
        }
        Err(e) => {
            return json!({
                "status": "fail",
                "label": label,
                "path": path.display().to_string(),
                "error": e.to_string(),
            });
        }
    };
    let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    json!({
        "status": "ok",
        "label": label,
        "path": path.display().to_string(),
        "lines": lines,
        "bytes": bytes,
    })
}

/// Audit log: file stats + parsed-entry summary (events by kind,
/// distinct session count, first/last timestamp). Wraps
/// `audit_cli::audit_cmd(["summary"])` so doctor agrees with the
/// dev command.
fn check_audit(path: &Path) -> Value {
    let mut base = check_log_file(path, "audit");
    // If the log doesn't exist (warn) or failed to read (fail),
    // skip the parsed summary — there's nothing to parse.
    let stat = base.get("status").and_then(|s| s.as_str()).unwrap_or("ok");
    if stat != "ok" {
        return base;
    }
    // Use a fresh args vec to avoid coupling to defaults — explicit
    // --path keeps the test/sandbox stories aligned with what doctor
    // reports.
    let args: Vec<String> = vec![
        "summary".into(),
        "--path".into(),
        path.display().to_string(),
    ];
    match audit_cli::audit_cmd(&args) {
        Ok(summary) => {
            // Strip the duplicate `path` field — base already has it.
            let mut summary = summary;
            if let Some(object) = summary.as_object_mut() {
                object.remove("path");
            }
            if summary
                .pointer("/integrity/legacy")
                .and_then(Value::as_bool)
                == Some(true)
            {
                base["status"] = json!("warn");
                base["warning"] =
                    json!("agent audit log is legacy and will be hash-chained on the next append");
            } else if summary.pointer("/integrity/status").and_then(Value::as_str)
                == Some("skipped")
            {
                base["status"] = json!("warn");
                base["warning"] =
                    json!("agent audit log is too large for automatic doctor verification; run `cos agent audit verify` explicitly");
            } else if summary.pointer("/integrity/valid").and_then(Value::as_bool) == Some(false) {
                base["status"] = json!("fail");
                base["error"] = json!("agent audit hash-chain verification failed");
            } else if summary
                .pointer("/integrity/warnings")
                .and_then(Value::as_array)
                .is_some_and(|warnings| !warnings.is_empty())
            {
                base["status"] = json!("warn");
                base["warning"] =
                    json!("agent audit chain is valid but contains recovery warnings");
            }
            base["summary"] = summary;
        }
        Err(error) => {
            base["status"] = json!("fail");
            base["error"] = json!(format!("agent audit verification failed: {error}"));
        }
    }
    base
}

/// Run-log usage totals: tokens / requests / errors, last 7 days.
/// Wraps `usage_cmd(["overall", "--since", <7d ago>])` so doctor
/// and the dev command agree. Window is 7 days because that's the
/// most useful "is anything happening on this box?" signal; users
/// who want all-time can run `cos agent dev usage overall`.
fn check_usage() -> Value {
    use chrono::Utc;
    let since = Utc::now() - chrono::Duration::days(7);
    let args: Vec<String> = vec![
        "overall".into(),
        "--since".into(),
        since.to_rfc3339(),
    ];
    match super::usage_cmd(&args) {
        Ok(v) => {
            let parse_errors = v
                .get("parse_errors")
                .and_then(|n| n.as_u64())
                .unwrap_or(0);
            // Parse errors mean we have a corrupt or partially-flushed
            // log; surface them but don't fail the whole report.
            let status = if parse_errors > 0 { "warn" } else { "ok" };
            json!({
                "status": status,
                "scope": "overall",
                "since": since.to_rfc3339(),
                "log": v.get("log").cloned().unwrap_or(Value::Null),
                "total": v.get("total").cloned().unwrap_or(Value::Null),
                "parse_errors": parse_errors,
            })
        }
        Err(e) => json!({
            "status": "fail",
            "error": e,
        }),
    }
}

/// Run-log insights overview: requests by provider/model, last 7
/// days. Wraps `insights_cmd(["overall", "--since", <7d ago>])`.
fn check_insights() -> Value {
    use chrono::Utc;
    let since = Utc::now() - chrono::Duration::days(7);
    let args: Vec<String> = vec![
        "overall".into(),
        "--since".into(),
        since.to_rfc3339(),
    ];
    match super::insights_cmd(&args) {
        Ok(v) => {
            let per_provider_count = v
                .get("per_provider")
                .and_then(|p| p.as_object())
                .map(|o| o.len() as u64)
                .unwrap_or(0);
            json!({
                "status": "ok",
                "scope": "overall",
                "since": since.to_rfc3339(),
                "log": v.get("log").cloned().unwrap_or(Value::Null),
                "overall": v.get("overall").cloned().unwrap_or(Value::Null),
                "providers_seen": per_provider_count,
            })
        }
        Err(e) => json!({
            "status": "fail",
            "error": e,
        }),
    }
}

fn check_skills() -> Value {
    let load = skills::loader::load_default();
    let loaded = load.loaded_count();
    let errors = load.errors.len();
    // Skill load errors don't break the agent (it just runs without
    // those skills) but they're worth surfacing.
    let status = if errors > 0 { "warn" } else { "ok" };
    json!({
        "status": status,
        "loaded": loaded,
        "disabled": load.disabled.len(),
        "errors": errors,
    })
}

fn check_hooks() -> Value {
    let registry = global_registry();
    let names = registry.names();
    let cfg = hooks_config::load(&paths::agent_hooks_path()).unwrap_or_default();
    let persisted: Vec<String> = cfg
        .enabled
        .iter()
        .map(|k| k.canonical().to_string())
        .collect();
    json!({
        "status": "ok",
        "registered": names.clone(),
        "registered_count": names.len(),
        "persisted": persisted,
        "config_path": paths::agent_hooks_path().display().to_string(),
    })
}

// Force the `tools` import to count even though doctor doesn't
// itself enumerate them — provider/skills/memory/etc cover the
// surface this command was originally going to expose. Keeping the
// import allows the future `--detail` flag to enumerate the full
// permitted tool set without re-importing.
#[allow(dead_code)]
fn _force_tools_use() {
    let _ = tools::registry::default_registry();
}

/// Shim matching the [`crate::agent::tools::cos_proxy::PrimitiveFn`]
/// signature so the LLM can call `cos_doctor` directly. The
/// `command` argument is ignored — `doctor_cmd` is single-shot
/// and consumes only flags (`--quick`, `--probe-network`,
/// `--probe-timeout <secs>`).
pub fn doctor_primitive(_command: &str, args: &[String]) -> Result<Value, String> {
    doctor_cmd(args)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/doctor_cli.rs"
    ));
}
