//! # claw-os-sdk
//!
//! Official Rust SDK for [Claw OS](https://github.com/xiaoyu-work/claw-os).
//! This crate is the typed, language-idiomatic surface a Rust app uses
//! to add AI capabilities to itself — call the system LLM, expose
//! tools to the agent, discover the kernel tool catalogue.
//!
//! See [the SDK README](https://github.com/xiaoyu-work/claw-os/tree/main/claw-os-sdk)
//! and `wire/v1/README.md` for the protocol specification this crate
//! implements.
//!
//! ## Scope
//!
//! claw-os-sdk is **the AI-facing public surface**. It deliberately
//! does *not* contain the helpers claw-os's own bundled apps use for
//! self-gating (`policy`) or filesystem-mutation routing (`fs`,
//! `exec`, `pkg`, `notify`, `net`). Those live in the internal
//! [`cos-runtime`](../../../cos-runtime/) crate, which is not
//! published. A regular Linux app written for claw-os does not need
//! `cos-runtime`; it imports `claw-os-sdk` only when it wants to call
//! the system LLM or be invoked as a tool by the system agent.
//!
//! ## What's in here
//!
//! | Module        | Wire family | Equivalent CLI                  |
//! |---------------|-------------|---------------------------------|
//! | [`ai`]        | `ai`        | `cos ai chat / embed / ...`     |
//! | [`tools`]     | `tool`      | `cos ai tool <name> --app <id>` |
//! | [`envelope`]  | shared      | the common reply envelope       |
//! | [`generated`] | shared      | typed structs codegen'd from `wire/v1/*.schema.json` |
//!
//! Everything except `generated` is hand-written. `generated.rs` is
//! recomputed by `claw-os-sdk/wire/codegen.py` whenever the schemas
//! change.
//!
//! ## Transport
//!
//! Every call shells out to the `cos` binary on `$PATH`. The
//! subprocess model is intentional — identity, audit, and session
//! context come from process ancestry. Set `CLAW_COS_BIN` to override
//! the resolved binary (used by tests + dev setups).
//!
//! ## Performance
//!
//! The first cut is subprocess-per-call (~50 ms per call). A wire v2
//! socket transport will replace this without changing the surface
//! you see here. See `wire/v2-design.md` for the plan.

use std::ffi::OsStr;
use std::io::Write;
use std::process::{Command, Stdio};

use serde::de::DeserializeOwned;

pub mod ai;
pub mod envelope;
pub mod generated;
pub mod tools;

/// Errors returned by every bridge call.
#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    /// The `cos` binary couldn't be spawned. Usually means it's not
    /// on `$PATH` and `CLAW_COS_BIN` is unset.
    #[error("cos binary not found (set CLAW_COS_BIN or install cos): {0}")]
    BinaryNotFound(std::io::Error),

    /// The app dispatcher returned a structured JSON error
    /// (`{"error": "...", "code": "..."}`). This is the common case —
    /// e.g. a denied capability or a missing file.
    #[error("cos app {app} {verb}: {message}")]
    AppError {
        app: String,
        verb: String,
        message: String,
        code: Option<String>,
    },

    /// The subprocess exited non-zero but stdout didn't parse as a
    /// structured error. Stderr is bubbled up for diagnostics.
    #[error("cos app {app} {verb} exited {status}: {stderr}")]
    NonZeroExit {
        app: String,
        verb: String,
        status: i32,
        stderr: String,
    },

    /// stdout produced bytes but they couldn't be parsed as JSON, or
    /// the JSON shape didn't match the typed wrapper's expectation.
    #[error("invalid response from cos app {app} {verb}: {message}")]
    Decode {
        app: String,
        verb: String,
        message: String,
    },

    /// Bare IO failure spawning the subprocess or writing stdin.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

impl BridgeError {
    /// True if the underlying app reported `code = "denied"` (the
    /// kebab-cased `caps::Denial::reason`). GUI callers use this to
    /// pop up an approval dialog instead of an error toast.
    pub fn is_denied(&self) -> bool {
        matches!(
            self,
            BridgeError::AppError { code: Some(c), .. } if c == "denied"
        )
    }
}

/// Truncate a diagnostic string to ``limit`` bytes, appending a marker
/// for the number of bytes elided. Used to keep `Decode` /
/// `AppError` messages from leaking large request/response payloads
/// (e.g. AI prompt text, embedding vectors, file contents) into log
/// streams that may be world-readable.
fn truncate_diag(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        return s.to_string();
    }
    let mut cut = limit;
    // Don't slice mid-codepoint — UTF-8 safe rewind.
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    format!(
        "{}… [{} more bytes elided]",
        &s[..cut],
        s.len() - cut
    )
}

const DIAG_LIMIT: usize = 256;

/// Idiomatic alias preferred by external SDK consumers. New code
/// should `use claw_os_sdk::Error;` rather than the historical
/// `BridgeError` name (kept as an alias to avoid churn inside the
/// crate's own modules).
pub type Error = BridgeError;

// ---------------------------------------------------------------------------
// Raw call — every typed wrapper funnels through here.
// ---------------------------------------------------------------------------

