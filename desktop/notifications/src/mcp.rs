//! Native notification intent uses the durable OS service, never a worker bus.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use claw_os_sdk::mcp::{App, AppError, CallContext, Tool, ToolResult};
use serde::Deserialize;
use serde_json::{Value, json};

fn default_sender() -> String {
    "Claw OS Agent".into()
}
fn default_icon() -> String {
    "com.clawos.Notifications".into()
}
fn default_expiry() -> i32 {
    -1
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Post {
    summary: String,
    #[serde(default)]
    body: String,
    #[serde(default = "default_sender")]
    app_name: String,
    #[serde(default = "default_icon")]
    icon: String,
    #[serde(default = "default_expiry")]
    expire_ms: i32,
    #[serde(default)]
    transient: bool,
    #[serde(default)]
    dedupe_key: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Close {
    id: String,
}

fn text(value: &str, max: usize, multiline: bool) -> bool {
    value.chars().count() <= max
        && !value.trim().is_empty()
        && !value
            .chars()
            .any(|ch| ch.is_control() && !(multiline && matches!(ch, '\n' | '\r' | '\t')))
}

fn request(name: &str, input: Value) -> Result<Value, String> {
    if name == "notify.post" {
        let mut post: Post =
            serde_json::from_value(input).map_err(|_| "invalid notification arguments")?;
        if post.app_name.is_empty() {
            post.app_name = default_sender();
        }
        if !text(&post.summary, 240, false)
            || (!post.body.is_empty() && !text(&post.body, 4000, true))
            || !text(&post.app_name, 128, false)
        {
            return Err(
                "summary (240), body (4000), or sender label (128) is invalid or too long".into(),
            );
        }
        if post.expire_ms < -1 {
            return Err("expire_ms must be -1, 0, or a positive signed 32-bit integer".into());
        }
        if post.icon.len() > 128
            || !post
                .icon
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        {
            return Err("icon must be an icon-theme name, not a path or URL".into());
        }
        if post.dedupe_key.as_ref().is_some_and(|key| {
            key.is_empty()
                || key.len() > 128
                || !key.bytes().all(|b| {
                    b.is_ascii_alphanumeric()
                        || matches!(b, b'.' | b'_' | b'-' | b':' | b'/' | b'@' | b'+')
                })
        }) {
            return Err("dedupe_key must be a bounded notification identifier".into());
        }
        let mut value = json!({
            "action":"post", "summary":post.summary, "body":post.body, "app_name":post.app_name,
            "icon":post.icon, "expire_ms":post.expire_ms, "transient":post.transient,
        });
        if let Some(key) = post.dedupe_key {
            value["dedupe_key"] = json!(key);
        }
        Ok(value)
    } else if name == "notify.close" {
        let close: Close = serde_json::from_value(input).map_err(
            |_| "id must be the durable string returned by notify.post, not a desktop integer",
        )?;
        if !close
            .id
            .strip_prefix("notif-")
            .is_some_and(|id| id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err("invalid durable notification id".into());
        }
        Ok(json!({"action":"close","id":close.id}))
    } else {
        Err("unknown notification operation".into())
    }
}

struct NotificationTool(&'static str);

#[async_trait]
impl Tool for NotificationTool {
    fn name(&self) -> &'static str {
        self.0
    }

    async fn handle(&self, input: Value, context: CallContext) -> ToolResult {
        if let Err(error) = context.check_cancelled() {
            return ToolResult::error(error.to_string());
        }
        let request = match request(self.0, input) {
            Ok(value) => value.to_string(),
            Err(error) => return ToolResult::error(error),
        };
        let deadline = match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(now) => (now.as_millis() as u64 + 5000)
                .min(context.deadline_unix_ms().unwrap_or(u64::MAX))
                .to_string(),
            Err(_) => return ToolResult::error("invalid system clock"),
        };
        let call = claw_os_sdk::cos_call_json_async_with_stdin_binary(
            "/usr/local/bin/cos",
            "notification",
            self.0,
            [
                "__notifications",
                "request",
                "--request-stdin",
                "--deadline",
                &deadline,
            ],
            request.as_bytes(),
        );
        tokio::select! {
            biased;
            _ = context.cancelled() => ToolResult::error("notification call cancelled"),
            result = call => match result {
                Ok(value) => ToolResult::text(value.to_string()),
                Err(error) => ToolResult::error(error.to_string()),
            },
        }
    }
}

pub(crate) fn run() -> anyhow::Result<()> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let mut app = App::from_environment()?;
            if app.id() != "cosmic-notifications" {
                return Err(AppError::Manifest(
                    "Notifications requires the cosmic-notifications identity".into(),
                ));
            }
            for name in ["notify.post", "notify.close"] {
                app.bind(Arc::new(NotificationTool(name)))?;
            }
            app.serve_stdio().await
        })
        .map_err(|error| anyhow::anyhow!("MCP server exited: {error}"))
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/mcp.rs"));
}
