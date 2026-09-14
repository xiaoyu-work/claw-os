use super::*;
use crate::agent::memory::conversation_bindings::MessageTaskBinding;
use crate::test_env::{lock_env, TestEnvVarGuard};
use serde_json::json;

struct Fixture {
    _data: TestEnvVarGuard,
    _root: tempfile::TempDir,
    client: crate::clawd::client_identity::ClientIdentity,
}

impl Fixture {
    fn new() -> Option<Self> {
        let uid = unsafe { libc::geteuid() } as u32;
        if uid == 0 {
            return None;
        }
        let root = tempfile::Builder::new()
            .prefix("conversation-bindings-")
            .tempdir()
            .unwrap();
        let data = TestEnvVarGuard::set("COS_DATA_DIR", root.path().canonicalize().unwrap());
        Some(Self {
            _data: data,
            _root: root,
            client: crate::clawd::client_identity::ClientIdentity {
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
        super::super::create(json!({}), &self.client).unwrap()["conversation"]["id"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap()
    }

    fn uid(&self) -> u32 {
        self.client.uid.unwrap()
    }

    fn submit(&self, session_id: &SessionId, prompt: &str) -> crate::agent::service::Job {
        Store::open_default()
            .unwrap()
            .submit(
                prompt.to_string(),
                Some(session_id.to_string()),
                None,
                Some(self.uid()),
                None,
            )
            .unwrap()
    }
}

fn binding(
    session_id: &SessionId,
    source_session_id: &SessionId,
    message_id: i64,
    source_message_id: i64,
    user_message_id: i64,
    source_user_message_id: i64,
    task_id: &str,
) -> MessageTaskBinding {
    MessageTaskBinding {
        message_id,
        session_id: session_id.to_string(),
        task_id: task_id.to_string(),
        user_message_id,
        source_session_id: source_session_id.to_string(),
        source_message_id,
        source_user_message_id,
        is_user_prompt: message_id == user_message_id,
    }
}

#[test]
fn complete_membership_verifies_and_orders_actual_jobs() {
    let _lock = lock_env();
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let session_id = fixture.create();
    let first = fixture.submit(&session_id, "first");
    let second = fixture.submit(&session_id, "second");
    let members = vec![
        binding(&session_id, &session_id, 1, 1, 1, 1, &first.id),
        binding(&session_id, &session_id, 2, 2, 1, 1, &first.id),
        binding(&session_id, &session_id, 3, 3, 3, 3, &second.id),
        binding(&session_id, &session_id, 4, 4, 3, 3, &second.id),
    ];
    let state = BindingState {
        originals: members.clone(),
        members,
        oversized: false,
    };
    let meta = crate::session::get_meta(&session_id).unwrap();

    let verified = verify(&meta, &Presentation::default(), &state, 4, fixture.uid()).unwrap();
    assert!(verified.jobs.task_bindings_complete);
    assert_eq!(verified.jobs.jobs[0].id, first.id);
    assert_eq!(verified.jobs.jobs[1].id, second.id);
}

#[test]
fn legacy_partial_and_missing_job_sets_fail_closed() {
    let _lock = lock_env();
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let session_id = fixture.create();
    let meta = crate::session::get_meta(&session_id).unwrap();
    assert!(verify(
        &meta,
        &Presentation::default(),
        &BindingState::default(),
        1,
        fixture.uid()
    )
    .is_err());

    let job = fixture.submit(&session_id, "partial");
    let partial = BindingState {
        members: vec![binding(&session_id, &session_id, 1, 1, 1, 1, &job.id)],
        originals: vec![
            binding(&session_id, &session_id, 1, 1, 1, 1, &job.id),
            binding(&session_id, &session_id, 2, 2, 1, 1, &job.id),
        ],
        oversized: false,
    };
    assert!(
        verify(&meta, &Presentation::default(), &partial, 1, fixture.uid())
            .unwrap_err()
            .contains("partial task history")
    );

    let missing = fixture.create();
    let missing_meta = crate::session::get_meta(&missing).unwrap();
    let complete = BindingState {
        members: vec![binding(&missing, &missing, 1, 1, 1, 1, "missing-task")],
        originals: vec![binding(&missing, &missing, 1, 1, 1, 1, "missing-task")],
        oversized: false,
    };
    assert!(verify(
        &missing_meta,
        &Presentation::default(),
        &complete,
        1,
        fixture.uid()
    )
    .unwrap_err()
    .contains("job set"));
}

#[test]
fn inherited_membership_requires_an_owned_ancestor_and_source_job() {
    let _lock = lock_env();
    let Some(fixture) = Fixture::new() else {
        return;
    };
    let parent = fixture.create();
    let source_job = fixture.submit(&parent, "source prompt");
    let child = fixture.create();
    let child_meta = crate::session::get_meta(&child).unwrap();
    let presentation = Presentation {
        parent_id: Some(parent.to_string()),
        inherited_tasks: vec![TaskMembership {
            task_id: source_job.id.clone(),
            source_session_id: parent.to_string(),
            message_ids: vec![1, 2],
        }],
        ..Presentation::default()
    };
    let members = vec![
        binding(&child, &parent, 11, 1, 11, 1, &source_job.id),
        binding(&child, &parent, 12, 2, 11, 1, &source_job.id),
    ];
    let state = BindingState {
        members,
        originals: Vec::new(),
        oversized: false,
    };

    let verified = verify(&child_meta, &presentation, &state, 2, fixture.uid()).unwrap();
    assert_eq!(verified.jobs.jobs[0].id, source_job.id);
    assert_eq!(verified.jobs.jobs[0].session_id, parent.as_str());
    assert_eq!(verified.memberships[0].source_session_id, parent.as_str());

    let mut outside_lineage = presentation;
    outside_lineage.parent_id = None;
    assert!(
        verify(&child_meta, &outside_lineage, &state, 2, fixture.uid())
            .unwrap_err()
            .contains("lineage")
    );
}
