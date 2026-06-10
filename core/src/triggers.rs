//! Event-driven trigger engine — the proactive half of the agent OS.
//!
//! ClawOS already (a) ingests structured app/system events into clawd's
//! `context.event` log and (b) can run the agent autonomously as a
//! background job (the agent service queue). What was missing was the
//! glue between them: a rules engine that watches the event stream and,
//! when an event matches a rule, submits an agent task. That is what
//! turns the agent from "answers when asked" into "notices and acts" —
//! the core promise of an agent-native OS.
//!
//! A rule is `when {source? / event_type? / contains?} then run <prompt>`.
//! Rules are JSON at `<data>/triggers/rules/<id>.json`. [`run`] exposes
//! the `cos triggers <add|list|remove|enable|disable|run|tick>` CLI.
//!
//! `tick` is meant to be called every minute by the same external
//! scheduler that drives `cron tick` (or by clawd). It scans
//! `context.event` records newer than a persisted cursor and fires every
//! enabled matching rule by enqueuing an agent job via
//! [`crate::agent::service::Store`]; the agent-service worker then runs
//! the job like any other. The append-only event log is never mutated —
//! progress is tracked in a sidecar cursor file.

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// One trigger rule: a match condition plus the prompt to run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerRule {
    pub id: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Match the event `source` exactly. `None` = match any source.
    #[serde(default)]
    pub source: Option<String>,
    /// Match the event `event_type` exactly. `None` = match any type.
    #[serde(default)]
    pub event_type: Option<String>,
    /// Require this substring somewhere in the raw event JSON.
    /// `None` = no substring constraint.
    #[serde(default)]
    pub contains: Option<String>,
    /// Prompt submitted to the agent when the rule fires.
    pub prompt: String,
    /// Optional cap on agent turns for the fired job.
    #[serde(default)]
    pub max_turns: Option<u32>,
    /// Epoch-ms of the last time this rule fired (diagnostics only).
    #[serde(default)]
    pub last_fired_ms: Option<u64>,
}

fn default_true() -> bool {
    true
}

fn triggers_dir() -> PathBuf {
    crate::paths::data_dir().join("triggers")
}
fn rules_dir() -> PathBuf {
    triggers_dir().join("rules")
}
fn cursor_path() -> PathBuf {
    triggers_dir().join(".cursor")
}
fn rule_path(id: &str) -> PathBuf {
    rules_dir().join(format!("{id}.json"))
}

