// SPDX-License-Identifier: GPL-3.0-only

//! Adapter around `cos agent ls / show / stop / undo / resume`. This
//! is the entire boundary between the applet and the cos kernel —
//! the applet never reads `$COS_DATA_DIR/sessions/` directly. Schema
//! is the JSON envelope documented in `skills/claw-os/sessions.md`
//! and produced by `core/src/agent/lifecycle.rs`.

use serde::Deserialize;
use std::process::Command;

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

pub fn load_tasks() -> Result<Vec<Task>, LoadError> {
    let output = Command::new(cos_binary())
        .args(["agent", "ls"])
        .output()
        .map_err(|e| LoadError(format!("spawn cos: {e}")))?;
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
    use super::*;

    /// The `cos agent ls` JSON envelope shape we consume.
    /// If this test ever breaks, either the kernel's response shape
    /// changed or our `Task` struct is wrong — this is the contract
    /// boundary the GUI relies on.
    #[test]
    fn parses_real_kernel_ls_response() {
        let raw = r#"{
            "n": 2,
            "tasks": [
                {
                    "id": "ses_0019e2566eb1f_e71a8d6a8ca4",
                    "purpose": "smoke test",
                    "status": "running",
                    "creator_runtime": "smoke",
                    "created_at": "2025-01-01T00:00:00Z",
                    "ended_at": null,
                    "lease": {
                        "pid": 12345,
                        "runtime": "cos-agent",
                        "started_at": "2025-01-01T00:00:01Z",
                        "heartbeat_at": "2025-01-01T00:00:30Z"
                    }
                },
                {
                    "id": "ses_0019e25670000_aaaaaaaaaaaa",
                    "purpose": "",
                    "status": "paused",
                    "creator_runtime": null,
                    "created_at": "2025-01-01T00:01:00Z",
                    "ended_at": null,
                    "lease": null
                }
            ]
        }"#;
        let env: LsEnvelope = serde_json::from_str(raw).unwrap();
        assert_eq!(env.n, 2);
        assert_eq!(env.tasks[0].status, "running");
        assert!(env.tasks[0].lease.is_some());
        assert_eq!(env.tasks[0].lease.as_ref().unwrap().pid, 12345);
        assert_eq!(env.tasks[1].status, "paused");
        assert!(env.tasks[1].lease.is_none());
        assert!(env.tasks[1].creator_runtime.is_none());
    }

    /// Empty envelope (no active tasks) must not crash the parser.
    #[test]
    fn parses_empty_ls_response() {
        let raw = r#"{"n": 0, "tasks": []}"#;
        let env: LsEnvelope = serde_json::from_str(raw).unwrap();
        assert_eq!(env.n, 0);
        assert!(env.tasks.is_empty());
    }

    /// `cos agent show` envelope. We only require the fields we
    /// actually render — extra fields in the kernel response are
    /// allowed (forward-compat for new info the kernel may add).
    #[test]
    fn parses_real_kernel_show_response() {
        let raw = r#"{
            "id": "ses_0019e2566eb1f_e71a8d6a8ca4",
            "purpose": "rebuild reports",
            "status": "running",
            "role": "worker",
            "parent_session": null,
            "creator_runtime": "cos-agent",
            "budget": {},
            "created_at": "2025-01-01T00:00:00Z",
            "ended_at": null,
            "lease": null,
            "turns": {"count": 7, "first_at": "2025-01-01T00:00:00Z", "last_at": "2025-01-01T00:01:30Z"},
            "mutations": {"count": 3, "by_kind": {"fs.write": 2, "fs.rename": 1}},
            "stop_requested": false
        }"#;
        let detail: TaskDetail = serde_json::from_str(raw).unwrap();
        assert_eq!(detail.turns.count, 7);
        assert_eq!(detail.mutations.count, 3);
        assert!(!detail.stop_requested);
    }
}
