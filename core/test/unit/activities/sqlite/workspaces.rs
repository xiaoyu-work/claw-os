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
