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
pub(crate) mod reads;

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
    let store = Store::open_default().map_err(|e| BrokerError::unavailable(e.to_string()))?;
    let _context_lock = store
        .lock_context_admission()
        .map_err(|e| BrokerError::unavailable(e.to_string()))?;
    let previous = request
        .request
        .id()
        .map(|id| {
            workspaces::open_default()
                .and_then(|service| service.workspace(owner, id))
                .map_err(error)
        })
        .transpose()?;
    let changed = request.request.clone();
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
            ) || job.worker_pid.is_some()
        }) {
            return Err(BrokerError::execution(
                "stop the Activity's tasks before changing its Workspace",
            )
            .classified("workspace_busy"));
        }
    }
    if let WorkspaceAction::ReadLink {
        id,
        target,
        sources,
        ..
    } = &request.request
    {
        match target {
            workspaces::ReadTarget::Job { id: job_id } => {
                let job = store
                    .locate_for_owner(job_id, Some(owner))
                    .map_err(|e| BrokerError::unavailable(e.to_string()))?
                    .map(|(_, job)| job)
                    .ok_or_else(|| BrokerError::execution("target Job not found"))?;
                if job.workspace_id.as_ref() != Some(id)
                    || !matches!(
                        job.status,
                        JobStatus::Pending | JobStatus::Running | JobStatus::WaitingApproval
                    )
                {
                    return Err(BrokerError::execution(
                        "temporary read requires an exact live Job in this Workspace",
                    ));
                }
            }
            workspaces::ReadTarget::Conversation { id: session_id } => {
                let meta = super::tasks::task_session_meta(session_id, owner)
                    .map_err(BrokerError::authorization)?;
                if meta.workspace_id.as_ref() != Some(id) {
                    return Err(BrokerError::execution(
                        "target conversation is in another Workspace",
                    ));
                }
            }
        }
        let home =
            super::system_caps::verified_owner_home(owner).map_err(BrokerError::authorization)?;
        for source in sources {
            crate::worker::derive::reject_forbidden_for_owner(
                std::path::Path::new(&source.path),
                &home,
            )
            .map_err(BrokerError::authorization)?;
            let _identity = FsIdentityGuard::enter(owner).map_err(BrokerError::authorization)?;
            let file = super::owner_files::source(std::path::Path::new(&source.path))
                .map_err(BrokerError::execution)?;
            let metadata = file
                .metadata()
                .map_err(|e| BrokerError::execution(e.to_string()))?;
            if !metadata.is_file() || metadata.nlink() != 1 {
                return Err(BrokerError::execution("temporary selections name exact regular files, never recursive directory grants"));
            }
        }
    }
    let service = workspaces::open_default().map_err(error)?;
    let mut view = service
        .change_workspace(owner, request.request)
        .map_err(error)?;
    if let WorkspaceAction::ReadLink { target, .. } = &changed {
        let link = view
            .workspace
            .read_links
            .iter()
            .find(|link| {
                previous
                    .as_ref()
                    .is_none_or(|old| !old.workspace.read_links.iter().any(|old| old.id == link.id))
            })
            .ok_or_else(|| BrokerError::unavailable("temporary read acknowledgement is missing"))?;
        let sid = match target {
            workspaces::ReadTarget::Conversation { id } => id.clone(),
            workspaces::ReadTarget::Job { id } => store
                .locate_for_owner(id, Some(owner))
                .map_err(|e| BrokerError::unavailable(e.to_string()))?
                .and_then(|(_, job)| job.session_id)
                .ok_or_else(|| {
                    BrokerError::unavailable("temporary read target lost its Root conversation")
                })?,
        };
        crate::workspaces::context_access::register(owner, &view.workspace.id, link, &sid)
            .map_err(BrokerError::unavailable)?;
    }
    if view.workspace.state == WorkspaceState::Active
        && previous
            .as_ref()
            .is_some_and(|old| old.workspace.access_revision == view.workspace.access_revision)
    {
        store
            .refresh_workspace_context(owner, &view.workspace.id, view.workspace.revision)
            .map_err(|e| {
                BrokerError::unavailable(format!(
                    "Workspace change persisted; task context refresh failed: {e}"
                ))
            })?;
    }
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
    let mut jobs = store
        .list_for_workspace(owner, &view.workspace.id)
        .map_err(|e| BrokerError::unavailable(e.to_string()))?;
    for id in &view.activity_ids {
        if let Some(work) = service.work(owner, id).map_err(error)? {
            if work.enabled {
                service
                    .change_work(owner, id, Some(work.revision), ActivityWorkChange::Disable)
                    .map_err(error)?;
            }
        }
        jobs.extend(
            store
                .list_for_activity(owner, id, usize::MAX)
                .map_err(|e| BrokerError::unavailable(e.to_string()))?,
        );
    }
    jobs.sort_by(|a, b| a.id.cmp(&b.id));
    jobs.dedup_by(|a, b| a.id == b.id);
    for job in jobs {
        if matches!(
            job.status,
            JobStatus::Pending | JobStatus::Running | JobStatus::WaitingApproval
        ) {
            let result = super::tasks::cancel(json!({"id":job.id}), client).map_err(|e| {
                BrokerError::unavailable(format!("Workspace denied; task cancellation failed: {e}"))
            })?;
            pending |= result.get("cancel_requested") == Some(&json!(true));
        }
        pending |= job.worker_pid.is_some();
    }
    if pending {
        return Ok(view);
    }
    service
        .finish_workspace_cleanup(owner, &view.workspace.id)
        .map_err(error)
}

