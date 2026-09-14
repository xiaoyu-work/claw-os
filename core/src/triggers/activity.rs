//! Activity admission and delivery evidence. Neither is execution authority.

use super::*;
use crate::activities::ActivityService;
use crate::agent::service::{Job, Store};

pub(super) fn canonical_id(activity_id: &str) -> Result<String, String> {
    uuid::Uuid::parse_str(activity_id)
        .map(|id| id.to_string())
        .map_err(|error| format!("invalid Activity UUID: {error}"))
}

pub(crate) fn validate_activity_trigger(
    owner_uid: u32,
    activity_id: &str,
) -> Result<String, String> {
    let activity_id = canonical_id(activity_id)?;
    // Admission is not an Activity-creation entry point. Avoid initializing
    // the default provider merely to reject an unknown association.
    match fs::metadata(crate::paths::data_dir().join("activities.db")) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => return Err("Activity database is not a regular file".to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(crate::activities::ActivityError::NotFound.to_string());
        }
        Err(error) => return Err(format!("inspect Activity database: {error}")),
    }
    let service = crate::activities::open_default().map_err(|error| error.to_string())?;
    let activity = service
        .get(owner_uid, &activity_id)
        .map_err(|error| error.to_string())?;
    if !activity.state.allows_work() {
        return Err(format!(
            "Activity {} is {}; trigger work is blocked",
            activity.id,
            activity.state.as_str()
        ));
    }
    let limits = service
        .execution_limits(owner_uid, &activity.id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| {
            "Activity triggers require configured finite execution limits".to_string()
        })?;
    if !limits.enabled {
        return Err("Activity execution limits are disabled".to_string());
    }
    limits
        .limits
        .validate()
        .map_err(|error| error.to_string())?;
    let expiry = chrono::DateTime::parse_from_rfc3339(&limits.limits.expires_at)
        .map_err(|error| format!("invalid Activity execution expiry: {error}"))?;
    if expiry <= chrono::Utc::now() {
        return Err("Activity execution limits have expired".to_string());
    }
    if limits.used_attempts >= limits.limits.max_attempts {
        return Err("Activity execution attempt limit reached".to_string());
    }
    if service
        .capability_policy(owner_uid, &activity.id)
        .map_err(|error| error.to_string())?
        .is_some_and(|policy| !policy.enabled)
    {
        return Err("Activity capability policy is disabled".to_string());
    }
    Ok(activity.id)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerDeliveryStatus {
    Submitted,
    Recovered,
    Blocked,
    Failed,
    Skipped,
    Indeterminate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerDeliveryDiagnostic {
    pub status: TriggerDeliveryStatus,
    pub at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub message: String,
}

pub(super) fn require_not_held(rule: &TriggerRule) -> Result<(), String> {
    if let Some(diagnostic) = rule
        .last_delivery
        .as_ref()
        .filter(|diagnostic| diagnostic.status == TriggerDeliveryStatus::Indeterminate)
    {
        return Err(format!(
            "trigger `{}` is held: {} Inspect the recorded Job/Session before explicitly enabling the rule.",
            rule.id, diagnostic.message
        ));
    }
    Ok(())
}

pub(super) fn record(
    rule: &TriggerRule,
    status: TriggerDeliveryStatus,
    job_id: Option<&str>,
    session_id: Option<&str>,
    message: &str,
) -> Result<(), String> {
    let diagnostic = TriggerDeliveryDiagnostic {
        status,
        at_ms: now_ms(),
        job_id: job_id.map(str::to_string),
        session_id: session_id.map(str::to_string),
        message: message.chars().take(2048).collect(),
    };
    update_rule(&rule.id, |mut current| {
        if current.owner_uid != rule.owner_uid
            || current.generation != rule.generation
            || current.activity_id != rule.activity_id
        {
            return Err(format!("trigger `{}` changed during delivery", rule.id));
        }
        if status == TriggerDeliveryStatus::Indeterminate {
            current.enabled = false;
        }
        if matches!(
            status,
            TriggerDeliveryStatus::Submitted | TriggerDeliveryStatus::Recovered
        ) {
            current.last_fired_ms = Some(diagnostic.at_ms);
        }
        current.last_delivery = Some(diagnostic.clone());
        Ok(current)
    })?;
    #[cfg(unix)]
    fs::File::open(rules_dir())
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("sync trigger diagnostic directory: {error}"))?;
    emit(
        rule.owner_uid,
        &rule.id,
        rule.activity_id.as_deref(),
        &diagnostic,
    );
    Ok(())
}

