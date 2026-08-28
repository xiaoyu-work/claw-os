//! Bridge to `apps/exec` — running commands, launching long-lived
//! processes, and consulting `which`.
//!
//! GUI shells (the launcher, the app library, the terminal session
//! registrar) all funnel here so process spawns are registered by PID and
//! their lifetime is visible to `cos app exec ps`.

use serde::Deserialize;

use super::{call, call_typed, BridgeError};

/// Maximum payload accepted by the explicit `exec.start` stdin API.
pub const MAX_START_STDIN_BYTES: usize = 128 * 1024;

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

/// Durable handle returned by `apps/exec start`.
#[derive(Debug, Clone, Deserialize)]
pub struct LaunchHandle {
    pub pid: u32,
    pub command: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("exec.start stdin is {actual} bytes; the limit is {limit} bytes")]
    InputTooLarge { actual: usize, limit: usize },

    #[error(transparent)]
    Bridge(#[from] BridgeError),
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

/// Spawn a long-lived process and register it in the process registry.
///
/// The returned PID can be passed to [`stop`]; `command` is the argv recorded
/// by the exec app.
pub fn start(argv: &[&str]) -> Result<LaunchHandle, BridgeError> {
    call_typed("exec", "start", argv.iter().copied(), None)
}

/// Spawn a registered process with one bounded, explicit stdin payload.
///
/// The `--stdin` routing flag is consumed by `cos` and is not forwarded to the
/// app or recorded child command. Calls to [`start`] never request or read
/// caller stdin.
pub fn start_with_stdin(argv: &[&str], stdin: &[u8]) -> Result<LaunchHandle, StartError> {
    if stdin.len() > MAX_START_STDIN_BYTES {
        return Err(StartError::InputTooLarge {
            actual: stdin.len(),
            limit: MAX_START_STDIN_BYTES,
        });
    }
    let mut args = Vec::with_capacity(argv.len() + 1);
    args.push("--stdin");
    args.extend_from_slice(argv);
    call_typed("exec", "start", args, Some(stdin)).map_err(StartError::Bridge)
}

/// Stop a registered background process by PID.
pub fn stop(pid: u32) -> Result<serde_json::Value, BridgeError> {
    call("exec", "stop", [pid.to_string()], None)
}

/// `which`-style lookup. `path` is `None` when the binary is not in
/// `$PATH`.
pub fn which(program: &str) -> Result<WhichResult, BridgeError> {
    call_typed("exec", "which", [program], None)
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/exec.rs"));
}
