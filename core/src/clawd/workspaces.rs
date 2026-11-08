//! Owner Workspace management. Model workers use the separate scoped read path.

use std::os::unix::fs::MetadataExt;

use serde_json::{json, Value};

use super::{
    client_identity::{ClientIdentity, FsIdentityGuard},
    protocol::BrokerError,
};
use crate::activities::{ActivityService, ActivityWorkChange};
use crate::agent::service::{JobStatus, Store};
use crate::workspaces::{self, WorkspaceAction, WorkspaceService, WorkspaceState, WorkspaceView};

async fn owner(client: &ClientIdentity) -> Result<u32, BrokerError> {
    super::app_sessions::require_non_app_caller(client)
        .await
        .map_err(BrokerError::authorization)?;
    if client.execution_uid.is_some() || client.extension_host.is_some() {
        return Err(BrokerError::authorization(
            "Workspace management is an owner surface; Agent workers cannot change sharing",
        ));
    }
    let owner = client.require_uid().map_err(BrokerError::authorization)?;
    if owner == 0 {
        return Err(BrokerError::authorization(
            "Workspaces require a non-root owner",
        ));
    }
    super::system_caps::verified_owner_home(owner).map_err(BrokerError::authorization)?;
    Ok(owner)
}

fn error(error: crate::activities::ActivityError) -> BrokerError {
    let message = error.to_string();
    use crate::activities::ActivityError;
    match error {
        ActivityError::Invalid(_) => {
            BrokerError::execution(message).classified("workspace_invalid")
        }
        ActivityError::NotFound => BrokerError::execution("Workspace or member not found")
            .classified("workspace_not_found"),
        ActivityError::Conflict(_) => {
            BrokerError::execution(message).classified("workspace_conflict")
        }
        ActivityError::LimitReached => {
            BrokerError::execution(message).classified("workspace_limit")
        }
        _ => BrokerError::unavailable(message).classified("workspace_unavailable"),
    }
}

pub async fn list(_params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    let owner = owner(client).await?;
    serde_json::to_value(
        workspaces::open_default()
            .map_err(error)?
            .list_workspaces(owner)
            .map_err(error)?,
    )
    .map_err(|e| BrokerError::unavailable(e.to_string()))
}

pub async fn get(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    let request: workspaces::GetRequest =
        serde_json::from_value(params).map_err(|e| BrokerError::execution(e.to_string()))?;
    workspaces::validate_id(&request.id).map_err(BrokerError::execution)?;
    let owner = owner(client).await?;
    let service = workspaces::open_default().map_err(error)?;
    let view = service.workspace(owner, &request.id).map_err(error)?;
    serde_json::to_value(view).map_err(|e| BrokerError::unavailable(e.to_string()))
}

pub async fn control(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    let mut request: workspaces::ControlRequest =
        serde_json::from_value(params).map_err(|e| BrokerError::execution(e.to_string()))?;
    let owner = owner(client).await?;
    if let WorkspaceAction::Tag { path, kind, .. } = &mut request.request {
        let home =
            super::system_caps::verified_owner_home(owner).map_err(BrokerError::authorization)?;
        let canonical = {
            let _identity = FsIdentityGuard::enter(owner).map_err(BrokerError::authorization)?;
            std::fs::canonicalize(&*path)
                .map_err(|e| BrokerError::execution(format!("resolve shared resource: {e}")))?
        };
        crate::worker::derive::reject_forbidden_for_owner(&canonical, &home)
            .map_err(BrokerError::authorization)?;
        let verified = {
            let _identity = FsIdentityGuard::enter(owner).map_err(BrokerError::authorization)?;
            let file = super::owner_files::source(&canonical).map_err(BrokerError::execution)?;
            let meta = file
                .metadata()
                .map_err(|e| BrokerError::execution(e.to_string()))?;
            if meta.is_dir() {
                workspaces::ResourceKind::Directory
            } else if meta.is_file() && meta.nlink() == 1 {
                workspaces::ResourceKind::File
            } else {
                return Err(BrokerError::execution(
                    "share only regular single-link files or directories",
                ));
            }
        };
        if kind.is_some_and(|kind| kind != verified) {
            return Err(BrokerError::execution(
                "shared resource kind does not match the current file",
            ));
        }
        *path = canonical
            .to_str()
            .ok_or_else(|| BrokerError::execution("shared path must be UTF-8"))?
            .into();
        *kind = Some(verified);
    }
    if let WorkspaceAction::BindActivity { activity_id, .. }
    | WorkspaceAction::UnbindActivity { activity_id, .. } = &request.request
    {
        let jobs = Store::open_default()
            .and_then(|store| store.list_for_activity(owner, activity_id, usize::MAX))
            .map_err(|e| BrokerError::unavailable(e.to_string()))?;
        if jobs.iter().any(|job| {
            matches!(
                job.status,
                JobStatus::Pending | JobStatus::Running | JobStatus::WaitingApproval
            )
        }) {
            return Err(BrokerError::execution(
                "stop the Activity's tasks before changing its Workspace",
            )
            .classified("workspace_busy"));
        }
    }
    let service = workspaces::open_default().map_err(error)?;
    let mut view = service
        .change_workspace(owner, request.request)
        .map_err(error)?;
    if view.workspace.state == WorkspaceState::Deleted {
        view = cleanup(owner, view, client)?;
    }
    serde_json::to_value(view).map_err(|e| BrokerError::unavailable(e.to_string()))
}

fn cleanup(
    owner: u32,
    view: WorkspaceView,
    client: &ClientIdentity,
) -> Result<WorkspaceView, BrokerError> {
    let service = workspaces::open_default().map_err(error)?;
    let store = Store::open_default().map_err(|e| BrokerError::unavailable(e.to_string()))?;
    let mut pending = false;
    for id in &view.activity_ids {
        if let Some(work) = service.work(owner, id).map_err(error)? {
            if work.enabled {
                service
                    .change_work(owner, id, Some(work.revision), ActivityWorkChange::Disable)
                    .map_err(error)?;
            }
        }
        for job in store
            .list_for_activity(owner, id, usize::MAX)
            .map_err(|e| BrokerError::unavailable(e.to_string()))?
        {
            if matches!(
                job.status,
                JobStatus::Pending | JobStatus::Running | JobStatus::WaitingApproval
            ) {
                let result = super::tasks::cancel(json!({"id":job.id}), client).map_err(|e| {
                    BrokerError::unavailable(format!(
                        "Workspace denied; task cancellation failed: {e}"
                    ))
                })?;
                pending |= result.get("cancel_requested") == Some(&json!(true));
            }
            pending |= job.worker_pid.is_some();
        }
    }
    if pending {
        return Ok(view);
    }
    service
        .finish_workspace_cleanup(owner, &view.workspace.id)
        .map_err(error)
}
