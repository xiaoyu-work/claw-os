//! Transactional Workspace metadata in the existing owner-scoped goal database.

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use super::{load_activity, timestamp, SqliteActivityService};
use crate::activities::{ActivityError, ActivityState};
use crate::workspaces::*;

pub(super) const MIGRATE_TO_V10: &str = r#"
CREATE TABLE workspaces (
    owner_uid INTEGER NOT NULL CHECK(owner_uid BETWEEN 0 AND 4294967295),
    id TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK(revision > 0),
    state TEXT NOT NULL CHECK(state IN ('active','archived','deleted')),
    record_json TEXT NOT NULL,
    PRIMARY KEY(owner_uid,id)
);
CREATE INDEX workspaces_owner_state ON workspaces(owner_uid,state,id);
CREATE TABLE workspace_notes (
    owner_uid INTEGER NOT NULL,
    workspace_id TEXT NOT NULL,
    id TEXT NOT NULL,
    record_json TEXT NOT NULL,
    PRIMARY KEY(owner_uid,workspace_id,id),
    FOREIGN KEY(owner_uid,workspace_id) REFERENCES workspaces(owner_uid,id)
);
CREATE TABLE workspace_tags (
    owner_uid INTEGER NOT NULL,
    workspace_id TEXT NOT NULL,
    id TEXT NOT NULL,
    path TEXT NOT NULL,
    record_json TEXT NOT NULL,
    PRIMARY KEY(owner_uid,workspace_id,id),
    UNIQUE(owner_uid,workspace_id,path),
    FOREIGN KEY(owner_uid,workspace_id) REFERENCES workspaces(owner_uid,id)
);
CREATE TABLE workspace_activities (
    owner_uid INTEGER NOT NULL,
    workspace_id TEXT NOT NULL,
    activity_id TEXT NOT NULL,
    PRIMARY KEY(owner_uid,activity_id),
    FOREIGN KEY(owner_uid,workspace_id) REFERENCES workspaces(owner_uid,id),
    FOREIGN KEY(owner_uid,activity_id) REFERENCES activities(owner_uid,id)
);
CREATE INDEX workspace_activities_workspace ON workspace_activities(owner_uid,workspace_id);
"#;

fn invalid(value: impl std::fmt::Display) -> ActivityError {
    ActivityError::Invalid(format!("Workspace: {value}"))
}

fn corrupt(value: impl std::fmt::Display) -> ActivityError {
    ActivityError::Corrupt(format!("Workspace: {value}"))
}

pub(super) fn validate_schema(conn: &Connection) -> Result<(), ActivityError> {
    conn.prepare("SELECT owner_uid,id,revision,state,record_json FROM workspaces LIMIT 0")?;
    conn.prepare("SELECT owner_uid,workspace_id,id,record_json FROM workspace_notes LIMIT 0")?;
    conn.prepare("SELECT owner_uid,workspace_id,id,path,record_json FROM workspace_tags LIMIT 0")?;
    conn.prepare("SELECT owner_uid,workspace_id,activity_id FROM workspace_activities LIMIT 0")?;
    for table in ["workspace_notes", "workspace_tags", "workspace_activities"] {
        let keys: i64 = conn.query_row(
            &format!("SELECT COUNT(*) FROM pragma_foreign_key_list('{table}') WHERE \"table\"='workspaces'"),
            [], |row| row.get(0),
        )?;
        if keys != 2 {
            return Err(corrupt(format!("{table} owner foreign key is missing")));
        }
    }
    let keys: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pragma_foreign_key_list('workspace_activities') WHERE \"table\"='activities'",
        [], |row| row.get(0),
    )?;
    if keys != 2 {
        return Err(corrupt("Activity owner foreign key is missing"));
    }
    Ok(())
}

fn state(workspace: &Workspace) -> &'static str {
    match workspace.state {
        WorkspaceState::Active => "active",
        WorkspaceState::Archived => "archived",
        WorkspaceState::Deleted => "deleted",
    }
}

