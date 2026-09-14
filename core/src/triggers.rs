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

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::caps::{Cap, CapSet, Role, Scope, Verb};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

mod activity;
mod delivery;

pub(crate) use activity::validate_activity_trigger;
pub use activity::{TriggerDeliveryDiagnostic, TriggerDeliveryStatus};

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity_id: Option<String>,
    /// Root-generated incarnation, changed when the owner re-arms a rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_delivery: Option<TriggerDeliveryDiagnostic>,
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
            activity_id: None,
            generation: None,
            last_delivery: None,
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
            activity_id: None,
            generation: None,
            last_delivery: None,
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
        && rule.activity_id.is_none()
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

fn is_unclaimed_seed(rule: &TriggerRule) -> bool {
    !rule.enabled
        && rule.owner_caps.is_none()
        && rule.activity_id.is_none()
        && is_claimable_seed(rule)
}

/// Extract `--name value` from an argument list.
fn flag(args: &[String], name: &str) -> Option<String> {
    let key = format!("--{name}");
    args.iter()
        .position(|a| a == &key)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn activity_flag(args: &[String]) -> Result<Option<String>, String> {
    flag(args, "activity")
        .map(|id| activity::canonical_id(&id).map_err(|error| format!("--activity: {error}")))
        .transpose()
}

/// First positional (non-flag) arg, or `--id <v>` as a fallback.
fn positional_or_id(args: &[String]) -> Option<String> {
    flag(args, "id").or_else(|| args.first().filter(|arg| !arg.starts_with("--")).cloned())
}

fn validate_arguments(command: &str, args: &[String]) -> Result<(), String> {
    let allowed: &[&str] = match command {
        "add" => &[
            "id",
            "prompt",
            "source",
            "event-type",
            "contains",
            "max-turns",
            "activity",
        ],
        "list" => &["activity"],
        "remove" | "rm" | "enable" | "disable" | "run" => &["id"],
        "tick" => &[],
        other => {
            return Err(format!(
                "unknown command '{other}'. valid: add | list | remove | enable | disable | run | tick"
            ));
        }
    };
    let accepts_id = matches!(command, "remove" | "rm" | "enable" | "disable" | "run");
    let mut seen = BTreeSet::new();
    let mut positional = None;
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        if let Some(name) = arg.strip_prefix("--") {
            if !allowed.contains(&name) || !seen.insert(name) {
                return Err(format!("unknown or repeated trigger flag '{arg}'"));
            }
            let value = args
                .get(index + 1)
                .filter(|value| !value.is_empty() && !value.starts_with("--"))
                .ok_or_else(|| format!("{arg} requires a value"))?;
            match name {
                "activity" => {
                    activity::canonical_id(value)
                        .map_err(|error| format!("--activity: {error}"))?;
                }
                "id" => {
                    sanitize_id(value).ok_or_else(|| format!("invalid id '{value}'"))?;
                }
                "max-turns" => {
                    value
                        .parse::<u32>()
                        .ok()
                        .filter(|turns| *turns > 0)
                        .ok_or_else(|| "--max-turns must be a positive integer".to_string())?;
                }
                "prompt" if value.trim().is_empty() => {
                    return Err("--prompt must not be empty".to_string());
                }
                _ => {}
            }
            index += 2;
        } else if accepts_id && positional.is_none() {
            sanitize_id(arg).ok_or_else(|| format!("invalid id '{arg}'"))?;
            positional = Some(arg);
            index += 1;
        } else {
            return Err(format!("unexpected trigger argument '{arg}'"));
        }
    }
    if positional.is_some() && seen.contains("id") {
        return Err("supply a positional trigger id or --id, not both".to_string());
    }
    if command == "add" && (!seen.contains("id") || !seen.contains("prompt")) {
        return Err("usage: cos triggers add --id <id> --prompt <text> [--source S] [--event-type T] [--contains STR] [--max-turns N] [--activity UUID]".to_string());
    }
    if accepts_id && positional.is_none() && !seen.contains("id") {
        return Err(format!("usage: cos triggers {command} <id>"));
    }
    Ok(())
}

fn with_trigger_lock<T>(operation: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    crate::agent::util::ensure_durable_private_dir(&triggers_dir())
        .map_err(|error| format!("create triggers dir: {error}"))?;
    crate::filelock::with_exclusive_path_lock(&triggers_dir().join(".dispatch"), || {
        ensure_seeded()?;
        operation()
    })
}

