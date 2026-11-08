//! Root-owned conversation and Job context identity, separate from process cwd.

use super::{WorkspaceService, WorkspaceState};
use crate::agent::service::Job;
use crate::session::{self, SessionMeta};

pub(crate) fn active(owner: u32, id: &str) -> Result<(), String> {
    let view = super::open_default()
        .map_err(|e| e.to_string())?
        .workspace(owner, id)
        .map_err(|e| e.to_string())?;
    if view.workspace.state != WorkspaceState::Active {
        return Err("Workspace is archived or deleted and does not accept work".into());
    }
    Ok(())
}

pub(crate) fn resolve(
    owner: u32,
    meta: Option<&SessionMeta>,
    activity_id: Option<&str>,
    requested: Option<&str>,
) -> Result<Option<String>, String> {
    if let Some(id) = requested {
        super::validate_id(id)?;
    }
    let activity_space = activity_id
        .map(|id| {
            super::open_default()
                .and_then(|service| service.activity_workspace(owner, id))
                .map_err(|e| e.to_string())
        })
        .transpose()?
        .flatten()
        .map(|workspace| workspace.id);
    let inherited = meta.and_then(|meta| meta.workspace_id.as_deref());
    let selected = requested.or(inherited).or(activity_space.as_deref());
    if activity_space
        .as_deref()
        .is_some_and(|id| Some(id) != selected)
        || inherited.is_some_and(|id| Some(id) != selected)
    {
        return Err("Workspace mismatch; switching context requires another conversation".into());
    }
    if let Some(meta) = meta {
        if selected.is_some() && meta.owner_uid != Some(owner) {
            return Err("Workspace conversation belongs to another owner".into());
        }
        if selected.is_some() && inherited.is_none() {
            let sid = meta.id.to_string();
            let store = crate::agent::service::Store::open_default().map_err(|e| e.to_string())?;
            for status in [
                crate::agent::service::JobStatus::Pending,
                crate::agent::service::JobStatus::Running,
                crate::agent::service::JobStatus::WaitingApproval,
                crate::agent::service::JobStatus::Ok,
            ] {
                if store
                    .list_bucket_for_owner(status, None, Some(owner))
                    .map_err(|e| e.to_string())?
                    .iter()
                    .any(|job| job.session_id.as_deref() == Some(&sid))
                {
                    return Err("a conversation with retained Jobs cannot move into a Workspace; create another conversation".into());
                }
            }
            let has_history = crate::clawd::memory::run_as_owner(owner, move |path| {
                crate::clawd::memory::open_owner_db(&path, owner)?
                    .map(|db| db.has_session(&sid).map_err(|e| e.to_string()))
                    .transpose()
                    .map(|present| present.unwrap_or(false))
            })?;
            if has_history {
                return Err("a conversation with history cannot move into a Workspace; create another conversation".into());
            }
        }
        if inherited.is_some() {
            super::context_access::validate(owner, meta, None)?;
        }
    }
    if let Some(id) = selected {
        active(owner, id)?;
    }
    Ok(selected.map(str::to_string))
}

pub(crate) fn bind_session(
    meta: &SessionMeta,
    owner: u32,
    workspace: Option<&str>,
) -> Result<(), String> {
    if meta.workspace_id.as_deref() == workspace {
        return Ok(());
    }
    if meta.owner_uid != Some(owner) || meta.workspace_id.is_some() {
        return Err("conversation context identity changed".into());
    }
    let mut result = Ok(());
    session::update_meta(&meta.id, |current| {
        if current.owner_uid != Some(owner) || current.workspace_id != meta.workspace_id {
            result = Err("conversation context identity changed".into());
        } else {
            current.workspace_id = workspace.map(str::to_string);
        }
    })
    .map_err(|e| e.to_string())?;
    result
}

pub(crate) fn check_job(job: &Job) -> Result<(), String> {
    let Some(owner) = job.owner_uid else {
        return if job.workspace_id.is_none() {
            Ok(())
        } else {
            Err("Workspace Job has no owner".into())
        };
    };
    let meta = job
        .session_id
        .as_deref()
        .map(|id| {
            let sid = match id.parse::<session::SessionId>() {
                Ok(sid) => sid,
                Err(_) if job.workspace_id.is_none() => return Ok(None),
                Err(error) => return Err(error.to_string()),
            };
            match session::get_meta(&sid) {
                Ok(meta) => Ok(Some(meta)),
                Err(session::SessionError::NotFound(_)) if job.workspace_id.is_none() => Ok(None),
                Err(error) => Err(error.to_string()),
            }
        })
        .transpose()?
        .flatten();
    if let Some(meta) = &meta {
        if meta.workspace_id != job.workspace_id
            || (job.workspace_id.is_some() && meta.owner_uid != Some(owner))
        {
            return Err("Root conversation Workspace does not match the Job".into());
        }
    } else if job.workspace_id.is_some() {
        return Err("Workspace Job requires a canonical conversation".into());
    }
    if let Some(activity_id) = job.activity_id.as_deref() {
        let workspace = super::open_default()
            .and_then(|s| s.activity_workspace(owner, activity_id))
            .map_err(|e| e.to_string())?;
        if workspace
            .as_ref()
            .is_some_and(|w| Some(&w.id) != job.workspace_id.as_ref())
        {
            return Err("Activity Workspace changed; a new conversation is required".into());
        }
    }
    if let Some(id) = job.workspace_id.as_deref() {
        active(owner, id)?;
        if let Some(meta) = &meta {
            super::context_access::validate(owner, meta, Some(&job.id))?;
        }
        if job.workspace_revision.is_some() {
            let stored = crate::agent::service::Store::open_default()
                .and_then(|store| store.locate_for_owner(&job.id, Some(owner)))
                .map_err(|e| e.to_string())?
                .map(|(_, job)| job)
                .ok_or("Root Workspace Job was removed")?;
            if stored.workspace_id != job.workspace_id || stored.session_id != job.session_id {
                return Err("Root Workspace Job association changed".into());
            }
            let live = super::open_default()
                .and_then(|service| service.workspace(owner, id))
                .map_err(|e| e.to_string())?;
            if stored.workspace_revision != Some(live.workspace.revision) {
                return Err("Workspace shared context or source rights changed; a fresh execution attempt is required".into());
            }
        }
    }
    Ok(())
}
