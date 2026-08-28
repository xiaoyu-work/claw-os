//! Bridge to `apps/exec` — running commands, launching long-lived
//! processes, and consulting `which`.
//!
//! GUI shells (the launcher, the app library, the terminal session
//! registrar) all funnel here. Ordinary starts are registry-backed and
//! identity-verified; explicit stdin starts are bounded, deadline-limited
//! transient launches with no output or registry artifacts.

use serde::Deserialize;
use std::time::Duration;

use super::{call, call_typed, call_typed_sensitive_with_timeout, BridgeError};

/// Maximum payload accepted by the explicit `exec.start` stdin API.
pub const MAX_START_STDIN_BYTES: usize = 128 * 1024;
pub const TRANSIENT_START_TIMEOUT: Duration = Duration::from_secs(5);

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
    pub launch_id: String,
    pub pid: u32,
    pub start_time_ticks: u64,
    pub command: Vec<String>,
}

/// Result of an unregistered, no-output transient launch.
#[derive(Debug, Clone, Deserialize)]
pub struct TransientLaunch {
    pub pid: u32,
    pub command: Vec<String>,
    pub transient: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("exec.start private stdin must not be empty")]
    EmptyInput,

    #[error("exec.start stdin is {actual} bytes; the limit is {limit} bytes")]
    InputTooLarge { actual: usize, limit: usize },

    #[error("exec.start returned an invalid transient launch response")]
    InvalidTransientResponse,

    #[error(transparent)]
    Bridge(#[from] BridgeError),
}

#[derive(Debug, thiserror::Error)]
pub enum StopError {
    #[error("exec.stop launch id must not be empty")]
    EmptyLaunchId,

    #[error("exec.stop PID must be between 1 and {max}")]
    InvalidPid { max: u32 },

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
/// The returned opaque `launch_id` can be passed to [`stop`]; PID and
/// start-time ticks are retained for diagnostics and reuse-safe verification.
pub fn start(argv: &[&str]) -> Result<LaunchHandle, BridgeError> {
    let args = start_arguments(argv, false);
    let handle: LaunchHandle =
        call_typed("exec", "start", args.iter().map(String::as_str), None)?;
    if handle.launch_id.trim().is_empty()
        || handle.pid == 0
        || handle.start_time_ticks == 0
        || handle.command.is_empty()
    {
        return Err(BridgeError::Decode {
            app: "exec".to_string(),
            verb: "start".to_string(),
            message: "invalid registered launch identity".to_string(),
        });
    }
    Ok(handle)
}

/// Spawn an unregistered, no-output process with one bounded stdin payload.
///
/// The `--stdin` and `--` routing markers are consumed before the exec app
/// receives the child command. The `cos` invocation is killed and reaped if it
/// does not return within [`TRANSIENT_START_TIMEOUT`]. Calls to [`start`] never
/// request or read caller stdin.
pub fn start_transient_with_stdin(
    argv: &[&str],
    stdin: &[u8],
) -> Result<TransientLaunch, StartError> {
    if stdin.is_empty() {
        return Err(StartError::EmptyInput);
    }
    if stdin.len() > MAX_START_STDIN_BYTES {
        return Err(StartError::InputTooLarge {
            actual: stdin.len(),
            limit: MAX_START_STDIN_BYTES,
        });
    }
    let args = start_arguments(argv, true);
    let launch: TransientLaunch = call_typed_sensitive_with_timeout(
        "exec",
        "start",
        args.iter().map(String::as_str),
        stdin,
        TRANSIENT_START_TIMEOUT,
    )
    .map_err(StartError::Bridge)?;
    if !launch.transient || launch.pid == 0 || launch.command.is_empty() {
        return Err(StartError::InvalidTransientResponse);
    }
    Ok(launch)
}

fn start_arguments(argv: &[&str], with_stdin: bool) -> Vec<String> {
    let mut args = Vec::with_capacity(argv.len() + usize::from(with_stdin) + 1);
    if with_stdin {
        args.push("--stdin".to_string());
    }
    args.push("--".to_string());
    args.extend(argv.iter().map(|argument| (*argument).to_string()));
    args
}

/// Stop a registered background process by opaque launch identity.
pub fn stop(launch_id: &str) -> Result<serde_json::Value, StopError> {
    if launch_id.trim().is_empty() {
        return Err(StopError::EmptyLaunchId);
    }
    call("exec", "stop", [launch_id], None).map_err(StopError::Bridge)
}

/// Compatibility stop by PID. The exec app resolves and verifies the
/// registered process identity before signaling it.
pub fn stop_pid(pid: u32) -> Result<serde_json::Value, StopError> {
    const MAX_PID: u32 = i32::MAX as u32;
    if pid == 0 || pid > MAX_PID {
        return Err(StopError::InvalidPid { max: MAX_PID });
    }
    call("exec", "stop", [pid.to_string()], None).map_err(StopError::Bridge)
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