/// Scheduler checks before consent. Never seed, persist diagnostics, or create
/// work here. Listing validates its filter without imposing admission limits.
pub(crate) fn preflight_activity_command(
    owner_uid: u32,
    command: &str,
    args: &[String],
) -> Result<(), String> {
    validate_arguments(command, args)?;
    if matches!(command, "add" | "list") {
        if let Some(activity_id) = activity_flag(args)? {
            if command == "add" {
                validate_activity_trigger(owner_uid, &activity_id)?;
                delivery::check_activity_progress()?;
            }
        }
    } else if matches!(command, "enable" | "run") {
        let id = positional_or_id(args).ok_or_else(|| "trigger id is required".to_string())?;
        let rule = match load_rule(&id) {
            Ok(rule) => rule,
            Err(error) => match fs::symlink_metadata(rule_path(&id)) {
                // Preserve explicit claiming of a built-in example on first
                // use without creating it during consent preflight.
                Err(io_error)
                    if io_error.kind() == std::io::ErrorKind::NotFound
                        && command == "enable"
                        && matches!(id.as_str(), "diagnose-low-memory" | "diagnose-high-load")
                        && !seeded_sentinel_path().exists() =>
                {
                    return Ok(());
                }
                _ => return Err(error),
            },
        };
        if command != "enable" || !is_unclaimed_seed(&rule) {
            require_rule_owner(&rule, owner_uid)?;
        }
        if command == "run" {
            activity::require_not_held(&rule)?;
        }
        if let Some(activity_id) = rule.activity_id.as_deref() {
            validate_activity_trigger(owner_uid, activity_id)?;
            delivery::check_activity_progress()?;
        }
    }
    Ok(())
}

