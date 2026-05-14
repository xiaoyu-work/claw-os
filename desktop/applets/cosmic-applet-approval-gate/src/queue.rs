// SPDX-License-Identifier: GPL-3.0-only

//! Adapter around `cos perms pending` (JSON pipe path), so the applet
//! never has to peek inside `$COS_DATA_DIR/approvals/` directly.
//! Mirrors the schema documented at `core/src/approvals.rs`.

use serde::Deserialize;
use std::process::Command;

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

impl Risk {
    pub fn label_key(&self) -> &'static str {
        match self {
            Risk::Low => "risk-low",
            Risk::Medium => "risk-medium",
            Risk::High => "risk-high",
            Risk::Critical => "risk-critical",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Meta {
    pub label: String,
    pub blurb: String,
    #[serde(default)]
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
    pub requester: Option<String>,
    pub requested_at: u64,
    #[serde(default)]
    pub meta: Option<Meta>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PendingEnvelope {
    pub count: usize,
    pub pending: Vec<Request>,
}

#[derive(Debug, Clone)]
pub struct LoadError(pub String);

/// Calls `cos perms pending` and parses the JSON envelope. Returns
/// the request list and any non-fatal warning string for the UI to
/// show in place of an empty list.
pub fn load_pending() -> Result<Vec<Request>, LoadError> {
    let output = Command::new(cos_binary())
        .args(["perms", "pending"])
        .env("COS_PERMS_JSON", "1")
        .output()
        .map_err(|e| LoadError(format!("spawn cos: {e}")))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(LoadError(if err.is_empty() {
            format!("cos perms pending exited with {}", output.status)
        } else {
            err
        }));
    }
    let envelope: PendingEnvelope = serde_json::from_slice(&output.stdout)
        .map_err(|e| LoadError(format!("parse cos output: {e}")))?;
    Ok(envelope.pending)
}

/// `cos perms approve <id> [--duration X]`. Returns the JSON output
/// on success.
pub fn approve(id: &str, duration: GrantDuration) -> Result<String, LoadError> {
    let status_arg = format!("{:?}", duration).to_lowercase();
    let output = Command::new(cos_binary())
        .args(["perms", "approve", id, "--duration", &status_arg])
        .env("COS_PERMS_JSON", "1")
        .output()
        .map_err(|e| LoadError(format!("spawn cos: {e}")))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(LoadError(if err.is_empty() {
            format!("cos perms approve exited with {}", output.status)
        } else {
            err
        }));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

pub fn deny(id: &str) -> Result<String, LoadError> {
    let output = Command::new(cos_binary())
        .args(["perms", "deny", id])
        .env("COS_PERMS_JSON", "1")
        .output()
        .map_err(|e| LoadError(format!("spawn cos: {e}")))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(LoadError(if err.is_empty() {
            format!("cos perms deny exited with {}", output.status)
        } else {
            err
        }));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[derive(Debug, Clone, Copy)]
pub enum GrantDuration {
    Once,
    Session,
    Forever,
}

fn cos_binary() -> String {
    std::env::var("COS_BIN").unwrap_or_else(|_| "cos".to_string())
}
