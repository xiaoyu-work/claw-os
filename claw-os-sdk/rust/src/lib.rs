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
//! | [`ai`]        | `ai`        | stable `cos ai chat`            |
//! | [`mcp`]       | MCP         | App-hosted tools over JSON-RPC  |
//! | [`tools`]     | `tool`      | `cos ai tool <name> --app <id>` |
//! | [`envelope`]  | shared      | the common reply envelope       |
//! | [`generated`] | shared      | typed structs codegen'd from `wire/v1/*.schema.json` |
//!
//! Everything except `generated` is hand-written. `generated.rs` is
//! recomputed by `claw-os-sdk/wire/codegen.py` whenever the schemas
//! change.
//!
//! ## Manifest-bound MCP Apps
//!
//! Native Apps load [`mcp::App`] from the authoritative App manifest, bind one
//! [`mcp::Tool`] implementation for every declared name, and serve MCP over
//! stdio. Tool descriptions and schemas are never authored in Rust. Each
//! handler receives validated arguments and a Gateway-authenticated
//! [`mcp::CallContext`] with immutable caller/lineage data, deadline,
//! cancellation, and optional progress reporting.
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
pub mod gui;
pub mod mcp;
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
        code: String,
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
            BridgeError::AppError { code, .. } if code == "PERMISSION_DENIED"
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
    format!("{}… [{} more bytes elided]", &s[..cut], s.len() - cut)
}

const DIAG_LIMIT: usize = 256;
const WIRE_V1_FLAG: &str = "--wire=1";

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
    cmd.arg(WIRE_V1_FLAG).arg("app").arg(app).arg(verb);
    for a in args {
        cmd.arg(a);
    }
    cmd
}

/// Invoke `cos --wire=1 app <app> <verb> <args...>` and return the
/// request-specific success payload.
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

    let mut child = cmd.spawn().map_err(|e| match e.kind() {
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

    decode_wire_response(app, verb, child.wait_with_output()?)
        .map_err(StructuredCosError::into_bridge)
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

/// Invoke any `cos` sub-command through wire v1 and return its success
/// data. Used by [`ai`] and [`tools`] for `cos ai ...`.
///
/// `family` and `verb` are surfaced in [`BridgeError`] variants when
/// the call fails — pass any human-meaningful strings.
#[doc(hidden)]
pub fn cos_call_json<A>(family: &str, verb: &str, args: A) -> Result<serde_json::Value, BridgeError>
where
    A: IntoIterator,
    A::Item: AsRef<OsStr>,
{
    cos_call_json_structured(family, verb, args).map_err(StructuredCosError::into_bridge)
}

#[derive(Debug)]
pub(crate) struct StructuredAppError {
    pub app: String,
    pub verb: String,
    pub message: String,
    pub code: String,
    pub payload: serde_json::Value,
}

#[derive(Debug)]
pub(crate) enum StructuredCosError {
    App(Box<StructuredAppError>),
    Bridge(BridgeError),
}

impl StructuredCosError {
    fn into_bridge(self) -> BridgeError {
        match self {
            StructuredCosError::App(error) => {
                let error = *error;
                BridgeError::AppError {
                    app: error.app,
                    verb: error.verb,
                    message: error.message,
                    code: error.code,
                }
            }
            StructuredCosError::Bridge(error) => error,
        }
    }
}

fn decode_wire_response(
    family: &str,
    verb: &str,
    out: std::process::Output,
) -> Result<serde_json::Value, StructuredCosError> {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if stdout.trim().is_empty() {
        return Err(BridgeError::NonZeroExit {
            app: family.to_string(),
            verb: verb.to_string(),
            status: out.status.code().unwrap_or(-1),
            stderr: truncate_diag(&stderr, DIAG_LIMIT),
        }
        .into());
    }

    let raw: serde_json::Value =
        serde_json::from_str(stdout.trim()).map_err(|error| BridgeError::Decode {
            app: family.to_string(),
            verb: verb.to_string(),
            message: format!("not JSON ({error}): {}", truncate_diag(&stdout, DIAG_LIMIT)),
        })?;
    let envelope =
        crate::envelope::Envelope::decode(raw.clone()).map_err(|error| BridgeError::Decode {
            app: family.to_string(),
            verb: verb.to_string(),
            message: error,
        })?;

    if envelope.ok {
        if !out.status.success() {
            return Err(BridgeError::Decode {
                app: family.to_string(),
                verb: verb.to_string(),
                message: format!(
                    "wire success envelope accompanied exit status {}",
                    out.status.code().unwrap_or(-1)
                ),
            }
            .into());
        }
        return envelope.data.ok_or_else(|| {
            BridgeError::Decode {
                app: family.to_string(),
                verb: verb.to_string(),
                message: "wire success envelope omitted data".to_string(),
            }
            .into()
        });
    }

    if out.status.success() {
        return Err(BridgeError::Decode {
            app: family.to_string(),
            verb: verb.to_string(),
            message: "wire error envelope accompanied exit status 0".to_string(),
        }
        .into());
    }
    Err(StructuredCosError::App(Box::new(StructuredAppError {
        app: family.to_string(),
        verb: verb.to_string(),
        message: envelope
            .error
            .expect("validated error envelope must include error"),
        code: envelope
            .code
            .expect("validated error envelope must include code"),
        payload: raw,
    })))
}

impl From<BridgeError> for StructuredCosError {
    fn from(error: BridgeError) -> Self {
        StructuredCosError::Bridge(error)
    }
}

impl From<std::io::Error> for StructuredCosError {
    fn from(error: std::io::Error) -> Self {
        StructuredCosError::Bridge(BridgeError::Io(error))
    }
}

pub(crate) fn cos_call_json_structured<A>(
    family: &str,
    verb: &str,
    args: A,
) -> Result<serde_json::Value, StructuredCosError>
where
    A: IntoIterator,
    A::Item: AsRef<OsStr>,
{
    let bin = std::env::var("CLAW_COS_BIN").unwrap_or_else(|_| "cos".into());
    let mut cmd = Command::new(bin);
    cmd.arg(WIRE_V1_FLAG);
    for a in args {
        cmd.arg(a);
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let child = cmd.spawn().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => BridgeError::BinaryNotFound(e),
        _ => BridgeError::Io(e),
    })?;

    decode_wire_response(family, verb, child.wait_with_output()?)
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/lib.rs"));
}

#[cfg(test)]
mod generated_tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/generated.rs"
    ));
}
