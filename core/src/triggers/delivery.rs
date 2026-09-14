//! Serialized event consumption and bounded Activity submission recovery.

use super::*;
use crate::agent::service::{Job, Store};
use activity::ActivityDelivery;

#[derive(Default, Serialize, Deserialize)]
struct TriggerCursor {
    next_line: usize,
    #[serde(default)]
    delivered_rules: BTreeSet<String>,
    #[serde(default)]
    pending: Vec<PendingDelivery>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    in_flight: Option<ActivityDelivery>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    owner_uid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    generation: Option<String>,
}

fn activity_cursor_marker_path() -> PathBuf {
    triggers_dir().join(".activity-cursor-initialized")
}

fn activity_cursor_initialized() -> Result<bool, String> {
    match fs::symlink_metadata(activity_cursor_marker_path()) {
        Ok(meta) if meta.is_file() => Ok(true),
        Ok(_) => Err("Activity trigger cursor marker is not a regular file; restore the original progress state".to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("inspect Activity trigger cursor marker: {error}")),
    }
}

fn has_activity_rules() -> bool {
    load_rules().iter().any(|rule| rule.activity_id.is_some())
}

fn read_cursor() -> Result<TriggerCursor, String> {
    match fs::metadata(cursor_path()) {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => return Err("trigger cursor is not a regular file; refusing delivery".to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if activity_cursor_initialized()? || has_activity_rules() {
                return Err(format!(
                    "Activity trigger cursor is missing; progress is indeterminate and Activity work is held. Restore the original durable cursor at {} before dispatch, adding or re-arming Activity rules; do not reset it.",
                    cursor_path().display()
                ));
            }
            return Ok(TriggerCursor::default());
        }
        Err(error) => return Err(format!("inspect trigger cursor: {error}")),
    }
    let raw = crate::filelock::read_locked(&cursor_path())?
        .ok_or_else(|| "trigger cursor disappeared while locked".to_string())?;
    if let Ok(cursor) = serde_json::from_str(&raw) {
        return Ok(cursor);
    }
    raw.trim()
        .parse::<usize>()
        .map(|next_line| TriggerCursor {
            next_line,
            ..TriggerCursor::default()
        })
        .map_err(|error| format!("invalid trigger cursor: {error}"))
}

fn write_cursor(cursor: &TriggerCursor) -> Result<(), String> {
    let data = serde_json::to_string(cursor)
        .map_err(|error| format!("serialize trigger cursor: {error}"))?;
    crate::filelock::write_locked(&cursor_path(), &data)?;
    sync_progress_directory()
}

fn sync_progress_directory() -> Result<(), String> {
    // Correlation must survive a power loss before the independent Job write.
    // filelock's directory sync is best-effort; this boundary requires success.
    #[cfg(unix)]
    crate::agent::util::sync_dir(&triggers_dir())
        .map_err(|error| format!("sync trigger progress directory: {error}"))?;
    Ok(())
}

/// Pure broker preflight; legitimate first initialization happens only after
/// authorization, under the shared trigger lock.
pub(super) fn check_activity_progress() -> Result<(), String> {
    read_cursor().map(|_| ())
}

/// Persist progress before an associated rule becomes visible or is re-armed.
pub(super) fn initialize_activity_progress() -> Result<(), String> {
    let cursor = read_cursor()?;
    write_cursor(&cursor)?;
    remember_activity_progress()
}

/// Keep the initialization witness even after the last associated rule is
/// removed. Otherwise removal/recreation could turn lost progress into a reset.
pub(super) fn remember_activity_progress() -> Result<(), String> {
    if !activity_cursor_initialized()? {
        crate::filelock::write_locked(&activity_cursor_marker_path(), "1")?;
    }
    sync_progress_directory()
}

fn consume(cursor: &mut TriggerCursor, rule_id: &str, line_index: Option<usize>) {
    if line_index == Some(cursor.next_line) {
        cursor.delivered_rules.insert(rule_id.to_string());
    }
}

