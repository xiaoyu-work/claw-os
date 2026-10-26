//! Shared nearby transfer service; no model or graphical session is required.

use super::{client_identity::ClientIdentity, protocol::BrokerError};
use clawd_client::nearby::*;
use serde_json::Value;

#[cfg(target_os = "linux")]
mod bluetooth;
#[cfg(target_os = "linux")]
mod files;
#[cfg(target_os = "linux")]
mod process;
#[cfg(target_os = "linux")]
mod radio;
#[cfg(target_os = "linux")]
mod runtime;

pub(super) fn clip(value: &str, limit: usize) -> String {
    let output: String = value
        .chars()
        .filter(|c| !c.is_control() && !matches!(c,'\u{202a}'..='\u{202e}'|'\u{2066}'..='\u{2069}'))
        .take(limit / 4)
        .collect();
    if output.trim().is_empty() {
        "No further detail available.".into()
    } else {
        output
    }
}

fn human_owner(client: &ClientIdentity) -> Result<u32, BrokerError> {
    let owner = client.require_uid()?;
    if owner == 0 || client.extension_host.is_some() {
        return Err(BrokerError::authorization(
            "Nearby sharing requires a non-root human owner",
        ));
    }
    Ok(owner)
}

pub async fn status(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    let _: super::wire::requests::NoBody =
        serde_json::from_value(params).map_err(|e| BrokerError::execution(e.to_string()))?;
    let owner = human_owner(client)?;
    #[cfg(target_os = "linux")]
    return serde_json::to_value(
        runtime::status(owner)
            .await
            .map_err(BrokerError::unavailable)?,
    )
    .map_err(|e| BrokerError::unavailable(e.to_string()));
    #[cfg(not(target_os = "linux"))]
    {
        let _ = owner;
        Err(BrokerError::unavailable(
            "Nearby sharing requires Linux and a compatible physical radio",
        ))
    }
}

pub async fn control(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    let mut body: super::wire::requests::NearbyControl =
        serde_json::from_value(params).map_err(|e| BrokerError::execution(e.to_string()))?;
    body.request.validate().map_err(BrokerError::execution)?;
    let owner = human_owner(client)?;
    #[cfg(target_os = "linux")]
    {
        use crate::caps::{Cap, Scope, Verb};
        let mut required = Vec::new();
        let store = files::Store::open(owner).map_err(BrokerError::unavailable)?;
        match &mut body.request {
            NearbyAction::Open {
                receive,
                expected_mode,
                allow_disruption,
                ..
            } => {
                runtime::preflight_open(*expected_mode, *allow_disruption)
                    .await
                    .map_err(BrokerError::execution)?;
                required.push(Cap::new(Verb::NET_MANAGE, Scope::name("nearby-sharing")));
                required.push(Cap::new(
                    Verb::DEVICE_BLUETOOTH,
                    Scope::name("nearby-sharing"),
                ));
                if *receive {
                    required.push(Cap::new(
                        Verb::FS_WRITE,
                        Scope::path(store.inbox().to_string_lossy()),
                    ));
                }
            }
            NearbyAction::Send {
                paths,
                session_id,
                peer_id,
                ..
            } => {
                let target = runtime::send_address(owner, session_id, peer_id)
                    .await
                    .map_err(BrokerError::execution)?;
                required.push(Cap::new(Verb::NET_DIAL, Scope::host(target)));
                let mut names = std::collections::HashSet::new();
                for raw in paths {
                    let path = store.source(raw).map_err(BrokerError::execution)?;
                    if !path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| file_name(name) && names.insert(name.to_string()))
                    {
                        return Err(BrokerError::execution("Select safe distinct file names before requesting sharing permissions."));
                    }
                    required.push(Cap::new(Verb::FS_READ, Scope::path(path.to_string_lossy())));
                    *raw = path.to_string_lossy().into_owned();
                }
            }
            _ => {}
        }
        body.request.validate().map_err(BrokerError::execution)?;
        if let NearbyAction::Send { uploads, .. } = &body.request {
            runtime::validate_upload_selection(&store, uploads)
                .await
                .map_err(BrokerError::execution)?;
        }
        if !required.is_empty() {
            let serialized = serde_json::to_vec(&body.request)
                .map_err(|e| BrokerError::execution(e.to_string()))?;
            let operation = crate::crypto::sha256_hex(&serialized);
            super::scheduler::authorize_nearby_effect(
                client,
                &required,
                &operation,
                &format!("Nearby sharing: {}", body.request.name()),
            )
            .await?;
        }
        let response = runtime::control(store, body.request.clone()).await?;
        response
            .validate(owner, &body.request)
            .map_err(BrokerError::indeterminate)?;
        super::audit::record_nearby_control(&body.request,&response.view).map_err(|error|
            BrokerError::indeterminate(format!("Sharing action was admitted but its audit record failed; refresh before retrying: {error}")))?;
        serde_json::to_value(response).map_err(|e| BrokerError::unavailable(e.to_string()))
    }
    #[cfg(not(target_os = "linux"))]
    Err(BrokerError::unavailable("Nearby sharing requires Linux"))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/nearby/mod.rs"
    ));
}
