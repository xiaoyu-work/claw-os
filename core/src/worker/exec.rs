//! Bounded execution of a prepared worker.
//!
//! The provider hands back a `Command`; this is what turns it into a
//! finished result without letting the worker decide when to stop.
//! Output is drained on background threads with a byte ceiling (so a
//! worker cannot deadlock the launcher by filling a pipe, nor exhaust
//! it by writing forever), the wall clock is owned by the launcher,
//! and expiry kills the whole cgroup and process group rather than the
//! direct child alone.

use std::io::{Read, Write};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use super::provider::PreparedLaunch;

/// What a bounded run produced.
pub struct WorkerOutput {
    pub status: std::process::ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub timed_out: bool,
}

impl WorkerOutput {
    pub fn stdout_string(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    pub fn stderr_string(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }

    /// Typed outcome facts for the audit trail: never worker output.
    pub fn audit_facts(&self) -> serde_json::Value {
        serde_json::json!({
            "exit_code": self.status.code(),
            "timed_out": self.timed_out,
            "stdout_bytes": self.stdout.len(),
            "stderr_bytes": self.stderr.len(),
            "stdout_truncated": self.stdout_truncated,
            "stderr_truncated": self.stderr_truncated,
        })
    }
}

/// Run a prepared launch to completion with captured output.
///
/// `on_spawn` runs the moment the sandbox exists and before any output
/// is read, which is where the launcher binds the session to the child
/// it just created. A failure there kills the worker rather than
/// letting an unbound process run.
pub fn run_captured(
    prepared: PreparedLaunch,
    stdin_data: Option<Vec<u8>>,
    limits: super::policy::Limits,
    on_spawn: impl FnOnce(u32) -> Result<(), String>,
) -> Result<WorkerOutput, String> {
    let PreparedLaunch {
        mut command,
        resources,
        ..
    } = prepared;
    if let Some(data) = &stdin_data {
        if data.len() as u64 > limits.input_bytes {
            return Err("worker stdin exceeds the launch input ceiling".to_string());
        }
    }
    command
        .stdin(if stdin_data.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command
        .spawn()
        .map_err(|error| format!("spawn sandboxed worker: {error}"))?;
    let pid = child.id();
    if let Err(error) = on_spawn(pid) {
        resources.kill_all(Some(pid));
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }

    let writer = match stdin_data {
        Some(data) => {
            let stdin = child
                .stdin
                .take()
                .ok_or_else(|| "sandboxed worker stdin is unavailable".to_string())?;
            Some(std::thread::spawn(move || {
                let mut stdin = stdin;
                let _ = stdin.write_all(&data);
            }))
        }
        None => None,
    };

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "sandboxed worker stdout is unavailable".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "sandboxed worker stderr is unavailable".to_string())?;
    let ceiling = limits.output_bytes;
    let stdout_reader = std::thread::spawn(move || read_bounded(stdout, ceiling));
    let stderr_reader = std::thread::spawn(move || read_bounded(stderr, ceiling));

    let (status, timed_out) = wait_bounded(&mut child, limits.deadline(), || {
        resources.kill_all(Some(pid));
    })?;
    if let Some(writer) = writer {
        let _ = writer.join();
    }
    let (stdout, stdout_truncated) = stdout_reader
        .join()
        .map_err(|_| "sandboxed worker stdout reader panicked".to_string())?;
    let (stderr, stderr_truncated) = stderr_reader
        .join()
        .map_err(|_| "sandboxed worker stderr reader panicked".to_string())?;
    // Anything the worker left behind — a double-forked daemon, a
    // background thread that outlived its parent — goes now.
    resources.kill_all(Some(pid));
    Ok(WorkerOutput {
        status,
        stdout,
        stderr,
        stdout_truncated,
        stderr_truncated,
        timed_out,
    })
}

/// Wait for `child`, killing everything it spawned when the deadline
/// expires.
pub fn wait_bounded(
    child: &mut Child,
    deadline: Option<Duration>,
    kill: impl Fn(),
) -> Result<(std::process::ExitStatus, bool), String> {
    let Some(deadline) = deadline else {
        let status = child
            .wait()
            .map_err(|error| format!("wait for sandboxed worker: {error}"))?;
        return Ok((status, false));
    };
    let expiry = Instant::now() + deadline;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok((status, false)),
            Ok(None) if Instant::now() < expiry => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Ok(None) => {
                kill();
                let _ = child.kill();
                let status = child
                    .wait()
                    .map_err(|error| format!("reap timed-out worker: {error}"))?;
                return Ok((status, true));
            }
            Err(error) => return Err(format!("wait for sandboxed worker: {error}")),
        }
    }
}

/// Read to EOF, keeping at most `ceiling` bytes and reporting whether
/// anything was dropped. The stream is still drained so the worker
/// never blocks on a full pipe.
fn read_bounded(reader: impl Read, ceiling: u64) -> (Vec<u8>, bool) {
    let mut reader = reader;
    let mut kept: Vec<u8> = Vec::new();
    let mut buffer = [0_u8; 32 * 1024];
    let mut total = 0_u64;
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        total = total.saturating_add(read as u64);
        let remaining = (ceiling as usize).saturating_sub(kept.len());
        if remaining > 0 {
            kept.extend_from_slice(&buffer[..read.min(remaining)]);
        }
    }
    (kept, total > ceiling)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/worker/exec.rs"
    ));
}
