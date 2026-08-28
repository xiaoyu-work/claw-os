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
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use crate::caps::{Cap, CapSet, Role, Scope, Verb};

/// One trigger rule: a match condition plus the prompt to run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerRule {
    pub id: String,
    #[serde(default)]
    pub seeded: bool,
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
    #[serde(default)]
    pub owner_uid: Option<u32>,
    #[serde(default)]
    pub owner_home: Option<String>,
    #[serde(default)]
    pub owner_caps: Option<CapSet>,
    #[serde(default)]
    pub owner_role: Option<Role>,
    #[serde(default)]
    pub owner_tier: Option<u8>,
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
fn seeded_sentinel_path() -> PathBuf {
    triggers_dir().join(".seeded-v2")
}
fn rule_path(id: &str) -> PathBuf {
    rules_dir().join(format!("{id}.json"))
}

/// Seed a few **disabled** example rules on first use, so proactivity is
/// discoverable out of the box (M ships its heartbeat with a default
/// prompt; we ship example rules that react to the system-vitals
/// heartbeat events). They are disabled by design — turning the agent
/// loose to act on system events is the operator's explicit choice
/// (`cos triggers enable <id>`). A sentinel ensures we seed exactly once
/// and never recreate a rule the user has deleted.
fn ensure_seeded() -> Result<(), String> {
    let sentinel = seeded_sentinel_path();
    if sentinel.exists() {
        return Ok(());
    }

    let defaults = [
        TriggerRule {
            id: "diagnose-low-memory".into(),
            seeded: true,
            enabled: false,
            source: Some(SOURCE_HEARTBEAT.into()),
            event_type: Some("memory_low.critical".into()),
            contains: None,
            prompt: LOW_MEMORY_PROMPT.into(),
            max_turns: Some(8),
            last_fired_ms: None,
            owner_uid: None,
            owner_home: None,
            owner_caps: None,
            owner_role: Some(Role::Observer),
            owner_tier: None,
        },
        TriggerRule {
            id: "diagnose-high-load".into(),
            seeded: true,
            enabled: false,
            source: Some(SOURCE_HEARTBEAT.into()),
            event_type: Some("load_high.critical".into()),
            contains: None,
            prompt: HIGH_LOAD_PROMPT.into(),
            max_turns: Some(8),
            last_fired_ms: None,
            owner_uid: None,
            owner_home: None,
            owner_caps: None,
            owner_role: Some(Role::Observer),
            owner_tier: None,
        },
    ];
    for rule in defaults {
        ensure_seed_rule(&rule)?;
    }
    crate::filelock::write_locked(&sentinel, "1")
}

/// Source tag emitted by the clawd heartbeat (see `clawd::heartbeat`).
/// Duplicated as a literal here to avoid a dependency cycle; kept in sync
/// with `clawd::heartbeat::SOURCE`.
const SOURCE_HEARTBEAT: &str = "heartbeat";
const LOW_MEMORY_PROMPT: &str = "System memory is critically low. Investigate which processes are consuming the most memory (use cos_sysinfo / cos_proc), summarise the likely cause, and suggest concrete remediation. Do not kill anything without approval.";
const HIGH_LOAD_PROMPT: &str = "System load is critically high. Identify the top CPU consumers and recent journal errors, then report the likely cause and a safe remediation. Do not change system state without approval.";

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

struct RuleOwner {
    uid: u32,
    home: String,
    caps: CapSet,
    role: Option<Role>,
    tier: Option<u8>,
}

fn current_owner() -> Result<RuleOwner, String> {
    let session = crate::proc::current_session_info_for_caps()
        .ok_or_else(|| "trigger changes require a registered session".to_string())?;
    let caps = session
        .caps
        .clone()
        .ok_or_else(|| "trigger owner session has no capabilities".to_string())?;
    let uid = crate::paths::current_owner_uid_override().unwrap_or_else(|| {
        #[cfg(unix)]
        unsafe {
            libc::geteuid() as u32
        }
        #[cfg(not(unix))]
        {
            0
        }
    });
    let home = crate::paths::verified_home_for_uid(uid)?;
    let role = session.role.as_deref().and_then(Role::parse);
    Ok(RuleOwner {
        uid,
        home: home.to_string_lossy().into_owned(),
        caps,
        role,
        tier: session.tier,
    })
}