pub(super) fn visible_match(rule: &TriggerRule, event: &Value, raw: &str) -> bool {
    rule.owner_uid.is_some_and(|uid| {
        crate::clawd::context_events::event_visible_to(event, (uid != 0).then_some(uid))
            && rule_matches(rule, event, raw)
    })
}

struct ActivityOutcome {
    status: TriggerDeliveryStatus,
    value: Value,
}

impl ActivityOutcome {
    fn queued(&self) -> bool {
        matches!(
            self.status,
            TriggerDeliveryStatus::Submitted | TriggerDeliveryStatus::Recovered
        )
    }
}

fn skipped(
    cursor: &mut TriggerCursor,
    rule: &TriggerRule,
    event: Option<(usize, &str)>,
    status: TriggerDeliveryStatus,
    message: &str,
) -> Result<ActivityOutcome, String> {
    let delivery = cursor.in_flight.take();
    let job_id = delivery.as_ref().map(|delivery| delivery.id.as_str());
    let session_id = delivery
        .as_ref()
        .and_then(|delivery| delivery.session_id.as_deref());
    // In particular, paused events are consumed rather than saved for resume.
    consume(cursor, &rule.id, event.map(|(line, _)| line));
    if status == TriggerDeliveryStatus::Indeterminate {
        activity::record(rule, status, job_id, session_id, message)?;
        write_cursor(cursor)?;
    } else {
        write_cursor(cursor)?;
        activity::record(rule, status, job_id, session_id, message)?;
    }
    Ok(ActivityOutcome {
        status,
        value: json!({
            "rule": rule.id,
            "activity_id": rule.activity_id,
            "status": status,
            "error": message,
            "job_id": job_id,
            "session_id": session_id,
        }),
    })
}

fn completed(
    cursor: &mut TriggerCursor,
    rule: &TriggerRule,
    job: Job,
    recovered: bool,
) -> Result<ActivityOutcome, String> {
    let delivery = cursor
        .in_flight
        .take()
        .ok_or_else(|| "Activity delivery lost its durable correlation".to_string())?;
    consume(cursor, &rule.id, delivery.line_index);
    write_cursor(cursor).map_err(|error| {
        format!("Job {} exists but delivery acknowledgement failed; recover before resubmitting: {error}", job.id)
    })?;
    let status = if recovered {
        TriggerDeliveryStatus::Recovered
    } else {
        TriggerDeliveryStatus::Submitted
    };
    let metadata_error = activity::record(
        rule,
        status,
        Some(&job.id),
        job.session_id.as_deref(),
        if recovered {
            "Recovered the existing Activity Job; no work was replayed."
        } else {
            "Queued an Activity Job."
        },
    )
    .err();
    if let Some(error) = metadata_error.as_deref() {
        tracing::warn!(trigger_id = %rule.id, job_id = %job.id, %error, "failed to record Activity delivery diagnostics");
    }
    publish_trigger_success(rule, &job);
    Ok(ActivityOutcome {
        status,
        value: json!({
            "rule": rule.id,
            "activity_id": job.activity_id,
            "job_id": job.id,
            "session_id": job.session_id,
            "status": status,
            "recovered": recovered,
            "metadata_error": metadata_error,
        }),
    })
}

fn indeterminate(
    cursor: &mut TriggerCursor,
    rule: &TriggerRule,
    reason: &str,
) -> Result<ActivityOutcome, String> {
    let delivery = cursor
        .in_flight
        .as_ref()
        .ok_or_else(|| "Activity delivery has no correlation to inspect".to_string())?;
    let event = delivery.line_index.zip(delivery.raw_event.clone());
    let message = format!(
        "Indeterminate Activity delivery {}: {reason}. No automatic replay; inspect this Job/Session, then explicitly enable the rule for future work.",
        delivery.id
    );
    skipped(
        cursor,
        rule,
        event.as_ref().map(|(line, raw)| (*line, raw.as_str())),
        TriggerDeliveryStatus::Indeterminate,
        &message,
    )
}

