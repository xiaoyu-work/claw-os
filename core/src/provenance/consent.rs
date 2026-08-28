//! Explicit human consent for running unsigned code.
//!
//! Trusting an unsigned package is the one provenance decision that
//! cannot be delegated to a flag. `--yes`, an environment variable or a
//! model-issued tool call would all make "unsigned code runs" reachable
//! from something other than a person at a keyboard, which is precisely
//! what the rest of this module exists to prevent.
//!
//! [`require_developer_consent`] therefore demands:
//!
//! 1. a real controlling terminal on stdin **and** stderr — no pipes,
//!    no `nohup`, no CI runner, no agent subprocess;
//! 2. no Agent, App or MCP session active for this owner, so a running
//!    model cannot drive the prompt through a hijacked terminal;
//! 3. the operator typing an exact phrase that names the package — not
//!    `y`, not `yes`, so it cannot be produced by a stray keystroke or
//!    a `yes |` pipeline.
//!
//! Automation that genuinely needs unsigned content uses an **offline
//! signed developer grant**: a `claw.trust-dev/v1` file produced on a
//! workstation and copied into the developer root. That is a
//! deliberate, auditable artifact rather than a runtime flag.
//!
//! ## What this does not defend against
//!
//! Malware already running as the same user can write the developer
//! root directly. Consent is not a sandbox against yourself; it exists
//! so that **the model, an App, an MCP server or a script** cannot
//! escalate unsigned code into a trusted one, and so the human decision
//! is recorded and visible afterwards.

use std::io::{BufRead, Write};

use super::envelope::PackageKind;

#[derive(Debug, thiserror::Error)]
pub enum ConsentError {
    #[error(
        "trusting unsigned code requires an interactive terminal; \
         this process has none. Create an offline signed developer grant \
         and copy it into {root} instead of automating this prompt"
    )]
    NotInteractive { root: String },
    #[error(
        "refusing to record developer trust while {what} is active: \
         stop it first so no running agent or package can drive this decision"
    )]
    SessionActive { what: String },
    #[error("developer trust was not confirmed")]
    Declined,
    #[error("{0}")]
    Io(String),
}

/// The exact phrase the operator must type.
pub fn confirmation_phrase(kind: PackageKind, id: &str) -> String {
    format!("trust unsigned {} {}", kind.as_str(), id)
}

#[cfg(unix)]
fn is_tty(fd: i32) -> bool {
    unsafe { libc::isatty(fd) == 1 }
}

#[cfg(not(unix))]
fn is_tty(_fd: i32) -> bool {
    false
}

/// Names an active session that must be stopped first, if any.
///
/// A live Agent turn, App session or MCP server means model-influenced
/// code is running right now. Recording new trust in that window is
/// exactly the escalation this guard exists to stop, so it is refused
/// even though the human is present.
fn active_session() -> Option<String> {
    if std::env::var_os("COS_SESSION").is_some() {
        return Some("a Claw session (COS_SESSION is set)".to_string());
    }
    let running = super::runtime::pending_or_running(super::runtime::current_owner());
    if running > 0 {
        return Some(format!("{running} package session(s)"));
    }
    None
}

/// Demand an explicit, interactive, phrase-matched decision.
///
/// `auto_yes` is accepted only so callers can report that it was
/// ignored: a `--yes` on the command line must never satisfy this.
pub fn require_developer_consent(
    kind: PackageKind,
    id: &str,
    path: &std::path::Path,
    digest: &str,
    developer_root: &std::path::Path,
    auto_yes: bool,
) -> Result<(), ConsentError> {
    if auto_yes {
        tracing::warn!(
            target: "provenance",
            "--yes does not satisfy developer trust; an interactive confirmation is still required"
        );
    }
    if !is_tty(libc::STDIN_FILENO) || !is_tty(libc::STDERR_FILENO) {
        return Err(ConsentError::NotInteractive {
            root: developer_root.display().to_string(),
        });
    }
    if let Some(what) = active_session() {
        return Err(ConsentError::SessionActive { what });
    }

    let phrase = confirmation_phrase(kind, id);
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(
        stderr,
        "\n\
         ── Developer trust ────────────────────────────────────────────\n\
         You are about to run UNSIGNED code with Claw OS authority.\n\
         \n\
           kind    {kind}\n\
           id      {id}\n\
           path    {path}\n\
           digest  {digest}\n\
         \n\
         Nobody has signed this package. The grant is bound to the digest\n\
         above and is withdrawn the moment the tree changes. The package\n\
         will run with a restricted capability ceiling: no system,\n\
         secret, network, process, device or cross-App access, and no\n\
         privileged broker routes.\n\
         \n\
         Type exactly:  {phrase}\n\
         ───────────────────────────────────────────────────────────────\n\
         > ",
        kind = kind.as_str(),
        id = id,
        path = path.display(),
        digest = digest,
        phrase = phrase,
    );
    let _ = stderr.flush();

    let mut answer = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut answer)
        .map_err(|e| ConsentError::Io(e.to_string()))?;
    if answer.trim() != phrase {
        return Err(ConsentError::Declined);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/provenance/consent.rs"
    ));
}
