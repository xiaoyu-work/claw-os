//! Workspace context domains above Activities. Persistence is shared with goals.

pub use clawd_client::workspaces::*;

use crate::activities::{ActivityError, SqliteActivityService};

pub mod cli;

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
}

pub fn open_default() -> Result<SqliteActivityService, ActivityError> {
    crate::activities::open_default()
}
