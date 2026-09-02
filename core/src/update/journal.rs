//! Auditable record of every floor decision.
//!
//! A refusal that nobody can reconstruct is not much of a control, so
//! each decision — allowed or refused, at install time or at daemon
//! start — is appended to a root-owned JSONL log and, when the system
//! journal is reachable, mirrored into it. The record names the
//! package, version, epoch, digest, decision class and whether the
//! release manifest signature was actually verified.
//!
//! Failing to write the record never turns a refusal into an
//! acceptance: journaling is best-effort *after* the decision, and the
//! decision itself is returned to the caller regardless.

use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{SecondsFormat, Utc};
use serde_json::json;

use super::canonical;
use super::decide::Decision;

/// Root-owned decision log.
pub const LOG_PATH: &str = "/var/log/cos/security-floor.jsonl";

/// Where the log lives under an alternate root.
pub fn log_path(root: &Path) -> PathBuf {
    super::signature::joined(root, LOG_PATH)
}

/// Record one decision.
pub fn record(root: &Path, stage: &str, decision: &Decision, package: &str, version: &str) {
    let entry = json!({
        "allowed": decision.allowed,
        "class": decision.class,
        "message": decision.message,
        "package": package,
        "recorded_at": Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        "signature_verified": decision.signature_verified,
        "stage": stage,
        "version": version,
    });
    let Ok(line) = canonical::to_bytes(&entry) else {
        return;
    };
    append(&log_path(root), &line);
    forward_to_system_journal(stage, decision, package, version);
}

#[cfg(unix)]
fn append(path: &Path, line: &[u8]) {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
        let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
    }
    let Ok(mut file) = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
    else {
        return;
    };
    let _ = file.write_all(line);
    let _ = file.sync_data();
}

#[cfg(not(unix))]
fn append(_path: &Path, _line: &[u8]) {}

/// Mirror into journald when it is reachable, so a refusal shows up in
/// `journalctl` beside the APT run that caused it.
fn forward_to_system_journal(stage: &str, decision: &Decision, package: &str, version: &str) {
    if !Path::new("/run/systemd/journal/socket").exists() {
        return;
    }
    let Some(logger) = ["/usr/bin/logger", "/bin/logger"]
        .into_iter()
        .map(Path::new)
        .find(|candidate| candidate.is_file())
    else {
        return;
    };
    let priority = if decision.allowed { "notice" } else { "err" };
    let _ = std::process::Command::new(logger)
        .arg("--tag")
        .arg("claw-security-floor")
        .arg("--priority")
        .arg(format!("auth.{priority}"))
        .arg("--")
        .arg(format!(
            "{stage}: {} {package} {version}: {}",
            decision.class, decision.message
        ))
        .status();
}