pub(super) fn emit(
    owner_uid: Option<u32>,
    rule_id: &str,
    activity_id: Option<&str>,
    diagnostic: &TriggerDeliveryDiagnostic,
) {
    let audit = json!({
        "ts": chrono::Utc::now(),
        "event": "clawd.trigger.delivery",
        "rule_id": crate::audit_policy::safe_identity(rule_id),
        "activity_id": activity_id.map(crate::audit_policy::safe_identity),
        "owner_uid": owner_uid,
        "job_id": diagnostic.job_id.as_deref().map(crate::audit_policy::safe_identity),
        "session_id": diagnostic.session_id.as_deref().map(crate::audit_policy::safe_identity),
        "status": diagnostic.status,
        "detail": crate::audit_policy::text_digest(&diagnostic.message),
        "source": "scheduled-trigger",
        "attended": false,
    });
    if let Err(error) = crate::clawd::audit::append_jsonl(&audit) {
        tracing::warn!(trigger_id = %rule_id, %error, "failed to audit trigger delivery");
    }
    if matches!(
        diagnostic.status,
        TriggerDeliveryStatus::Submitted | TriggerDeliveryStatus::Recovered
    ) {
        return;
    }
    let Some(owner_uid) = owner_uid.filter(|uid| *uid != 0) else {
        return;
    };
    let indeterminate = diagnostic.status == TriggerDeliveryStatus::Indeterminate;
    let mut draft = crate::notifications::NotificationDraft::new(
        "trigger",
        if indeterminate {
            "trigger.indeterminate"
        } else {
            "trigger.blocked"
        },
        if indeterminate {
            crate::notifications::Severity::Error
        } else {
            crate::notifications::Severity::Warning
        },
        if indeterminate {
            "Automation needs inspection"
        } else {
            "Automation did not start"
        },
        format!("Trigger `{rule_id}`: {}", diagnostic.message),
    )
    .dedupe(format!(
        "trigger:{}:{}",
        rule_id,
        crate::crypto::sha256_hex(
            format!("{:?}:{}", diagnostic.status, diagnostic.message).as_bytes()
        )
    ));
    draft.job_id = Some(rule_id.to_string());
    draft.task_id = diagnostic.job_id.clone();
    draft.session_id = diagnostic.session_id.clone();
    if let Err(error) = crate::clawd::notifications::publish_for_owner(owner_uid, draft) {
        tracing::warn!(trigger_id = %rule_id, owner_uid, %error, "failed to notify trigger delivery");
    }
}

/// At most one Activity delivery occupies the cursor. Only correlation data is
/// retained; permissions must still come from the live rule and Root queue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ActivityDelivery {
    pub id: String,
    pub rule_id: String,
    pub generation: String,
    pub owner_uid: u32,
    pub activity_id: String,
    pub line_index: Option<usize>,
    pub raw_event: Option<String>,
    pub prompt_sha256: String,
    pub session_id: Option<String>,
}

