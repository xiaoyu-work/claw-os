// SPDX-License-Identifier: GPL-3.0-only

//! Shared read-only `cos agent ls` adapter. Task mutation and Agent UI stay separate.

use serde::Deserialize;
use std::{process::Command, time::Duration};
use tokio::{process::Command as TokioCommand, time::timeout};

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

#[derive(Debug, Clone)]
pub struct LoadError(pub String);

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
    let output = timeout(Duration::from_secs(3), command.output())
        .await
        .map_err(|_| LoadError("cos agent ls timed out".to_string()))?
        .map_err(|e| LoadError(format!("spawn cos: {e}")))?;
    parse_tasks_output(output)
}

pub async fn observe() -> Result<Vec<Task>, String> {
    crate::policy::require("agent.observe", crate::policy::Scope::Name("tasks")).await?;
    load_tasks_async().await.map_err(|error| error.0)
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

fn cos_binary() -> String {
    std::env::var("COS_BIN").unwrap_or_else(|_| "cos".to_string())
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/tasks.rs"));
}
