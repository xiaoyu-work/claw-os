use super::*;
use crate::activities::{ActivityDraft, ActivityService, DATABASE_SCHEMA_VERSION};

fn create(service: &SqliteActivityService) -> WorkspaceView {
    service
        .change_workspace(
            1000,
            WorkspaceAction::Create {
                draft: WorkspaceDraft {
                    name: "Research".into(),
                    description: "Shared background".into(),
                    context: "Sources remain data.".into(),
                },
            },
        )
        .unwrap()
}

#[test]
fn workspace_metadata_is_owner_scoped_and_every_change_uses_exact_revision() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let first = create(&service);
    let id = &first.workspace.id;
    assert!(matches!(
        service.workspace(1001, id),
        Err(ActivityError::NotFound)
    ));
    assert!(matches!(
        service.workspace(0, id),
        Err(ActivityError::NotFound)
    ));
    let next = service
        .change_workspace(
            1000,
            WorkspaceAction::PutNote {
                id: id.clone(),
                expected_revision: 1,
                note_id: None,
                note: NoteDraft {
                    title: "Audience".into(),
                    content: "Beginners".into(),
                },
            },
        )
        .unwrap();
    assert_eq!(next.workspace.revision, 2);
    assert_eq!(next.notes[0].source, NoteSource::OwnerNote);
    assert!(matches!(
        service.change_workspace(
            1000,
            WorkspaceAction::Archive {
                id: id.clone(),
                expected_revision: 1,
                archived: true,
            }
        ),
        Err(ActivityError::Conflict(_))
    ));
    assert_eq!(service.workspace(1000, id).unwrap(), next);
}

#[test]
fn deleting_workspace_removes_shared_bytes_but_preserves_paused_goals_and_tagged_files() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let first = create(&service);
    let id = first.workspace.id;
    let goal = service
        .create(
            1000,
            ActivityDraft {
                title: "Prepare notes".into(),
                goal: "Explain the sources".into(),
                completion_criteria: String::new(),
                boundaries: String::new(),
                resources: vec![],
            },
        )
        .unwrap();
    let linked = service
        .change_workspace(
            1000,
            WorkspaceAction::BindActivity {
                id: id.clone(),
                expected_revision: 1,
                activity_id: goal.id.clone(),
            },
        )
        .unwrap();
    let tagged = service
        .change_workspace(
            1000,
            WorkspaceAction::Tag {
                id: id.clone(),
                expected_revision: linked.workspace.revision,
                path: "/home/example/Documents".into(),
                kind: Some(ResourceKind::Directory),
            },
        )
        .unwrap();
    assert!(tagged.tags[0].covers(std::path::Path::new("/home/example/Documents/new/note.md")));
    assert!(!tagged.tags[0].covers(std::path::Path::new(
        "/home/example/Documents-private/note.md"
    )));
    let deleted = service
        .change_workspace(
            1000,
            WorkspaceAction::Delete {
                id: id.clone(),
                expected_revision: tagged.workspace.revision,
                confirm: true,
            },
        )
        .unwrap();
    assert_eq!(deleted.workspace.state, WorkspaceState::Deleted);
    assert!(deleted.workspace.cleanup_pending);
    assert!(
        deleted.notes.is_empty() && deleted.tags.is_empty() && deleted.workspace.context.is_empty()
    );
    assert_eq!(
        service.get(1000, &goal.id).unwrap().state,
        ActivityState::Paused
    );
    assert_eq!(service.get(1000, &goal.id).unwrap().goal, goal.goal);
    assert!(service.list_workspaces(1000).unwrap().workspaces.is_empty());
    assert!(service
        .change_workspace(
            1000,
            WorkspaceAction::Archive {
                id: id.clone(),
                expected_revision: deleted.workspace.revision,
                archived: false,
            }
        )
        .is_err());
    assert!(
        !service
            .finish_workspace_cleanup(1000, &id)
            .unwrap()
            .workspace
            .cleanup_pending
    );
}

#[test]
fn workspace_schema_migration_preserves_existing_activity_metadata() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let goal = service
        .create(
            1000,
            ActivityDraft {
                title: "Existing goal".into(),
                goal: "Keep its history".into(),
                completion_criteria: String::new(),
                boundaries: String::new(),
                resources: vec![],
            },
        )
        .unwrap();
    {
        let conn = service.lock().unwrap();
        conn.execute_batch(
            "DROP TABLE workspace_activities; DROP TABLE workspace_tags; DROP TABLE workspace_notes;
             DROP TABLE workspaces; PRAGMA user_version=9;",
        ).unwrap();
    }
    let conn = std::sync::Arc::try_unwrap(service.conn)
        .unwrap()
        .into_inner()
        .unwrap();
    let migrated = SqliteActivityService::initialize(conn, false).unwrap();
    assert_eq!(migrated.get(1000, &goal.id).unwrap(), goal);
    let version: u32 = migrated
        .lock()
        .unwrap()
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, DATABASE_SCHEMA_VERSION);
    create(&migrated);
}

#[test]
fn workspace_agent_reports_are_job_sourced_and_owner_rules_remain_immutable() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let first = create(&service);
    let job = uuid::Uuid::new_v4().to_string();
    let report = service
        .report_workspace(
            1000,
            &first.workspace.id,
            &job,
            NoteDraft {
                title: "Finding".into(),
                content: "Caller-reported, not owner rules.".into(),
            },
        )
        .unwrap();
    assert_eq!(report.source, NoteSource::AgentReport);
    assert_eq!(report.source_job_id.as_deref(), Some(job.as_str()));
    assert!(service
        .change_workspace(
            1000,
            WorkspaceAction::PutNote {
                id: first.workspace.id.clone(),
                expected_revision: 2,
                note_id: Some(report.id),
                note: NoteDraft {
                    title: "Owner rule".into(),
                    content: "Replace provenance".into()
                }
            }
        )
        .is_err());
    assert_eq!(
        service
            .workspace(1000, &first.workspace.id)
            .unwrap()
            .workspace
            .access_revision,
        1
    );
}

#[test]
fn workspace_revocation_advances_visibility_without_deleting_original_evidence() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let first = create(&service);
    let note = service
        .change_workspace(
            1000,
            WorkspaceAction::PutNote {
                id: first.workspace.id.clone(),
                expected_revision: 1,
                note_id: None,
                note: NoteDraft {
                    title: "Shared".into(),
                    content: "Small shared note".into(),
                },
            },
        )
        .unwrap();
    assert_eq!(note.workspace.access_revision, 1);
    let next = service
        .change_workspace(
            1000,
            WorkspaceAction::ForgetNote {
                id: first.workspace.id.clone(),
                expected_revision: 2,
                note_id: note.notes[0].id.clone(),
            },
        )
        .unwrap();
    assert_eq!(next.workspace.access_revision, 2);
    let mut old = serde_json::to_value(&next.workspace).unwrap();
    old.as_object_mut().unwrap().remove("access_revision");
    old.as_object_mut().unwrap().remove("read_links");
    let restored: Workspace = serde_json::from_value(old).unwrap();
    assert_eq!(restored.access_revision, 1);
    assert!(restored.read_links.is_empty());
}