fn require_rule_owner(rule: &TriggerRule, uid: u32) -> Result<(), String> {
    match rule.owner_uid {
        Some(owner_uid) if owner_uid == uid => Ok(()),
        Some(_) => Err(format!("trigger `{}` belongs to another user", rule.id)),
        None => Err(format!("trigger `{}` has no trusted owner", rule.id)),
    }
}

fn is_claimable_seed(rule: &TriggerRule) -> bool {
    if rule.seeded {
        return true;
    }
    rule.source.as_deref() == Some(SOURCE_HEARTBEAT)
        && matches!(
            (rule.id.as_str(), rule.event_type.as_deref()),
            ("diagnose-low-memory", Some("memory_low.critical"))
                | ("diagnose-high-load", Some("load_high.critical"))
        )
        && rule.contains.is_none()
        && rule.owner_caps.is_none()
        && matches!(rule.owner_uid, None | Some(0))
        && match rule.id.as_str() {
            "diagnose-low-memory" => rule.prompt == LOW_MEMORY_PROMPT,
            "diagnose-high-load" => rule.prompt == HIGH_LOAD_PROMPT,
            _ => false,
        }
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
    ensure_seeded()?;
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

fn create_rule(rule: &TriggerRule) -> Result<(), String> {
    let id = sanitize_id(&rule.id).ok_or_else(|| format!("invalid rule id '{}'", rule.id))?;
    crate::storage::ensure_private_dir(&rules_dir())
        .map_err(|e| format!("create triggers dir: {e}"))?;
    let data = serde_json::to_string_pretty(rule).map_err(|e| format!("serialize rule: {e}"))?;
    crate::filelock::update_locked::<_, String>(&rule_path(&id), |existing| {
        if existing.is_some() {
            return Err(format!("trigger `{id}` already exists"));
        }
        Ok(data)
    })
    .map_err(|error| error.to_string())
}

fn ensure_seed_rule(rule: &TriggerRule) -> Result<(), String> {
    let id = sanitize_id(&rule.id).ok_or_else(|| format!("invalid rule id '{}'", rule.id))?;
    crate::storage::ensure_private_dir(&rules_dir())
        .map_err(|e| format!("create triggers dir: {e}"))?;
    let data = serde_json::to_string_pretty(rule).map_err(|e| format!("serialize rule: {e}"))?;
    crate::filelock::update_locked::<_, String>(&rule_path(&id), |existing| {
        Ok(existing.unwrap_or(data))
    })
    .map_err(|error| error.to_string())
}

fn update_rule<F>(id: &str, transform: F) -> Result<TriggerRule, String>
where
    F: FnOnce(TriggerRule) -> Result<TriggerRule, String>,
{
    let id = sanitize_id(id).ok_or_else(|| format!("invalid rule id '{id}'"))?;
    let captured = std::cell::RefCell::new(None);
    crate::filelock::update_locked::<_, String>(&rule_path(&id), |existing| {
        let raw = existing.ok_or_else(|| format!("no such trigger '{id}'"))?;
        let rule: TriggerRule =
            serde_json::from_str(&raw).map_err(|e| format!("corrupt trigger '{id}': {e}"))?;
        let next = transform(rule)?;
        let data = serde_json::to_string_pretty(&next)
            .map_err(|e| format!("serialize trigger '{id}': {e}"))?;
        *captured.borrow_mut() = Some(next);
        Ok(data)
    })
    .map_err(|error| error.to_string())?;
    captured
        .into_inner()
        .ok_or_else(|| "internal: trigger update lost rule".to_string())
}

fn load_rule(id: &str) -> Result<TriggerRule, String> {
    let id = sanitize_id(id).ok_or_else(|| format!("invalid rule id '{id}'"))?;
    let raw = crate::filelock::read_locked(&rule_path(&id))
        .map_err(|error| format!("read trigger '{id}': {error}"))?
        .ok_or_else(|| format!("no such trigger '{id}'"))?;
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
            if let Ok(Some(raw)) = crate::filelock::read_locked(&p) {
                if let Ok(rule) = serde_json::from_str::<TriggerRule>(&raw) {
                    out.push(rule);
                }
            }
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

#[derive(Default, Serialize, Deserialize)]
struct TriggerCursor {
    next_line: usize,
    #[serde(default)]
    delivered_rules: BTreeSet<String>,
    #[serde(default)]
    pending: Vec<PendingDelivery>,
}

#[derive(Serialize, Deserialize)]
struct PendingDelivery {
    line_index: usize,
    rule_id: String,
    raw_event: String,
    #[serde(default)]
    attempts: u32,
    #[serde(default)]
    last_error: Option<String>,
}

fn read_cursor() -> Result<TriggerCursor, String> {
    let Some(raw) = crate::filelock::read_locked(&cursor_path())? else {
        return Ok(TriggerCursor::default());
    };
    if let Ok(cursor) = serde_json::from_str(&raw) {
        return Ok(cursor);
    }
    raw.trim()
        .parse::<usize>()
        .map(|next_line| TriggerCursor {
            next_line,
            delivered_rules: BTreeSet::new(),
            pending: Vec::new(),
        })
        .map_err(|error| format!("invalid trigger cursor: {error}"))
}

fn write_cursor(cursor: &TriggerCursor) -> Result<(), String> {
    crate::storage::ensure_private_dir(&triggers_dir())
        .map_err(|error| format!("create triggers dir: {error}"))?;
    let data = serde_json::to_string(cursor)
        .map_err(|error| format!("serialize trigger cursor: {error}"))?;
    crate::filelock::write_locked(&cursor_path(), &data)
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
struct TriggerExecutionOwner {
    uid: u32,
    home: PathBuf,
    caps: CapSet,
    tier: u8,
}

fn execution_owner(rule: &TriggerRule) -> Result<TriggerExecutionOwner, String> {
    let owner_uid = rule
        .owner_uid
        .ok_or_else(|| format!("trigger `{}` has no owner uid", rule.id))?;
    let recorded_home = rule
        .owner_home
        .as_deref()
        .filter(|home| !home.is_empty())
        .ok_or_else(|| format!("trigger `{}` has no owner home", rule.id))?;
    let owner_home = crate::paths::verified_home_for_uid(owner_uid)?;
    let recorded_home = PathBuf::from(recorded_home)
        .canonicalize()
        .map_err(|error| format!("canonicalize trigger owner home: {error}"))?;
    if recorded_home != owner_home {
        return Err(format!(
            "trigger `{}` owner home no longer matches the account database",
            rule.id
        ));
    }
    let stored_caps = rule
        .owner_caps
        .clone()
        .ok_or_else(|| format!("trigger `{}` has no capability snapshot", rule.id))?;
    let safe_caps = Role::AgentHost.caps_with_scopes(
        Some(Scope::path(format!("{}/**", owner_home.display()))),
        Some(Scope::Wild),
        Some(Scope::Wild),
    );
    let caps = stored_caps.intersect(&safe_caps);
    if !caps.covers(&Cap::new(Verb::AGENT_SPAWN, Scope::Wild)) {
        return Err(format!("trigger `{}` owner lacks agent.spawn:*", rule.id));
    }
    Ok(TriggerExecutionOwner {
        uid: owner_uid,
        home: owner_home,
        caps,
        tier: rule
            .owner_tier
            .unwrap_or(Role::AgentHost.credential_tier())
            .max(Role::AgentHost.credential_tier()),
    })
}

fn submit_job(
    rule: &TriggerRule,
    prompt: String,
) -> Result<String, String> {
    let owner = execution_owner(rule)?;
    let session = crate::session::create(format!("trigger: {}", rule.id))
        .map_err(|error| format!("create trigger session: {error}"))?;
    if let Err(error) = crate::session::update_meta(&session, |meta| {
        meta.creator_runtime = Some("trigger".to_string());
        meta.role = Some(Role::AgentHost);
        meta.credential_tier = Some(owner.tier);
        // The rule's owner, not whichever account happens to run the
        // heartbeat. Everything downstream — path roots, memory
        // database, the execution-time capability clamp — keys off
        // this, so recording the daemon's own uid here would derive
        // the wrong account's policy.
        meta.owner_uid = Some(owner.uid);
        // Provenance for the execution-time clamp: this snapshot is
        // authority the owner proved (or had approved) when the rule
        // was created, so the worker may keep its `agent.spawn` and
        // exactly-named credentials. Only believed because `clawd`
        // writes this record as root.
        meta.origin = Some(crate::session::SessionOrigin::TriggerDelegation);
        meta.client = crate::session::SessionClient::new(
            crate::session::SessionSource::ScheduledTrigger,
            false,
            true,
        );
    })
    {
        let _ = crate::session::end(&session, crate::session::Status::Failed);
        return Err(format!("configure trigger session: {error}"));
    }
    if let Err(error) = crate::session::set_caps(&session, &owner.caps) {
        let _ = crate::session::end(&session, crate::session::Status::Failed);
        return Err(format!("set trigger session caps: {error}"));
    }
    let store = match crate::agent::service::Store::open_default() {
        Ok(store) => store,
        Err(error) => {
            let _ = crate::session::end(&session, crate::session::Status::Failed);
            return Err(format!("open agent job store: {error}"));
        }
    };
    let job = match store.submit_with_context_and_client(
        prompt,
        None,
        None,
        Some(session.as_str().to_string()),
        rule.max_turns,
        Some(owner.uid),
        Some(owner.home.to_string_lossy().into_owned()),
        crate::session::SessionClient::new(
            crate::session::SessionSource::ScheduledTrigger,
            false,
            true,
        ),
    ) {
        Ok(job) => job,
        Err(error) => {
            let _ = crate::session::end(&session, crate::session::Status::Failed);
            return Err(format!("submit job: {error}"));
        }
    };
    Ok(job.id)
}

fn record_fired(rule_id: &str) {
    let fired_at = now_ms();
    if let Err(error) = update_rule(rule_id, |mut current| {
        current.last_fired_ms = Some(fired_at);
        Ok(current)
    }) {
        tracing::warn!(
            trigger_id = %rule_id,
            error = %error,
            "failed to persist trigger timestamp"
        );
    }
}

fn quarantine_invalid_rule(rule_id: &str, error: &str) {
    if let Err(update_error) = update_rule(rule_id, |mut current| {
        current.enabled = false;
        Ok(current)
    }) {
        tracing::error!(
            trigger_id = %rule_id,
            error = %update_error,
            "failed to quarantine invalid trigger"
        );
    }
    tracing::error!(
        trigger_id = %rule_id,
        error = %error,
        "disabled trigger with invalid owner context"
    );
}

fn cmd_add(args: &[String]) -> Result<Value, String> {
    crate::caps::require(Verb::TIME_CRON, Scope::Wild)
        .map_err(|denial| denial.summary())?;
    let id = flag(args, "id").ok_or_else(|| {
        "usage: cos triggers add --id <id> --prompt <text> [--source S] [--event-type T] [--contains STR] [--max-turns N]"
            .to_string()
    })?;
    let id = sanitize_id(&id)
        .ok_or_else(|| format!("invalid id '{id}' (allowed: alphanumerics, '-', '_', '.')"))?;
    let prompt = flag(args, "prompt").ok_or_else(|| "--prompt is required".to_string())?;
    let max_turns = flag(args, "max-turns").and_then(|s| s.parse::<u32>().ok());
    let owner = current_owner()?;
    if !owner
        .caps
        .covers(&Cap::new(Verb::AGENT_SPAWN, Scope::Wild))
    {
        return Err("trigger owner lacks agent.spawn:*".to_string());
    }
    let rule = TriggerRule {
        id: id.clone(),
        seeded: false,
        enabled: true,
        source: flag(args, "source"),
        event_type: flag(args, "event-type"),
        contains: flag(args, "contains"),
        prompt,
        max_turns,
        last_fired_ms: None,
        owner_uid: Some(owner.uid),
        owner_home: Some(owner.home),
        owner_caps: Some(owner.caps),
        owner_role: owner.role,
        owner_tier: owner.tier,
    };
    create_rule(&rule)?;
    Ok(json!({ "ok": true, "id": id, "rule": rule }))
}

fn cmd_list() -> Result<Value, String> {
    crate::caps::require(Verb::TIME_CRON, Scope::Wild)
        .map_err(|denial| denial.summary())?;
    let owner_uid = current_owner()?.uid;
    let all_rules = load_rules();
    let available_seeds: Vec<_> = all_rules
        .iter()
        .filter(|rule| !rule.enabled && rule.owner_caps.is_none() && is_claimable_seed(rule))
        .map(|rule| rule.id.clone())
        .collect();
    let legacy_unowned = all_rules
        .iter()
        .filter(|rule| rule.owner_uid.is_none() && !is_claimable_seed(rule))
        .count();
    let rules: Vec<_> = all_rules
        .into_iter()
        .filter(|rule| rule.owner_uid == Some(owner_uid))
        .collect();
    Ok(json!({
        "count": rules.len(),
        "triggers": rules,
        "available_seeds": available_seeds,
        "legacy_unowned": legacy_unowned,
        "migration": (legacy_unowned > 0).then_some(
            "legacy ownerless triggers are quarantined; recreate them to bind a trusted owner"
        ),
    }))
}

fn cmd_remove(args: &[String]) -> Result<Value, String> {
    crate::caps::require(Verb::TIME_CRON, Scope::Wild)
        .map_err(|denial| denial.summary())?;
    let id =
        positional_or_id(args).ok_or_else(|| "usage: cos triggers remove <id>".to_string())?;
    let id = sanitize_id(&id).ok_or_else(|| format!("invalid id '{id}'"))?;
    let owner_uid = current_owner()?.uid;
    let rule = load_rule(&id)?;
    require_rule_owner(&rule, owner_uid)?;
    match fs::remove_file(rule_path(&id)) {
        Ok(()) => Ok(json!({ "ok": true, "removed": id })),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(format!("no such trigger '{id}'"))
        }
        Err(e) => Err(format!("remove trigger '{id}': {e}")),
    }
}

fn cmd_set_enabled(args: &[String], enabled: bool) -> Result<Value, String> {
    crate::caps::require(Verb::TIME_CRON, Scope::Wild)
        .map_err(|denial| denial.summary())?;
    let verb = if enabled { "enable" } else { "disable" };
    let id =
        positional_or_id(args).ok_or_else(|| format!("usage: cos triggers {verb} <id>"))?;
    let owner = current_owner()?;
    if enabled
        && !owner
            .caps
            .covers(&Cap::new(Verb::AGENT_SPAWN, Scope::Wild))
        {
            return Err("trigger owner lacks agent.spawn:*".to_string());
        }
    let rule = update_rule(&id, |mut rule| {
        let unclaimed = !rule.enabled && rule.owner_caps.is_none() && is_claimable_seed(&rule);
        if !unclaimed {
            require_rule_owner(&rule, owner.uid)?;
        }
        if enabled {
            rule.owner_uid = Some(owner.uid);
            rule.owner_home = Some(owner.home);
            rule.owner_caps = Some(owner.caps);
            rule.owner_role = owner.role;
            rule.owner_tier = owner.tier;
            rule.seeded = false;
        }
        rule.enabled = enabled;
        Ok(rule)
    })?;
    Ok(json!({ "ok": true, "id": rule.id, "enabled": enabled }))
}

fn cmd_run(args: &[String]) -> Result<Value, String> {
    crate::caps::require(Verb::AGENT_SPAWN, Scope::Wild)
        .map_err(|denial| denial.summary())?;
    let id = positional_or_id(args).ok_or_else(|| "usage: cos triggers run <id>".to_string())?;
    let owner_uid = current_owner()?.uid;
    let rule = load_rule(&id)?;
    require_rule_owner(&rule, owner_uid)?;
    let job_id = submit_job(&rule, rule.prompt.clone())?;
    let fired_at = now_ms();
    let metadata_error = update_rule(&id, |mut current| {
        require_rule_owner(&current, owner_uid)?;
        current.last_fired_ms = Some(fired_at);
        Ok(current)
    })
    .err();
    Ok(json!({
        "ok": true,
        "id": rule.id,
        "job_id": job_id,
        "metadata_error": metadata_error,
    }))
}

/// Scan `context.event` records newer than the cursor and fire every
/// enabled matching rule. Returns what fired. Intended to be invoked
/// once per minute by an external scheduler (like `cron tick`).
fn cmd_tick() -> Result<Value, String> {
    crate::caps::require(Verb::SYS_KERNEL, Scope::Wild)
        .map_err(|denial| denial.summary())?;
    let rules = load_rules();
    let content = match fs::read_to_string(crate::paths::context_events_log_path()) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(format!("read context event log: {error}")),
    };
    let lines: Vec<&str> = content.lines().collect();
    let mut cursor = read_cursor()?;
    if cursor.next_line > lines.len() {
        cursor.next_line = lines.len();
        cursor.delivered_rules.clear();
        write_cursor(&cursor)?;
    }
    let started_at = cursor.next_line;

    let mut fired: Vec<Value> = Vec::new();
    let mut quarantined_rules = BTreeSet::new();
    let mut retrying = Vec::new();
    for mut delivery in std::mem::take(&mut cursor.pending) {
        let Some(rule) = rules
            .iter()
            .find(|rule| rule.enabled && rule.id == delivery.rule_id)
        else {
            continue;
        };
        let ev: Value = match serde_json::from_str(&delivery.raw_event) {
            Ok(event) => event,
            Err(error) => {
                tracing::error!(
                    trigger_id = %delivery.rule_id,
                    error = %error,
                    "discarding corrupt pending trigger delivery"
                );
                continue;
            }
        };
        if let Err(error) = execution_owner(rule) {
            quarantine_invalid_rule(&rule.id, &error);
            quarantined_rules.insert(rule.id.clone());
            continue;
        }
        match submit_job(rule, fired_prompt(rule, &ev)) {
            Ok(job_id) => {
                if delivery.line_index == cursor.next_line {
                    cursor.delivered_rules.insert(rule.id.clone());
                }
                record_fired(&rule.id);
                fired.push(json!({
                    "rule": rule.id,
                    "job_id": job_id,
                    "source": ev.get("source"),
                    "event_type": ev.get("event_type"),
                    "retried": true,
                }));
            }
            Err(error) => {
                delivery.attempts = delivery.attempts.saturating_add(1);
                delivery.last_error = Some(error.clone());
                tracing::warn!(
                    trigger_id = %delivery.rule_id,
                    attempts = delivery.attempts,
                    error = %error,
                    "trigger delivery remains pending"
                );
                retrying.push(delivery);
            }
        }
    }
    cursor.pending = retrying;
    write_cursor(&cursor)?;

    for (line_index, raw) in lines.iter().enumerate().skip(cursor.next_line) {
        if raw.trim().is_empty() {
            cursor.next_line = line_index + 1;
            cursor.delivered_rules.clear();
            write_cursor(&cursor)?;
            continue;
        }
        let ev: Value = match serde_json::from_str(raw) {
            Ok(v) => v,
            Err(error) => {
                tracing::warn!(
                    line = line_index,
                    error = %error,
                    "skipping malformed context event"
                );
                cursor.next_line = line_index + 1;
                cursor.delivered_rules.clear();
                write_cursor(&cursor)?;
                continue;
            }
        };
        for rule in rules.iter().filter(|r| r.enabled) {
            if quarantined_rules.contains(&rule.id) {
                continue;
            }
            if cursor.delivered_rules.contains(&rule.id) {
                continue;
            }
            if cursor
                .pending
                .iter()
                .any(|delivery| {
                    delivery.line_index == line_index && delivery.rule_id == rule.id
                })
            {
                continue;
            }
            let Some(owner_uid) = rule.owner_uid else {
                continue;
            };
            if !crate::clawd::context_events::event_visible_to(
                &ev,
                (owner_uid != 0).then_some(owner_uid),
            ) {
                continue;
            }
            if !rule_matches(rule, &ev, raw) {
                continue;
            }
            if let Err(error) = execution_owner(rule) {
                quarantine_invalid_rule(&rule.id, &error);
                quarantined_rules.insert(rule.id.clone());
                continue;
            }
            match submit_job(rule, fired_prompt(rule, &ev)) {
                Ok(job_id) => {
                    cursor.delivered_rules.insert(rule.id.clone());
                    write_cursor(&cursor)?;
                    record_fired(&rule.id);
                    fired.push(json!({
                        "rule": rule.id,
                        "job_id": job_id,
                        "source": ev.get("source"),
                        "event_type": ev.get("event_type"),
                    }));
                }
                Err(error) => {
                    cursor.pending.push(PendingDelivery {
                        line_index,
                        rule_id: rule.id.clone(),
                        raw_event: (*raw).to_string(),
                        attempts: 1,
                        last_error: Some(error.clone()),
                    });
                    write_cursor(&cursor)?;
                    tracing::warn!(
                        trigger_id = %rule.id,
                        line = line_index,
                        error = %error,
                        "queued trigger delivery for retry"
                    );
                    fired.push(json!({
                        "rule": rule.id,
                        "pending": true,
                        "error": error,
                    }));
                }
            }
        }
        cursor.next_line = line_index + 1;
        cursor.delivered_rules.clear();
        write_cursor(&cursor)?;
    }

    Ok(json!({
        "processed": cursor.next_line.saturating_sub(started_at),
        "cursor": cursor.next_line,
        "fired": fired,
        "pending": cursor.pending.len(),
    }))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/triggers.rs"
    ));
}