fn load(conn: &Connection, owner: u32, id: &str) -> Result<Workspace, ActivityError> {
    validate_id(id).map_err(invalid)?;
    let (revision, stored_state, json) = conn
        .query_row(
            "SELECT revision,state,record_json FROM workspaces WHERE owner_uid=?1 AND id=?2",
            params![owner, id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?
        .ok_or(ActivityError::NotFound)?;
    let record: Workspace = serde_json::from_str(&json).map_err(corrupt)?;
    record.validate().map_err(corrupt)?;
    if record.id != id || record.revision != revision as u64 || state(&record) != stored_state {
        return Err(corrupt("stored identity, revision or lifecycle mismatch"));
    }
    Ok(record)
}

fn save(conn: &Connection, owner: u32, workspace: &Workspace) -> Result<(), ActivityError> {
    workspace.validate().map_err(invalid)?;
    conn.execute(
        "UPDATE workspaces SET revision=?3,state=?4,record_json=?5 WHERE owner_uid=?1 AND id=?2",
        params![
            owner,
            workspace.id,
            i64::try_from(workspace.revision).map_err(invalid)?,
            state(workspace),
            serde_json::to_string(workspace)?
        ],
    )?;
    Ok(())
}

fn view(conn: &Connection, owner: u32, id: &str) -> Result<WorkspaceView, ActivityError> {
    let workspace = load(conn, owner, id)?;
    let mut notes = conn.prepare(
        "SELECT id,record_json FROM workspace_notes WHERE owner_uid=?1 AND workspace_id=?2 ORDER BY id",
    )?;
    let notes = notes
        .query_map(params![owner, id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .map(|row| {
            let (id, json) = row?;
            let note: WorkspaceNote = serde_json::from_str(&json).map_err(corrupt)?;
            if note.id != id {
                return Err(corrupt("note identity mismatch"));
            }
            Ok(note)
        })
        .collect::<Result<Vec<_>, ActivityError>>()?;
    let mut tags = conn.prepare(
        "SELECT id,path,record_json FROM workspace_tags WHERE owner_uid=?1 AND workspace_id=?2 ORDER BY path,id",
    )?;
    let tags = tags
        .query_map(params![owner, id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .map(|row| {
            let (id, path, json) = row?;
            let tag: ResourceTag = serde_json::from_str(&json).map_err(corrupt)?;
            if tag.id != id || tag.path != path {
                return Err(corrupt("tag identity/path mismatch"));
            }
            Ok(tag)
        })
        .collect::<Result<Vec<_>, ActivityError>>()?;
    let mut activities = conn.prepare(
        "SELECT activity_id FROM workspace_activities WHERE owner_uid=?1 AND workspace_id=?2 ORDER BY activity_id",
    )?;
    let activity_ids = activities
        .query_map(params![owner, id], |row| row.get(0))?
        .collect::<Result<Vec<String>, _>>()?;
    let result = WorkspaceView {
        schema: SCHEMA_VERSION,
        workspace,
        notes,
        tags,
        activity_ids,
    };
    result.validate().map_err(corrupt)?;
    Ok(result)
}

impl WorkspaceService for SqliteActivityService {
    fn list_workspaces(&self, owner: u32) -> Result<WorkspaceList, ActivityError> {
        let conn = self.lock()?;
        let mut query = conn.prepare(
            "SELECT id FROM workspaces WHERE owner_uid=?1 AND state!='deleted' ORDER BY id",
        )?;
        let ids = query
            .query_map([owner], |row| row.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        if ids.len() > MAX_WORKSPACES {
            return Err(corrupt("Workspace count exceeds its limit"));
        }
        let mut records = ids
            .iter()
            .map(|id| load(&conn, owner, id))
            .collect::<Result<Vec<_>, _>>()?;
        records.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| b.id.cmp(&a.id))
        });
        Ok(WorkspaceList {
            schema: SCHEMA_VERSION,
            workspaces: records,
        })
    }

    fn workspace(&self, owner: u32, id: &str) -> Result<WorkspaceView, ActivityError> {
        let conn = self.lock()?;
        view(&conn, owner, id)
    }

    fn activity_workspace(
        &self,
        owner: u32,
        activity_id: &str,
    ) -> Result<Option<Workspace>, ActivityError> {
        let conn = self.lock()?;
        let activity = load_activity(&conn, owner, &super::parse_id(activity_id)?)?;
        let id: Option<String> = conn.query_row(
            "SELECT workspace_id FROM workspace_activities WHERE owner_uid=?1 AND activity_id=?2",
            params![owner,activity.id],|row|row.get(0),
        ).optional()?;
        id.map(|id| load(&conn, owner, &id)).transpose()
    }

    fn change_workspace(
        &self,
        owner: u32,
        request: WorkspaceAction,
    ) -> Result<WorkspaceView, ActivityError> {
        request.validate().map_err(invalid)?;
        let creating = matches!(&request, WorkspaceAction::Create { .. });
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = timestamp();
        let mut workspace = if let WorkspaceAction::Create { draft } = &request {
            let count: i64 = tx.query_row(
                "SELECT COUNT(*) FROM workspaces WHERE owner_uid=?1 AND state!='deleted'",
                [owner],
                |row| row.get(0),
            )?;
            let retained: i64 = tx.query_row(
                "SELECT COUNT(*) FROM workspaces WHERE owner_uid=?1",
                [owner],
                |row| row.get(0),
            )?;
            if count >= MAX_WORKSPACES as i64 || retained >= 1000 {
                return Err(ActivityError::LimitReached);
            }
            let workspace = Workspace {
                id: uuid::Uuid::new_v4().to_string(),
                name: draft.name.trim().into(),
                description: draft.description.trim().into(),
                context: draft.context.trim().into(),
                state: WorkspaceState::Active,
                revision: 1,
                access_revision:1,
                cleanup_pending: false,
                read_links: Vec::new(),
                created_at: now.clone(),
                updated_at: now.clone(),
            };
            tx.execute(
                "INSERT INTO workspaces(owner_uid,id,revision,state,record_json) VALUES (?1,?2,?3,?4,?5)",
                params![owner,workspace.id,i64::try_from(workspace.revision).map_err(invalid)?,state(&workspace),serde_json::to_string(&workspace)?],
            )?;
            workspace
        } else {
            let workspace = load(
                &tx,
                owner,
                request.id().ok_or_else(|| invalid("missing identifier"))?,
            )?;
            if Some(workspace.revision) != request.expected_revision() {
                return Err(ActivityError::Conflict(
                    "Workspace changed; refresh the displayed revision".into(),
                ));
            }
            if workspace.state == WorkspaceState::Deleted
                && !matches!(
                    request,
                    WorkspaceAction::UnbindActivity { .. } | WorkspaceAction::Delete { .. }
                )
            {
                return Err(ActivityError::Conflict(
                    "Workspace was deleted; it cannot be reopened or read".into(),
                ));
            }
            workspace
        };
        let id = workspace.id.clone();
        match request {
            WorkspaceAction::Create { .. } => {}
            WorkspaceAction::Update { draft, .. } => {
                if workspace.context!=draft.context.trim(){
                    workspace.access_revision=workspace.access_revision.checked_add(1).ok_or_else(||invalid("visibility revision exhausted"))?;
                }
                workspace.name = draft.name.trim().into();
                workspace.description = draft.description.trim().into();
                workspace.context = draft.context.trim().into();
            }
            WorkspaceAction::Archive { archived, .. } => {
                let next = if archived {
                    WorkspaceState::Archived
                } else {
                    WorkspaceState::Active
                };
                if workspace.state == next {
                    return Err(invalid("Workspace already has that state"));
                }
                workspace.state = next;
            }
            WorkspaceAction::Delete { .. } => {
                workspace.access_revision=workspace.access_revision.checked_add(1).ok_or_else(||invalid("visibility revision exhausted"))?;
                if workspace.state == WorkspaceState::Deleted && !workspace.cleanup_pending {
                    return Err(ActivityError::Conflict(
                        "Workspace has already been deleted".into(),
                    ));
                }
                workspace.state = WorkspaceState::Deleted;
                workspace.context.clear();
                workspace.description.clear();
                workspace.read_links.clear();
                workspace.cleanup_pending = true;
                tx.execute(
                    "DELETE FROM workspace_notes WHERE owner_uid=?1 AND workspace_id=?2",
                    params![owner, id],
                )?;
                tx.execute(
                    "DELETE FROM workspace_tags WHERE owner_uid=?1 AND workspace_id=?2",
                    params![owner, id],
                )?;
                tx.execute(
                    "UPDATE activities SET state='paused',updated_at=?3
                     WHERE owner_uid=?1 AND state='active' AND id IN
                     (SELECT activity_id FROM workspace_activities WHERE owner_uid=?1 AND workspace_id=?2)",
                    params![owner,id,now],
                )?;
            }
            WorkspaceAction::ReadLink { source_workspace_id, target, note_ids, sources, expires_at, .. } => {
                if workspace.state != WorkspaceState::Active {
                    return Err(invalid("temporary reads require an active Workspace"));
                }
                let source = view(&tx, owner, &source_workspace_id)?;
                if source.workspace.state != WorkspaceState::Active {
                    return Err(invalid("source Workspace is not active"));
                }
                if note_ids.iter().any(|id| !source.notes.iter().any(|note| note.id == *id))
                    || sources.iter().any(|selected| !source.tags.iter().any(|tag|
                        tag.id == selected.tag_id && tag.covers(std::path::Path::new(&selected.path))))
                {
                    return Err(invalid("selection is not shared by the source Workspace"));
                }
                let expiry = chrono::DateTime::parse_from_rfc3339(&expires_at).map_err(invalid)?;
                let now_time = chrono::DateTime::parse_from_rfc3339(&now).map_err(invalid)?;
                if expiry <= now_time || expiry > now_time + chrono::Duration::hours(24) {
                    return Err(invalid("temporary reads expire within 24 hours"));
                }
                workspace.read_links.retain(|link|chrono::DateTime::parse_from_rfc3339(&link.expires_at).is_ok_and(|expiry|expiry>now_time));
                if workspace.read_links.len() >= MAX_READ_LINKS {
                    return Err(ActivityError::LimitReached);
                }
                workspace.read_links.push(ReadLink {
                    id: uuid::Uuid::new_v4().to_string(),
                    source_workspace_id, target, note_ids, sources,
                    expires_at, created_at: now.clone(),
                });
            }
            WorkspaceAction::RevokeLink { link_id, .. } => {
                let count = workspace.read_links.len();
                workspace.read_links.retain(|link| link.id != link_id);
                if count == workspace.read_links.len() { return Err(ActivityError::NotFound); }
                workspace.access_revision=workspace.access_revision.checked_add(1).ok_or_else(||invalid("visibility revision exhausted"))?;
            }
            WorkspaceAction::Tag { path, kind, .. } => {
                let kind =
                    kind.ok_or_else(|| invalid("resource kind must be verified by the broker"))?;
                let exists: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM workspace_tags WHERE owner_uid=?1 AND workspace_id=?2 AND path=?3)",
                    params![owner,id,path],|row|row.get(0),
                )?;
                if exists {
                    return Err(ActivityError::Conflict(
                        "resource already has this Workspace tag".into(),
                    ));
                }
                let count: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM workspace_tags WHERE owner_uid=?1 AND workspace_id=?2",
                    params![owner, id],
                    |row| row.get(0),
                )?;
                if count >= MAX_TAGS as i64 {
                    return Err(ActivityError::LimitReached);
                }
                let tag = ResourceTag {
                    id: uuid::Uuid::new_v4().to_string(),
                    workspace_id: id.clone(),
                    path,
                    kind,
                    created_at: now.clone(),
                };
                tag.validate().map_err(invalid)?;
                tx.execute(
                    "INSERT INTO workspace_tags(owner_uid,workspace_id,id,path,record_json) VALUES (?1,?2,?3,?4,?5)",
                    params![owner,id,tag.id,tag.path,serde_json::to_string(&tag)?],
                )?;
            }
            WorkspaceAction::Untag { tag_id, .. } => {
                if tx.execute(
                    "DELETE FROM workspace_tags WHERE owner_uid=?1 AND workspace_id=?2 AND id=?3",
                    params![owner, id, tag_id],
                )? != 1
                {
                    return Err(ActivityError::NotFound);
                }
                workspace.access_revision=workspace.access_revision.checked_add(1).ok_or_else(||invalid("visibility revision exhausted"))?;
            }
            WorkspaceAction::PutNote { note_id, note, .. } => {
                let previous = note_id.as_ref().map(|note_id| {
                    tx.query_row(
                        "SELECT record_json FROM workspace_notes WHERE owner_uid=?1 AND workspace_id=?2 AND id=?3",
                        params![owner,id,note_id],|row|row.get::<_,String>(0),
                    ).optional()?.ok_or(ActivityError::NotFound)
                        .and_then(|json|serde_json::from_str::<WorkspaceNote>(&json).map_err(corrupt))
                }).transpose()?;
                if let Some(previous) = &previous {
                    previous.validate().map_err(corrupt)?;
                    if previous.source != NoteSource::OwnerNote {
                        return Err(ActivityError::Conflict(
                            "Agent reports remain immutable; add an owner correction note".into(),
                        ));
                    }
                    if previous.content!=note.content.trim() || previous.title!=note.title.trim(){
                        workspace.access_revision=workspace.access_revision.checked_add(1).ok_or_else(||invalid("visibility revision exhausted"))?;
                    }
                } else {
                    let count: i64 = tx.query_row(
                        "SELECT COUNT(*) FROM workspace_notes WHERE owner_uid=?1 AND workspace_id=?2",
                        params![owner,id],|row|row.get(0),
                    )?;
                    if count >= MAX_NOTES as i64 {
                        return Err(ActivityError::LimitReached);
                    }
                }
                let record = WorkspaceNote {
                    id: previous
                        .as_ref()
                        .map(|n| n.id.clone())
                        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                    workspace_id: id.clone(),
                    title: note.title.trim().into(),
                    content: note.content.trim().into(),
                    source: NoteSource::OwnerNote,
                    source_job_id: None,
                    revision: previous
                        .as_ref()
                        .map(|n| {
                            n.revision
                                .checked_add(1)
                                .ok_or_else(|| invalid("note revision exhausted"))
                        })
                        .transpose()?
                        .unwrap_or(1),
                    created_at: previous
                        .as_ref()
                        .map(|n| n.created_at.clone())
                        .unwrap_or_else(|| now.clone()),
                    updated_at: now.clone(),
                };
                record.validate().map_err(invalid)?;
                tx.execute(
                    "INSERT INTO workspace_notes(owner_uid,workspace_id,id,record_json) VALUES (?1,?2,?3,?4)
                     ON CONFLICT(owner_uid,workspace_id,id) DO UPDATE SET record_json=excluded.record_json",
                    params![owner,id,record.id,serde_json::to_string(&record)?],
                )?;
            }
            WorkspaceAction::ForgetNote { note_id, .. } => {
                if tx.execute(
                    "DELETE FROM workspace_notes WHERE owner_uid=?1 AND workspace_id=?2 AND id=?3",
                    params![owner, id, note_id],
                )? != 1
                {
                    return Err(ActivityError::NotFound);
                }
                workspace.access_revision=workspace.access_revision.checked_add(1).ok_or_else(||invalid("visibility revision exhausted"))?;
            }
            WorkspaceAction::BindActivity { activity_id, .. } => {
                let activity = load_activity(&tx, owner, &activity_id)?;
                if !matches!(
                    activity.state,
                    ActivityState::Active | ActivityState::Paused
                ) {
                    return Err(invalid(
                        "only active or paused Activities may join a Workspace",
                    ));
                }
                if workspace.state != WorkspaceState::Active {
                    return Err(invalid("new Activities require an active Workspace"));
                }
                let previous: Option<String> = tx.query_row(
                    "SELECT workspace_id FROM workspace_activities WHERE owner_uid=?1 AND activity_id=?2",
                    params![owner,activity_id],|row|row.get(0),
                ).optional()?;
                if previous.is_some() {
                    return Err(ActivityError::Conflict(
                        "Activity already belongs to a Workspace; explicitly detach it first"
                            .into(),
                    ));
                }
                super::work::hold_for_workspace_move(&tx,&activity)?;
                tx.execute(
                    "INSERT INTO workspace_activities(owner_uid,workspace_id,activity_id) VALUES (?1,?2,?3)",
                    params![owner,id,activity_id],
                )?;
            }
            WorkspaceAction::UnbindActivity { activity_id, .. } => {
                let activity = load_activity(&tx, owner, &activity_id)?;
                if activity.state == ActivityState::Active {
                    return Err(invalid("pause the Activity before changing its Workspace"));
                }
                super::work::hold_for_workspace_move(&tx,&activity)?;
                if tx.execute(
                    "DELETE FROM workspace_activities WHERE owner_uid=?1 AND workspace_id=?2 AND activity_id=?3",
                    params![owner,id,activity_id],
                )? != 1 { return Err(ActivityError::NotFound); }
            }
        }
        if !creating {
            workspace.revision = workspace
                .revision
                .checked_add(1)
                .ok_or_else(|| invalid("revision exhausted"))?;
        }
        workspace.updated_at = now;
        save(&tx, owner, &workspace)?;
        let result = view(&tx, owner, &id)?;
        tx.commit()?;
        Ok(result)
    }

    fn finish_workspace_cleanup(
        &self,
        owner: u32,
        id: &str,
    ) -> Result<WorkspaceView, ActivityError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut workspace = load(&tx, owner, id)?;
        if workspace.state != WorkspaceState::Deleted {
            return Err(invalid("cleanup applies only to deleted Workspaces"));
        }
        if workspace.cleanup_pending {
            workspace.cleanup_pending = false;
            workspace.revision = workspace
                .revision
                .checked_add(1)
                .ok_or_else(|| invalid("revision exhausted"))?;
            workspace.updated_at = timestamp();
            save(&tx, owner, &workspace)?;
        }
        let result = view(&tx, owner, id)?;
        tx.commit()?;
        Ok(result)
    }

    fn report_workspace(&self, owner: u32, id: &str, job_id: &str, note: NoteDraft) -> Result<WorkspaceNote, ActivityError> {
        validate_id(job_id).map_err(invalid)?;
        note.validate().map_err(invalid)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut workspace = load(&tx, owner, id)?;
        if workspace.state != WorkspaceState::Active {
            return Err(invalid("Agent reports require an active Workspace"));
        }
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM workspace_notes WHERE owner_uid=?1 AND workspace_id=?2",
            params![owner, id], |row| row.get(0),
        )?;
        if count >= MAX_NOTES as i64 { return Err(ActivityError::LimitReached); }
        let now = timestamp();
        let record = WorkspaceNote {
            id: uuid::Uuid::new_v4().to_string(), workspace_id: id.into(),
            title: note.title, content: note.content, source: NoteSource::AgentReport,
            source_job_id: Some(job_id.into()), revision: 1,
            created_at: now.clone(), updated_at: now.clone(),
        };
        record.validate().map_err(invalid)?;
        tx.execute(
            "INSERT INTO workspace_notes(owner_uid,workspace_id,id,record_json) VALUES (?1,?2,?3,?4)",
            params![owner, id, record.id, serde_json::to_string(&record)?],
        )?;
        workspace.revision = workspace.revision.checked_add(1).ok_or_else(|| invalid("revision exhausted"))?;
        workspace.updated_at = now;
        save(&tx, owner, &workspace)?;
        tx.commit()?;
        Ok(record)
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/activities/sqlite/workspaces.rs"
    ));
}
