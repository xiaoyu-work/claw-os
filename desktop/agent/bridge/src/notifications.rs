//! Pulls pending notifications from `clawd` into the user's D-Bus session.

use std::collections::HashMap;
use std::time::Duration;

use clawd_client::Command;
use serde::Deserialize;
use serde_json::{Value, json};
use zbus::Connection;
use zbus::zvariant::Value as ZValue;

use crate::state::AppState;

const CLAIM_LIMIT: u64 = 16;
const LEASE_MS: u64 = 30_000;
const SUBSCRIBE_MS: u64 = 15_000;

#[zbus::proxy(
    interface = "org.freedesktop.Notifications",
    default_service = "org.freedesktop.Notifications",
    default_path = "/org/freedesktop/Notifications"
)]
trait Notifications {
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: &[&str],
        hints: HashMap<&str, &ZValue<'_>>,
        expire_timeout: i32,
    ) -> zbus::Result<u32>;
}

#[derive(Debug, Deserialize)]
struct DeliveryEnvelope {
    #[serde(default)]
    deliveries: Vec<DeliveryClaim>,
}

#[derive(Debug, Deserialize)]
struct DeliveryClaim {
    notification: Notification,
}

#[derive(Debug, Deserialize)]
struct Notification {
    id: String,
    severity: String,
    title: String,
    body: String,
}

pub fn spawn(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut cursor = 0_u64;
        loop {
            match run_session(&state, &mut cursor).await {
                Ok(()) => {}
                Err(error) => {
                    tracing::warn!(%error, "desktop notification delivery disconnected");
                    tokio::time::sleep(Duration::from_secs(3)).await;
                }
            }
        }
    })
}

async fn run_session(state: &AppState, cursor: &mut u64) -> anyhow::Result<()> {
    let connection = Connection::session().await?;
    let proxy = NotificationsProxy::new(&connection).await?;
    loop {
        let value = state
            .clawd
            .call(
                Command::NotificationDeliveryClaim,
                json!({
                    "channel": "desktop",
                    "limit": CLAIM_LIMIT,
                    "lease_ms": LEASE_MS,
                }),
            )
            .await?;
        let claims: DeliveryEnvelope = serde_json::from_value(value)?;
        if claims.deliveries.is_empty() {
            let update = state
                .clawd
                .call(
                    Command::NotificationSubscribe,
                    json!({
                        "cursor": *cursor,
                        "limit": CLAIM_LIMIT,
                        "timeout_ms": SUBSCRIBE_MS,
                    }),
                )
                .await?;
            if let Some(next) = update.get("cursor").and_then(Value::as_u64) {
                *cursor = next;
            }
            continue;
        }

        for claim in claims.deliveries {
            let result = post(&proxy, &claim.notification).await;
            let failed = result.is_err();
            let params = match &result {
                Ok(_) => json!({
                    "id": claim.notification.id,
                    "channel": "desktop",
                    "status": "delivered",
                }),
                Err(error) => {
                    tracing::warn!(
                        notification_id = %claim.notification.id,
                        %error,
                        "failed to post desktop notification"
                    );
                    json!({
                        "id": claim.notification.id,
                        "channel": "desktop",
                        "status": "failed",
                        "error_code": "dbus",
                    })
                }
            };
            state
                .clawd
                .call(Command::NotificationDeliveryComplete, params)
                .await?;
            if failed {
                anyhow::bail!("session notification bus is unavailable");
            }
        }
    }
}

async fn post(proxy: &NotificationsProxy<'_>, notification: &Notification) -> zbus::Result<u32> {
    let urgency = ZValue::U8(urgency(&notification.severity));
    let transient = ZValue::Bool(false);
    let mut hints: HashMap<&str, &ZValue<'_>> = HashMap::new();
    hints.insert("urgency", &urgency);
    hints.insert("transient", &transient);
    proxy
        .notify(
            "Claw OS Agent",
            0,
            "com.clawos.Agent",
            &notification.title,
            &notification.body,
            &[],
            hints,
            if notification.severity == "critical" {
                0
            } else {
                -1
            },
        )
        .await
}

fn urgency(severity: &str) -> u8 {
    match severity {
        "critical" | "error" => 2,
        "warning" => 1,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/notifications.rs"
    ));
}
