// SPDX-License-Identifier: GPL-3.0-only

//! Shared read-only `cos agent ls` adapter. Task mutation and Agent UI stay separate.

use serde::Deserialize;
use std::{process::Command, time::Duration};
use tokio::process::Command as TokioCommand;

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
    n: usize,
    tasks: Vec<Task>,
}

#[derive(Debug, Clone)]
pub struct LoadError(pub String);

pub fn load_tasks() -> Result<Vec<Task>, LoadError> {
    let output = Command::new(crate::command::cos_binary())
        .args(["agent", "ls"])
        .output()
        .map_err(|e| LoadError(format!("spawn cos: {e}")))?;
    parse_tasks_output(output)
}

pub async fn load_tasks_async() -> Result<Vec<Task>, LoadError> {
    let mut command = TokioCommand::new(crate::command::cos_binary());
    command.args(["agent", "ls"]);
    let output = crate::command::output(
        &mut command,
        crate::command::OUTPUT_BYTES,
        Duration::from_secs(3),
    )
    .await
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
    if envelope.n != envelope.tasks.len() {
        return Err(LoadError(
            "cos agent ls returned an inconsistent task count".into(),
        ));
    }
    Ok(envelope.tasks)
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/tasks.rs"));
}
