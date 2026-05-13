//! Bridge to `apps/notify` — system notifications routed through the
//! kernel so the audit log can show "ui.notify by app X at time T".

use serde::Deserialize;

use super::{call, call_typed, BridgeError};

/// One row from `apps/notify list`.
#[derive(Debug, Clone, Deserialize)]
pub struct Notification {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    pub message: String,
    #[serde(default)]
    pub timestamp: Option<f64>,
}

/// Response from `apps/notify list`.
#[derive(Debug, Clone, Deserialize)]
pub struct ListResult {
    pub notifications: Vec<Notification>,
}

/// Send a notification. `title` is optional; some receivers (mobile,
/// headless tty) silently drop it.
pub fn send(title: Option<&str>, message: &str) -> Result<serde_json::Value, BridgeError> {
    let mut args = Vec::with_capacity(3);
    if let Some(t) = title {
        args.push("--title".to_string());
        args.push(t.to_string());
    }
    args.push(message.to_string());
    call("notify", "send", args.iter().map(String::as_str), None)
}

/// List recent notifications (delivered or pending).
pub fn list() -> Result<ListResult, BridgeError> {
    call_typed("notify", "list", std::iter::empty::<&str>(), None)
}
