use super::{refuse_active_tasks, ClientIdentity, FsIdentityGuard, ROOT_OWNER_REFUSAL};
use crate::agent::memory::inspection::{
    self as service, ForgetRequest, InspectRequest, ReadRequest, UpdateRequest,
};
use serde_json::Value;

async fn owner(client: &ClientIdentity) -> Result<u32, String> {
    crate::clawd::app_sessions::require_non_app_caller(client).await?;
    if client.execution_uid.is_some() || client.extension_host.is_some() {
        return Err(
            "memory management is an owner UI surface; Agent workers must use guarded memory tools"
                .into(),
        );
    }
    let uid = client.require_uid()?;
    if uid == 0 {
        return Err(ROOT_OWNER_REFUSAL.into());
    }
    crate::clawd::system_caps::verified_owner_home(uid)?;
    Ok(uid)
}

async fn run<F>(uid: u32, operation: F) -> Result<Value, String>
where
    F: FnOnce(&std::path::Path) -> Result<Value, String> + Send + 'static,
{
    let path = crate::paths::clawd_user_agent_state_dir(uid);
    tokio::task::spawn_blocking(move || {
        let _identity = FsIdentityGuard::enter(uid)?;
        operation(&path)
    })
    .await
    .map_err(|error| format!("owner memory operation failed: {error}"))?
}

pub async fn inspect(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    let request: InspectRequest =
        serde_json::from_value(params).map_err(|error| error.to_string())?;
    run(owner(client).await?, move |path| {
        service::inspect_at(path, request)
    })
    .await
}
pub async fn read(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    let request: ReadRequest = serde_json::from_value(params).map_err(|error| error.to_string())?;
    run(owner(client).await?, move |path| {
        serde_json::to_value(service::read_at(path, request.reference)?)
            .map_err(|error| error.to_string())
    })
    .await
}
pub async fn update(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    let request: UpdateRequest =
        serde_json::from_value(params).map_err(|error| error.to_string())?;
    let uid = owner(client).await?;
    refuse_active_tasks(uid)?;
    run(uid, move |path| {
        serde_json::to_value(service::update_at(path, request)?).map_err(|error| error.to_string())
    })
    .await
}
pub async fn forget(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    let request: ForgetRequest =
        serde_json::from_value(params).map_err(|error| error.to_string())?;
    let uid = owner(client).await?;
    refuse_active_tasks(uid)?;
    run(uid, move |path| service::forget_at(path, request)).await
}
