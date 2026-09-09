//! Bounded native App intent over the authoritative Notification Service.

use serde_json::{json, Value};

use super::authority::Decision;
use super::client_identity::ClientIdentity;
use super::protocol::BrokerError;
use super::wire::requests::{AppNotificationControl, AppNotificationRequest};
use crate::caps::{Cap, Verb};
use crate::notifications::{
    self, NotificationDraft, NotificationError, NotificationMutation, NotificationPresentation,
    NotificationService, Severity,
};

const APP: &str = "cosmic-notifications";
const SOURCE: &str = "app:cosmic-notifications";

enum Intent {
    Post(NotificationDraft),
    Close(String),
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
    }
}

fn authorize(authority: &Decision, owner: u32) -> Result<(), BrokerError> {
    authority
        .require_app(APP)
        .map_err(BrokerError::authorization)?;
    if authority.owner_uid() != owner {
        return Err(BrokerError::authorization("notification owner mismatch"));
    }
    let _authorized = authority
        .require(Cap::unscoped(Verb::UI_NOTIFY))
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

fn apply(
    service: &dyn NotificationService,
    intent: Intent,
    owner: u32,
    authority: &Decision,
    until: u64,
) -> Result<Value, BrokerError> {
    deadline(until)?;
    authorize(authority, owner)?;
    deadline(until)?;
    let (notification, event, result) = match intent {
        Intent::Post(mut draft) => {
            draft.session_id = authority.session_id().map(ToOwned::to_owned);
            draft.task_id = authority.task_id().map(ToOwned::to_owned);
            let record = service.publish(owner, draft).map_err(service_error)?;
            let result = json!({"id": record.id});
            (record, "notification.published", result)
        }
        Intent::Close(id) => {
            let record = service
                .mutate_source(owner, SOURCE, &id, NotificationMutation::Dismiss)
                .map_err(service_error)?;
            let result = json!({"ok": true, "id": record.id, "state": record.state});
            (record, "notification.state-changed", result)
        }
    };
    super::system_journal::record_notification_event(event, &notification);
    Ok(result)
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
    authorize(authority, owner)?;
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
