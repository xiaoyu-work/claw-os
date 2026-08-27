use serde_json::{json, Value};

use crate::caps::{Cap, Scope, Verb};

use super::client_identity::ClientIdentity;
use super::protocol::BrokerError;

pub async fn oauth_refresh(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (params, client);
        return Err(BrokerError::unavailable(
            "credential OAuth broker requires Linux",
        ));
    }

    #[cfg(target_os = "linux")]
    {
        if unsafe { libc::geteuid() } != 0 {
            return Err(BrokerError::unavailable(
                "credential OAuth broker requires root clawd",
            ));
        }
        let uid = client.require_uid()?;
        let gid = client
            .gid
            .ok_or_else(|| "clawd peer gid is unavailable".to_string())?;
        let home = client.require_home_dir()?;
        let peer_pid = client
            .pid
            .ok_or_else(|| "clawd peer pid is unavailable".to_string())?;
        let session_id = required_string(&params, "session")?;
        let namespace = required_string(&params, "namespace")?;
        let credential = required_string(&params, "credential")?;
        crate::paths::with_user_override(uid, home.clone(), async {
            authorize_session(&session_id, peer_pid, &namespace, &credential)
        })
        .await?;
        validate_component("namespace", &namespace).map_err(BrokerError::execution)?;
        if !matches!(
            credential.as_str(),
            "GOOGLE_ACCESS_TOKEN" | "MICROSOFT_ACCESS_TOKEN"
        ) {
            return Err(BrokerError::execution(format!(
                "credential `{credential}` is not eligible for OAuth refresh"
            )));
        }
        crate::paths::with_user_override(uid, home, async move {
            let _identity = FsIdentityGuard::enter(uid, gid).map_err(BrokerError::execution)?;
            let result =
                crate::credential::broker_refresh_access_token(&credential, &namespace)
                    .map_err(BrokerError::execution)?;
            Ok(json!({
                "credential": credential,
                "namespace": namespace,
                "refreshed": true,
                "provider_result": result,
            }))
        })
        .await
    }

}

#[cfg(target_os = "linux")]
struct FsIdentityGuard {
    previous_uid: libc::c_int,
    previous_gid: libc::c_int,
}

#[cfg(target_os = "linux")]
impl FsIdentityGuard {
    fn enter(uid: u32, gid: u32) -> Result<Self, String> {
        let previous_gid = unsafe { libc::setfsgid(gid as libc::gid_t) };
        let previous_uid = unsafe { libc::setfsuid(uid as libc::uid_t) };
        let current_uid = unsafe { libc::setfsuid(!0 as libc::uid_t) };
        let current_gid = unsafe { libc::setfsgid(!0 as libc::gid_t) };
        if current_uid != uid as libc::c_int || current_gid != gid as libc::c_int {
            unsafe {
                libc::setfsuid(previous_uid as libc::uid_t);
                libc::setfsgid(previous_gid as libc::gid_t);
            }
            return Err(format!(
                "failed to enter credential filesystem identity {uid}:{gid}"
            ));
        }
        Ok(Self {
            previous_uid,
            previous_gid,
        })
    }
}

#[cfg(target_os = "linux")]
impl Drop for FsIdentityGuard {
    fn drop(&mut self) {
        unsafe {
            libc::setfsuid(self.previous_uid as libc::uid_t);
            libc::setfsgid(self.previous_gid as libc::gid_t);
        }
    }
}

fn authorize_session(
    session_id: &str,
    peer_pid: u32,
    namespace: &str,
    credential: &str,
) -> Result<(), BrokerError> {
    let session = crate::proc::session_info_by_id(session_id)
        .ok_or_else(|| {
            BrokerError::authorization(format!("credential session not found: {session_id}"))
        })?;
    if session.pending_bind || session.pid == 0 {
        return Err(BrokerError::authorization(
            "credential session is not bound to a process",
        ));
    }
    let expected_start = session
        .start_time_ticks
        .ok_or_else(|| {
            BrokerError::authorization("credential session has no process identity")
        })?;
    if crate::proc::read_start_time_ticks_pub(session.pid) != Some(expected_start) {
        return Err(BrokerError::authorization(
            "credential session process identity is stale",
        ));
    }
    if !crate::proc::process_descends_from(peer_pid, session.pid) {
        return Err(BrokerError::authorization(
            "credential request did not originate from the authorized session",
        ));
    }
    let requested = Cap::new(
        Verb::SECRET_READ,
        Scope::name(format!("{namespace}/{credential}")),
    );
    let caps = session
        .caps
        .as_ref()
        .ok_or_else(|| BrokerError::authorization("credential session has no capabilities"))?;
    if !caps.covers(&requested) {
        return Err(BrokerError::authorization(format!(
            "credential session lacks {}:{}",
            requested.verb.as_str(),
            requested.scope
        )));
    }
    Ok(())
}

fn required_string(params: &Value, key: &str) -> Result<String, String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("missing {key}"))
}

fn validate_component(kind: &str, value: &str) -> Result<(), String> {
    if value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        Ok(())
    } else {
        Err(format!(
            "{kind} must be alphanumeric (hyphens/underscores allowed)"
        ))
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/credentials.rs"
    ));
}
