//! Bridge to `apps/exec` — running commands, launching long-lived
//! processes, and consulting `which`.
//!
//! GUI shells (the launcher, the app library, the terminal session
//! registrar) all funnel here so process spawns get a session id and
//! their lifetime is visible to `cos proc ps`.

use serde::Deserialize;

use super::{call, call_typed, BridgeError};

/// Response from `apps/exec run`.
#[derive(Debug, Clone, Deserialize)]
pub struct RunResult {
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
    #[serde(default)]
    pub exit_code: i32,
    #[serde(default)]
    pub timed_out: bool,
}

/// Response from `apps/exec which`.
#[derive(Debug, Clone, Deserialize)]
pub struct WhichResult {
    pub program: String,
    #[serde(default)]
    pub path: Option<String>,
}

/// Response from `apps/exec start`.
#[derive(Debug, Clone, Deserialize)]
pub struct StartResult {
    pub session_id: String,
    pub pid: u32,
}

/// Run `argv` synchronously, with an optional timeout in seconds.
pub fn run(argv: &[&str], timeout_secs: Option<u32>) -> Result<RunResult, BridgeError> {
    let mut a: Vec<String> = Vec::with_capacity(argv.len() + 2);
    if let Some(t) = timeout_secs {
        a.push("--timeout".into());
        a.push(t.to_string());
    }
    for x in argv {
        a.push((*x).into());
    }
    call_typed("exec", "run", a.iter().map(String::as_str), None)
}

/// Spawn a long-lived process and register it in the session
/// registry. Returns `{session_id, pid}` — the GUI is responsible for
/// keeping `session_id` around so caps continue to apply to the
/// child.
pub fn start(argv: &[&str]) -> Result<StartResult, BridgeError> {
    call_typed("exec", "start", argv.iter().copied(), None)
}

/// Stop a registered session.
pub fn stop(session_id: &str) -> Result<serde_json::Value, BridgeError> {
    call("exec", "stop", [session_id], None)
}

/// `which`-style lookup. `path` is `None` when the binary is not in
/// `$PATH`.
pub fn which(program: &str) -> Result<WhichResult, BridgeError> {
    call_typed("exec", "which", [program], None)
}