/// CLI entry — dispatched from the router under `cos triggers`.
pub fn run(command: &str, args: &[String]) -> Result<Value, String> {
    validate_arguments(command, args)?;
    match command {
        "add" => cmd_add(args),
        "list" => cmd_list(args),
        "remove" | "rm" => cmd_remove(args),
        "enable" => cmd_set_enabled(args, true),
        "disable" => cmd_set_enabled(args, false),
        "run" => cmd_run(args),
        "tick" => {
            crate::caps::require(Verb::SYS_KERNEL, Scope::Wild)
                .map_err(|denial| denial.summary())?;
            with_trigger_lock(delivery::tick)
        }
        _ => unreachable!("validated trigger command"),
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
/// Build the prompt a fired trigger submits.
///
/// The rule's own prompt is owner-authored and stays the request text.
/// The event's `source` and `event_type` are chosen by whoever appended
/// the context event, so they are fenced as
/// [`SourceKind::ContextEvent`](crate::agent::trust::SourceKind::ContextEvent)
/// data rather than interpolated beside the rule text where a crafted
/// value would read as an equal-trust instruction.
pub(crate) fn fired_prompt(rule: &TriggerRule, ev: &Value) -> String {
    let src = ev.get("source").and_then(|v| v.as_str()).unwrap_or("?");
    let et = ev.get("event_type").and_then(|v| v.as_str()).unwrap_or("?");
    let fenced = crate::agent::safety::untrusted::wrap_labeled(
        crate::agent::trust::SourceKind::ContextEvent,
        Some(&rule.id),
        &format!("source={src}\nevent_type={et}"),
    );
    format!(
        "{}\n\n[Fired by ClawOS trigger '{}' on a system event. The event's own \
         fields follow as data.]\n{fenced}",
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

fn prepare_job(rule: &TriggerRule, prompt: String) -> Result<crate::agent::service::Job, String> {
    let owner = execution_owner(rule)?;
    let activity_id = rule
        .activity_id
        .as_deref()
        .map(|id| validate_activity_trigger(owner.uid, id))
        .transpose()?;
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
        meta.client = trigger_client();
        // Provenance for the execution-time clamp: this snapshot is
        // authority the owner proved (or had approved) when the rule
        // was created, so the worker may keep its `agent.spawn` and
        // exactly-named credentials. Only believed because `clawd`
        // writes this record as root.
        meta.origin = Some(crate::session::SessionOrigin::TriggerDelegation);
    }) {
        let _ = crate::session::end(&session, crate::session::Status::Failed);
        return Err(format!("configure trigger session: {error}"));
    }
    if let Err(error) = crate::session::set_caps(&session, &owner.caps) {
        let _ = crate::session::end(&session, crate::session::Status::Failed);
        return Err(format!("set trigger session caps: {error}"));
    }
    let mut job = crate::agent::service::Job::new_pending_with_client(
        prompt,
        None,
        None,
        Some(session.as_str().to_string()),
        rule.max_turns,
        Some(owner.uid),
        Some(owner.home.to_string_lossy().into_owned()),
        trigger_client(),
    );
    job.activity_id = activity_id;
    Ok(job)
}

fn trigger_client() -> crate::session::SessionClient {
    crate::session::SessionClient::new(crate::session::SessionSource::ScheduledTrigger, false, true)
}

fn end_unpublished_session(job: &crate::agent::service::Job) {
    if let Some(session) = job
        .session_id
        .as_deref()
        .and_then(|id| id.parse::<crate::session::SessionId>().ok())
    {
        let _ = crate::session::end(&session, crate::session::Status::Failed);
    }
}

fn submit_job(rule: &TriggerRule, prompt: String) -> Result<crate::agent::service::Job, String> {
    if rule.activity_id.is_some() {
        return Err("Activity triggers require durable delivery correlation".to_string());
    }
    let store = crate::agent::service::Store::open_default()
        .map_err(|error| format!("open agent job store: {error}"))?;
    let pending = prepare_job(rule, prompt)?;
    let job = match store.publish(pending.clone()) {
        Ok(job) => job,
        Err(error) => {
            end_unpublished_session(&pending);
            return Err(format!("submit job: {error}"));
        }
    };
    publish_trigger_success(rule, &job);
    Ok(job)
}

fn publish_trigger_success(rule: &TriggerRule, job: &crate::agent::service::Job) {
    let Some(owner_uid) = job.owner_uid else {
        return;
    };
    let mut draft = crate::notifications::NotificationDraft::new(
        "trigger",
        "trigger.matched",
        crate::notifications::Severity::Info,
        "Automation triggered",
        format!("Trigger `{}` matched and queued an Agent task.", rule.id),
    )
    .activity()
    .dedupe(format!("trigger:{}:task:{}", rule.id, job.id));
    draft.task_id = Some(job.id.clone());
    draft.session_id = job.session_id.clone();
    draft.job_id = Some(rule.id.clone());
    if let Err(error) = crate::clawd::notifications::publish_for_owner(owner_uid, draft) {
        tracing::warn!(
            trigger_id = %rule.id,
            task_id = %job.id,
            owner_uid,
            %error,
            "failed to publish trigger notification"
        );
    }
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

fn publish_trigger_failure(rule: &TriggerRule) {
    let Some(owner_uid) = rule.owner_uid.filter(|uid| *uid != 0) else {
        return;
    };
    let mut draft = crate::notifications::NotificationDraft::new(
        "trigger",
        "trigger.failed",
        crate::notifications::Severity::Error,
        "Automation failed",
        format!(
            "Trigger `{}` could not start its Agent task. Open the automation details for more information.",
            rule.id
        ),
    )
    .dedupe(format!("trigger:{}:submission-failed", rule.id));
    draft.job_id = Some(rule.id.clone());
    if let Err(error) = crate::clawd::notifications::publish_for_owner(owner_uid, draft) {
        tracing::warn!(
            trigger_id = %rule.id,
            owner_uid,
            %error,
            "failed to publish trigger failure notification"
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
    let id = flag(args, "id").ok_or_else(|| {
        "usage: cos triggers add --id <id> --prompt <text> [--source S] [--event-type T] [--contains STR] [--max-turns N] [--activity UUID]"
            .to_string()
    })?;
    let id = sanitize_id(&id)
        .ok_or_else(|| format!("invalid id '{id}' (allowed: alphanumerics, '-', '_', '.')"))?;
    let prompt = flag(args, "prompt").ok_or_else(|| "--prompt is required".to_string())?;
    let max_turns = flag(args, "max-turns").and_then(|s| s.parse::<u32>().ok());
    let owner = current_owner()?;
    let activity_id = activity_flag(args)?
        .map(|id| validate_activity_trigger(owner.uid, &id))
        .transpose()?;
    if activity_id.is_some() {
        delivery::check_activity_progress()?;
    }
    crate::caps::require(Verb::TIME_CRON, Scope::Wild).map_err(|denial| denial.summary())?;
    if !owner.caps.covers(&Cap::new(Verb::AGENT_SPAWN, Scope::Wild)) {
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
        activity_id,
        generation: Some(uuid::Uuid::new_v4().to_string()),
        last_delivery: None,
        last_fired_ms: None,
        owner_uid: Some(owner.uid),
        owner_home: Some(owner.home),
        owner_caps: Some(owner.caps),
        owner_role: owner.role,
        owner_tier: owner.tier,
    };
    with_trigger_lock(|| {
        if let Some(activity_id) = rule.activity_id.as_deref() {
            validate_activity_trigger(owner.uid, activity_id)?;
            delivery::initialize_activity_progress()?;
        }
        create_rule(&rule)?;
        Ok(json!({ "ok": true, "id": id, "rule": rule }))
    })
}

fn cmd_list(args: &[String]) -> Result<Value, String> {
    let activity_id = activity_flag(args)?;
    crate::caps::require(Verb::TIME_CRON, Scope::Wild).map_err(|denial| denial.summary())?;
    let owner_uid = current_owner()?.uid;
    with_trigger_lock(|| {
        let all_rules = load_rules();
        let available_seeds: Vec<_> = all_rules
            .iter()
            .filter(|rule| activity_id.is_none() && is_unclaimed_seed(rule))
            .map(|rule| rule.id.clone())
            .collect();
        let legacy_unowned = all_rules
            .iter()
            .filter(|rule| rule.owner_uid.is_none() && !is_claimable_seed(rule))
            .count();
        let rules: Vec<_> = all_rules
            .into_iter()
            .filter(|rule| {
                rule.owner_uid == Some(owner_uid)
                    && activity_id
                        .as_ref()
                        .is_none_or(|id| rule.activity_id.as_ref() == Some(id))
            })
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
    })
}

fn cmd_remove(args: &[String]) -> Result<Value, String> {
    crate::caps::require(Verb::TIME_CRON, Scope::Wild).map_err(|denial| denial.summary())?;
    let id = positional_or_id(args).ok_or_else(|| "usage: cos triggers remove <id>".to_string())?;
    let id = sanitize_id(&id).ok_or_else(|| format!("invalid id '{id}'"))?;
    let owner_uid = current_owner()?.uid;
    with_trigger_lock(|| {
        let rule = load_rule(&id)?;
        require_rule_owner(&rule, owner_uid)?;
        if rule.activity_id.is_some() {
            delivery::remember_activity_progress()?;
        }
        match fs::remove_file(rule_path(&id)) {
            Ok(()) => Ok(json!({ "ok": true, "removed": id })),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(format!("no such trigger '{id}'"))
            }
            Err(e) => Err(format!("remove trigger '{id}': {e}")),
        }
    })
}

fn cmd_set_enabled(args: &[String], enabled: bool) -> Result<Value, String> {
    let verb = if enabled { "enable" } else { "disable" };
    let id = positional_or_id(args).ok_or_else(|| format!("usage: cos triggers {verb} <id>"))?;
    let owner = current_owner()?;
    if enabled {
        preflight_activity_command(owner.uid, "enable", args)?;
    }
    crate::caps::require(Verb::TIME_CRON, Scope::Wild).map_err(|denial| denial.summary())?;
    if enabled && !owner.caps.covers(&Cap::new(Verb::AGENT_SPAWN, Scope::Wild)) {
        return Err("trigger owner lacks agent.spawn:*".to_string());
    }
    with_trigger_lock(|| {
        let rule = update_rule(&id, |mut rule| {
            let unclaimed = is_unclaimed_seed(&rule);
            if !unclaimed {
                require_rule_owner(&rule, owner.uid)?;
            }
            if enabled {
                if let Some(id) = rule.activity_id.as_deref() {
                    rule.activity_id = Some(validate_activity_trigger(owner.uid, id)?);
                    delivery::initialize_activity_progress()?;
                }
                rule.owner_uid = Some(owner.uid);
                rule.owner_home = Some(owner.home);
                rule.owner_caps = Some(owner.caps);
                rule.owner_role = owner.role;
                rule.owner_tier = owner.tier;
                rule.seeded = false;
                rule.generation = Some(uuid::Uuid::new_v4().to_string());
                rule.last_delivery = None;
            } else if rule.activity_id.is_some() {
                delivery::remember_activity_progress()?;
            }
            rule.enabled = enabled;
            Ok(rule)
        })?;
        Ok(json!({ "ok": true, "id": rule.id, "enabled": enabled }))
    })
}

fn cmd_run(args: &[String]) -> Result<Value, String> {
    let id = positional_or_id(args).ok_or_else(|| "usage: cos triggers run <id>".to_string())?;
    let owner_uid = current_owner()?.uid;
    with_trigger_lock(|| {
        let rule = load_rule(&id)?;
        require_rule_owner(&rule, owner_uid)?;
        activity::require_not_held(&rule)?;
        if let Some(activity_id) = rule.activity_id.as_deref() {
            if let Err(error) = validate_activity_trigger(owner_uid, activity_id) {
                activity::record(&rule, TriggerDeliveryStatus::Blocked, None, None, &error)?;
                return Err(error);
            }
            delivery::check_activity_progress()?;
        }
        crate::caps::require(Verb::AGENT_SPAWN, Scope::Wild).map_err(|denial| denial.summary())?;
        if rule.activity_id.is_some() {
            return delivery::manual(&rule);
        }
        let job = match submit_job(&rule, rule.prompt.clone()) {
            Ok(job) => job,
            Err(error) => {
                publish_trigger_failure(&rule);
                return Err(error);
            }
        };
        let metadata_error = update_rule(&id, |mut current| {
            require_rule_owner(&current, owner_uid)?;
            current.last_fired_ms = Some(now_ms());
            Ok(current)
        })
        .err();
        Ok(json!({
            "ok": true,
            "id": rule.id,
            "job_id": job.id,
            "session_id": job.session_id,
            "metadata_error": metadata_error,
        }))
    })
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/triggers.rs"
    ));
}