impl ActivityDelivery {
    pub fn new(
        rule: &TriggerRule,
        prompt: &str,
        event: Option<(usize, &str)>,
    ) -> Result<Self, String> {
        if event.is_some_and(|(_, raw)| raw.len() > 64 * 1024) {
            return Err(
                "Activity trigger event exceeds the 64 KiB delivery bound; event skipped"
                    .to_string(),
            );
        }
        let generation = rule.generation.as_deref().ok_or_else(|| {
            "Activity trigger has no delivery identity; explicitly enable it first".to_string()
        })?;
        uuid::Uuid::parse_str(generation)
            .map_err(|_| "Activity trigger delivery identity is not a UUID".to_string())?;
        Ok(Self {
            id: uuid::Uuid::new_v4().to_string(),
            rule_id: rule.id.clone(),
            generation: generation.to_string(),
            owner_uid: rule
                .owner_uid
                .ok_or_else(|| "Activity trigger has no owner".to_string())?,
            activity_id: rule
                .activity_id
                .clone()
                .ok_or_else(|| "trigger has no Activity".to_string())?,
            line_index: event.map(|(line, _)| line),
            raw_event: event.map(|(_, raw)| raw.to_string()),
            prompt_sha256: crate::crypto::sha256_hex(prompt.as_bytes()),
            session_id: None,
        })
    }

    pub fn matches_rule(&self, rule: &TriggerRule) -> bool {
        self.rule_id == rule.id
            && rule.generation.as_deref() == Some(self.generation.as_str())
            && rule.owner_uid == Some(self.owner_uid)
            && rule.activity_id.as_deref() == Some(self.activity_id.as_str())
    }

    pub fn recover(&self, rule: &TriggerRule, store: &Store) -> Result<Job, String> {
        if !self.matches_rule(rule) {
            return Err(
                "trigger delivery belongs to a different rule incarnation or owner".to_string(),
            );
        }
        let prompt = match (self.line_index, self.raw_event.as_deref()) {
            (Some(_), Some(raw)) => {
                let event: Value = serde_json::from_str(raw)
                    .map_err(|error| format!("corrupt in-flight event: {error}"))?;
                if !delivery::visible_match(rule, &event, raw) {
                    return Err(
                        "in-flight event is no longer visible to or matched by its owner rule"
                            .to_string(),
                    );
                }
                fired_prompt(rule, &event)
            }
            (None, None) => rule.prompt.clone(),
            _ => return Err("in-flight event correlation is incomplete".to_string()),
        };
        if crate::crypto::sha256_hex(prompt.as_bytes()) != self.prompt_sha256 {
            return Err("in-flight trigger prompt no longer matches its rule".to_string());
        }
        let (_, job) = store
            .locate_for_owner(&self.id, Some(self.owner_uid))
            .map_err(|error| format!("inspect in-flight Job {}: {error}", self.id))?
            .ok_or_else(|| {
                format!(
                    "no owned Job {} can prove whether submission occurred",
                    self.id
                )
            })?;
        if job.id != self.id
            || job.activity_id.as_deref() != Some(self.activity_id.as_str())
            || self.session_id.is_none()
            || job.session_id != self.session_id
            || job.owner_home != rule.owner_home
            || job.max_turns != rule.max_turns
            || job.client != trigger_client()
            || crate::crypto::sha256_hex(job.prompt.as_bytes()) != self.prompt_sha256
        {
            return Err(format!(
                "Job {} does not match the recorded Activity delivery",
                self.id
            ));
        }
        let session = job
            .session_id
            .as_deref()
            .ok_or_else(|| "delivery Job has no Session".to_string())?
            .parse::<crate::session::SessionId>()
            .map_err(|error| format!("invalid delivery Session: {error}"))?;
        let meta = crate::session::get_meta(&session)
            .map_err(|error| format!("inspect delivery Session: {error}"))?;
        if meta.owner_uid != Some(self.owner_uid)
            || meta.activity_id.as_deref() != Some(self.activity_id.as_str())
            || meta.client != trigger_client()
            || meta.origin != Some(crate::session::SessionOrigin::TriggerDelegation)
        {
            return Err("delivery Session no longer matches its owned Activity Job".to_string());
        }
        Ok(job)
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/triggers/activity.rs"
    ));
}
