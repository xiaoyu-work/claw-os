//! Conservative model-replay constraints; original conversation evidence stays.

use super::{ReadLink, ReadTarget, WorkspaceService, WorkspaceState};
use crate::session::{self, SessionId, SessionMeta};
use serde::{Deserialize, Serialize};

const STATE: &str = "workspace_temporary_context";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Dependency {
    link_id: String,
    source_workspace_id: String,
    source_access_revision: u64,
    target: ReadTarget,
    expires_at: String,
}
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Dependencies {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    reads: Vec<Dependency>,
}

fn dependencies(id: &SessionId) -> Result<Dependencies, String> {
    let value = session::read_state(id, STATE).map_err(|e| e.to_string())?;
    if value.is_null() {
        return Ok(Dependencies {
            version: 1,
            reads: Vec::new(),
        });
    }
    let result: Dependencies = serde_json::from_value(value)
        .map_err(|e| format!("invalid temporary context history: {e}"))?;
    if result.version != 1 || result.reads.len() > super::MAX_READ_LINKS {
        return Err("temporary context history exceeds its version or bounds".into());
    }
    for read in &result.reads {
        super::validate_id(&read.link_id)?;
        super::validate_id(&read.source_workspace_id)?;
        read.target.validate()?;
        if read.source_access_revision == 0 {
            return Err("invalid temporary source visibility revision".into());
        }
        chrono::DateTime::parse_from_rfc3339(&read.expires_at).map_err(|e| e.to_string())?;
    }
    Ok(result)
}

pub(crate) fn bind_epoch(meta: &SessionMeta, owner: u32, id: &str) -> Result<(), String> {
    let workspace = super::open_default()
        .and_then(|service| service.workspace(owner, id))
        .map_err(|e| e.to_string())?
        .workspace;
    let mut result = Ok(());
    session::update_meta(&meta.id, |current| {
        if current.owner_uid != Some(owner) || current.workspace_id.as_deref() != Some(id) {
            result = Err("Root Workspace conversation association changed".into());
        } else if current.workspace_access_revision.is_none() {
            current.workspace_access_revision = Some(workspace.access_revision);
        }
    })
    .map_err(|e| e.to_string())?;
    result
}

pub(crate) fn register(
    owner: u32,
    destination: &str,
    link: &ReadLink,
    session_id: &str,
) -> Result<(), String> {
    let sid = session_id.parse::<SessionId>().map_err(|e| e.to_string())?;
    let meta = session::get_meta(&sid).map_err(|e| e.to_string())?;
    if meta.owner_uid != Some(owner)
        || meta.workspace_id.as_deref() != Some(destination)
        || !session::record_is_root_owned(&sid)
    {
        return Err(
            "temporary context requires an exact Root-owned same-owner conversation".into(),
        );
    }
    let origin = super::open_default()
        .and_then(|service| service.workspace(owner, &link.source_workspace_id))
        .map_err(|e| e.to_string())?;
    let mut state = dependencies(&sid)?;
    if state.reads.iter().any(|read| read.link_id == link.id) {
        return Err("temporary context was already registered".into());
    }
    if state.reads.len() >= super::MAX_READ_LINKS {
        return Err("temporary context history limit reached; start another conversation".into());
    }
    state.reads.push(Dependency {
        link_id: link.id.clone(),
        source_workspace_id: link.source_workspace_id.clone(),
        source_access_revision: origin.workspace.access_revision,
        target: link.target.clone(),
        expires_at: link.expires_at.clone(),
    });
    session::write_state(
        &sid,
        STATE,
        serde_json::to_value(state).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

fn valid(owner: u32, meta: &SessionMeta, job_id: Option<&str>) -> Result<bool, String> {
    let Some(id) = meta.workspace_id.as_deref() else {
        return Ok(true);
    };
    let view = super::open_default()
        .and_then(|service| service.workspace(owner, id))
        .map_err(|e| e.to_string())?;
    if view.workspace.state != WorkspaceState::Active {
        return Ok(false);
    }
    if meta
        .workspace_access_revision
        .is_some_and(|revision| revision != view.workspace.access_revision)
    {
        return Ok(false);
    }
    for dependency in dependencies(&meta.id)?.reads {
        let scope_matches = match &dependency.target {
            ReadTarget::Job { id } => job_id == Some(id),
            ReadTarget::Conversation { id } => id == meta.id.as_str(),
        };
        let expiry = chrono::DateTime::parse_from_rfc3339(&dependency.expires_at)
            .map_err(|e| e.to_string())?;
        if !scope_matches || expiry <= chrono::Utc::now() {
            return Ok(false);
        }
        let Some(link) = view.workspace.read_links.iter().find(|link| {
            link.id == dependency.link_id
                && link.target == dependency.target
                && link.source_workspace_id == dependency.source_workspace_id
                && link.expires_at == dependency.expires_at
        }) else {
            return Ok(false);
        };
        let origin = super::open_default()
            .and_then(|service| service.workspace(owner, &dependency.source_workspace_id))
            .map_err(|e| e.to_string())?;
        if origin.workspace.state != WorkspaceState::Active
            || origin.workspace.access_revision != dependency.source_access_revision
            || link
                .note_ids
                .iter()
                .any(|id| !origin.notes.iter().any(|note| note.id == *id))
            || link.sources.iter().any(|source| {
                !origin.tags.iter().any(|tag| {
                    tag.id == source.tag_id && tag.covers(std::path::Path::new(&source.path))
                })
            })
        {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(crate) fn validate(owner: u32, meta: &SessionMeta, job_id: Option<&str>) -> Result<(), String> {
    if !valid(owner, meta, job_id)? {
        return Err("Workspace visibility or an exact temporary read scope changed/expired; start a new conversation. Owner-visible history and execution evidence remain.".into());
    }
    Ok(())
}

pub(crate) fn has_temporary(id: &SessionId) -> Result<bool, String> {
    Ok(!dependencies(id)?.reads.is_empty())
}

pub(crate) fn recallable(
    owner: u32,
    meta: &SessionMeta,
    job: &crate::agent::service::Job,
) -> Result<bool, String> {
    if has_temporary(&meta.id)? && job.session_id.as_deref() != Some(meta.id.as_str()) {
        return Ok(false);
    }
    valid(owner, meta, Some(&job.id))
}

pub(crate) fn registered_link(id: &SessionId, link: &ReadLink) -> Result<bool, String> {
    Ok(dependencies(id)?.reads.iter().any(|dependency| {
        dependency.link_id == link.id
            && dependency.source_workspace_id == link.source_workspace_id
            && dependency.target == link.target
            && dependency.expires_at == link.expires_at
    }))
}

pub(crate) fn copy_constraints(parent: &SessionId, child: &SessionId) -> Result<(), String> {
    let state = dependencies(parent)?;
    if !state.reads.is_empty() {
        session::write_state(
            child,
            STATE,
            serde_json::to_value(state).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}
