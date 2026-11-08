use super::*;

#[test]
fn temporary_preview_pagination_retains_saved_link_target_and_revision() {
    let id = uuid::Uuid::new_v4().to_string();
    let link_id = uuid::Uuid::new_v4().to_string();
    let target = ReadTarget::Job {
        id: uuid::Uuid::new_v4().to_string(),
    };
    let mut state = Workspaces::default();
    state.view = Some(WorkspaceView {
        schema: 1,
        workspace: Workspace {
            id: id.clone(),
            name: "Preview".into(),
            description: String::new(),
            context: String::new(),
            state: WorkspaceState::Active,
            revision: 1,
            access_revision: 1,
            cleanup_pending: false,
            read_links: Vec::new(),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
        },
        notes: Vec::new(),
        tags: Vec::new(),
        activity_ids: Vec::new(),
    });
    state.source = Some(SourcePage {
        path: "/home/example/file.txt".into(),
        source_workspace_id: uuid::Uuid::new_v4().to_string(),
        imported: true,
        source: "current_pinned_utf8_file".into(),
        revision: "a".repeat(64),
        content: "first".into(),
        offset: 0,
        next_offset: Some(5),
        total_chars: 10,
        source_complete: false,
    });
    state.source_preview = Some((link_id.clone(), target.clone()));
    let action = state.update(Message::NextSource, true).unwrap().action;
    match action {
        Action::Preview(
            destination,
            WorkspaceRead::Source {
                link_id: binding,
                offset,
                revision,
                ..
            },
            selected,
        ) => {
            assert_eq!(destination, id);
            assert_eq!(binding.as_deref(), Some(link_id.as_str()));
            assert_eq!(offset, 5);
            assert_eq!(revision.as_deref(), Some("a".repeat(64).as_str()));
            assert_eq!(selected, target);
        }
        _ => panic!("temporary source page widened into an ordinary read"),
    }
}
