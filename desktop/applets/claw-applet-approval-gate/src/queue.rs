// SPDX-License-Identifier: GPL-3.0-only

//! Adapter around clawd's approval API.
//! Mirrors the storage schema documented at `core/src/approvals.rs`.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

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

#[derive(Debug, Clone)]
pub struct LoadError(pub String);

#[derive(Debug, Clone, Deserialize)]
struct PendingEnvelope {
    requests: Vec<Request>,
}

/// Reads pending requests from clawd's approval queue.
pub async fn load_pending() -> Result<Vec<Request>, LoadError> {
    let value = clawd_request("permission.pending", json!({ "limit": 100 })).await?;
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
    let mut params = json!({
        "id": id,
        "decision": decision,
        "decided_by": "claw-applet-approval-gate",
    });
    if let Some(duration) = duration {
        let duration = serde_json::to_value(duration)
            .map_err(|e| LoadError(format!("encode grant duration: {e}")))?;
        params
            .as_object_mut()
            .expect("params is an object")
            .insert("duration".into(), duration);
    }
    let value = clawd_request("permission.decide", params).await?;
    Ok(value.to_string())
}

#[derive(Debug, Serialize)]
struct ClawdRequest<'a> {
    id: u64,
    command: &'a str,
    params: Value,
}

#[derive(Debug, Deserialize)]
struct ClawdResponse {
    ok: bool,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<ClawdError>,
}

#[derive(Debug, Deserialize)]
struct ClawdError {
    code: String,
    message: String,
}

async fn clawd_request(command: &str, params: Value) -> Result<Value, LoadError> {
    let socket = clawd_socket_path();
    let stream = UnixStream::connect(&socket)
        .await
        .map_err(|e| LoadError(format!("connect clawd {}: {e}", socket.display())))?;
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    let request = ClawdRequest {
        id: 1,
        command,
        params,
    };
    let mut line = serde_json::to_vec(&request)
        .map_err(|e| LoadError(format!("encode clawd request {command}: {e}")))?;
    line.push(b'\n');
    writer
        .write_all(&line)
        .await
        .map_err(|e| LoadError(format!("write clawd request {command}: {e}")))?;
    writer
        .flush()
        .await
        .map_err(|e| LoadError(format!("flush clawd request {command}: {e}")))?;

    let line = lines
        .next_line()
        .await
        .map_err(|e| LoadError(format!("read clawd response {command}: {e}")))?
        .ok_or_else(|| LoadError(format!("clawd closed before responding to {command}")))?;
    let response: ClawdResponse = serde_json::from_str(&line)
        .map_err(|e| LoadError(format!("decode clawd response {command}: {e}")))?;
    if response.ok {
        Ok(response.result.unwrap_or(Value::Null))
    } else if let Some(error) = response.error {
        Err(LoadError(format!(
            "clawd {command} failed ({}): {}",
            error.code, error.message
        )))
    } else {
        Err(LoadError(format!("clawd {command} failed")))
    }
}

fn clawd_socket_path() -> PathBuf {
    if let Some(path) = std::env::var_os("COS_CLAWD_SOCKET") {
        return PathBuf::from(path);
    }
    runtime_dir().join("clawd.sock")
}

fn runtime_dir() -> PathBuf {
    std::env::var_os("COS_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new("/run/cos").to_path_buf())
}
