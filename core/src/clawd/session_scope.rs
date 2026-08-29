use std::env;
use std::path::Path;

use crate::caps::{CapSet, ConsentContext, Role};
use crate::proc::SessionInfo;
use crate::session::{self, SessionId, SessionMeta, SessionOrigin, Status as SessionStatus};

/// Build the trusted session the daemon installs around one piece of
/// work it runs on a user's behalf.
///
/// The capabilities it carries are re-derived here rather than trusted.
/// Whatever the session directory records is clamped by the policy that
/// matches the session's *provenance*:
///
/// * an ambient system-Agent task gets the minimal baseline
///   ([`super::system_caps::system_agent_caps`]) and nothing else;
/// * a `clawd`-issued scheduler snapshot additionally keeps the exact
///   executor verb and named credentials the owner proved or had
///   approved when the job was created, so unattended work still runs.
///
/// The owner comes from the session's own metadata and that uid's
/// canonical passwd home, both daemon-derived, and the provenance
/// marker is believed only when the session record is root-owned. An
/// override can therefore only ever be narrower than daemon policy —
/// never additive, and never a place a one-shot approval turns into
/// standing authority.
pub fn trusted_session_info(session_id: &SessionId, runtime: &str) -> Result<SessionInfo, String> {
    let session_id_string = session_id.as_str().to_string();
    let meta = session::get_meta(session_id).map_err(|err| err.to_string())?;
    let root_owned = session::record_is_root_owned(session_id);
    let caps = session_caps(session_id, &meta)?;
    let role = meta.role;
    let credential_tier = meta
        .credential_tier
        .or_else(|| role.map(Role::credential_tier));

    session::update_meta(session_id, |meta| {
        if meta.status.is_active() {
            meta.status = SessionStatus::Running;
        }
        if meta.creator_runtime.is_none() {
            meta.creator_runtime = Some(runtime.to_string());
        }
    })
    .map_err(|err| err.to_string())?;

    Ok(SessionInfo {
        session_id: session_id_string,
        pid: std::process::id(),
        command: vec![runtime.to_string()],
        started_at: chrono::Utc::now().to_rfc3339(),
        stdout_path: String::new(),
        stderr_path: String::new(),
        group: Some("clawd".to_string()),
        parent: None,
        workdir: env::current_dir()
            .ok()
            .map(|path| path.to_string_lossy().to_string()),
        exit_code: None,
        ended_at: None,
        tier: credential_tier,
        scope: Some("clawd-task".to_string()),
        priority: None,
        caps: Some(caps),
        transient_caps: None,
        role: role.map(|value| value.name().to_string()),
        app_id: None,
        pending_bind: false,
        start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
        client: if root_owned {
            meta.client
        } else {
            crate::session::SessionClient::default()
        },
    })
}

fn session_caps(session_id: &SessionId, meta: &SessionMeta) -> Result<CapSet, String> {
    let stored = session::get_caps(session_id).map_err(|err| err.to_string())?;
    let owner_uid = meta.owner_uid.ok_or_else(|| {
        format!(
            "session {} records no owner; refusing to enter clawd scope",
            session_id.as_str()
        )
    })?;
    let owner_home = super::system_caps::verified_owner_home(owner_uid)?;
    let root_owned = session::record_is_root_owned(session_id);
    let caps = scoped_caps(&stored, meta, root_owned, owner_uid, &owner_home);
    if caps.is_empty() {
        return Err(format!(
            "session {} has no capabilities within owner policy; refusing to enter clawd scope",
            session_id.as_str()
        ));
    }
    Ok(caps)
}

/// Pure policy half of [`session_caps`], kept separate so the decision
/// is testable without a passwd entry or a real session directory.
fn scoped_caps(
    stored: &CapSet,
    meta: &SessionMeta,
    root_owned: bool,
    owner_uid: u32,
    owner_home: &Path,
) -> CapSet {
    let origin = trusted_origin(meta, root_owned);
    super::system_caps::clamp_for_origin(stored, origin, owner_uid, owner_home)
}

/// Provenance the daemon is willing to act on.
///
/// A delegation marker is authority — it decides whether an unattended
/// snapshot keeps its executor verb — so it counts only when the record
/// carrying it is root-owned, which on a `0700` session directory means
/// only `clawd` could have written it. A marker on a record the
/// delegated account could author is ignored, and so is a missing one,
/// leaving the ambient baseline.
fn trusted_origin(meta: &SessionMeta, root_owned: bool) -> SessionOrigin {
    match meta.origin {
        Some(origin @ (SessionOrigin::CronDelegation | SessionOrigin::TriggerDelegation))
            if root_owned =>
        {
            origin
        }
        _ => SessionOrigin::SystemAgentTask,
    }
}

/// Whether a capability denial in this durable session may open an
/// interactive consent request.
///
/// Only daemon-authored scheduler provenance selects unattended mode.
/// Missing, legacy, or user-writable provenance is treated as an
/// ordinary attended conversation; this can create a prompt but can
/// never grant authority by itself.
pub fn consent_context(session_id: &SessionId) -> Result<ConsentContext, String> {
    let meta = session::get_meta(session_id).map_err(|err| err.to_string())?;
    let origin = trusted_origin(&meta, session::record_is_root_owned(session_id));
    Ok(match origin {
        SessionOrigin::SystemAgentTask => ConsentContext::Attended,
        SessionOrigin::CronDelegation | SessionOrigin::TriggerDelegation => {
            ConsentContext::Unattended
        }
    })
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/session_scope.rs"
    ));
}
