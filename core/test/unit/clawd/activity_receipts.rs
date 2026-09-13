use super::*;
use crate::activities::{ReceiptOutcome, ReceiptSource};
use crate::test_env::TestEnvVarGuard;

fn peer(uid: u32) -> ClientIdentity {
    ClientIdentity {
        pid: Some(std::process::id()),
        uid: Some(uid),
        gid: Some(uid),
        start_time_ticks: Some(1),
        ..ClientIdentity::unknown()
    }
}

fn app(root: &std::path::Path) -> crate::apps::App {
    let dir = root.join("demo");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("app.json"), json!({
        "schema_version":2,"id":"demo","version":"1","name":{"en":"Demo"},
        "operations":{"get":{"label":{"en":"Get"},"args":[{"name":"key","kind":"name","required":true}],
        "effects":[{"kind":"read","label":{"en":"Read key"},"target_arg":"key","recovery":"not_applicable"}]}},
        "mcp":{"entry":"server.py","tools":[{"name":"demo.get","summary":{"en":"Read through the service"}}]}
    }).to_string()).unwrap();
    std::fs::write(
        dir.join("main.py"),
        "raise AssertionError('recording never executes')\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("server.py"),
        "raise AssertionError('recording never starts a session')\n",
    )
    .unwrap();
    crate::test_env::sign_test_package(&dir, crate::provenance::PackageKind::App, "demo");
    crate::apps::find_verified(root, "demo").unwrap()
}

#[test]
fn receipt_recording_derives_source_and_owner_without_claiming_execution() {
    let _lock = crate::test_env::lock_env();
    let data = tempfile::tempdir().unwrap();
    let apps = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", data.path());
    let _apps = TestEnvVarGuard::set("COS_APPS_DIR", apps.path());
    let app = app(apps.path());
    let activity =
        super::super::activities::create(json!({"title":"Goal","goal":"Read key"}), &peer(1000))
            .unwrap();
    let report = crate::operations::receipts::capture(
        uuid::Uuid::new_v4().to_string(),
        "demo".into(),
        "get".into(),
        app.require_verified().unwrap().content_digest().into(),
        Ok(Some(r#"{"value":"ready"}"#.into())),
    );
    let request = json!({"id":activity["id"],"report":report});
    let saved = record(request.clone(), &peer(1000)).unwrap();
    assert_eq!(saved["source"], "caller_reported");
    assert_eq!(saved["owner_uid"], 1000);
    assert_eq!(saved["report"]["outcome"], "returned");
    assert!(saved["declaration_error"].is_null());
    assert_eq!(saved["declaration"]["effects"][0]["kind"], "read");
    assert_eq!(record(request.clone(), &peer(1000)).unwrap(), saved);
    assert!(record(request.clone(), &peer(0)).is_err());
    assert!(record(request, &peer(2000)).is_err());
    let listed = list(json!({"id":activity["id"]}), &peer(1000)).unwrap();
    assert_eq!(listed["receipts"], json!([saved]));
    assert!(list(json!({"id":activity["id"]}), &peer(0)).is_err());
    let current = activities::open_default()
        .unwrap()
        .get(1000, activity["id"].as_str().unwrap())
        .unwrap();
    assert_eq!(current.state, activities::ActivityState::Active);
    assert!(!crate::paths::agent_jobs_dir().exists());
}

#[test]
fn receipt_keeps_unmatched_reports_and_redacts_text_without_authenticating_them() {
    let _lock = crate::test_env::lock_env();
    let data = tempfile::tempdir().unwrap();
    let apps = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", data.path());
    let _apps = TestEnvVarGuard::set("COS_APPS_DIR", apps.path());
    let _app = app(apps.path());
    let activity =
        super::super::activities::create(json!({"title":"Goal","goal":"Read key"}), &peer(1000))
            .unwrap();
    let token = format!("ghp_{}", "a".repeat(36));
    let report = ReceiptReport {
        id: uuid::Uuid::new_v4().to_string(),
        app_id: "demo".into(),
        operation: "get".into(),
        package_digest: format!("sha256:{}", "b".repeat(64)),
        outcome: ReceiptOutcome::Indeterminate,
        result: None,
        error: Some(token.clone()),
    };
    let saved = record(json!({"id":activity["id"],"report":report}), &peer(1000)).unwrap();
    assert!(saved["declaration"].is_null());
    assert!(!saved["declaration_error"].as_str().unwrap().is_empty());
    assert!(!saved.to_string().contains(&token));
    assert_eq!(
        saved["source"],
        serde_json::to_value(ReceiptSource::CallerReported).unwrap()
    );
}

#[test]
fn session_declarations_never_borrow_effects_from_a_same_named_operation() {
    let _lock = crate::test_env::lock_env();
    let apps = tempfile::tempdir().unwrap();
    let _apps = TestEnvVarGuard::set("COS_APPS_DIR", apps.path());
    let app = app(apps.path());
    let mut report = crate::operations::receipts::capture_session(
        "demo",
        "demo.get",
        app.require_verified().unwrap().content_digest(),
        Ok(("value".into(), false)),
    );
    let metadata = declaration(&report).unwrap();
    assert_eq!(metadata.operation_label, "Read through the service");
    assert!(metadata.effects.is_empty());
    report.operation = "get".into();
    let metadata = declaration(&report).unwrap();
    assert_eq!(metadata.operation_label, "Get");
    assert_eq!(metadata.effects.len(), 1);
    report.operation = ReceiptReport::session_operation("missing");
    assert!(declaration(&report).unwrap_err().contains("session tool"));
    report.operation = ReceiptReport::session_operation("demo.get");
    report.package_digest = format!("sha256:{}", "b".repeat(64));
    assert!(declaration(&report).unwrap_err().contains("differs"));
}

#[test]
fn app_service_receipt_keeps_the_original_result_and_shared_owner_ledger() {
    use crate::agent::tools::mcp::protocol::{CallToolResult, ContentItem};
    let _lock = crate::test_env::lock_env();
    let data = tempfile::tempdir().unwrap();
    let apps = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", data.path());
    let _apps = TestEnvVarGuard::set("COS_APPS_DIR", apps.path());
    let app = app(apps.path());
    let activity = super::super::activities::create(
        json!({"title":"Service result","goal":"Keep an App report"}),
        &peer(1000),
    )
    .unwrap();
    let id = activity["id"].as_str().unwrap();
    let output = CallToolResult {
        content: vec![ContentItem::Text {
            text: r#"{"value":"ready"}"#.to_string(),
        }],
        is_error: Some(false),
    };
    let returned = record_service_result(
        1000,
        id,
        "demo",
        "demo.get",
        app.require_verified().unwrap().content_digest(),
        None,
        Ok(output.clone()),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(returned).unwrap(),
        serde_json::to_value(output).unwrap()
    );
    let receipts = activities::open_default()
        .unwrap()
        .receipts(1000, id, 100)
        .unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].report.operation, "session:demo.get");
    assert_eq!(receipts[0].report.outcome, ReceiptOutcome::Returned);
    assert_eq!(receipts[0].source, ReceiptSource::CallerReported);
    assert_eq!(receipts[0].report.result.as_ref().unwrap().bytes, 17);
    assert_eq!(
        activities::open_default()
            .unwrap()
            .get(1000, id)
            .unwrap()
            .state,
        activities::ActivityState::Active
    );
}