/// Build the command invocation but don't run it. Tests use this to
/// inspect the argv that would be spawned without actually shelling
/// out to a `cos` binary.
fn build_command<A>(app: &str, verb: &str, args: A) -> Command
where
    A: IntoIterator,
    A::Item: AsRef<OsStr>,
{
    let bin = std::env::var("CLAW_COS_BIN").unwrap_or_else(|_| "cos".into());
    let mut cmd = Command::new(bin);
    cmd.arg("app").arg(app).arg(verb);
    for a in args {
        cmd.arg(a);
    }
    cmd
}

/// Invoke `cos app <app> <verb> <args...>` and return the raw JSON
/// response as a [`serde_json::Value`].
///
/// `stdin` is the body to forward to the subprocess on stdin (used by
/// `fs.write` to ship the file content without re-encoding through
/// argv). Pass `None` to inherit the parent's stdin behaviour.
pub fn call<A>(
    app: &str,
    verb: &str,
    args: A,
    stdin: Option<&[u8]>,
) -> Result<serde_json::Value, BridgeError>
where
    A: IntoIterator,
    A::Item: AsRef<OsStr>,
{
    let mut cmd = build_command(app, verb, args);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    if stdin.is_some() {
        cmd.stdin(Stdio::piped());
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => BridgeError::BinaryNotFound(e),
            _ => BridgeError::Io(e),
        })?;

    if let Some(bytes) = stdin {
        if let Some(mut s) = child.stdin.take() {
            s.write_all(bytes)?;
            // Explicitly drop the stdin handle so the subprocess sees
            // EOF and can shut down its read side. Without this drop
            // `wait_with_output` below would deadlock on subprocesses
            // that read all of stdin before producing any output
            // (which is exactly what the Python apps do).
            drop(s);
        }
    }

    let out = child.wait_with_output()?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();

    // First: try to parse stdout as JSON. The Python apps always
    // print one JSON object — success or error — to stdout. Stderr
    // is only used by the wrapper itself when the subprocess hard-
    // failed before the app could respond.
    if !stdout.trim().is_empty() {
        let parsed: serde_json::Value =
            serde_json::from_str(stdout.trim()).map_err(|e| BridgeError::Decode {
                app: app.to_string(),
                verb: verb.to_string(),
                message: format!(
                    "not JSON ({e}): {}",
                    truncate_diag(&stdout, DIAG_LIMIT)
                ),
            })?;

        if let Some(err) = parsed.get("error").and_then(|v| v.as_str()) {
            return Err(BridgeError::AppError {
                app: app.to_string(),
                verb: verb.to_string(),
                message: err.to_string(),
                code: parsed
                    .get("code")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
            });
        }

        // Even if stdout parsed cleanly, a non-zero exit means the
        // app actually failed (e.g. the wrapper crashed *after*
        // printing a partial result, or the JSON didn't include an
        // `error` field but the process still aborted). Surface that
        // as a hard failure so callers don't see a "success" payload
        // that contradicts the kernel's audit log.
        if !out.status.success() {
            return Err(BridgeError::NonZeroExit {
                app: app.to_string(),
                verb: verb.to_string(),
                status: out.status.code().unwrap_or(-1),
                stderr: truncate_diag(&stderr, DIAG_LIMIT),
            });
        }

        return Ok(parsed);
    }

    // Empty stdout — the wrapper itself failed (e.g. `cos` binary
    // crashed). Use the exit code + stderr.
    Err(BridgeError::NonZeroExit {
        app: app.to_string(),
        verb: verb.to_string(),
        status: out.status.code().unwrap_or(-1),
        stderr: truncate_diag(&stderr, DIAG_LIMIT),
    })
}

/// Typed variant of [`call`] — deserialises stdout into the caller's
/// chosen struct. Used by the typed app wrappers (`fs::read`,
/// `pkg::has`, …) — both inside this crate and inside the internal
/// `cos-runtime` crate.
#[doc(hidden)]
pub fn call_typed<A, R>(
    app: &str,
    verb: &str,
    args: A,
    stdin: Option<&[u8]>,
) -> Result<R, BridgeError>
where
    A: IntoIterator,
    A::Item: AsRef<OsStr>,
    R: DeserializeOwned,
{
    let v = call(app, verb, args, stdin)?;
    serde_json::from_value(v.clone()).map_err(|e| BridgeError::Decode {
        app: app.to_string(),
        verb: verb.to_string(),
        message: format!(
            "type mismatch ({e}): {}",
            truncate_diag(&v.to_string(), DIAG_LIMIT)
        ),
    })
}

// ---------------------------------------------------------------------------
// Raw cos-CLI call — used by ai/policy/tools modules whose request
// surface lives outside `cos app <id> <verb>`. The `family` and
// `verb` strings are propagated into [`BridgeError`] for diagnostics.
// ---------------------------------------------------------------------------

