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
    assert_eq!(value["conversation"]["title"], DEFAULT_TITLE);
    assert!(
        uuid::Uuid::parse_str(value["conversation"]["presentation_id"].as_str().unwrap()).is_ok()
    );
    let listed = list(json!({}), &fixture.client).unwrap();
    assert_eq!(listed["conversation_count"], 1);
    assert_eq!(listed["conversations"][0]["id"], id.as_str());
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
    let mut root = fixture.client.clone();
    root.uid = Some(0);
    for result in [
        create(json!({}), &root),
        get(json!({"id": id}), &root),
        list(json!({}), &root),
        update(json!({"id": id, "deleted": true}), &root),
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