/// Filesystem-safe rule id (mirrors the cron / skills conventions).
fn sanitize_id(id: &str) -> Option<String> {
    if id.is_empty() || id.starts_with('.') {
        return None;
    }
    let ok = id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.');
    if ok {
        Some(id.to_string())
    } else {
        None
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Extract `--name value` from an argument list.
fn flag(args: &[String], name: &str) -> Option<String> {
    let key = format!("--{name}");
    args.iter()
        .position(|a| a == &key)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

/// First positional (non-flag) arg, or `--id <v>` as a fallback.
fn positional_or_id(args: &[String]) -> Option<String> {
    args.iter()
        .find(|a| !a.starts_with("--"))
        .cloned()
        .or_else(|| flag(args, "id"))
}

/// CLI entry — dispatched from the router under `cos triggers`.
pub fn run(command: &str, args: &[String]) -> Result<Value, String> {
    match command {
        "add" => cmd_add(args),
        "list" => cmd_list(),
        "remove" | "rm" => cmd_remove(args),
        "enable" => cmd_set_enabled(args, true),
        "disable" => cmd_set_enabled(args, false),
        "run" => cmd_run(args),
        "tick" => cmd_tick(),
        other => Err(format!(
            "unknown command '{other}'. valid: add | list | remove | enable | disable | run | tick"
        )),
    }
}

fn save_rule(rule: &TriggerRule) -> Result<(), String> {
    let id = sanitize_id(&rule.id).ok_or_else(|| format!("invalid rule id '{}'", rule.id))?;
    fs::create_dir_all(rules_dir()).map_err(|e| format!("create triggers dir: {e}"))?;
    let data = serde_json::to_string_pretty(rule).map_err(|e| format!("serialize rule: {e}"))?;
    // Atomic write: temp + rename so a crash can't leave a half-written rule.
    let path = rule_path(&id);
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, data).map_err(|e| format!("write rule: {e}"))?;
    fs::rename(&tmp, &path).map_err(|e| format!("commit rule: {e}"))?;
    Ok(())
}

fn load_rule(id: &str) -> Result<TriggerRule, String> {
    let id = sanitize_id(id).ok_or_else(|| format!("invalid rule id '{id}'"))?;
    let raw = fs::read_to_string(rule_path(&id)).map_err(|_| format!("no such trigger '{id}'"))?;
    serde_json::from_str(&raw).map_err(|e| format!("corrupt trigger '{id}': {e}"))
}

fn load_rules() -> Vec<TriggerRule> {
    let mut out = Vec::new();
    let rd = match fs::read_dir(rules_dir()) {
        Ok(rd) => rd,
        Err(_) => return out,
    };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) == Some("json") {
            if let Ok(raw) = fs::read_to_string(&p) {
                if let Ok(rule) = serde_json::from_str::<TriggerRule>(&raw) {
                    out.push(rule);
                }
            }
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

fn read_cursor() -> usize {
    fs::read_to_string(cursor_path())
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

fn write_cursor(n: usize) {
    let _ = fs::create_dir_all(triggers_dir());
    let _ = fs::write(cursor_path(), n.to_string());
}

/// Does `rule` match the parsed `ev` (with `raw` its source line)?
fn rule_matches(rule: &TriggerRule, ev: &Value, raw: &str) -> bool {
    if let Some(src) = &rule.source {
        if ev.get("source").and_then(|v| v.as_str()) != Some(src.as_str()) {
            return false;
        }
    }
    if let Some(et) = &rule.event_type {
        if ev.get("event_type").and_then(|v| v.as_str()) != Some(et.as_str()) {
            return false;
        }
    }
    if let Some(sub) = &rule.contains {
        if !raw.contains(sub.as_str()) {
            return false;
        }
    }
    true
}

/// Compose the agent prompt for a fired rule, tagging the originating
/// event so the agent has context for why it woke up.
fn fired_prompt(rule: &TriggerRule, ev: &Value) -> String {
    let src = ev.get("source").and_then(|v| v.as_str()).unwrap_or("?");
    let et = ev.get("event_type").and_then(|v| v.as_str()).unwrap_or("?");
    format!(
        "{}\n\n[Fired by ClawOS trigger '{}' on system event: source={src}, type={et}]",
        rule.prompt, rule.id
    )
}

/// Enqueue an agent job. The agent-service worker (clawd / `cos agent`
/// runner) claims and executes it; this only submits.
fn submit_job(prompt: String, max_turns: Option<u32>) -> Result<String, String> {
    let store = crate::agent::service::Store::open_default()
        .map_err(|e| format!("open agent job store: {e}"))?;
    let job = store
        .submit(prompt, None, max_turns, None, None)
        .map_err(|e| format!("submit job: {e}"))?;
    Ok(job.id)
}

fn cmd_add(args: &[String]) -> Result<Value, String> {
    let id = flag(args, "id").ok_or_else(|| {
        "usage: cos triggers add --id <id> --prompt <text> [--source S] [--event-type T] [--contains STR] [--max-turns N]"
            .to_string()
    })?;
    let id = sanitize_id(&id)
        .ok_or_else(|| format!("invalid id '{id}' (allowed: alphanumerics, '-', '_', '.')"))?;
    let prompt = flag(args, "prompt").ok_or_else(|| "--prompt is required".to_string())?;
    let max_turns = flag(args, "max-turns").and_then(|s| s.parse::<u32>().ok());
    let rule = TriggerRule {
        id: id.clone(),
        enabled: true,
        source: flag(args, "source"),
        event_type: flag(args, "event-type"),
        contains: flag(args, "contains"),
        prompt,
        max_turns,
        last_fired_ms: None,
    };
    save_rule(&rule)?;
    Ok(json!({ "ok": true, "id": id, "rule": rule }))
}

fn cmd_list() -> Result<Value, String> {
    let rules = load_rules();
    Ok(json!({ "count": rules.len(), "triggers": rules }))
}

fn cmd_remove(args: &[String]) -> Result<Value, String> {
    let id =
        positional_or_id(args).ok_or_else(|| "usage: cos triggers remove <id>".to_string())?;
    let id = sanitize_id(&id).ok_or_else(|| format!("invalid id '{id}'"))?;
    match fs::remove_file(rule_path(&id)) {
        Ok(()) => Ok(json!({ "ok": true, "removed": id })),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(format!("no such trigger '{id}'"))
        }
        Err(e) => Err(format!("remove trigger '{id}': {e}")),
    }
}

fn cmd_set_enabled(args: &[String], enabled: bool) -> Result<Value, String> {
    let verb = if enabled { "enable" } else { "disable" };
    let id =
        positional_or_id(args).ok_or_else(|| format!("usage: cos triggers {verb} <id>"))?;
    let mut rule = load_rule(&id)?;
    rule.enabled = enabled;
    save_rule(&rule)?;
    Ok(json!({ "ok": true, "id": rule.id, "enabled": enabled }))
}

fn cmd_run(args: &[String]) -> Result<Value, String> {
    let id = positional_or_id(args).ok_or_else(|| "usage: cos triggers run <id>".to_string())?;
    let mut rule = load_rule(&id)?;
    let job_id = submit_job(rule.prompt.clone(), rule.max_turns)?;
    rule.last_fired_ms = Some(now_ms());
    let _ = save_rule(&rule);
    Ok(json!({ "ok": true, "id": rule.id, "job_id": job_id }))
}

/// Scan `context.event` records newer than the cursor and fire every
/// enabled matching rule. Returns what fired. Intended to be invoked
/// once per minute by an external scheduler (like `cron tick`).
fn cmd_tick() -> Result<Value, String> {
    let rules = load_rules();
    let content = fs::read_to_string(crate::paths::context_events_log_path()).unwrap_or_default();
    let lines: Vec<&str> = content.lines().collect();
    let cursor = read_cursor().min(lines.len());

    let mut fired: Vec<Value> = Vec::new();
    let mut last_fired: std::collections::HashMap<String, u64> = std::collections::HashMap::new();

    for raw in &lines[cursor..] {
        if raw.trim().is_empty() {
            continue;
        }
        let ev: Value = match serde_json::from_str(raw) {
            Ok(v) => v,
            Err(_) => continue,
        };
        for rule in rules.iter().filter(|r| r.enabled) {
            if !rule_matches(rule, &ev, raw) {
                continue;
            }
            match submit_job(fired_prompt(rule, &ev), rule.max_turns) {
                Ok(job_id) => {
                    fired.push(json!({
                        "rule": rule.id,
                        "job_id": job_id,
                        "source": ev.get("source"),
                        "event_type": ev.get("event_type"),
                    }));
                    last_fired.insert(rule.id.clone(), now_ms());
                }
                Err(e) => fired.push(json!({ "rule": rule.id, "error": e })),
            }
        }
    }

    // Advance the cursor past everything we just scanned.
    write_cursor(lines.len());

    // Persist last-fired timestamps (best-effort).
    for (id, ts) in &last_fired {
        if let Ok(mut r) = load_rule(id) {
            r.last_fired_ms = Some(*ts);
            let _ = save_rule(&r);
        }
    }

    Ok(json!({
        "processed": lines.len().saturating_sub(cursor),
        "cursor": lines.len(),
        "fired": fired,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(source: &str, etype: &str) -> Value {
        json!({ "source": source, "event_type": etype, "payload": {} })
    }

    fn rule(source: Option<&str>, etype: Option<&str>, contains: Option<&str>) -> TriggerRule {
        TriggerRule {
            id: "r".into(),
            enabled: true,
            source: source.map(str::to_string),
            event_type: etype.map(str::to_string),
            contains: contains.map(str::to_string),
            prompt: "do it".into(),
            max_turns: None,
            last_fired_ms: None,
        }
    }

    #[test]
    fn empty_rule_matches_anything() {
        let e = ev("mail", "received");
        assert!(rule_matches(&rule(None, None, None), &e, "{}"));
    }

    #[test]
    fn source_and_type_must_both_match() {
        let e = ev("mail", "received");
        assert!(rule_matches(&rule(Some("mail"), Some("received"), None), &e, "x"));
        assert!(!rule_matches(&rule(Some("mail"), Some("sent"), None), &e, "x"));
        assert!(!rule_matches(&rule(Some("calendar"), None, None), &e, "x"));
    }

    #[test]
    fn contains_checks_raw_line() {
        let e = ev("mail", "received");
        let raw = r#"{"source":"mail","payload":{"from":"boss@x.com"}}"#;
        assert!(rule_matches(&rule(None, None, Some("boss@x.com")), &e, raw));
        assert!(!rule_matches(&rule(None, None, Some("nope")), &e, raw));
    }

    #[test]
    fn sanitize_rejects_traversal_and_dotfiles() {
        assert!(sanitize_id("morning-brief").is_some());
        assert!(sanitize_id("../etc/passwd").is_none());
        assert!(sanitize_id(".hidden").is_none());
        assert!(sanitize_id("").is_none());
    }
}
