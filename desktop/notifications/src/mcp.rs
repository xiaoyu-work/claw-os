//! MCP tool surface for `cosmic-notifications` when launched in
//! server mode (`COS_MCP_SERVER=1`).
//!
//! Important context: the cosmic-notifications **process** is the
//! freedesktop notification daemon — it *receives* notifications.
//! In MCP mode we don't run the daemon (we don't want two daemons
//! on the same session bus). Instead we act as a **client**: the
//! `notify.post` tool sends an `org.freedesktop.Notifications.Notify`
//! call to whichever daemon is already running on the session bus.
//! This is how `cosmic-screenshot` already posts its "saved to …"
//! notification (see `desktop/screenshot/src/main.rs`).

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use claw_os_sdk::mcp::{App, CallContext, Tool, ToolResult};
use serde_json::{json, Value};
use zbus::zvariant::Value as ZValue;
use zbus::Connection;

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

struct PostTool;

fn sender_name(input: &Value) -> String {
    input
        .get("app_name")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("Claw OS Agent")
        .to_string()
}

#[async_trait]
impl Tool for PostTool {
    fn name(&self) -> &'static str {
        "notify.post"
    }

    async fn handle(&self, input: Value, context: CallContext) -> ToolResult {
        if let Err(error) = context.check_cancelled() {
            return ToolResult::error(error.to_string());
        }
        let summary = match input.get("summary").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return ToolResult::error("missing required field: summary"),
        };
        let body = input
            .get("body")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let icon = input
            .get("icon")
            .and_then(|v| v.as_str())
            .unwrap_or("com.clawos.Notifications")
            .to_string();
        let app = sender_name(&input);
        let expire_ms = match input.get("expire_ms").and_then(|v| v.as_i64()) {
            Some(value) => match i32::try_from(value) {
                Ok(value) => value,
                Err(_) => return ToolResult::error("expire_ms is outside the supported range"),
            },
            None => -1,
        };
        let transient = input
            .get("transient")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let conn = match Connection::session().await {
            Ok(c) => c,
            Err(e) => return ToolResult::error(format!("connect session bus: {e}")),
        };
        let proxy = match NotificationsProxy::new(&conn).await {
            Ok(p) => p,
            Err(e) => return ToolResult::error(format!("create proxy: {e}")),
        };

        let transient_val = ZValue::Bool(transient);
        let mut hints: HashMap<&str, &ZValue<'_>> = HashMap::new();
        if transient {
            hints.insert("transient", &transient_val);
        }

        match proxy
            .notify(&app, 0, &icon, &summary, &body, &[], hints, expire_ms)
            .await
        {
            Ok(id) => ToolResult::text(json!({ "id": id }).to_string()),
            Err(e) => ToolResult::error(format!("notify call failed: {e}")),
        }
    }
}

struct CloseTool;

#[async_trait]
impl Tool for CloseTool {
    fn name(&self) -> &'static str {
        "notify.close"
    }

    async fn handle(&self, input: Value, context: CallContext) -> ToolResult {
        if let Err(error) = context.check_cancelled() {
            return ToolResult::error(error.to_string());
        }
        let id = match input
            .get("id")
            .and_then(|v| v.as_u64())
            .and_then(|value| u32::try_from(value).ok())
        {
            Some(n) => n,
            None => return ToolResult::error("missing or non-integer field: id"),
        };
        let conn = match Connection::session().await {
            Ok(c) => c,
            Err(e) => return ToolResult::error(format!("connect session bus: {e}")),
        };
        let proxy = match NotificationsProxy::new(&conn).await {
            Ok(p) => p,
            Err(e) => return ToolResult::error(format!("create proxy: {e}")),
        };
        match proxy.close_notification(id).await {
            Ok(()) => ToolResult::text(json!({ "ok": true }).to_string()),
            Err(e) => ToolResult::error(format!("close call failed: {e}")),
        }
    }
}

pub(crate) fn run() -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let mut app = App::from_environment()?;
        app.bind(Arc::new(PostTool))?;
        app.bind(Arc::new(CloseTool))?;
        app.serve_stdio().await
    })
    .map_err(|error| anyhow::anyhow!("MCP server exited: {error}"))
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/mcp.rs"));
}
