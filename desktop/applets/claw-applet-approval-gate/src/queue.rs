// SPDX-License-Identifier: GPL-3.0-only

//! Adapter around clawd's approval API.
//! Mirrors the storage schema documented at `core/src/approvals.rs`.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    future::Future,
    io,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::time::timeout;

const CLAWD_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const CLAWD_WRITE_TIMEOUT: Duration = Duration::from_secs(2);
const CLAWD_FLUSH_TIMEOUT: Duration = Duration::from_secs(2);
const CLAWD_READ_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy)]
struct ClawdTimeouts {
    connect: Duration,
    write: Duration,
    flush: Duration,
    read: Duration,
}

impl Default for ClawdTimeouts {
    fn default() -> Self {
        Self {
            connect: CLAWD_CONNECT_TIMEOUT,
            write: CLAWD_WRITE_TIMEOUT,
            flush: CLAWD_FLUSH_TIMEOUT,
            read: CLAWD_READ_TIMEOUT,
        }
    }
}

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
    clawd_request_at(&socket, command, params, ClawdTimeouts::default()).await
}

async fn clawd_request_at(
    socket: &Path,
    command: &str,
    params: Value,
    timeouts: ClawdTimeouts,
) -> Result<Value, LoadError> {
    let stream = io_with_timeout(
        timeouts.connect,
        format!("connect clawd {}", socket.display()),
        UnixStream::connect(socket),
    )
    .await?;
    clawd_request_on_stream(stream, command, params, timeouts).await
}

async fn clawd_request_on_stream(
    stream: UnixStream,
    command: &str,
    params: Value,
    timeouts: ClawdTimeouts,
) -> Result<Value, LoadError> {
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
    io_with_timeout(
        timeouts.write,
        format!("write clawd request {command}"),
        writer.write_all(&line),
    )
    .await?;
    io_with_timeout(
        timeouts.flush,
        format!("flush clawd request {command}"),
        writer.flush(),
    )
    .await?;

    let line = io_with_timeout(
        timeouts.read,
        format!("read clawd response {command}"),
        lines.next_line(),
    )
    .await?
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

async fn io_with_timeout<T, F>(
    duration: Duration,
    operation: String,
    future: F,
) -> Result<T, LoadError>
where
    F: Future<Output = io::Result<T>>,
{
    match timeout(duration, future).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(LoadError(format!("{operation}: {error}"))),
        Err(_) => Err(LoadError(format!(
            "{operation} timed out after {} ms",
            duration.as_millis()
        ))),
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

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/queue.rs"));
}
