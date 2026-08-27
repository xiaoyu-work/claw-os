use serde_json::{json, Value};

use crate::caps::{Cap, Scope, Verb};

use super::authority::{Authorized, Decision};
use super::client_identity::ClientIdentity;
use super::protocol::BrokerError;

pub async fn oauth_refresh(
    params: Value,
    client: &ClientIdentity,
    authority: &Decision,
) -> Result<Value, BrokerError> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (params, client, authority);
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
        let namespace = required_string(&params, "namespace")?;
        let credential = required_string(&params, "credential")?;
        // The authority decision the broker already took is the whole
        // session check; what remains is this route's own contract on
        // the values it will act with, kept ahead of any backend probe.
        let _authorized = authorize_session(authority, &namespace, &credential)?;
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
            let result = crate::credential::broker_refresh_access_token(&credential, &namespace)
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

/// Final provider check, taken against the decision the broker already
/// made.
///
/// The session, its App identity, the process allowed to act under it,
/// and the capabilities it holds all come from the grant `clawd`
/// issued and the middleware resolved. Nothing here re-reads the
/// process registry or re-derives policy, so the two can no longer
/// disagree; the check still runs, because a privileged mutation
/// should be refused twice.
/// Final provider check, taken against the decision the broker already
/// made.
///
/// The session, the process allowed to act under it, and the
/// capabilities it holds all come from the grant `clawd` issued and the
/// middleware resolved. The route is `PeerSession` with transient
/// capabilities *excluded*, which preserves exactly what this broker
/// checked before the authority existed: `session.caps` and never the
/// set an unrelated MCP tool call was granted for one invocation. A
/// refusal is an authorization failure, so it carries the same stable
/// code every other one does.
fn authorize_session(
    authority: &Decision,
    namespace: &str,
    credential: &str,
) -> Result<Authorized, BrokerError> {
    authority
        .require(Cap::new(
            Verb::SECRET_READ,
            Scope::name(format!("{namespace}/{credential}")),
        ))
        .map_err(BrokerError::authorization)
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
