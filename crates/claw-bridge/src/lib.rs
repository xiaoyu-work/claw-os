//! Claw OS bridge library — the single seam every desktop GUI uses
//! to call the system apps.
//!
//! ## Why it exists
//!
//! Claw OS ships two front-ends to the same application backends
//! under `apps/<name>/main.py`:
//!
//! * The terminal binary `cos`, via `cos app fs read /etc/hosts` etc.
//! * The desktop apps (`desktop/files`, `desktop/edit`, `desktop/store`,
//!   …), which were forked from COSMIC.
//!
//! Without this crate the desktop apps would (and did) talk to the
//! kernel directly — `std::fs::read`, `Command::new("apt")` — bypassing
//! the capability check, the audit log, and the snapshot/checkpoint
//! pipeline. `claw_bridge::*` puts every such call through
//! `cos app <name> <verb>` so the GUI and the terminal share one
//! enforcement path.
//!
//! ## API shape
//!
//! Each module mirrors one of the apps in `apps/`:
//!
//! * [`fs`]      — `apps/fs`
//! * [`exec`]    — `apps/exec`
//! * [`pkg`]     — `apps/pkg`
//! * [`notify`]  — `apps/notify`
//! * [`net`]     — `apps/net`
//!
//! For the small set of verbs we currently use from the desktop, the
//! module exposes a typed function (`fs::read`, `fs::ls`, …) that
//! returns deserialised structs. Calls that aren't yet wrapped can
//! fall back to [`call`] which returns raw JSON.
//!
//! ## Resolving the `cos` binary
//!
//! By default we spawn the binary named `cos` from `$PATH`. Set
//! `CLAW_COS_BIN` to override (used by tests + dev setups).
//!
//! ## Performance
//!
//! The first cut is subprocess-per-call. A file manager listing a
//! 5 000-entry directory will pay 5 000 × (~50 ms python boot) which
//! is not acceptable in the long run. A follow-up commit will add a
//! warm daemon and an in-process Rust fast path for the read-only
//! verbs (`fs.ls`, `fs.stat`, `fs.read`). The contract surfaced here
//! is stable across that migration.

use std::ffi::OsStr;
use std::io::Write;
use std::process::{Command, Stdio};

use serde::de::DeserializeOwned;

pub mod exec;
pub mod fs;
pub mod net;
pub mod notify;
pub mod pkg;

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
                message: format!("not JSON ({e}): {stdout}"),
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

        return Ok(parsed);
    }

    // Empty stdout — the wrapper itself failed (e.g. `cos` binary
    // crashed). Use the exit code + stderr.
    Err(BridgeError::NonZeroExit {
        app: app.to_string(),
        verb: verb.to_string(),
        status: out.status.code().unwrap_or(-1),
        stderr,
    })
}

/// Typed variant of [`call`] — deserialises stdout into the caller's
/// chosen struct. Use this in the typed app wrappers (`fs::read`,
/// `pkg::has`, …).
pub(crate) fn call_typed<A, R>(
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
        message: format!("type mismatch ({e}): {v}"),
    })
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

    #[test]
    #[cfg(unix)]
    fn fs_read_bytes_decodes_base64() {
        // The Python side returns {"base64": "..."} for read_bytes;
        // the bridge wrapper has to base64-decode that on its way
        // back to the Rust caller.
        let dir = tempfile::tempdir().unwrap();
        // "hello world" base64-encoded is "aGVsbG8gd29ybGQ=".
        let bin = write_fake_cos(
            dir.path(),
            r#"{"path":"/x","base64":"aGVsbG8gd29ybGQ=","bytes_returned":11,"total_size":11}"#,
        );
        std::env::set_var("CLAW_COS_BIN", &bin);
        let v = fs::read_bytes("/x").unwrap();
        assert_eq!(v, b"hello world");
        std::env::remove_var("CLAW_COS_BIN");
    }
}
