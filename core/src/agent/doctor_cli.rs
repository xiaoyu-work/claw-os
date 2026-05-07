//! `cos agent doctor` — single-shot holistic self-check.
//!
//! Aggregates many small checks into one pass/warn/fail summary so
//! operators can answer "is my agent stack healthy?" in one
//! command. Each subcheck is intentionally cheap (file stats,
//! in-memory counts, no provider network round-trips by default).
//! For network-level provider validation, use the existing
//! `cos agent provider-doctor` instead — `doctor` calls it only in
//! summary form.
//!
//! Output shape (always JSON, machine-friendly):
//!
//! ```json
//! {
//!   "status": "ok" | "warn" | "fail",
//!   "summary": {"ok": N, "warn": N, "fail": N},
//!   "checks": {
//!     "provider":  { "status": "ok",   ... },
//!     "engines":   { "status": "warn", ... },
//!     "memory":    { "status": "ok",   ... },
//!     "audit":     { "status": "ok",   ... },
//!     "run_log":   { "status": "ok",   ... },
//!     "skills":    { "status": "ok",   ... },
//!     "hooks":     { "status": "ok",   ... },
//!     "honcho":    { "status": "ok",   ... }
//!   }
//! }
//! ```
//!
//! Severity rollup:
//! - `fail`: at least one check is `fail` (e.g., the configured
//!   provider isn't even registered)
//! - `warn`: no fails but at least one `warn` (e.g., no engines
//!   linked, semantic store disabled, no recent activity)
//! - `ok`:   everything green
//!
//! `warn` is the default for "configured-but-empty" cases — those
//! are normal on a fresh install and shouldn't trip alerting.

use std::path::Path;

use serde_json::{json, Value};

use crate::agent::llm;
use crate::agent::memory::sqlite_fts::MemoryDb;
use crate::agent::runtime::hooks::global_registry;
use crate::agent::runtime::hooks_config;
use crate::agent::skills;
use crate::agent::tools;
use crate::config;
use crate::model::engines::engines_linked;
use crate::paths;

/// Top-level dispatcher. Currently no subcommands; future
/// `cos agent doctor --quick` / `--filter <name>` flags can be
/// added here without breaking the JSON contract.
pub fn doctor_cmd(args: &[String]) -> Result<Value, String> {
    let quick = args.iter().any(|a| a == "--quick");

    let provider = check_provider();
    let engines = check_engines();
    let memory = check_memory();
    let audit = if quick {
        json!({"status": "skipped", "reason": "--quick"})
    } else {
        check_log_file(&paths::agent_audit_log_path(), "audit")
    };
    let run_log = if quick {
        json!({"status": "skipped", "reason": "--quick"})
    } else {
        check_log_file(&paths::llm_run_log_path(), "run_log")
    };
    let skills = check_skills();
    let hooks = check_hooks();
    let honcho = check_honcho();

    let checks = json!({
        "provider": provider,
        "engines": engines,
        "memory": memory,
        "audit": audit,
        "run_log": run_log,
        "skills": skills,
        "hooks": hooks,
        "honcho": honcho,
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
        "checks": checks,
    }))
}

// ---------------------------------------------------------------------------
// Subchecks. Each returns a JSON object with at least
// `{"status": "ok"|"warn"|"fail"|"skipped", ...}`.
// ---------------------------------------------------------------------------

