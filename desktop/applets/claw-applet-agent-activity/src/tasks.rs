// SPDX-License-Identifier: GPL-3.0-only

//! Adapter around `cos agent ls / show / stop / undo / resume`. This
//! is the entire boundary between the applet and the cos kernel —
//! the applet never reads `$COS_DATA_DIR/sessions/` directly. Schema
//! is the JSON envelope documented in `skills/claw-os/sessions.md`
//! and produced by `core/src/agent/lifecycle.rs`.

use serde::Deserialize;
use std::process::Command;
use std::time::Duration;
use tokio::{process::Command as TokioCommand, time::timeout};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(3);

/// One row in the `cos agent ls` response.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Task {
    pub id: String,
    #[serde(default)]
    pub purpose: String,
    pub status: String,
    #[serde(default)]
    pub creator_runtime: Option<String>,
    pub created_at: String,
    #[serde(default)]
    pub ended_at: Option<String>,
    #[serde(default)]
    pub lease: Option<LeaseInfo>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct LeaseInfo {
    pub pid: u32,
    #[serde(default)]
    pub runtime: Option<String>,
    pub started_at: String,
    pub heartbeat_at: String,
}

#[derive(Debug, Clone, Deserialize)]
struct LsEnvelope {
    #[serde(default)]
    n: usize,
    #[serde(default)]
    tasks: Vec<Task>,
}

/// Fields returned by `cos agent show <id>` that we surface in the
/// detail card. Anything else in the envelope is ignored — adding
/// fields to the kernel response will not break this applet.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct TaskDetail {
    pub id: String,
    #[serde(default)]
    pub purpose: String,
    pub status: String,
    #[serde(default)]
    pub creator_runtime: Option<String>,
    pub turns: TurnSummary,
    pub mutations: MutationSummary,
    #[serde(default)]
    pub stop_requested: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct TurnSummary {
    pub count: usize,
    #[serde(default)]
    pub last_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct MutationSummary {
    pub count: usize,
}

#[derive(Debug, Clone)]
pub struct LoadError(pub String);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AgentMode {
    Local,
    Cloud,
    Unconfigured,
    #[default]
    Unknown,
}

pub fn load_tasks() -> Result<Vec<Task>, LoadError> {
    let output = Command::new(cos_binary())
        .args(["agent", "ls"])
        .output()
        .map_err(|e| LoadError(format!("spawn cos: {e}")))?;
    parse_tasks_output(output)
}

pub async fn load_tasks_async() -> Result<Vec<Task>, LoadError> {
    let mut command = TokioCommand::new(cos_binary());
    command.args(["agent", "ls"]).kill_on_drop(true);
    let output = timeout(COMMAND_TIMEOUT, command.output())
        .await
        .map_err(|_| LoadError("cos agent ls timed out".to_string()))?
        .map_err(|e| LoadError(format!("spawn cos: {e}")))?;
    parse_tasks_output(output)
}

fn parse_tasks_output(output: std::process::Output) -> Result<Vec<Task>, LoadError> {
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(LoadError(if err.is_empty() {
            format!("cos agent ls exited with {}", output.status)
        } else {
            err
        }));
    }
    let envelope: LsEnvelope = serde_json::from_slice(&output.stdout)
        .map_err(|e| LoadError(format!("parse cos agent ls output: {e}")))?;
    debug_assert_eq!(envelope.n, envelope.tasks.len());
    Ok(envelope.tasks)
}

/// Read the configured LLM provider through the public `cos` surface.
/// The applet must not parse the config file directly: config-path and
/// migration rules belong to the kernel.
pub fn load_mode() -> Result<AgentMode, LoadError> {
    let output = Command::new(cos_binary())
        .args(["agent", "setup", "llm", "--status"])
        .output()
        .map_err(|e| LoadError(format!("spawn cos: {e}")))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(LoadError(if err.is_empty() {
            format!("cos agent setup llm --status exited with {}", output.status)
        } else {
            err
        }));
    }

    let status: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| LoadError(format!("parse cos agent setup status: {e}")))?;
    Ok(classify_provider(
        status.get("provider").and_then(serde_json::Value::as_str),
        status.get("endpoint").and_then(serde_json::Value::as_str),
    ))
}

fn classify_provider(provider: Option<&str>, endpoint: Option<&str>) -> AgentMode {
    if endpoint.is_some_and(endpoint_is_local) {
        return AgentMode::Local;
    }
    match provider.map(str::trim).filter(|p| !p.is_empty()) {
        Some("ollama" | "llama_local" | "local") => AgentMode::Local,
        Some("none" | "mock") | None => AgentMode::Unconfigured,
        Some(_) => AgentMode::Cloud,
    }
}

fn endpoint_is_local(endpoint: &str) -> bool {
    let endpoint = endpoint.trim().to_ascii_lowercase();
    let authority = endpoint
        .strip_prefix("http://")
        .or_else(|| endpoint.strip_prefix("https://"))
        .unwrap_or(&endpoint)
        .split('/')
        .next()
        .unwrap_or_default();
    authority == "localhost"
        || authority.starts_with("localhost:")
        || authority == "[::1]"
        || authority.starts_with("[::1]:")
        || authority == "127.0.0.1"
        || authority.starts_with("127.0.0.1:")
}

pub fn show_task(id: &str) -> Result<TaskDetail, LoadError> {
    let output = Command::new(cos_binary())
        .args(["agent", "show", id])
        .output()
        .map_err(|e| LoadError(format!("spawn cos: {e}")))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(LoadError(if err.is_empty() {
            format!("cos agent show exited with {}", output.status)
        } else {
            err
        }));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|e| LoadError(format!("parse cos agent show output: {e}")))
}

pub fn stop_task(id: &str) -> Result<String, LoadError> {
    run_simple("stop", id)
}

pub fn undo_task(id: &str) -> Result<String, LoadError> {
    run_simple("undo", id)
}

pub fn resume_task(id: &str) -> Result<String, LoadError> {
    run_simple("resume", id)
}

fn run_simple(verb: &str, id: &str) -> Result<String, LoadError> {
    let output = Command::new(cos_binary())
        .args(["agent", verb, id])
        .output()
        .map_err(|e| LoadError(format!("spawn cos: {e}")))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(LoadError(if err.is_empty() {
            format!("cos agent {verb} exited with {}", output.status)
        } else {
            err
        }));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn cos_binary() -> String {
    std::env::var("COS_BIN").unwrap_or_else(|_| "cos".to_string())
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/tasks.rs"
    ));
}
