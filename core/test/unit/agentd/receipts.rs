use super::*;
use crate::activities::{
    self, Activity, ActivityDraft, ActivityService, ActivityState, ReceiptSource,
};
use crate::test_env::TestEnvVarGuard;

fn fixture(data: &std::path::Path) -> (Store, Activity, Job) {
    let service = activities::open_default().unwrap();
    let activity = service.create(1000, draft()).unwrap();
    let store = Store::with_root(data.join("jobs")).unwrap();
    let job = store
        .submit_with_activity(
            "Read the resource".into(),
            None,
            None,
            None,
            None,
            true,
            Some(1000),
            None,
            Some(activity.id.clone()),
        )
        .unwrap();
    (store, activity, job)
}

fn draft() -> ActivityDraft {
    serde_json::from_value(serde_json::json!({"title":"Goal","goal":"Read a resource"})).unwrap()
}

fn report() -> ReceiptReport {
    crate::operations::receipts::capture(
        uuid::Uuid::new_v4().to_string(),
        "demo".into(),
        "read".into(),
        format!("sha256:{}", "a".repeat(64)),
        Ok(Some(r#"{"value":"ready"}"#.into())),
    )
}

#[test]
fn task_receipts_use_the_broker_association_and_keep_caller_reported_provenance() {
    let _lock = crate::test_env::lock_env();
    let data = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", data.path());
    let _apps = TestEnvVarGuard::set("COS_APPS_DIR", data.path().join("no-apps"));
    let (store, activity, job) = fixture(data.path());
    let report = report();
    let mut used = 0;
    let expected = ReceiptReply::Recorded {
        receipt_id: report.id.clone(),
    };
    assert_eq!(
        record(&mut used, &store, 1000, &job.id, &job, report.clone()),
        expected
    );
    assert_eq!(
        record(&mut used, &store, 1000, &job.id, &job, report),
        expected
    );
    let service = activities::open_default().unwrap();
    let receipts = service.receipts(1000, &activity.id, 100).unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].source, ReceiptSource::CallerReported);
    assert!(receipts[0].declaration.is_none());
    assert!(receipts[0].declaration_error.is_some());
    assert_eq!(
        service.get(1000, &activity.id).unwrap().state,
        ActivityState::Active
    );
    let (_, events) = store.read_stream_events(&job.id, 0).unwrap();
    let links: Vec<_> = events
        .iter()
        .filter(|event| event["progress"]["kind"] == "activity_receipt")
        .collect();
    assert_eq!(links.len(), 2);
    for event in links {
        assert_eq!(event["progress"]["activity_id"], activity.id);
        assert_eq!(event["progress"]["receipt_id"], receipts[0].id);
        assert_eq!(event["progress"]["source"], "caller_reported");
        assert!(!event.to_string().contains("ready"));
    }
}

#[test]
fn missing_foreign_or_mismatched_associations_never_select_another_activity() {
    let _lock = crate::test_env::lock_env();
    let data = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", data.path());
    let _apps = TestEnvVarGuard::set("COS_APPS_DIR", data.path().join("no-apps"));
    let (store, activity, job) = fixture(data.path());
    let service = activities::open_default().unwrap();
    let foreign = service.create(2000, draft()).unwrap();
    let mut no_activity = job.clone();
    no_activity.activity_id = None;
    let mut foreign_activity = job.clone();
    foreign_activity.activity_id = Some(foreign.id.clone());
    for (owner, task, job) in [
        (0, job.id.as_str(), &job),
        (2000, job.id.as_str(), &job),
        (1000, "other-task", &job),
        (1000, job.id.as_str(), &no_activity),
        (1000, job.id.as_str(), &foreign_activity),
    ] {
        assert!(matches!(
            record(&mut 0, &store, owner, task, job, report()),
            ReceiptReply::Refused { .. }
        ));
    }
    assert!(service
        .receipts(1000, &activity.id, 100)
        .unwrap()
        .is_empty());
    assert!(service.receipts(2000, &foreign.id, 100).unwrap().is_empty());
}

#[test]
fn late_receipts_do_not_reopen_an_activity_and_reporting_is_bounded() {
    let _lock = crate::test_env::lock_env();
    let data = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", data.path());
    let _apps = TestEnvVarGuard::set("COS_APPS_DIR", data.path().join("no-apps"));
    let (store, activity, job) = fixture(data.path());
    let service = activities::open_default().unwrap();
    service
        .transition(1000, &activity.id, ActivityState::Cancelled, None)
        .unwrap();
    let mut used = protocol::MAX_RECEIPT_REPORTS - 1;
    assert!(matches!(
        record(&mut used, &store, 1000, &job.id, &job, report()),
        ReceiptReply::Recorded { .. }
    ));
    assert!(matches!(
        record(&mut used, &store, 1000, &job.id, &job, report()),
        ReceiptReply::Refused { .. }
    ));
    assert_eq!(service.receipts(1000, &activity.id, 100).unwrap().len(), 1);
    assert_eq!(
        service.get(1000, &activity.id).unwrap().state,
        ActivityState::Cancelled
    );
}

#[test]
fn a_task_link_failure_reports_partial_persistence_and_allows_record_only_retry() {
    let _lock = crate::test_env::lock_env();
    let data = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", data.path());
    let _apps = TestEnvVarGuard::set("COS_APPS_DIR", data.path().join("no-apps"));
    let (store, activity, job) = fixture(data.path());
    let stream = store
        .root()
        .join("streams")
        .join(format!("{}.jsonl", job.id));
    store
        .append_stream_progress(&job.id, serde_json::json!({"kind":"fixture"}))
        .unwrap();
    let backup = stream.with_extension("saved");
    std::fs::rename(&stream, &backup).unwrap();
    std::fs::create_dir(&stream).unwrap();
    let report = report();
    let mut used = 0;
    let reply = record(&mut used, &store, 1000, &job.id, &job, report.clone());
    let ReceiptReply::Refused { message } = reply else {
        panic!("must report the missing task link")
    };
    assert!(message.contains("was stored"), "{message}");
    assert_eq!(
        activities::open_default()
            .unwrap()
            .receipts(1000, &activity.id, 100)
            .unwrap()
            .len(),
        1
    );
    std::fs::remove_dir(&stream).unwrap();
    std::fs::rename(backup, stream).unwrap();
    assert_eq!(
        record(&mut used, &store, 1000, &job.id, &job, report.clone()),
        ReceiptReply::Recorded {
            receipt_id: report.id
        },
    );
    assert_eq!(
        activities::open_default()
            .unwrap()
            .receipts(1000, &activity.id, 100)
            .unwrap()
            .len(),
        1
    );
}