fn recover_in_flight(cursor: &mut TriggerCursor) -> Result<Option<ActivityOutcome>, String> {
    let Some(delivery) = cursor.in_flight.clone() else {
        return Ok(None);
    };
    let id = sanitize_id(&delivery.rule_id)
        .ok_or_else(|| "invalid in-flight trigger id; manual inspection is required".to_string())?;
    let rule = match load_rule(&id) {
        Ok(rule) => Some(rule),
        Err(error) => match fs::symlink_metadata(rule_path(&id)) {
            Err(io_error) if io_error.kind() == std::io::ErrorKind::NotFound => None,
            _ => {
                return Err(format!(
                    "cannot inspect in-flight trigger `{id}`; no replay: {error}"
                ))
            }
        },
    };
    let Some(rule) = rule.filter(|rule| delivery.matches_rule(rule)) else {
        cursor.in_flight = None;
        consume(cursor, &delivery.rule_id, delivery.line_index);
        write_cursor(cursor)?;
        let diagnostic = TriggerDeliveryDiagnostic {
            status: TriggerDeliveryStatus::Skipped,
            at_ms: now_ms(),
            job_id: Some(delivery.id.clone()),
            session_id: delivery.session_id.clone(),
            message: "In-flight delivery retired because the rule was removed, re-armed, or replaced; no work was replayed. Inspect the recorded Job for any earlier submission.".to_string(),
        };
        activity::emit(
            Some(delivery.owner_uid),
            &delivery.rule_id,
            Some(&delivery.activity_id),
            &diagnostic,
        );
        return Ok(Some(ActivityOutcome {
            status: diagnostic.status,
            value: json!({
                "rule": delivery.rule_id,
                "activity_id": delivery.activity_id,
                "status": diagnostic.status,
                "error": diagnostic.message,
                "job_id": delivery.id,
                "session_id": delivery.session_id,
            }),
        }));
    };
    let recovered = Store::open_default()
        .map_err(|error| format!("open job store for recovery: {error}"))
        .and_then(|store| delivery.recover(&rule, &store));
    match recovered {
        Ok(job) => completed(cursor, &rule, job, true).map(Some),
        Err(error) => indeterminate(cursor, &rule, &error).map(Some),
    }
}

fn dispatch_activity(
    cursor: &mut TriggerCursor,
    rule: &TriggerRule,
    prompt: String,
    event: Option<(usize, &str)>,
) -> Result<ActivityOutcome, String> {
    activity::require_not_held(rule)?;
    if cursor.in_flight.is_some() {
        return Err("another Activity delivery must be recovered before dispatch".to_string());
    }
    let owner_uid = rule
        .owner_uid
        .ok_or_else(|| "Activity trigger has no owner".to_string())?;
    let activity_id = rule
        .activity_id
        .as_deref()
        .ok_or_else(|| "trigger has no Activity".to_string())?;
    if let Err(error) = validate_activity_trigger(owner_uid, activity_id) {
        return skipped(cursor, rule, event, TriggerDeliveryStatus::Blocked, &error);
    }
    let store = match Store::open_default() {
        Ok(store) => store,
        Err(error) => {
            return skipped(
                cursor,
                rule,
                event,
                TriggerDeliveryStatus::Failed,
                &format!("open job store before submission: {error}"),
            )
        }
    };
    let mut delivery = match ActivityDelivery::new(rule, &prompt, event) {
        Ok(delivery) => delivery,
        Err(error) => return skipped(cursor, rule, event, TriggerDeliveryStatus::Blocked, &error),
    };
    cursor.in_flight = Some(delivery.clone());
    write_cursor(cursor)?;
    let mut job = match prepare_job(rule, prompt) {
        Ok(job) => job,
        Err(error) => return skipped(cursor, rule, event, TriggerDeliveryStatus::Blocked, &error),
    };
    job.id = delivery.id.clone();
    delivery.session_id = job.session_id.clone();
    cursor.in_flight = Some(delivery.clone());
    if let Err(error) = write_cursor(cursor) {
        end_unpublished_session(&job);
        return Err(format!(
            "persist Activity delivery before publication: {error}"
        ));
    }
    if let Err(error) = validate_activity_trigger(owner_uid, activity_id) {
        end_unpublished_session(&job);
        return skipped(cursor, rule, event, TriggerDeliveryStatus::Blocked, &error);
    }
    match store.publish(job) {
        Ok(job) => completed(cursor, rule, job, false),
        Err(error) => match delivery.recover(rule, &store) {
            Ok(job) => completed(cursor, rule, job, true),
            Err(recovery_error) => indeterminate(
                cursor,
                rule,
                &format!("publish failed: {error}; {recovery_error}"),
            ),
        },
    }
}

