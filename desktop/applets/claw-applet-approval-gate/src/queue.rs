// SPDX-License-Identifier: GPL-3.0-only

//! Adapter around clawd's approval API.
//! Mirrors the storage schema documented at `core/src/approvals.rs`.

use clawd_client::{Client, Command};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "value", rename_all = "kebab-case")]
pub enum Scope {
    Path(String),
    Host(String),
    Name(String),
    SelfRef(String),
    Wild,
}

impl Scope {
    pub fn render(&self) -> String {
        match self {
            Scope::Path(p) => format!("path:{p}"),
            Scope::Host(h) => format!("host:{h}"),
            Scope::Name(n) => format!("name:{n}"),
            Scope::SelfRef(s) => format!("self:{s}"),
            Scope::Wild => "WILD".to_string(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Risk {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Meta {
    pub label: String,
    pub blurb: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub icon: Option<String>,
    pub risk: Risk,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Request {
    pub id: String,
    pub verb: String,
    pub scope: Scope,
    pub session: String,
    pub reason: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub owner_uid: Option<u32>,
    #[serde(default)]
    pub requester: Option<String>,
    pub requested_at: u64,
    #[serde(default)]
    pub meta: Option<Meta>,
}

#[derive(Debug, Clone)]
pub struct LoadError(pub String);

#[derive(Debug, Clone, Deserialize)]
struct PendingEnvelope {
    requests: Vec<Request>,
}

/// Reads pending requests from clawd's approval queue.
pub async fn load_pending() -> Result<Vec<Request>, LoadError> {
    let value = Client::from_env()
        .map_err(|error| LoadError(error.to_string()))?
        .call(Command::PermissionPending, json!({ "limit": 100 }))
        .await
        .map_err(|error| LoadError(error.to_string()))?;
    let mut envelope: PendingEnvelope =
        serde_json::from_value(value).map_err(|e| LoadError(format!("parse pending: {e}")))?;
    envelope.requests.sort_by_key(|req| req.requested_at);
    Ok(envelope.requests)
}

pub async fn approve(id: &str, duration: GrantDuration) -> Result<String, LoadError> {
    decide(id, "approve", Some(duration)).await
}

pub async fn deny(id: &str) -> Result<String, LoadError> {
    decide(id, "deny", None).await
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantDuration {
    Once,
    Session,
    Forever,
}

async fn decide(
    id: &str,
    decision: &str,
    duration: Option<GrantDuration>,
) -> Result<String, LoadError> {
    let mut command = tokio::process::Command::new("pkexec");
    command
        .arg("/usr/local/bin/claw-approval-helper")
        .arg("--id")
        .arg(id)
        .arg("--decision")
        .arg(decision);
    if let Some(duration) = duration {
        let duration = match duration {
            GrantDuration::Once => "once",
            GrantDuration::Session => "session",
            GrantDuration::Forever => "forever",
        };
        command.arg("--duration").arg(duration);
    }
    let output = command
        .output()
        .await
        .map_err(|e| LoadError(format!("launch approval helper: {e}")))?;
    if !output.status.success() {
        return Err(LoadError(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/queue.rs"));
}
