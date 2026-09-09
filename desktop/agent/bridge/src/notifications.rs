//! The single desktop delivery consumer. Durable state stays in `clawd`;
//! numeric D-Bus ids are connection-local presentation handles, never App ids.

use std::collections::HashMap;
use std::fs::Metadata;
use std::os::unix::fs::MetadataExt;
use std::time::Duration;

use clawd_client::{Client, Command};
use futures::StreamExt;
use serde::Deserialize;
use serde_json::json;
use zbus::zvariant::Value as ZValue;
use zbus::{Connection, fdo::DBusProxy, names::BusName};

use crate::state::AppState;

const CLAIM_LIMIT: u64 = 16;
const LEASE_MS: u64 = 30_000;
const SERVICE: &str = "org.freedesktop.Notifications";
const OBJECT: &str = "/org/freedesktop/Notifications";
const CALL_TIMEOUT: Duration = Duration::from_secs(3);

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
    fn close_notification(&self, id: u32) -> zbus::Result<()>;
}

#[derive(Debug, Deserialize)]
struct DeliveryEnvelope {
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
    state: String,
    #[serde(default)]
    presentation: Option<Presentation>,
}

#[derive(Debug, Deserialize)]
struct Presentation {
    app_name: String,
    icon: String,
    expire_ms: i32,
    transient: bool,
}

#[derive(Deserialize)]
struct Changes {
    cursor: u64,
    changes: Vec<Change>,
}

#[derive(Deserialize)]
struct Change {
    notification: Notification,
}

pub fn spawn(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            if let Err(error) = run_session(&state).await {
                tracing::warn!(%error, "desktop notification delivery disconnected");
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    })
}

async fn run_session(state: &AppState) -> anyhow::Result<()> {
    let installed = std::fs::symlink_metadata("/usr/bin/cosmic-notifications")?;
    anyhow::ensure!(
        installed.is_file() && installed.uid() == 0 && installed.mode() & 0o022 == 0,
        "notification presenter is not an immutable installed binary"
    );
    let connection = Connection::session().await?;
    run_connection(&state.clawd, &connection, &installed).await
}

async fn presenter(connection: &Connection, expected: &Metadata) -> anyhow::Result<String> {
    let bus = DBusProxy::new(connection).await?;
    let name = BusName::try_from(SERVICE)?;
    let unique = bus.get_name_owner(name.clone()).await?;
    let unique_name = BusName::from(unique.clone());
    let uid = bus.get_connection_unix_user(unique_name.clone()).await?;
    let pid = bus.get_connection_unix_process_id(unique_name).await?;
    let process = std::fs::metadata(format!("/proc/{pid}"))?;
    let executable = std::fs::metadata(format!("/proc/{pid}/exe"))?;
    let owner = std::fs::metadata("/proc/self")?.uid();
    anyhow::ensure!(
        uid == owner
            && process.uid() == owner
            && executable.dev() == expected.dev()
            && executable.ino() == expected.ino(),
        "notification service is not the owner's native presenter"
    );
    anyhow::ensure!(
        bus.get_name_owner(name).await? == unique,
        "notification presenter changed"
    );
    Ok(unique.to_string())
}