/// Invoke any `cos` sub-command and parse stdout (or stderr fall-back)
/// as JSON. Used by [`ai`] and [`tools`] (and `cos-runtime`'s
/// `policy`) for `cos ai ...`, hidden policy checks, etc.
///
/// `family` and `verb` are surfaced in [`BridgeError`] variants when
/// the call fails — pass any human-meaningful strings.
#[doc(hidden)]
pub fn cos_call_json<A>(
    family: &str,
    verb: &str,
    args: A,
) -> Result<serde_json::Value, BridgeError>
where
    A: IntoIterator,
    A::Item: AsRef<OsStr>,
{
    let bin = std::env::var("CLAW_COS_BIN").unwrap_or_else(|_| "cos".into());
    let mut cmd = Command::new(bin);
    for a in args {
        cmd.arg(a);
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let child = cmd
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => BridgeError::BinaryNotFound(e),
            _ => BridgeError::Io(e),
        })?;

    let out = child.wait_with_output()?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let candidate = if !stdout.trim().is_empty() {
        stdout.trim().to_string()
    } else if !stderr.trim().is_empty() {
        stderr.trim().to_string()
    } else {
        return Err(BridgeError::NonZeroExit {
            app: family.to_string(),
            verb: verb.to_string(),
            status: out.status.code().unwrap_or(-1),
            stderr: truncate_diag(&stderr, DIAG_LIMIT),
        });
    };

    let parsed: serde_json::Value =
        serde_json::from_str(&candidate).map_err(|e| BridgeError::Decode {
            app: family.to_string(),
            verb: verb.to_string(),
            message: format!(
                "not JSON ({e}): {}",
                truncate_diag(&candidate, DIAG_LIMIT)
            ),
        })?;

    if let Some(err) = parsed.get("error").and_then(|v| v.as_str()) {
        return Err(BridgeError::AppError {
            app: family.to_string(),
            verb: verb.to_string(),
            message: err.to_string(),
            code: parsed
                .get("code")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        });
    }

    // Same correctness fix as `call`: a clean-shaped JSON object on
    // stdout with no `error` field is not enough to call this a
    // success when the subprocess actually crashed. Audit log will
    // record the failure either way; surfacing it here keeps callers
    // from acting on a phantom-success.
    if !out.status.success() {
        return Err(BridgeError::NonZeroExit {
            app: family.to_string(),
            verb: verb.to_string(),
            status: out.status.code().unwrap_or(-1),
            stderr: truncate_diag(&stderr, DIAG_LIMIT),
        });
    }

    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_command_uses_env_override() {
        std::env::set_var("CLAW_COS_BIN", "/tmp/fake-cos");
        let cmd = build_command("fs", "ls", &["/tmp"]);
        assert_eq!(cmd.get_program(), "/tmp/fake-cos");
        let argv: Vec<&OsStr> = cmd.get_args().collect();
        assert_eq!(argv, &["app", "fs", "ls", "/tmp"]);
        std::env::remove_var("CLAW_COS_BIN");
    }

    #[test]
    fn build_command_default_is_path_lookup() {
        std::env::remove_var("CLAW_COS_BIN");
        let cmd = build_command("fs", "ls", std::iter::empty::<&str>());
        assert_eq!(cmd.get_program(), "cos");
    }

    /// Fake `cos` binary that emits a fixed JSON object so we can
    /// exercise the parsing path without a real backend. Used by
    /// several integration-style tests.
    fn write_fake_cos(dir: &std::path::Path, json: &str) -> std::path::PathBuf {
        let script = dir.join("cos");
        std::fs::write(
            &script,
            format!("#!/bin/sh\ncat <<'EOF'\n{json}\nEOF\n"),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script, perms).unwrap();
        }
        script
    }

    #[test]
    #[cfg(unix)]
    fn call_parses_success_json() {
        let dir = tempfile::tempdir().unwrap();
        let bin = write_fake_cos(dir.path(), r#"{"hello":"world","n":3}"#);
        std::env::set_var("CLAW_COS_BIN", &bin);

        let v = call("noop", "ping", std::iter::empty::<&str>(), None).unwrap();
        assert_eq!(v["hello"], "world");
        assert_eq!(v["n"], 3);

        std::env::remove_var("CLAW_COS_BIN");
    }

    #[test]
    #[cfg(unix)]
    fn call_surfaces_error_field_as_app_error() {
        let dir = tempfile::tempdir().unwrap();
        let bin = write_fake_cos(
            dir.path(),
            r#"{"error":"file not found: /x","code":"not-found"}"#,
        );
        std::env::set_var("CLAW_COS_BIN", &bin);

        let err = call("fs", "read", &["/x"], None).unwrap_err();
        match err {
            BridgeError::AppError {
                app,
                verb,
                message,
                code,
            } => {
                assert_eq!(app, "fs");
                assert_eq!(verb, "read");
                assert_eq!(message, "file not found: /x");
                assert_eq!(code.as_deref(), Some("not-found"));
            }
            other => panic!("expected AppError, got {other:?}"),
        }

        std::env::remove_var("CLAW_COS_BIN");
    }

    #[test]
    fn is_denied_recognises_denied_code() {
        let err = BridgeError::AppError {
            app: "fs".into(),
            verb: "write".into(),
            message: "permission denied".into(),
            code: Some("denied".into()),
        };
        assert!(err.is_denied());
    }
}