pub(super) fn manual(rule: &TriggerRule) -> Result<Value, String> {
    let mut cursor = read_cursor()?;
    remember_activity_progress()?;
    let same_delivery = cursor
        .in_flight
        .as_ref()
        .is_some_and(|delivery| delivery.matches_rule(rule));
    if let Some(outcome) = recover_in_flight(&mut cursor)? {
        if same_delivery {
            return manual_result(rule, outcome);
        }
    }
    let outcome = dispatch_activity(&mut cursor, rule, rule.prompt.clone(), None)?;
    manual_result(rule, outcome)
}

fn manual_result(rule: &TriggerRule, mut outcome: ActivityOutcome) -> Result<Value, String> {
    if !outcome.queued() {
        return Err(outcome.value["error"]
            .as_str()
            .unwrap_or("Activity delivery was refused")
            .to_string());
    }
    outcome.value["ok"] = json!(true);
    outcome.value["id"] = json!(rule.id);
    Ok(outcome.value)
}

pub(super) fn tick() -> Result<Value, String> {
    let mut cursor = read_cursor()?;
    if cursor.in_flight.is_some() || has_activity_rules() {
        remember_activity_progress()?;
    }
    let mut fired = Vec::new();
    let mut skipped_events = Vec::new();
    if let Some(outcome) = recover_in_flight(&mut cursor)? {
        if outcome.queued() {
            fired.push(outcome.value);
        } else {
            skipped_events.push(outcome.value);
        }
    }
    let rules = load_rules();
    let content = match fs::read_to_string(crate::paths::context_events_log_path()) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(format!("read context event log: {error}")),
    };
    let lines: Vec<&str> = content.lines().collect();
    if cursor.next_line > lines.len() {
        cursor.next_line = lines.len();
        cursor.delivered_rules.clear();
        write_cursor(&cursor)?;
    }
    let started_at = cursor.next_line;
    let mut unavailable_rules = BTreeSet::new();
    let mut retrying = Vec::new();
    for mut pending in std::mem::take(&mut cursor.pending) {
        let rule = rules.iter().find(|rule| {
            rule.enabled
                && rule.id == pending.rule_id
                && pending
                    .owner_uid
                    .is_none_or(|owner| rule.owner_uid == Some(owner))
                && pending
                    .generation
                    .as_ref()
                    .is_none_or(|generation| rule.generation.as_ref() == Some(generation))
        });
        let event = serde_json::from_str::<Value>(&pending.raw_event).ok();
        let Some((rule, event)) = rule
            .zip(event)
            .filter(|(rule, event)| visible_match(rule, event, &pending.raw_event))
        else {
            consume(&mut cursor, &pending.rule_id, Some(pending.line_index));
            skipped_events.push(json!({ "rule": pending.rule_id, "status": "skipped", "error": "pending event no longer matches a visible, enabled owner rule" }));
            continue;
        };
        if unavailable_rules.contains(&rule.id) {
            continue;
        }
        if rule.activity_id.is_some() {
            let error = "Indeterminate legacy pending Activity delivery has no durable Job identity; no automatic replay. Inspect prior tasks and explicitly enable the rule.";
            activity::record(
                rule,
                TriggerDeliveryStatus::Indeterminate,
                None,
                None,
                error,
            )?;
            consume(&mut cursor, &rule.id, Some(pending.line_index));
            unavailable_rules.insert(rule.id.clone());
            skipped_events.push(json!({
                "rule": rule.id, "activity_id": rule.activity_id,
                "status": "indeterminate", "error": error,
            }));
            continue;
        }
        if let Err(error) = execution_owner(rule) {
            quarantine_invalid_rule(&rule.id, &error);
            unavailable_rules.insert(rule.id.clone());
            continue;
        }
        match submit_job(rule, fired_prompt(rule, &event)) {
            Ok(job) => {
                consume(&mut cursor, &rule.id, Some(pending.line_index));
                record_fired(&rule.id);
                fired.push(json!({
                    "rule": rule.id, "job_id": job.id, "source": event.get("source"),
                    "event_type": event.get("event_type"), "retried": true,
                }));
            }
            Err(error) => {
                publish_trigger_failure(rule);
                pending.attempts = pending.attempts.saturating_add(1);
                pending.last_error = Some(error);
                retrying.push(pending);
            }
        }
    }
    cursor.pending = retrying;
    write_cursor(&cursor)?;

    for (line_index, raw) in lines.iter().enumerate().skip(cursor.next_line) {
        let event = if raw.trim().is_empty() {
            None
        } else {
            match serde_json::from_str::<Value>(raw) {
                Ok(event) => Some(event),
                Err(error) => {
                    tracing::warn!(line = line_index, %error, "skipping malformed context event");
                    None
                }
            }
        };
        if let Some(event) = event {
            for rule in rules.iter().filter(|rule| rule.enabled) {
                if unavailable_rules.contains(&rule.id)
                    || cursor.delivered_rules.contains(&rule.id)
                    || cursor.pending.iter().any(|pending| {
                        pending.line_index == line_index && pending.rule_id == rule.id
                    })
                    || !visible_match(rule, &event, raw)
                {
                    continue;
                }
                if let Err(error) = execution_owner(rule) {
                    quarantine_invalid_rule(&rule.id, &error);
                    unavailable_rules.insert(rule.id.clone());
                    skipped_events
                        .push(json!({ "rule": rule.id, "status": "blocked", "error": error }));
                    continue;
                }
                if rule.activity_id.is_some() {
                    let outcome = dispatch_activity(
                        &mut cursor,
                        rule,
                        fired_prompt(rule, &event),
                        Some((line_index, raw)),
                    )?;
                    if outcome.status == TriggerDeliveryStatus::Indeterminate {
                        unavailable_rules.insert(rule.id.clone());
                    }
                    if outcome.queued() {
                        fired.push(outcome.value);
                    } else {
                        skipped_events.push(outcome.value);
                    }
                    continue;
                }
                match submit_job(rule, fired_prompt(rule, &event)) {
                    Ok(job) => {
                        cursor.delivered_rules.insert(rule.id.clone());
                        write_cursor(&cursor)?;
                        record_fired(&rule.id);
                        fired.push(json!({
                            "rule": rule.id, "job_id": job.id,
                            "source": event.get("source"), "event_type": event.get("event_type"),
                        }));
                    }
                    Err(error) => {
                        publish_trigger_failure(rule);
                        cursor.pending.push(PendingDelivery {
                            line_index,
                            rule_id: rule.id.clone(),
                            raw_event: (*raw).to_string(),
                            attempts: 1,
                            last_error: Some(error.clone()),
                            owner_uid: rule.owner_uid,
                            generation: rule.generation.clone(),
                        });
                        write_cursor(&cursor)?;
                        fired.push(json!({ "rule": rule.id, "pending": true, "error": error }));
                    }
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
        "skipped": skipped_events,
        "pending": cursor.pending.len(),
    }))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/triggers/delivery.rs"
    ));
}
