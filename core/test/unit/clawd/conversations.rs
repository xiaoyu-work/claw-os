use super::*;
use crate::agent::memory::sqlite_fts::MemoryDb;
use crate::caps::Role;
use crate::test_env::{lock_env, TestEnvVarGuard};
use serde_json::json;

struct Fixture {
    _data: TestEnvVarGuard,
    _root: tempfile::TempDir,
    client: ClientIdentity,
}

impl Fixture {
    fn new() -> Option<Self> {
        let uid = unsafe { libc::geteuid() } as u32;
        if uid == 0 {
            return None;
        }
        let root = tempfile::Builder::new()
            .prefix("conversations-")
            .tempdir()
            .unwrap();
        let data = TestEnvVarGuard::set("COS_DATA_DIR", root.path().canonicalize().unwrap());
        Some(Self {
            _data: data,
            _root: root,
            client: ClientIdentity {
                pid: Some(std::process::id()),
                uid: Some(uid),
                gid: Some(unsafe { libc::getegid() } as u32),
                execution_uid: None,
                start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
                attended_local: false,
                extension_host: None,
            },
        })
    }

    fn create(&self) -> SessionId {
        create(json!({}), &self.client).unwrap()["conversation"]["id"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap()
    }

    fn uid(&self) -> u32 {
        self.client.uid.unwrap()
    }

    fn db(&self) -> MemoryDb {
        MemoryDb::open(crate::paths::clawd_user_memory_db_path(self.uid())).unwrap()
    }

    fn finish(&self, task_id: &str) {
        let store = Store::open_default().unwrap();
        let claimed = store.claim_one().unwrap().unwrap();
        assert_eq!(claimed.id, task_id);
        store
            .finish(
                claimed,
                crate::agent::service::FinishOutcome::Error("test worker ended".to_string()),
            )
            .unwrap();
    }
}

#[test]
fn conversation_create_is_empty_durable_and_uses_the_system_agent_baseline() {
    let _lock = lock_env();
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let id = fixture.create();
    let meta = session::get_meta(&id).unwrap();
    assert_eq!(meta.status, session::Status::Pending);
    assert_eq!(meta.owner_uid, Some(fixture.uid()));
    assert_eq!(meta.creator_runtime.as_deref(), Some("clawd"));
    assert_eq!(meta.role, Some(Role::Observer));
    assert_eq!(meta.origin, Some(SessionOrigin::SystemAgentTask));
    let home = super::super::system_caps::verified_owner_home(fixture.uid()).unwrap();
    let expected = super::super::system_caps::system_agent_caps(fixture.uid(), &home);
    let caps = session::get_caps(&id).unwrap();
    assert!(caps.covers_all(&expected));
    assert!(expected.covers_all(&caps));
    assert!(!crate::paths::clawd_user_memory_db_path(fixture.uid()).exists());

    let value = get(json!({"id": id}), &fixture.client).unwrap();
    assert_eq!(value["conversation"]["messages"], json!([]));
    assert_eq!(value["conversation"]["message_count"], 0);
    assert_eq!(value["conversation"]["messages_truncated"], false);
    assert_eq!(value["conversation"]["jobs"], json!([]));
    assert_eq!(value["conversation"]["job_count"], 0);
    assert_eq!(value["conversation"]["jobs_truncated"], false);
    assert_eq!(value["conversation"]["task_bindings_complete"], true);
    assert!(value["conversation"].get("task_bindings_error").is_none());
    assert_eq!(value["conversation"]["title"], DEFAULT_TITLE);
    assert!(
        uuid::Uuid::parse_str(value["conversation"]["presentation_id"].as_str().unwrap()).is_ok()
    );
    let listed = list(json!({}), &fixture.client).unwrap();
    assert_eq!(listed["conversation_count"], 1);
    assert_eq!(listed["conversations"][0]["id"], id.as_str());
}

#[test]
fn conversation_verifies_and_annotates_canonical_task_history() {
    let _lock = lock_env();
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let id = fixture.create();
    let job = Store::open_default()
        .unwrap()
        .submit(
            "bound prompt".to_string(),
            Some(id.to_string()),
            None,
            Some(fixture.uid()),
            None,
        )
        .unwrap();
    let db = fixture.db();
    let turn = db
        .record_task_user_message(
            id.as_str(),
            &job.id,
            &crate::agent::trust::LabeledSegment::of(
                crate::agent::trust::SourceKind::UserMessage,
                "",
            ),
            "bound prompt",
        )
        .unwrap();
    let answer_id = db
        .record_task_message(
            &turn,
            "assistant",
            &crate::agent::trust::LabeledSegment::of(
                crate::agent::trust::SourceKind::ModelResponse,
                "",
            ),
            "bound answer",
        )
        .unwrap();

    let value = get(json!({"id": id}), &fixture.client).unwrap()["conversation"].clone();
    assert_eq!(value["task_bindings_complete"], true);
    assert!(value.get("task_bindings_error").is_none());
    assert_eq!(value["job_count"], 1);
    assert_eq!(value["jobs"][0]["id"], job.id);
    assert_eq!(value["messages"][0]["id"], turn.user_message_id());
    assert_eq!(value["messages"][0]["task_id"], job.id);
    assert_eq!(value["messages"][0]["is_user_prompt"], true);
    assert_eq!(value["messages"][1]["id"], answer_id);
    assert_eq!(value["messages"][1]["task_id"], job.id);
    assert_eq!(value["messages"][1]["is_user_prompt"], false);
}

#[test]
fn conversation_binding_verification_fails_closed() {
    let _lock = lock_env();
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let legacy = fixture.create();
    let db = fixture.db();
    db.record_message(legacy.as_str(), "user", "legacy prompt")
        .unwrap();
    let legacy_view = get(json!({"id": legacy}), &fixture.client).unwrap()["conversation"].clone();
    assert_eq!(legacy_view["task_bindings_complete"], false);
    assert!(legacy_view["task_bindings_error"]
        .as_str()
        .unwrap()
        .contains("legacy or unbound"));
    assert!(legacy_view["messages"][0].get("task_id").is_none());

    let partial = fixture.create();
    let job = Store::open_default()
        .unwrap()
        .submit(
            "partial prompt".to_string(),
            Some(partial.to_string()),
            None,
            Some(fixture.uid()),
            None,
        )
        .unwrap();
    let turn = db
        .record_task_user_message(
            partial.as_str(),
            &job.id,
            &crate::agent::trust::LabeledSegment::of(
                crate::agent::trust::SourceKind::UserMessage,
                "",
            ),
            "partial prompt",
        )
        .unwrap();
    db.record_task_message(
        &turn,
        "assistant",
        &crate::agent::trust::LabeledSegment::of(
            crate::agent::trust::SourceKind::ModelResponse,
            "",
        ),
        "purged answer",
    )
    .unwrap();
    db.lock_conn()
        .unwrap()
        .execute(
            "DELETE FROM messages
             WHERE session_id = ? AND role = 'assistant'",
            [partial.as_str()],
        )
        .unwrap();
    let partial_view =
        get(json!({"id": partial}), &fixture.client).unwrap()["conversation"].clone();
    assert_eq!(partial_view["task_bindings_complete"], false);
    assert!(partial_view["task_bindings_error"]
        .as_str()
        .unwrap()
        .contains("partial task history"));
    assert!(partial_view["messages"][0].get("task_id").is_none());
}

#[test]
fn conversation_fork_copies_a_verified_prefix_without_copying_jobs_or_authority() {
    let _lock = lock_env();
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let parent = create(json!({"title": "Parent title"}), &fixture.client).unwrap()["conversation"]
        ["id"]
        .as_str()
        .unwrap()
        .parse::<SessionId>()
        .unwrap();
    let db = fixture.db();
    db.freeze_system_prompt(parent.as_str(), "frozen policy", 7)
        .unwrap();
    let store = Store::open_default().unwrap();

    let first = store
        .submit(
            "first prompt".to_string(),
            Some(parent.to_string()),
            None,
            Some(fixture.uid()),
            None,
        )
        .unwrap();
    let first_turn = db
        .record_task_user_message(
            parent.as_str(),
            &first.id,
            &crate::agent::trust::LabeledSegment::of(
                crate::agent::trust::SourceKind::UserMessage,
                "",
            ),
            "first prompt",
        )
        .unwrap();
    db.record_task_message(
        &first_turn,
        "assistant",
        &crate::agent::trust::LabeledSegment::of(
            crate::agent::trust::SourceKind::ModelResponse,
            "",
        ),
        "first answer",
    )
    .unwrap();
    fixture.finish(&first.id);

    let second = store
        .submit(
            "second prompt".to_string(),
            Some(parent.to_string()),
            None,
            Some(fixture.uid()),
            None,
        )
        .unwrap();
    let second_turn = db
        .record_task_user_message(
            parent.as_str(),
            &second.id,
            &crate::agent::trust::LabeledSegment::of(
                crate::agent::trust::SourceKind::UserMessage,
                "",
            ),
            "second prompt",
        )
        .unwrap();
    db.record_task_message(
        &second_turn,
        "assistant",
        &crate::agent::trust::LabeledSegment::of(
            crate::agent::trust::SourceKind::ModelResponse,
            "",
        ),
        "second answer",
    )
    .unwrap();
    fixture.finish(&second.id);

    assert_eq!(
        get(json!({"id": parent}), &fixture.client).unwrap()["conversation"]["job_count"],
        2
    );
    let forked = fork(
        json!({"id": parent, "before_user_turn": 1}),
        &fixture.client,
    )
    .unwrap()["conversation"]
        .clone();
    let child: SessionId = forked["id"].as_str().unwrap().parse().unwrap();
    assert_ne!(child, parent);
    assert_eq!(forked["parent_id"], parent.as_str());
    assert_eq!(forked["title"], "Parent title");
    assert_eq!(forked["message_count"], 2);
    assert_eq!(forked["job_count"], 1);
    assert_eq!(forked["jobs"][0]["id"], first.id);
    assert_eq!(forked["jobs"][0]["session_id"], parent.as_str());
    assert_eq!(forked["messages"][0]["task_id"], first.id);
    assert_eq!(forked["messages"][0]["source_session_id"], parent.as_str());
    assert_eq!(forked["task_bindings_complete"], true);
    assert_eq!(
        db.system_prompt_for(child.as_str(), 7).unwrap().as_deref(),
        Some("frozen policy")
    );
    assert_eq!(jobs::load(&child, fixture.uid()).unwrap().job_count, 0);

    let child_meta = session::get_meta(&child).unwrap();
    let home = super::super::system_caps::verified_owner_home(fixture.uid()).unwrap();
    let expected = super::super::system_caps::system_agent_caps(fixture.uid(), &home);
    let caps = session::get_caps(&child).unwrap();
    assert!(caps.covers_all(&expected));
    assert!(expected.covers_all(&caps));
    assert_eq!(child_meta.owner_uid, Some(fixture.uid()));

    let grandchild = fork(json!({"id": child}), &fixture.client).unwrap()["conversation"].clone();
    assert_eq!(grandchild["parent_id"], child.as_str());
    assert_eq!(grandchild["job_count"], 1);
    assert_eq!(grandchild["jobs"][0]["id"], first.id);
    assert_eq!(
        grandchild["messages"][0]["source_session_id"],
        parent.as_str()
    );
}

#[test]
fn conversation_fork_rejects_active_deleted_and_unverified_sources_before_creation() {
    let _lock = lock_env();
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let active = fixture.create();
    Store::open_default()
        .unwrap()
        .submit(
            "still running".to_string(),
            Some(active.to_string()),
            None,
            Some(fixture.uid()),
            None,
        )
        .unwrap();
    assert!(fork(json!({"id": active}), &fixture.client)
        .unwrap_err()
        .contains("active task"));

    let deleted = fixture.create();
    update(json!({"id": deleted, "deleted": true}), &fixture.client).unwrap();
    assert!(fork(json!({"id": deleted}), &fixture.client)
        .unwrap_err()
        .contains("soft-deleted"));

    let legacy = fixture.create();
    fixture
        .db()
        .record_message(legacy.as_str(), "user", "legacy prompt")
        .unwrap();
    let before = session::list().unwrap().len();
    assert!(fork(json!({"id": legacy}), &fixture.client)
        .unwrap_err()
        .contains("cannot fork task history"));
    assert_eq!(session::list().unwrap().len(), before);
}

#[test]
fn conversation_revert_hides_whole_tasks_but_retains_jobs_and_evidence() {
    let _lock = lock_env();
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let id = fixture.create();
    let db = fixture.db();
    let store = Store::open_default().unwrap();
    let mut task_ids = Vec::new();
    for (prompt, answer) in [
        ("first prompt", "first answer"),
        ("second prompt", "second answer"),
        ("third prompt", "third answer"),
    ] {
        let job = store
            .submit(
                prompt.to_string(),
                Some(id.to_string()),
                None,
                Some(fixture.uid()),
                None,
            )
            .unwrap();
        let turn = db
            .record_task_user_message(
                id.as_str(),
                &job.id,
                &crate::agent::trust::LabeledSegment::of(
                    crate::agent::trust::SourceKind::UserMessage,
                    "",
                ),
                prompt,
            )
            .unwrap();
        db.record_task_message(
            &turn,
            "assistant",
            &crate::agent::trust::LabeledSegment::of(
                crate::agent::trust::SourceKind::ModelResponse,
                "",
            ),
            answer,
        )
        .unwrap();
        fixture.finish(&job.id);
        task_ids.push(job.id);
    }

    let reverted = revert(json!({"id": id, "user_turns": 2}), &fixture.client).unwrap()
        ["conversation"]
        .clone();
    assert_eq!(reverted["message_count"], 2);
    assert_eq!(reverted["messages"][0]["text"], "first prompt");
    assert_eq!(reverted["messages"][1]["text"], "first answer");
    assert_eq!(reverted["job_count"], 1);
    assert_eq!(reverted["jobs"][0]["id"], task_ids[0]);
    assert_eq!(reverted["task_bindings_complete"], true);

    let raw = db.recent(id.as_str(), 10).unwrap();
    assert_eq!(raw.len(), 6);
    assert_eq!(
        Store::open_default()
            .unwrap()
            .list_bucket_for_owner(
                crate::agent::service::JobStatus::Ok,
                None,
                Some(fixture.uid())
            )
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        db.lock_conn()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM conversation_replay_exclusions
                 WHERE session_id = ?",
                [id.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        4
    );

    let next = store
        .submit(
            "replacement prompt".to_string(),
            Some(id.to_string()),
            None,
            Some(fixture.uid()),
            None,
        )
        .unwrap();
    let next_turn = db
        .record_task_user_message(
            id.as_str(),
            &next.id,
            &crate::agent::trust::LabeledSegment::of(
                crate::agent::trust::SourceKind::UserMessage,
                "",
            ),
            "replacement prompt",
        )
        .unwrap();
    db.record_task_message(
        &next_turn,
        "assistant",
        &crate::agent::trust::LabeledSegment::of(
            crate::agent::trust::SourceKind::ModelResponse,
            "",
        ),
        "replacement answer",
    )
    .unwrap();
    fixture.finish(&next.id);
    let continued = get(json!({"id": id}), &fixture.client).unwrap()["conversation"].clone();
    assert_eq!(continued["message_count"], 4);
    assert_eq!(continued["messages"][2]["text"], "replacement prompt");
    assert_eq!(continued["job_count"], 2);
    assert_eq!(continued["jobs"][1]["id"], next.id);
}

#[test]
fn conversation_revert_rejects_active_deleted_invalid_and_unverified_sources() {
    let _lock = lock_env();
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let active = fixture.create();
    Store::open_default()
        .unwrap()
        .submit(
            "still running".to_string(),
            Some(active.to_string()),
            None,
            Some(fixture.uid()),
            None,
        )
        .unwrap();
    assert!(
        revert(json!({"id": active, "user_turns": 1}), &fixture.client)
            .unwrap_err()
            .contains("active task")
    );

    let deleted = fixture.create();
    update(json!({"id": deleted, "deleted": true}), &fixture.client).unwrap();
    assert!(
        revert(json!({"id": deleted, "user_turns": 1}), &fixture.client)
            .unwrap_err()
            .contains("soft-deleted")
    );

    let legacy = fixture.create();
    fixture
        .db()
        .record_message(legacy.as_str(), "user", "legacy prompt")
        .unwrap();
    let legacy_error = revert(json!({"id": legacy, "user_turns": 1}), &fixture.client).unwrap_err();
    assert!(
        legacy_error.contains("conversation has 0"),
        "{legacy_error}"
    );
    assert_eq!(
        fixture
            .db()
            .lock_conn()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM conversation_replay_exclusions
                 WHERE session_id = ?",
                [legacy.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    assert!(
        revert(json!({"id": legacy, "user_turns": 0}), &fixture.client)
            .unwrap_err()
            .contains("greater than zero")
    );
    assert!(revert(json!({"id": legacy, "user_turns": 2}), &fixture.client).is_err());
}

#[test]
fn conversation_owner_boundary_and_inputs_fail_before_side_effects() {
    let _lock = lock_env();
    let Some(fixture) = Fixture::new() else {
        return;
    };
    for invalid in [
        json!({"title": ""}),
        json!({"title": "bad\nlabel"}),
        json!({"title": "x".repeat(MAX_TITLE_CHARS + 1)}),
        json!({"owner_uid": fixture.uid()}),
    ] {
        assert!(create(invalid, &fixture.client).is_err());
        assert!(!session::sessions_root().exists());
    }
    let id = fixture.create();
    let mut foreign = fixture.client.clone();
    foreign.uid = Some(fixture.uid() + 1);
    assert_eq!(
        get(json!({"id": id}), &foreign).unwrap_err(),
        format!("conversation not found: {id}")
    );
    assert!(update(json!({"id": id, "title": "foreign"}), &foreign).is_err());
    assert_eq!(
        fork(json!({"id": id}), &foreign).unwrap_err(),
        format!("conversation not found: {id}")
    );
    assert!(fork(
        json!({"id": derive_presentation_id(&id).as_str()}),
        &fixture.client
    )
    .unwrap_err()
    .contains("invalid conversation id"));
    let mut root = fixture.client.clone();
    root.uid = Some(0);
    for result in [
        create(json!({}), &root),
        get(json!({"id": id}), &root),
        list(json!({}), &root),
        update(json!({"id": id, "deleted": true}), &root),
        fork(json!({"id": id}), &root),
        revert(json!({"id": id, "user_turns": 1}), &root),
    ] {
        assert_eq!(result.unwrap_err(), ROOT_OWNER_REFUSAL);
    }
    assert_eq!(session::list().unwrap().len(), 1);
}

#[test]
fn conversation_presentation_state_renames_archives_and_soft_deletes() {
    let _lock = lock_env();
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let id = fixture.create();
    let db = fixture.db();
    db.record_message(id.as_str(), "user", "retained evidence")
        .unwrap();
    let renamed = update(
        json!({"id": id, "title": "  Chosen title  ", "archived": true}),
        &fixture.client,
    )
    .unwrap();
    assert_eq!(renamed["conversation"]["title"], "Chosen title");
    assert_eq!(renamed["conversation"]["archived"], true);
    assert_eq!(
        list(json!({}), &fixture.client).unwrap()["conversation_count"],
        0
    );
    assert_eq!(
        list(json!({"archived": true}), &fixture.client).unwrap()["conversation_count"],
        1
    );
    update(
        json!({"id": id, "archived": false, "deleted": true}),
        &fixture.client,
    )
    .unwrap();
    assert_eq!(
        list(json!({}), &fixture.client).unwrap()["conversation_count"],
        0
    );
    let deleted = get(json!({"id": id}), &fixture.client).unwrap();
    assert_eq!(deleted["conversation"]["deleted"], true);
    assert_eq!(
        deleted["conversation"]["messages"][0]["text"],
        "retained evidence"
    );
    assert_eq!(deleted["conversation"]["message_count"], 1);
    assert_eq!(deleted["conversation"]["messages_truncated"], false);
    assert_eq!(db.count_session(id.as_str()).unwrap(), 1);
}

#[test]
fn conversation_presentation_uuid_is_stable_owner_scoped_and_read_only_for_legacy_state() {
    let _lock = lock_env();
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let id = fixture.create();
    let alias = derive_presentation_id(&id);
    assert_eq!(
        get(
            json!({"id": alias.as_str().to_uppercase()}),
            &fixture.client
        )
        .unwrap()["conversation"]["id"],
        id.as_str()
    );
    let mut presentation = session::read_state(&id, PRESENTATION_NAMESPACE).unwrap();
    presentation
        .as_object_mut()
        .unwrap()
        .remove("presentation_id");
    session::write_state(&id, PRESENTATION_NAMESPACE, presentation.clone()).unwrap();
    assert_eq!(
        get(json!({"id": alias.as_str()}), &fixture.client).unwrap()["conversation"]["id"],
        id.as_str()
    );
    assert_eq!(
        session::read_state(&id, PRESENTATION_NAMESPACE).unwrap(),
        presentation
    );
    assert!(update(
        json!({"id": alias.as_str(), "archived": true}),
        &fixture.client
    )
    .is_err());
    let mut foreign = fixture.client.clone();
    foreign.uid = Some(fixture.uid() + 1);
    assert_eq!(
        get(json!({"id": alias.as_str()}), &foreign).unwrap_err(),
        format!("conversation not found: {}", alias.as_str())
    );
}

#[test]
fn conversation_uuid_projection_and_list_pagination_are_deterministic() {
    let id: SessionId = "ses_0000000000001_000000000001".parse().unwrap();
    assert_eq!(
        derive_presentation_id(&id).as_str(),
        "078ed458-0e17-882b-b0aa-ca1088683b25"
    );

    let _lock = lock_env();
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let older = fixture.create();
    session::update_meta(&older, |meta| {
        meta.created_at = "2020-01-01T00:00:00Z".to_string();
    })
    .unwrap();
    let newer = fixture.create();
    let value = list(json!({"limit": 1}), &fixture.client).unwrap();
    assert_eq!(value["conversations"][0]["id"], newer.as_str());
    assert_eq!(value["conversation_count"], 2);
    assert_eq!(value["conversations_truncated"], true);
}

#[test]
fn conversation_update_refuses_active_tasks_and_corrupt_metadata() {
    let _lock = lock_env();
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let id = fixture.create();
    let store = Store::open_default().unwrap();
    store
        .submit(
            "queued turn".to_string(),
            Some(id.to_string()),
            None,
            Some(fixture.uid()),
            None,
        )
        .unwrap();
    assert!(update(json!({"id": id, "archived": true}), &fixture.client)
        .unwrap_err()
        .contains("active task"));

    let other = fixture.create();
    session::update_meta(&other, |meta| {
        meta.created_at = "not a timestamp".to_string()
    })
    .unwrap();
    assert!(get(json!({"id": other}), &fixture.client)
        .unwrap_err()
        .contains("timestamp"));
    assert!(list(json!({}), &fixture.client)
        .unwrap_err()
        .contains("timestamp"));

    let corrupt = fixture.create();
    session::write_state(
        &corrupt,
        PRESENTATION_NAMESPACE,
        json!({"title": "bad\nstate"}),
    )
    .unwrap();
    assert!(get(json!({"id": corrupt}), &fixture.client)
        .unwrap_err()
        .contains("control"));
}

#[test]
fn conversation_initialization_failure_retains_failed_session_evidence() {
    let _lock = lock_env();
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let home = super::super::system_caps::verified_owner_home(fixture.uid()).unwrap();
    let result = super::super::tasks::create_agent_session_with(
        "failed initialization".to_string(),
        fixture.uid(),
        &home,
        |_| Err::<(), _>("specific initialization failure".to_string()),
    );
    assert!(result
        .unwrap_err()
        .contains("specific initialization failure"));
    let retained = session::list().unwrap();
    assert_eq!(retained.len(), 1);
    assert_eq!(retained[0].status, session::Status::Failed);
    assert_eq!(
        list(json!({}), &fixture.client).unwrap()["conversation_count"],
        0
    );
}

#[test]
fn conversation_update_does_not_commit_when_history_cannot_be_projected() {
    let _lock = lock_env();
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let id = fixture.create();
    let before = session::read_state(&id, PRESENTATION_NAMESPACE).unwrap();
    let path = crate::paths::clawd_user_memory_db_path(fixture.uid());
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"not a sqlite database").unwrap();

    assert!(update(
        json!({"id": id, "title": "must not persist"}),
        &fixture.client
    )
    .is_err());
    assert_eq!(
        session::read_state(&id, PRESENTATION_NAMESPACE).unwrap(),
        before
    );
}

#[test]
fn conversation_edit_lock_serializes_task_admission_without_a_placeholder_job() {
    use std::sync::mpsc;
    use std::time::Duration;

    let _lock = lock_env();
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let id = fixture.create();
    let store = Store::open_default().unwrap();
    let guard = store.lock_idle_session(id.as_str()).unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (result_tx, result_rx) = mpsc::channel();
    std::thread::scope(|scope| {
        let handle = scope.spawn(|| {
            entered_tx.send(()).unwrap();
            result_tx
                .send(
                    store
                        .submit(
                            "queued after the edit".to_string(),
                            Some(id.to_string()),
                            None,
                            Some(fixture.uid()),
                            None,
                        )
                        .unwrap(),
                )
                .unwrap();
        });
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(matches!(
            result_rx.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        assert_eq!(store.counts().unwrap(), (0, 0, 0, 0));
        drop(guard);
        let job = result_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(job.session_id.as_deref(), Some(id.as_str()));
        handle.join().unwrap();
    });
    assert_eq!(store.counts().unwrap(), (1, 0, 0, 0));
}