pub(crate) async fn run_connection(
    client: &Client,
    connection: &Connection,
    expected: &Metadata,
) -> anyhow::Result<()> {
    let unique = tokio::time::timeout(CALL_TIMEOUT, presenter(connection, expected)).await??;
    let proxy = NotificationsProxy::builder(connection)
        .destination(unique.as_str())?
        .build()
        .await?;
    // One ordered stream preserves ActionInvoked before NotificationClosed.
    let mut signals = proxy.inner().receive_all_signals().await?;
    let mut visible: HashMap<String, u32> = HashMap::new();
    let mut cursor = 0_u64;
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            biased;
            signal = signals.next() => {
                let signal = signal.ok_or_else(|| anyhow::anyhow!("notification signal stream closed"))?;
                let header = signal.header();
                if header.sender().map(|value| value.as_str()) != Some(unique.as_str())
                    || header.path().map(|value| value.as_str()) != Some(OBJECT)
                {
                    continue;
                }
                let action = match header.member().map(|value| value.as_str()) {
                    Some("ActionInvoked") => {
                        let (id, action): (u32, String) = signal.body().deserialize()?;
                        (action == "default").then_some((id, Command::NotificationAcknowledge))
                    }
                    Some("NotificationClosed") => {
                        let (id, reason): (u32, u32) = signal.body().deserialize()?;
                        close_action(&mut visible, id, reason)
                    }
                    _ => None,
                };
                if let Some((desktop_id, command)) = action {
                    if let Some(id) = visible.iter().find_map(|(id, value)| (*value == desktop_id).then(|| id.clone())) {
                        client.call(command, json!({"id":id})).await?;
                        visible.remove(&id);
                        tokio::time::timeout(CALL_TIMEOUT, proxy.close_notification(desktop_id)).await??;
                    }
                }
            }
            _ = tick.tick() => {
                let bus = DBusProxy::new(connection).await?;
                anyhow::ensure!(bus.get_name_owner(BusName::try_from(SERVICE)?).await?.as_str() == unique,
                    "notification presenter disconnected");
                let changes: Changes = serde_json::from_value(client.call(Command::NotificationSubscribe,
                    json!({"cursor":cursor,"limit":500,"timeout_ms":0})).await?)?;
                for change in changes.changes {
                    if matches!(change.notification.state.as_str(), "dismissed" | "acknowledged") {
                        if let Some(id) = visible.remove(&change.notification.id) {
                            tokio::time::timeout(CALL_TIMEOUT, proxy.close_notification(id)).await??;
                        }
                    }
                }
                cursor = changes.cursor;
                let claims: DeliveryEnvelope = serde_json::from_value(client.call(
                    Command::NotificationDeliveryClaim,
                    json!({"channel":"desktop","limit":CLAIM_LIMIT,"lease_ms":LEASE_MS}),
                ).await?)?;
                for claim in claims.deliveries {
                    let notification = claim.notification;
                    let previous = visible.get(&notification.id).copied().unwrap_or(0);
                    let result = tokio::time::timeout(CALL_TIMEOUT, post(&proxy, &notification, previous)).await;
                    let params = match result {
                        Ok(Ok(id)) if id != 0 => {
                            visible.insert(notification.id.clone(), id);
                            json!({"id":notification.id,"channel":"desktop","status":"delivered"})
                        }
                        _ => json!({"id":notification.id,"channel":"desktop","status":"failed","error_code":"dbus"}),
                    };
                    client.call(Command::NotificationDeliveryComplete, params).await?;
                }
            }
        }
    }
}

fn plain_body(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn close_action(
    visible: &mut HashMap<String, u32>,
    id: u32,
    reason: u32,
) -> Option<(u32, Command)> {
    match reason {
        2 => Some((id, Command::NotificationDismiss)),
        1 | 3 => {
            visible.retain(|_, value| *value != id);
            None
        }
        _ => None,
    }
}

async fn post(
    proxy: &NotificationsProxy<'_>,
    notification: &Notification,
    replaces: u32,
) -> zbus::Result<u32> {
    let presentation = notification.presentation.as_ref();
    let urgency = ZValue::U8(urgency(&notification.severity));
    let transient = ZValue::Bool(presentation.is_some_and(|value| value.transient));
    let connection_bound = ZValue::Bool(true);
    let hints = HashMap::from([
        ("urgency", &urgency),
        ("transient", &transient),
        ("x-claw-connection-bound", &connection_bound),
    ]);
    proxy
        .notify(
            presentation.map_or("Claw OS Agent", |value| &value.app_name),
            replaces,
            presentation.map_or("com.clawos.Agent", |value| &value.icon),
            &notification.title,
            &plain_body(&notification.body),
            &["default", "Acknowledge"],
            hints,
            presentation.map_or(
                if notification.severity == "critical" {
                    0
                } else {
                    -1
                },
                |value| value.expire_ms,
            ),
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
