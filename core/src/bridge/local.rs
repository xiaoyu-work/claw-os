//! In-process Root registration uses the existing owner review and deny gates.

use super::{AppLaunch, CapSet, ClawdCallError, SessionInfo};

pub(super) fn use_clawd_backend() -> Result<bool, String> {
    #[cfg(test)]
    if std::env::var_os("COS_TEST_LOCAL_APP_SESSIONS").is_some() {
        return Ok(false);
    }
    #[cfg(unix)]
    {
        if unsafe { libc::geteuid() } == 0 {
            return Ok(false);
        }
        if crate::paths::current_owner_uid_override().is_some() {
            return Err(
                "owner-overridden App launches require the authenticated App Host; \
                 local registration cannot bypass OS review or owner policy"
                    .to_string(),
            );
        }
        Ok(true)
    }
    #[cfg(not(unix))]
    {
        Err("App registration requires the Unix OS broker".to_string())
    }
}

pub(super) fn authorize(
    launch: &AppLaunch,
    parent: &SessionInfo,
    parent_caps: &CapSet,
    local_caps: impl FnOnce(&CapSet) -> Result<CapSet, String>,
) -> Result<CapSet, String> {
    #[cfg(test)]
    if std::env::var_os("COS_TEST_LOCAL_APP_SESSIONS").is_some() {
        return local_caps(parent_caps);
    }
    #[cfg(unix)]
    {
        if unsafe { libc::geteuid() } != 0 {
            return Err("local App registration requires the Root authority".to_string());
        }
        crate::clawd::app_sessions::require_non_app_session_ancestry(
            &crate::proc::registry_sessions(),
            parent,
        )?;
        let owner = crate::provenance::runtime::current_owner();
        let app = crate::apps::App {
            manifest: launch.manifest().clone(),
            dir: launch.dir().to_path_buf(),
            provenance: Ok(launch.package().clone()),
        };
        let requester = format!(
            "uid:{owner} pid:{} session:{}",
            std::process::id(),
            parent.session_id
        );
        // Never call the daemon's public socket from its own execution scope.
        crate::clawd::system_review::require_app_review(owner, &app, &requester)
            .map_err(ClawdCallError::from)
            .map_err(String::from)?;
        let caps = local_caps(parent_caps)?;
        crate::approvals::app_policy::require(owner, launch.app_id(), &caps)?;
        Ok(caps)
    }
    #[cfg(not(unix))]
    {
        let _ = (launch, parent, parent_caps, local_caps);
        Err("local App registration requires the Unix Root authority".to_string())
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/bridge/local.rs"
    ));
}
