use std::env;

use crate::caps::{CapSet, Role};
use crate::proc::SessionInfo;
use crate::session::{self, SessionId, Status as SessionStatus};

pub fn trusted_session_info(
    session_id: &SessionId,
    runtime: &str,
) -> Result<SessionInfo, String> {
    let session_id_string = session_id.as_str().to_string();
    let caps = session_caps(session_id)?;
    let meta = session::get_meta(session_id).map_err(|err| err.to_string())?;
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
    })
}

fn session_caps(session_id: &SessionId) -> Result<CapSet, String> {
    let caps = session::get_caps(session_id).map_err(|err| err.to_string())?;
    if caps.is_empty() {
        return Err(format!(
            "session {} has no capabilities; refusing to enter clawd scope",
            session_id.as_str()
        ));
    }
    Ok(caps)
}