pub async fn read(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    let request: workspaces::ReadRequest =
        serde_json::from_value(params).map_err(|e| BrokerError::execution(e.to_string()))?;
    request.validate().map_err(BrokerError::execution)?;
    let owner = owner(client).await?;
    let result = match request.target {
        None => reads::read(owner, &request.id, request.request, None).await,
        Some(workspaces::ReadTarget::Job { id }) => {
            let job = Store::open_default()
                .and_then(|store| store.locate_for_owner(&id, Some(owner)))
                .map_err(|error| BrokerError::unavailable(error.to_string()))?
                .map(|(_, job)| job)
                .ok_or_else(|| {
                    BrokerError::authorization("temporary preview Job not found for this owner")
                })?;
            if job.workspace_id.as_deref() != Some(&request.id)
                || job.cancel_requested_at.is_some()
                || !matches!(
                    job.status,
                    JobStatus::Pending | JobStatus::Running | JobStatus::WaitingApproval
                )
            {
                return Err(BrokerError::authorization(
                    "temporary preview requires an exact live Job in this Workspace",
                ));
            }
            let meta = preview_conversation(
                owner,
                &request.id,
                job.session_id.as_deref().ok_or_else(|| {
                    BrokerError::authorization("preview Job has no canonical conversation")
                })?,
            )?;
            crate::workspaces::context_access::validate(owner, &meta, Some(&job.id))
                .map_err(BrokerError::authorization)?;
            crate::workspaces::association::check_job(&job).map_err(BrokerError::authorization)?;
            reads::read(owner, &request.id, request.request, Some(&job)).await
        }
        Some(workspaces::ReadTarget::Conversation { id }) => {
            let meta = preview_conversation(owner, &request.id, &id)?;
            crate::workspaces::context_access::validate(owner, &meta, None)
                .map_err(BrokerError::authorization)?;
            reads::read_conversation(owner, &request.id, request.request, &meta).await
        }
    };
    result.map_err(BrokerError::execution)
}

fn preview_conversation(
    owner: u32,
    workspace: &str,
    id: &str,
) -> Result<crate::session::SessionMeta, BrokerError> {
    let meta = super::tasks::task_session_meta(id, owner).map_err(BrokerError::authorization)?;
    if meta.workspace_id.as_deref() != Some(workspace)
        || !crate::session::record_is_root_owned(&meta.id)
    {
        return Err(BrokerError::authorization(
            "temporary preview requires a Root-owned conversation in this Workspace",
        ));
    }
    Ok(meta)
}

pub async fn index_source(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    let request: workspaces::IndexRequest =
        serde_json::from_value(params).map_err(|e| BrokerError::execution(e.to_string()))?;
    workspaces::validate_id(&request.id).map_err(BrokerError::execution)?;
    workspaces::validate_id(&request.tag_id).map_err(BrokerError::execution)?;
    let owner = owner(client).await?;
    let view = workspaces::open_default()
        .and_then(|s| s.workspace(owner, &request.id))
        .map_err(error)?;
    if view.workspace.state != WorkspaceState::Active
        || view.workspace.revision != request.expected_revision
    {
        return Err(BrokerError::execution(
            "Workspace changed; refresh the displayed revision",
        ));
    }
    let tag = view
        .tags
        .iter()
        .find(|tag| tag.id == request.tag_id)
        .ok_or_else(|| BrokerError::execution("tag not found"))?;
    let directory = match tag.kind {
        workspaces::ResourceKind::Directory => std::path::Path::new(&tag.path),
        workspaces::ResourceKind::File => std::path::Path::new(&tag.path)
            .parent()
            .ok_or_else(|| BrokerError::execution("file has no parent"))?,
    };
    let home =
        super::system_caps::verified_owner_home(owner).map_err(BrokerError::authorization)?;
    crate::worker::derive::reject_forbidden_for_owner(directory, &home)
        .map_err(BrokerError::authorization)?;
    {
        let _identity = FsIdentityGuard::enter(owner).map_err(BrokerError::authorization)?;
        super::owner_files::directory(directory).map_err(BrokerError::execution)?;
    }
    let gid = crate::agentd::spawn::resolve_identity(owner)
        .map_err(BrokerError::execution)?
        .gid;
    let mut command = tokio::process::Command::new("/usr/local/bin/claw-semantic");
    command
        .args([
            "add-root",
            directory
                .to_str()
                .ok_or_else(|| BrokerError::execution("index path must be UTF-8"))?,
        ])
        .env_clear()
        .env("HOME", &home)
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .kill_on_drop(true);
    unsafe {
        command.pre_exec(move || {
            if libc::setgroups(0, std::ptr::null()) != 0
                || libc::setresgid(gid, gid, gid) != 0
                || libc::setresuid(owner, owner, owner) != 0
                || libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            libc::umask(0o077);
            Ok(())
        });
    }
    let output = tokio::time::timeout(std::time::Duration::from_secs(15), command.output())
        .await
        .map_err(|_| BrokerError::unavailable("index source registration timed out"))?
        .map_err(|e| BrokerError::unavailable(e.to_string()))?;
    if !output.status.success() {
        return Err(BrokerError::execution(
            String::from_utf8_lossy(&output.stderr)
                .chars()
                .take(2048)
                .collect::<String>(),
        ));
    }
    Ok(json!({"configuration_saved":true,"directory":directory,
            "coverage":"This directory is added to the independent personal-file index, including files not tagged in this Workspace. Retrieval still requires live Workspace sharing.",
            "indexing":"the existing owner indexer reloads source directories; this acknowledgement is not completed indexing"}))
}
