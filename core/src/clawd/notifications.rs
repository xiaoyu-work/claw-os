//! Owner-scoped notification broker and delivery orchestration.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use crate::caps::Role;
use crate::notifications::{
    self, DeliveryAdapter, DeliveryChannel, DeliveryResult, Notification, NotificationAction,
    NotificationDraft, NotificationMutation, NotificationPreferences, NotificationService,
    NtfyAdapter, NtfyTarget, Severity,
};

use super::client_identity::ClientIdentity;

const DEFAULT_SUBSCRIBE_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_DELIVERY_LEASE_MS: i64 = 30_000;
const EXTERNAL_BATCH_SIZE: usize = 16;

pub fn publish(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    let owner_uid = client.require_uid()?;
    let source = required_string(&params, "source")?;
    if owner_uid != 0
        && matches!(
            source.as_str(),
            "agent" | "approval" | "cron" | "heartbeat" | "nudge" | "trigger"
        )
    {
        return Err(format!("reserved notification source: {source}"));
    }
    let mut draft = NotificationDraft::new(
        source,
        required_string(&params, "kind")?,
        Severity::parse(&required_string(&params, "severity")?)
            .map_err(|error| error.to_string())?,
        required_string(&params, "title")?,
        required_string(&params, "body")?,
    );
    if let Some(policy) = optional_string(&params, "delivery_policy") {
        draft.delivery_policy = crate::notifications::DeliveryPolicy::parse(&policy)
            .map_err(|error| error.to_string())?;
    }
    draft.dedupe_key = optional_string(&params, "dedupe_key");
    draft.task_id = optional_string(&params, "task_id");
    draft.session_id = optional_string(&params, "session_id");
    draft.job_id = optional_string(&params, "job_id");
    draft.expires_at_ms = params.get("expires_at_ms").and_then(Value::as_i64);
    if let Some(actions) = params.get("actions") {
        draft.actions = serde_json::from_value::<Vec<NotificationAction>>(actions.clone())
            .map_err(|error| format!("invalid notification actions: {error}"))?;
    }
    publish_for_owner(owner_uid, draft).and_then(|notification| {
        serde_json::to_value(notification).map_err(|error| error.to_string())
    })
}

pub fn list(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    let owner_uid = client.require_uid()?;
    let include_dismissed = params
        .get("include_dismissed")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let limit = optional_limit(&params)?;
    let service = notifications::open_default().map_err(|error| error.to_string())?;
    let records = service
        .list(owner_uid, include_dismissed, limit)
        .map_err(|error| error.to_string())?;
    let cursor = service
        .cursor(owner_uid)
        .map_err(|error| error.to_string())?;
    let unread = records
        .iter()
        .filter(|record| {
            matches!(
                record.state,
                crate::notifications::NotificationState::Unread
            )
        })
        .count();
    Ok(json!({
        "schema": crate::notifications::SCHEMA_VERSION,
        "cursor": cursor,
        "unread": unread,
        "notifications": records,
    }))
}