fn check_provider() -> Value {
    let cfg = &config::get().agent;
    let registered = llm::registry::is_registered(&cfg.provider);
    let available = llm::available_providers();
    let status = if registered { "ok" } else { "fail" };
    json!({
        "status": status,
        "active": cfg.provider,
        "registered": registered,
        "available": available,
        "model": cfg.model,
        "max_turns": cfg.max_turns,
    })
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
        Err(e) => json!({
            "status": "fail",
            "path": paths::agent_memory_db_path().display().to_string(),
            "error": e.to_string(),
        }),
    };

    // Semantic store is opt-in (needs an embedder configured). When
    // it returns Ok(None), report disabled rather than failed.
    let semantic = match crate::agent::memory::semantic::SemanticStore::open_default() {
        Ok(Some(s)) => json!({
            "status": "ok",
            "path": paths::agent_semantic_db_path().display().to_string(),
            "row_count": s.count(None).unwrap_or(0),
        }),
        Ok(None) => json!({
            "status": "warn",
            "configured": false,
            "reason": "no embedder configured",
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
        return json!({
            "status": "warn",
            "label": label,
            "path": path.display().to_string(),
            "reason": "log file not yet created",
        });
    }
    let lines = match std::fs::read_to_string(path) {
        Ok(s) => s.lines().count() as u64,
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

fn check_honcho() -> Value {
    use crate::agent::memory::honcho::HonchoConfig;
    match HonchoConfig::from_env() {
        None => json!({
            "status": "ok",
            "configured": false,
            "reason": "HONCHO_BASE_URL not set",
        }),
        Some(cfg) => json!({
            "status": "ok",
            "configured": true,
            "base_url": cfg.base_url,
            "workspace_id": cfg.workspace_id,
            "auth": if cfg.api_key.is_some() { "bearer" } else { "none" },
            "timeout_secs": cfg.timeout_secs,
        }),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn args(strs: &[&str]) -> Vec<String> {
        strs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn doctor_returns_top_level_shape() {
        let v = doctor_cmd(&args(&[])).unwrap();
        assert!(v.get("status").is_some());
        assert!(v.get("summary").is_some());
        let checks = v.get("checks").unwrap().as_object().unwrap();
        for k in [
            "provider", "engines", "memory", "audit", "run_log", "skills", "hooks", "honcho",
        ] {
            assert!(checks.contains_key(k), "missing check: {k}");
            assert!(
                checks[k].get("status").is_some(),
                "check {k} missing status"
            );
        }
    }

    #[test]
    fn doctor_summary_matches_subcheck_counts() {
        let v = doctor_cmd(&args(&[])).unwrap();
        let summary = v.get("summary").unwrap().as_object().unwrap();
        let checks = v.get("checks").unwrap().as_object().unwrap();
        let mut ok = 0u32;
        let mut warn = 0u32;
        let mut fail = 0u32;
        for (_k, c) in checks {
            match c.get("status").and_then(|s| s.as_str()).unwrap_or("ok") {
                "ok" => ok += 1,
                "warn" => warn += 1,
                "fail" => fail += 1,
                _ => {}
            }
        }
        assert_eq!(summary["ok"], json!(ok));
        assert_eq!(summary["warn"], json!(warn));
        assert_eq!(summary["fail"], json!(fail));
    }

    #[test]
    fn quick_mode_skips_log_file_scans() {
        let v = doctor_cmd(&args(&["--quick"])).unwrap();
        let checks = v.get("checks").unwrap();
        assert_eq!(checks["audit"]["status"], json!("skipped"));
        assert_eq!(checks["run_log"]["status"], json!("skipped"));
    }

    #[test]
    fn check_provider_reports_active_provider() {
        let v = check_provider();
        assert!(v.get("active").is_some());
        assert!(v.get("registered").is_some());
        assert!(v.get("available").is_some());
        let status = v.get("status").and_then(|s| s.as_str()).unwrap();
        assert!(matches!(status, "ok" | "fail"));
    }

    #[test]
    fn check_engines_returns_list_and_status() {
        let v = check_engines();
        let linked = v.get("linked").unwrap().as_array().unwrap();
        let status = v.get("status").and_then(|s| s.as_str()).unwrap();
        if linked.is_empty() {
            assert_eq!(status, "warn");
        } else {
            assert_eq!(status, "ok");
        }
    }

    #[test]
    fn check_memory_attaches_stats_block_when_db_open() {
        // The default DB lives at agent_memory_db_path() and may or
        // may not exist; check_memory creates it on demand. Either
        // way the stats block should be present (object, possibly
        // with all-zero counts on a fresh install).
        let v = check_memory();
        let memory_db = v.get("memory_db").expect("memory_db field");
        // Only assert the stats sub-shape when the memory_db itself
        // opened successfully — fail-path doesn't carry stats.
        if memory_db.get("status").and_then(|s| s.as_str()) == Some("ok") {
            let stats = memory_db.get("stats").expect("stats field");
            assert!(stats.is_object(), "stats must be an object");
            assert!(stats.get("messages_last_7d").is_some());
            assert!(stats.get("total_sessions").is_some());
        }
    }

    #[test]
    fn check_log_file_warns_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("does-not-exist.jsonl");
        let v = check_log_file(&p, "test");
        assert_eq!(v["status"], json!("warn"));
        assert_eq!(v["label"], json!("test"));
    }

    #[test]
    fn check_log_file_reports_lines_when_present() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("present.jsonl");
        std::fs::write(&p, "{}\n{}\n{}\n").unwrap();
        let v = check_log_file(&p, "test");
        assert_eq!(v["status"], json!("ok"));
        assert_eq!(v["lines"], json!(3));
    }

    #[test]
    fn check_honcho_unconfigured_when_env_unset() {
        // We snapshot env vars to avoid stomping on other tests.
        let saved = std::env::var("HONCHO_BASE_URL").ok();
        std::env::remove_var("HONCHO_BASE_URL");
        let v = check_honcho();
        if let Some(s) = saved {
            std::env::set_var("HONCHO_BASE_URL", s);
        }
        assert_eq!(v["status"], json!("ok"));
        assert_eq!(v["configured"], json!(false));
    }

    #[test]
    fn check_skills_warns_on_load_errors_only() {
        let v = check_skills();
        let status = v.get("status").and_then(|s| s.as_str()).unwrap();
        let errors = v.get("errors").and_then(|n| n.as_u64()).unwrap_or(0);
        if errors > 0 {
            assert_eq!(status, "warn");
        } else {
            assert_eq!(status, "ok");
        }
    }

    #[test]
    fn check_hooks_returns_registered_and_persisted() {
        let v = check_hooks();
        assert!(v.get("registered").is_some());
        assert!(v.get("persisted").is_some());
        assert!(v.get("config_path").is_some());
    }
}
