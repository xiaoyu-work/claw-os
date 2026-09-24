//! @agent-file
//! Responsibility: expose fixed owner-scoped Agent account status and Copilot logout.
//! Key dependencies: encrypted credential storage, owner filesystem identity, and Job state.
//! Constraints: no caller-selected credential identity and no logout while owner tasks are active.

use serde_json::{json, Value};

use crate::agent::service::{JobStatus, Store};
use crate::agentd::spawn::ROOT_OWNER_REFUSAL;

use super::client_identity::{ClientIdentity, FsIdentityGuard};

const PROVIDER: &str = "copilot";
const NAMESPACE: &str = "agent";

pub async fn status(_params: Value, client: &ClientIdentity) -> Result<Value, String> {
    let credential_present = with_owner_credentials(client, || {
        crate::credential::is_configured(credential_name(), NAMESPACE)
    })
    .await?;
    Ok(json!({
        "provider": PROVIDER,
        "credential_present": credential_present,
    }))
}

pub async fn logout(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    let request: super::wire::requests::AgentAccountLogout = serde_json::from_value(params)
        .map_err(|error| format!("invalid Agent account logout: {error}"))?;
    if !request.confirm {
        return Err("Agent account logout requires confirm=true".to_string());
    }
    let owner_uid = owner_uid(client)?;
    refuse_active_tasks(owner_uid)?;
    let was_present = with_owner_credentials(client, || {
        crate::credential::revoke_for_broker(credential_name(), NAMESPACE)
    })
    .await?;
    crate::agent::llm::providers::copilot_auth::try_forget_all_cached().map_err(|error| {
        format!("Copilot credential was revoked but cache cleanup failed: {error}")
    })?;
    Ok(json!({
        "provider": PROVIDER,
        "credential_present": false,
        "was_present": was_present,
    }))
}

fn credential_name() -> &'static str {
    crate::agent::llm::providers::copilot_auth::COPILOT_GITHUB_TOKEN_CREDENTIAL
}

fn owner_uid(client: &ClientIdentity) -> Result<u32, String> {
    let owner_uid = client.require_uid()?;
    if owner_uid == 0 {
        return Err(ROOT_OWNER_REFUSAL.to_string());
    }
    Ok(owner_uid)
}

async fn with_owner_credentials<T>(
    client: &ClientIdentity,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let owner_uid = owner_uid(client)?;
    let owner_home = client.require_home_dir()?;
    crate::paths::with_user_override(owner_uid, owner_home, async move {
        let _identity = FsIdentityGuard::enter(owner_uid)?;
        operation()
    })
    .await
}

fn refuse_active_tasks(owner_uid: u32) -> Result<(), String> {
    let store = Store::open_default().map_err(|error| error.to_string())?;
    for status in [
        JobStatus::Pending,
        JobStatus::Running,
        JobStatus::WaitingApproval,
    ] {
        if !store
            .list_bucket_for_owner(status, Some(1), Some(owner_uid))
            .map_err(|error| error.to_string())?
            .is_empty()
        {
            return Err(
                "Agent account cannot log out while this owner has an active Agent task"
                    .to_string(),
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/agent_account.rs"
    ));
}
