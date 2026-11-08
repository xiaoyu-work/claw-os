//! Data constraints carried through Root App grants; never capability grants.

use super::{ResourceKind, WorkspaceService};
use crate::agent::service::{Job, Store};
use crate::caps::{Cap, CapSet, Scope, Verb};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkspaceBinding {
    pub owner_uid: u32,
    pub workspace_id: String,
    pub session_id: String,
    pub job_id: Option<String>,
}

impl WorkspaceBinding {
    pub(crate) fn for_session(
        owner: u32,
        session: &str,
        task: Option<&str>,
    ) -> Result<Option<Self>, String> {
        let Ok(sid) = session.parse::<crate::session::SessionId>() else {
            return Ok(None);
        };
        let meta = match crate::session::get_meta(&sid) {
            Ok(meta) => meta,
            Err(crate::session::SessionError::NotFound(_)) => return Ok(None),
            Err(error) => return Err(error.to_string()),
        };
        let Some(id) = meta.workspace_id else {
            return Ok(None);
        };
        if meta.owner_uid != Some(owner) || !crate::session::record_is_root_owned(&sid) {
            return Err("Workspace context requires Root-owned same-owner session metadata".into());
        }
        let binding = Self {
            owner_uid: owner,
            workspace_id: id,
            session_id: session.into(),
            job_id: task.map(str::to_string),
        };
        binding.check(&[])?;
        Ok(Some(binding))
    }

    fn job(&self) -> Result<Option<Job>, String> {
        self.job_id
            .as_deref()
            .map(|id| {
                let job = Store::open_default()
                    .and_then(|store| store.locate_for_owner(id, Some(self.owner_uid)))
                    .map_err(|e| e.to_string())?
                    .map(|(_, job)| job)
                    .ok_or("Root Workspace Job is unavailable")?;
                if job.session_id.as_deref() != Some(&self.session_id)
                    || job.workspace_id.as_deref() != Some(&self.workspace_id)
                {
                    return Err("Workspace grant no longer matches its Root Job".into());
                }
                super::association::check_job(&job)?;
                Ok(job)
            })
            .transpose()
    }

    pub(crate) fn check(&self, required: &[Cap]) -> Result<(), String> {
        super::association::active(self.owner_uid, &self.workspace_id)?;
        let sid = self
            .session_id
            .parse::<crate::session::SessionId>()
            .map_err(|e| e.to_string())?;
        let meta = crate::session::get_meta(&sid).map_err(|e| e.to_string())?;
        if meta.owner_uid != Some(self.owner_uid)
            || meta.workspace_id.as_deref() != Some(&self.workspace_id)
        {
            return Err("Root conversation Workspace changed".into());
        }
        let job = self.job()?;
        for cap in required {
            if cap.verb == Verb::MEMORY_WRITE {
                return Err("Workspace work cannot write unscoped learned/App memory; report a sourced Workspace finding instead".into());
            }
            if cap.verb == Verb::FS_READ {
                crate::clawd::workspaces::reads::filesystem_read(
                    self.owner_uid,
                    &self.workspace_id,
                    &cap.scope,
                    job.as_ref(),
                )?;
            }
        }
        Ok(())
    }
}

pub(crate) fn task_capability(job: &Job, cap: &Cap) -> Result<(), String> {
    super::association::check_job(job)?;
    let Some(id) = job.workspace_id.as_deref() else {
        return Ok(());
    };
    if cap.verb == Verb::MEMORY_WRITE {
        return Err("Workspace work cannot write global learned/App memory".into());
    }
    if cap.verb == Verb::FS_READ {
        crate::clawd::workspaces::reads::filesystem_read(
            job.owner_uid.ok_or("Workspace has no owner")?,
            id,
            &cap.scope,
            Some(job),
        )?;
    }
    Ok(())
}

pub(crate) fn restrict_caps(job: &Job, existing: &CapSet) -> Result<CapSet, String> {
    let Some(id) = job.workspace_id.as_deref() else {
        return Ok(existing.clone());
    };
    let owner = job.owner_uid.ok_or("Workspace has no owner")?;
    let view = super::open_default()
        .and_then(|service| service.workspace(owner, id))
        .map_err(|e| e.to_string())?;
    let mut caps = CapSet::from_caps(
        existing
            .iter()
            .filter(|cap| cap.verb != Verb::FS_READ && cap.verb != Verb::MEMORY_WRITE)
            .cloned(),
    );
    for tag in view.tags {
        let scope = match tag.kind {
            ResourceKind::File => tag.path,
            ResourceKind::Directory => format!("{}/**", tag.path.trim_end_matches('/')),
        };
        let cap = Cap::new(Verb::FS_READ, Scope::path(scope));
        if existing.covers(&cap) {
            caps.extend([cap]);
        }
    }
    Ok(caps)
}