pub async fn subscribe(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    let owner_uid = client.require_uid()?;
    let cursor = params.get("cursor").and_then(Value::as_u64).unwrap_or(0);
    let limit = optional_limit(&params)?;
    let timeout_ms = params
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_SUBSCRIBE_TIMEOUT_MS);
    let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
    let service = notifications::open_default().map_err(|error| error.to_string())?;
    loop {
        let batch = service
            .changes(owner_uid, cursor, limit)
            .map_err(|error| error.to_string())?;
        if !batch.changes.is_empty() || timeout_ms == 0 {
            return Ok(json!({
                "schema": crate::notifications::SCHEMA_VERSION,
                "cursor": batch.cursor,
                "timed_out": false,
                "changes": batch.changes,
            }));
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(json!({
                "schema": crate::notifications::SCHEMA_VERSION,
                "cursor": cursor,
                "timed_out": true,
                "changes": [],
            }));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

pub fn mark_read(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    mutate(params, client, NotificationMutation::Read)
}

pub fn acknowledge(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    mutate(params, client, NotificationMutation::Acknowledge)
}

pub fn dismiss(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    mutate(params, client, NotificationMutation::Dismiss)
}

fn mutate(
    params: Value,
    client: &ClientIdentity,
    mutation: NotificationMutation,
) -> Result<Value, String> {
    let owner_uid = client.require_uid()?;
    let id = required_string(&params, "id")?;
    let service = notifications::open_default().map_err(|error| error.to_string())?;
    let notification = service
        .mutate(owner_uid, &id, mutation)
        .map_err(|error| error.to_string())?;
    super::system_journal::record_notification_event("notification.state-changed", &notification);
    serde_json::to_value(notification).map_err(|error| error.to_string())
}

pub fn get_preferences(client: &ClientIdentity) -> Result<Value, String> {
    let owner_uid = client.require_uid()?;
    let service = notifications::open_default().map_err(|error| error.to_string())?;
    let preferences = service
        .preferences(owner_uid)
        .map_err(|error| error.to_string())?;
    serde_json::to_value(preferences).map_err(|error| error.to_string())
}

pub fn set_preferences(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    let owner_uid = client.require_uid()?;
    let preferences = NotificationPreferences {
        web_enabled: required_bool(&params, "web_enabled")?,
        desktop_enabled: required_bool(&params, "desktop_enabled")?,
        ntfy_enabled: required_bool(&params, "ntfy_enabled")?,
        web_min_severity: Severity::parse(&required_string(&params, "web_min_severity")?)
            .map_err(|error| error.to_string())?,
        desktop_min_severity: Severity::parse(&required_string(&params, "desktop_min_severity")?)
            .map_err(|error| error.to_string())?,
        ntfy_min_severity: Severity::parse(&required_string(&params, "ntfy_min_severity")?)
            .map_err(|error| error.to_string())?,
        muted_kinds: params
            .get("muted_kinds")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .map(|value| {
                        value
                            .as_str()
                            .map(ToOwned::to_owned)
                            .ok_or_else(|| "muted_kinds must contain strings".to_string())
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?
            .unwrap_or_default(),
        dnd_start_minute_utc: params
            .get("dnd_start_minute_utc")
            .and_then(Value::as_u64)
            .map(|value| {
                u16::try_from(value).map_err(|_| "dnd_start_minute_utc is too large".to_string())
            })
            .transpose()?,
        dnd_end_minute_utc: params
            .get("dnd_end_minute_utc")
            .and_then(Value::as_u64)
            .map(|value| {
                u16::try_from(value).map_err(|_| "dnd_end_minute_utc is too large".to_string())
            })
            .transpose()?,
        critical_bypasses_dnd: required_bool(&params, "critical_bypasses_dnd")?,
        retention_days: params
            .get("retention_days")
            .and_then(Value::as_u64)
            .ok_or_else(|| "retention_days is required".to_string())
            .and_then(|value| {
                u16::try_from(value).map_err(|_| "retention_days is too large".to_string())
            })?,
        ntfy_server: required_string(&params, "ntfy_server")?,
        ntfy_topic: optional_string(&params, "ntfy_topic"),
    };
    let service = notifications::open_default().map_err(|error| error.to_string())?;
    let preferences = service
        .set_preferences(owner_uid, preferences)
        .map_err(|error| error.to_string())?;
    serde_json::to_value(preferences).map_err(|error| error.to_string())
}

pub fn claim_deliveries(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    let owner_uid = client.require_uid()?;
    let channel = DeliveryChannel::parse(&required_string(&params, "channel")?)
        .map_err(|error| error.to_string())?;
    if channel == DeliveryChannel::Ntfy {
        return Err("ntfy delivery is owned by the daemon dispatcher".to_string());
    }
    let limit = optional_limit(&params)?;
    let lease_ms = params
        .get("lease_ms")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_DELIVERY_LEASE_MS as u64);
    let lease_ms =
        i64::try_from(lease_ms).map_err(|_| "delivery lease is too large".to_string())?;
    let service = notifications::open_default().map_err(|error| error.to_string())?;
    let deliveries = service
        .claim_deliveries(Some(owner_uid), channel, limit, lease_ms)
        .map_err(|error| error.to_string())?;
    Ok(json!({ "deliveries": deliveries }))
}

pub fn complete_delivery(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    let owner_uid = client.require_uid()?;
    let id = required_string(&params, "id")?;
    let channel = DeliveryChannel::parse(&required_string(&params, "channel")?)
        .map_err(|error| error.to_string())?;
    if channel == DeliveryChannel::Ntfy {
        return Err("ntfy delivery is owned by the daemon dispatcher".to_string());
    }
    let result = match required_string(&params, "status")?.as_str() {
        "delivered" => DeliveryResult::Delivered,
        "suppressed" => DeliveryResult::Suppressed,
        "failed" => DeliveryResult::Failed {
            error_code: required_string(&params, "error_code")?,
            retry_at_ms: crate::notifications::now_ms() + 30_000,
        },
        _ => return Err("delivery status must be delivered, suppressed, or failed".to_string()),
    };
    let service = notifications::open_default().map_err(|error| error.to_string())?;
    let notification = service
        .complete_delivery(owner_uid, &id, channel, result)
        .map_err(|error| error.to_string())?;
    serde_json::to_value(notification).map_err(|error| error.to_string())
}

pub fn publish_for_owner(owner_uid: u32, draft: NotificationDraft) -> Result<Notification, String> {
    let service = notifications::open_default().map_err(|error| error.to_string())?;
    let notification = service
        .publish(owner_uid, draft)
        .map_err(|error| error.to_string())?;
    super::system_journal::record_notification_event("notification.published", &notification);
    Ok(notification)
}

pub fn publish_for_known_owners(draft: NotificationDraft) {
    match known_owner_uids() {
        Ok(owners) => {
            for owner_uid in owners {
                if let Err(error) = publish_for_owner(owner_uid, draft.clone()) {
                    tracing::warn!(
                        owner_uid,
                        %error,
                        "failed to publish system notification"
                    );
                }
            }
        }
        Err(error) => tracing::warn!(%error, "failed to enumerate notification owners"),
    }
}

pub fn publish_due_nudges() {
    let now = crate::agent::nudge::now_epoch_s();
    let owners = match known_owner_uids() {
        Ok(owners) => owners,
        Err(error) => {
            tracing::warn!(%error, "failed to enumerate owners for due nudges");
            return;
        }
    };
    for owner_uid in owners {
        let path = crate::paths::clawd_user_agent_state_dir(owner_uid).join("nudges.json");
        if !path.is_file() {
            continue;
        }
        let store = crate::agent::nudge::NudgeStore::new(path);
        for nudge in store.due(now) {
            let mut draft = NotificationDraft::new(
                "nudge",
                "nudge.due",
                Severity::Info,
                "Reminder",
                crate::notifications::bounded_body(&nudge.message),
            )
            .dedupe(format!("nudge:{}:{}", nudge.id, nudge.due_at_epoch_s));
            draft.actions.push(NotificationAction {
                id: "open-agent".to_string(),
                label: "Open Agent".to_string(),
                uri: "clawos://agent".to_string(),
            });
            match publish_for_owner(owner_uid, draft) {
                Ok(_) => {
                    if let Err(error) = store.fire(&nudge.id, now) {
                        tracing::warn!(
                            owner_uid,
                            nudge_id = %nudge.id,
                            %error,
                            "failed to advance delivered nudge"
                        );
                    }
                }
                Err(error) => tracing::warn!(
                    owner_uid,
                    nudge_id = %nudge.id,
                    %error,
                    "failed to publish due nudge"
                ),
            }
        }
    }
}

pub fn spawn_external_dispatcher() -> tokio::task::JoinHandle<()> {
    tokio::spawn(async {
        let service = match notifications::open_default() {
            Ok(service) => Arc::new(service),
            Err(error) => {
                tracing::error!(%error, "notification dispatcher could not open its store");
                return;
            }
        };
        let adapter = NtfyAdapter::default();
        let mut ticker = tokio::time::interval(Duration::from_secs(5));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            if let Err(error) = dispatch_ntfy_once(service.as_ref(), &adapter).await {
                tracing::warn!(%error, "notification ntfy dispatch failed");
            }
        }
    })
}

async fn dispatch_ntfy_once(
    service: &impl NotificationService,
    adapter: &NtfyAdapter,
) -> Result<usize, String> {
    let claims = service
        .claim_deliveries(
            None,
            DeliveryChannel::Ntfy,
            EXTERNAL_BATCH_SIZE,
            DEFAULT_DELIVERY_LEASE_MS,
        )
        .map_err(|error| error.to_string())?;
    let mut delivered = 0usize;
    for claim in claims {
        let owner_uid = claim.notification.owner_uid;
        let preferences = service
            .preferences(owner_uid)
            .map_err(|error| error.to_string())?;
        if !preferences.ntfy_enabled {
            service
                .complete_delivery(
                    owner_uid,
                    &claim.notification.id,
                    DeliveryChannel::Ntfy,
                    DeliveryResult::Suppressed,
                )
                .map_err(|error| error.to_string())?;
            continue;
        }
        let target = match ntfy_target(owner_uid, &preferences) {
            Ok(target) => target,
            Err(error) => {
                service
                    .complete_delivery(
                        owner_uid,
                        &claim.notification.id,
                        DeliveryChannel::Ntfy,
                        DeliveryResult::Failed {
                            error_code: "configuration".to_string(),
                            retry_at_ms: crate::notifications::now_ms()
                                + retry_delay_ms(claim.attempts),
                        },
                    )
                    .map_err(|store_error| store_error.to_string())?;
                tracing::warn!(
                    owner_uid,
                    notification_id = %claim.notification.id,
                    %error,
                    "ntfy target is unavailable"
                );
                continue;
            }
        };
        let result = match adapter.deliver(&claim.notification, &target).await {
            Ok(()) => {
                delivered += 1;
                DeliveryResult::Delivered
            }
            Err(error) => DeliveryResult::Failed {
                error_code: error.code().to_string(),
                retry_at_ms: crate::notifications::now_ms() + retry_delay_ms(claim.attempts),
            },
        };
        service
            .complete_delivery(
                owner_uid,
                &claim.notification.id,
                DeliveryChannel::Ntfy,
                result,
            )
            .map_err(|error| error.to_string())?;
    }
    Ok(delivered)
}

fn ntfy_target(
    owner_uid: u32,
    preferences: &NotificationPreferences,
) -> Result<NtfyTarget, String> {
    let topic = preferences
        .ntfy_topic
        .clone()
        .ok_or_else(|| "ntfy topic is not configured".to_string())?;
    let home = crate::paths::verified_home_for_uid(owner_uid)?;
    let bearer_token = crate::credential::load_optional_for_scheduler(
        "ntfy_token",
        "default",
        &home,
        owner_uid,
        Role::AgentHost.credential_tier(),
    )?;
    Ok(NtfyTarget {
        server: preferences.ntfy_server.clone(),
        topic,
        bearer_token,
    })
}

fn retry_delay_ms(attempts: u32) -> i64 {
    let exponent = attempts.saturating_sub(1).min(7);
    (30_000_i64.saturating_mul(1_i64 << exponent)).min(60 * 60 * 1_000)
}

fn known_owner_uids() -> Result<Vec<u32>, String> {
    let service = notifications::open_default().map_err(|error| error.to_string())?;
    let mut owners: BTreeSet<u32> = service
        .known_owner_uids()
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|uid| *uid != 0)
        .collect();
    let users = crate::paths::data_dir().join("users");
    match std::fs::read_dir(&users) {
        Ok(entries) => {
            for entry in entries.flatten() {
                if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
                    continue;
                }
                if let Some(uid) = entry
                    .file_name()
                    .to_str()
                    .and_then(|value| value.parse::<u32>().ok())
                    .filter(|uid| *uid != 0)
                {
                    owners.insert(uid);
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("read notification owner directory: {error}")),
    }
    Ok(owners.into_iter().collect())
}

fn optional_limit(params: &Value) -> Result<usize, String> {
    params
        .get("limit")
        .and_then(Value::as_u64)
        .map(|limit| {
            usize::try_from(limit)
                .map_err(|_| format!("limit is too large for this platform: {limit}"))
        })
        .transpose()
        .map(|limit| limit.unwrap_or(crate::notifications::DEFAULT_LIST_LIMIT))
}

fn required_bool(params: &Value, key: &str) -> Result<bool, String> {
    params
        .get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("{key} is required"))
}

fn required_string(params: &Value, key: &str) -> Result<String, String> {
    optional_string(params, key).ok_or_else(|| format!("{key} is required"))
}

fn optional_string(params: &Value, key: &str) -> Option<String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/notifications.rs"
    ));
}
