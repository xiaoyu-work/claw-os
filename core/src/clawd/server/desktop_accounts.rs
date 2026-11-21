//! Additional desktop-only access to the primary socket, not an admin grant.

use super::{ClientIdentity, Command, resolve_group_gid};
use serde_json::Value;

pub(super) const GROUP: &str = "claw-desktop-users";

pub(super) fn enabled() -> bool {
    std::env::var("CLAWD_DESKTOP_ACCOUNTS").as_deref() == Ok("1")
}

pub(in crate::clawd) fn restricted(uid: u32) -> bool {
    if uid == 0 || !enabled() {
        return false;
    }
    let admin = super::super::config::socket_group().unwrap_or_else(|| "sudo".into());
    resolve_group_gid(&admin).ok().is_none_or(|gid| {
        super::super::client_identity::owner_groups(uid)
            .map_or(true, |(_, groups)| !groups.contains(&gid))
    })
}

pub(super) fn permits(command: Command, params: &Value, client: &ClientIdentity) -> bool {
    if !client.uid.is_some_and(restricted) {
        return true;
    }

    use Command::*;
    match command {
        AppSessionRegister => params["kind"].as_str() == Some("gui"),
        SystemUsersControl => matches!(params["action"].as_str(), Some("status" | "show-user")),
        SystemRegionalSettingsControl => matches!(
            params["action"].as_str(),
            Some("status" | "time_status" | "owner_language")
        ),
        DaemonHealth
        | AppGuiLaunch
        | AppGuiWait
        | AppGuiStop
        | AppSessionCheckLive
        | AppSessionDeregister
        | SystemReviewPending
        | SystemReviewShow
        | SystemReviewCancel
        | PermissionPending
        | PermissionStatus
        | SystemDesktopControl
        | SystemDesktopPreferencesControl
        | SystemFilesystemRead
        | SystemFilesystemWrite
        | SystemFileReplace
        | SystemAudioControl
        | SystemCalendarDay
        | SystemMediaPlayerControl
        | SystemNotificationControl
        | SystemAppPermissions => true,
        _ => false,
    }
}

pub(super) fn add_socket_group(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let gid = resolve_group_gid(GROUP).map_err(std::io::Error::other)?;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    // Keep root:sudo rw and add an rw named group; no access for others.
    let mut acl = 2u32.to_le_bytes().to_vec();
    for (tag, mode, id) in [
        (1u16, 6u16, u32::MAX),
        (4, 6, u32::MAX),
        (8, 6, gid),
        (16, 6, u32::MAX),
        (32, 0, u32::MAX),
    ] {
        acl.extend_from_slice(&tag.to_le_bytes());
        acl.extend_from_slice(&mode.to_le_bytes());
        acl.extend_from_slice(&id.to_le_bytes());
    }
    if unsafe {
        libc::lsetxattr(
            path.as_ptr(),
            c"system.posix_acl_access".as_ptr(),
            acl.as_ptr().cast(),
            acl.len(),
            0,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}
