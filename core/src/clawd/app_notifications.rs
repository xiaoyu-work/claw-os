//! Exact App/action intent over the authoritative Notification Service.

use serde_json::{json, Value};

use super::authority::Decision;
use super::client_identity::ClientIdentity;
use super::protocol::BrokerError;
use super::wire::requests::{AppNotificationControl, AppNotificationRequest};
use crate::caps::{Cap, Scope, Verb};
use crate::notifications::{
    self, Notification, NotificationDraft, NotificationError, NotificationMutation,
    NotificationPresentation, NotificationService, NotificationState, Severity,
};

const APP: &str = "cosmic-notifications";
const SOURCE: &str = "app:cosmic-notifications";
const NOTIFY_APP: &str = "notify";
const NOTIFY_SOURCE: &str = "app:notify";

enum Intent {
    Post(NotificationDraft),
    Close(String),
    Send(NotificationDraft),
    List(usize),
}

impl Intent {
    fn requirement(&self) -> (&'static str, Cap) {
        match self {
            Self::Post(_) | Self::Close(_) => (APP, Cap::unscoped(Verb::UI_NOTIFY)),
            Self::Send(_) => (NOTIFY_APP, Cap::unscoped(Verb::UI_NOTIFY)),
            Self::List(_) => (NOTIFY_APP, Cap::new(Verb::DATA_INBOX_READ, Scope::Wild)),
        }
    }
}

fn validate(request: AppNotificationRequest) -> Result<Intent, BrokerError> {
    match request {
        AppNotificationRequest::Post {
            summary,
            body,
            app_name,
            icon,
            expire_ms,
            transient,
            dedupe_key,
        } => {
            let mut draft = NotificationDraft::new(
                SOURCE,
                "app.notification",
                Severity::Info,
                summary.as_str(),
                body.as_str(),
            );
            draft.presentation = Some(NotificationPresentation {
                app_name: if app_name.as_str().is_empty() {
                    "Claw OS Agent".into()
                } else {
                    app_name.as_str().into()
                },
                icon: icon.as_str().into(),
                expire_ms,
                transient,
            });
            draft.dedupe_key = dedupe_key.map(|key| key.as_str().to_owned());
            draft.validate().map_err(service_error)?;
            Ok(Intent::Post(draft))
        }
        AppNotificationRequest::Close { id } => {
            let id = id.as_str();
            if !id.strip_prefix("notif-").is_some_and(|value| {
                value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
            }) {
                return Err(BrokerError::execution(
                    "id must be the durable string returned by notify.post; desktop integers are not accepted",
                ));
            }
            Ok(Intent::Close(id.to_owned()))
        }
        AppNotificationRequest::Send { message, urgent } => {
            if message.as_str().trim().is_empty() {
                return Err(BrokerError::execution("message must be non-empty"));
            }
            let mut draft = NotificationDraft::new(
                NOTIFY_SOURCE,
                "app.notification",
                if urgent {
                    Severity::Warning
                } else {
                    Severity::Info
                },
                if urgent {
                    "Urgent notification"
                } else {
                    "Notification"
                },
                message.as_str(),
            );
            draft.presentation = Some(NotificationPresentation {
                app_name: "Notifications".into(),
                icon: "com.clawos.Notifications".into(),
                expire_ms: -1,
                transient: false,
            });
            draft.validate().map_err(service_error)?;
            Ok(Intent::Send(draft))
        }
        AppNotificationRequest::List { limit } => {
            if !(1..=100).contains(&limit) {
                return Err(BrokerError::execution("limit must be 1..100"));
            }
            Ok(Intent::List(limit as usize))
        }
    }
}

fn authorize(authority: &Decision, owner: u32, intent: &Intent) -> Result<(), BrokerError> {
    let (app, capability) = intent.requirement();
    authority
        .require_app(app)
        .map_err(BrokerError::authorization)?;
    if authority.owner_uid() != owner {
        return Err(BrokerError::authorization("notification owner mismatch"));
    }
    let _authorized = authority
        .require(capability)
        .map_err(BrokerError::authorization)?;
    Ok(())
}

fn deadline(value: u64) -> Result<(), BrokerError> {
    if u64::try_from(notifications::now_ms()).map_or(true, |now| now >= value) {
        return Err(BrokerError::execution(
            "notification request deadline expired",
        ));
    }
    Ok(())
}

fn service_error(error: NotificationError) -> BrokerError {
    match error {
        NotificationError::NotFound => BrokerError::execution("notification not found"),
        NotificationError::Invalid(_) => BrokerError::execution(error.to_string()),
        _ => BrokerError::unavailable("Notification Service storage is unavailable"),
    }
}

fn publish(
    service: &dyn NotificationService,
    mut draft: NotificationDraft,
    owner: u32,
    authority: &Decision,
) -> Result<Notification, BrokerError> {
    draft.session_id = authority.session_id().map(ToOwned::to_owned);
    draft.task_id = authority.task_id().map(ToOwned::to_owned);
    let record = service.publish(owner, draft).map_err(service_error)?;
    super::system_journal::record_notification_event("notification.published", &record);
    Ok(record)
}

fn notify_projection(record: &Notification, include_state: bool) -> Result<Value, BrokerError> {
    let timestamp = chrono::DateTime::from_timestamp_millis(record.created_at_ms)
        .ok_or_else(|| BrokerError::unavailable("Notification Service timestamp is invalid"))?
        .format("%Y-%m-%dT%H:%M:%S")
        .to_string();
    let mut result = json!({
        "id": record.id,
        "message": record.body,
        "urgent": record.severity >= Severity::Warning,
        "timestamp": timestamp,
    });
    if include_state {
        result["read"] = json!(record.state != NotificationState::Unread);
        result["state"] = json!(record.state);
    }
    Ok(result)
}

fn apply(
    service: &dyn NotificationService,
    intent: Intent,
    owner: u32,
    authority: &Decision,
    until: u64,
) -> Result<Value, BrokerError> {
    deadline(until)?;
    authorize(authority, owner, &intent)?;
    deadline(until)?;
    match intent {
        Intent::Post(draft) => {
            let record = publish(service, draft, owner, authority)?;
            Ok(json!({"id": record.id}))
        }
        Intent::Close(id) => {
            let record = service
                .mutate_source(owner, SOURCE, &id, NotificationMutation::Dismiss)
                .map_err(service_error)?;
            super::system_journal::record_notification_event("notification.state-changed", &record);
            Ok(json!({"ok": true, "id": record.id, "state": record.state}))
        }
        Intent::Send(draft) => {
            let record = publish(service, draft, owner, authority)?;
            notify_projection(&record, false)
        }
        Intent::List(limit) => {
            let page = service
                .list_source(owner, NOTIFY_SOURCE, limit)
                .map_err(service_error)?;
            let rows = page
                .notifications
                .iter()
                .map(|record| notify_projection(record, true))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(json!({"notifications": rows, "total": page.total}))
        }
    }
}

pub fn control(
    params: Value,
    client: &ClientIdentity,
    authority: &Decision,
) -> Result<Value, BrokerError> {
    let request: AppNotificationControl = serde_json::from_value(params)
        .map_err(|_| BrokerError::execution("invalid notification request"))?;
    let intent = validate(request.request)?;
    deadline(request.deadline_unix_ms)?;
    let owner = client.require_uid().map_err(BrokerError::authorization)?;
    authorize(authority, owner, &intent)?;
    let service = notifications::open_default().map_err(service_error)?;
    apply(&service, intent, owner, authority, request.deadline_unix_ms)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/app_notifications.rs"
    ));
}