#[test]
fn app_service_recording_failure_returns_original_data_and_only_a_recording_retry() {
    use crate::agent::tools::mcp::protocol::{CallToolResult, ContentItem};
    let _lock = crate::test_env::lock_env();
    let data = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", data.path());
    let missing = uuid::Uuid::new_v4().to_string();
    let digest = format!("sha256:{}", "a".repeat(64));
    let output = record_service_result(
        1000,
        &missing,
        "demo",
        "demo.get",
        &digest,
        None,
        Ok(CallToolResult {
            content: vec![ContentItem::Text {
                text: "original App result".to_string(),
            }],
            is_error: None,
        }),
    )
    .unwrap();
    assert_eq!(output.is_error, Some(true));
    assert!(
        matches!(&output.content[0], ContentItem::Text { text } if text == "original App result")
    );
    assert!(
        matches!(&output.content[1], ContentItem::Text { text } if text.contains("Do not repeat")
        && text.contains("record-receipt") && text.contains("\"id\":"))
    );
    let error = record_service_result(
        1000,
        &missing,
        "demo",
        "demo.get",
        &digest,
        None,
        Err(BrokerError::indeterminate("original transport uncertainty")),
    )
    .unwrap_err();
    assert_eq!(
        error.kind,
        super::super::protocol::BrokerErrorKind::Indeterminate
    );
    assert!(error.message.starts_with("original transport uncertainty"));
    assert!(error.message.contains("Do not repeat"));
}

#[test]
fn task_receipt_links_require_the_stored_owner_and_activity_even_after_cancellation() {
    let _lock = crate::test_env::lock_env();
    let data = tempfile::tempdir().unwrap();
    let apps = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", data.path());
    let _apps = TestEnvVarGuard::set("COS_APPS_DIR", apps.path());
    let app = app(apps.path());
    let store = crate::agent::service::Store::with_root(data.path().join("jobs")).unwrap();
    let mut jobs = Vec::new();
    for owner in [1000, 1000, 2000] {
        let activity = super::super::activities::create(
            json!({"title":"Task report","goal":"Keep exact ownership"}),
            &peer(owner),
        )
        .unwrap();
        jobs.push(
            store
                .submit_with_activity(
                    "work".into(),
                    None,
                    None,
                    None,
                    None,
                    true,
                    Some(owner),
                    None,
                    Some(activity["id"].as_str().unwrap().to_string()),
                )
                .unwrap(),
        );
    }
    let receipt = record_for_owner(
        1000,
        jobs[0].activity_id.as_deref().unwrap(),
        crate::operations::receipts::capture_session(
            "demo",
            "demo.get",
            app.require_verified().unwrap().content_digest(),
            Ok(("returned once".into(), false)),
        ),
    )
    .unwrap();
    for job in &jobs[1..] {
        assert!(link_to_task(&store, &job.id, &receipt).is_err());
        assert!(store.read_stream_events(&job.id, 0).unwrap().1.is_empty());
    }
    let missing = uuid::Uuid::new_v4().to_string();
    assert!(link_to_task(&store, &missing, &receipt).is_err());
    assert!(store.read_stream_events(&missing, 0).unwrap().1.is_empty());
    store.cancel_pending(&jobs[0].id).unwrap().unwrap();
    link_to_task(&store, &jobs[0].id, &receipt).unwrap();
    let events = store.read_stream_events(&jobs[0].id, 0).unwrap().1;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["progress"]["receipt_id"], receipt.id);
    assert_eq!(events[0]["progress"]["activity_id"], receipt.activity_id);
}
