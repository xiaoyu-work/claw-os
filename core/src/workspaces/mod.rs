//! Workspace context domains above Activities. Persistence is shared with goals.

pub use clawd_client::workspaces::*;

use crate::activities::{ActivityError, SqliteActivityService};

pub(crate) mod association;
pub(crate) mod boundary;
pub mod cli;
pub(crate) mod context_access;
pub(crate) mod gateway;

pub trait WorkspaceService: Send + Sync {
    fn list_workspaces(&self, owner: u32) -> Result<WorkspaceList, ActivityError>;
    fn workspace(&self, owner: u32, id: &str) -> Result<WorkspaceView, ActivityError>;
    fn change_workspace(
        &self,
        owner: u32,
        request: WorkspaceAction,
    ) -> Result<WorkspaceView, ActivityError>;
    fn activity_workspace(
        &self,
        owner: u32,
        activity_id: &str,
    ) -> Result<Option<Workspace>, ActivityError>;
    fn finish_workspace_cleanup(
        &self,
        owner: u32,
        id: &str,
    ) -> Result<WorkspaceView, ActivityError>;
    fn report_workspace(
        &self,
        owner: u32,
        workspace_id: &str,
        job_id: &str,
        note: NoteDraft,
    ) -> Result<WorkspaceNote, ActivityError>;
}

pub fn open_default() -> Result<SqliteActivityService, ActivityError> {
    crate::activities::open_default()
}
