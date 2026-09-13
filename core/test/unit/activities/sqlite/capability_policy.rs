use super::super::tests::{
    draft as activity_draft, receipt_report, unverified_receipt, TestDirectory,
};
use super::*;
use crate::activities::{
    ActivityResource, ActivityService, CapabilityBoundaryDecision, CapabilityRuleMode,
    ExecutionLimitsDraft, ObjectStateContent, ObjectStateDraft, DATABASE_SCHEMA_VERSION,
    SCHEMA_VERSION,
};
use crate::caps::{Cap, Scope, Verb};
use std::path::Path;
use std::sync::{Arc, Barrier};

fn draft() -> CapabilityPolicyDraft {
    CapabilityPolicyDraft {
        rules: vec![
            ActivityCapabilityRule {
                verb: Verb::FS_WRITE,
                mode: CapabilityRuleMode::RequireApproval,
                scopes: vec![Scope::path("/drafts/**")],
            },
            ActivityCapabilityRule {
                verb: Verb::FS_DELETE,
                mode: CapabilityRuleMode::Deny,
                scopes: vec![],
            },
        ],
    }
}

fn configured(service: &dyn ActivityService, owner: u32) -> (Activity, ActivityCapabilityPolicy) {
    let activity = service.create(owner, activity_draft()).unwrap();
    let policy = service
        .set_capability_policy(owner, &activity.id, None, draft())
        .unwrap();
    (activity, policy)
}

fn rows(conn: &Connection, table: &str) -> Vec<Vec<rusqlite::types::Value>> {
    let mut statement = conn
        .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
        .unwrap();
    let count = statement.column_count();
    let mapped = statement
        .query_map([], |row| (0..count).map(|column| row.get(column)).collect())
        .unwrap();
    mapped.collect::<Result<Vec<_>, _>>().unwrap()
}

fn exists(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name = ?1)",
        [name],
        |row| row.get(0),
    )
    .unwrap()
}

#[test]
fn capability_policy_absence_is_legacy_metadata_not_an_implicit_grant_or_created_policy() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    for state in [
        ActivityState::Active,
        ActivityState::Paused,
        ActivityState::Completed,
        ActivityState::Cancelled,
    ] {
        let mut activity = service.create(7, activity_draft()).unwrap();
        if state != ActivityState::Active {
            activity = service
                .transition(
                    7,
                    &activity.id,
                    state,
                    (state == ActivityState::Completed).then(|| "Confirmed".into()),
                )
                .unwrap();
        }
        assert!(service
            .capability_policy(7, &activity.id)
            .unwrap()
            .is_none());
        assert!(matches!(
            service.set_capability_policy_enabled(7, &activity.id, 1, false),
            Err(ActivityError::NotFound)
        ));
        assert_eq!(service.get(7, &activity.id).unwrap(), activity);
    }
    assert!(rows(&service.lock().unwrap(), "activity_capability_policies").is_empty());
}

#[test]
fn policies_persist_canonical_rules_and_the_exact_server_owned_fields_across_reopen() {
    let directory = TestDirectory::new();
    let (activity, policy) = {
        let provider = SqliteActivityService::open(directory.database()).unwrap();
        let service: &dyn ActivityService = &provider;
        let (activity, policy) = configured(service, 7);
        assert_eq!(policy.activity_id, activity.id);
        assert_eq!(policy.owner_uid, 7);
        assert_eq!(policy.revision, 1);
        assert!(policy.enabled);
        assert_eq!(policy.created_at, policy.updated_at);
        parse_timestamp(&policy.created_at).unwrap();
        assert_eq!(policy.rules, draft().canonicalized().unwrap().rules);
        assert_eq!(service.get(7, &activity.id).unwrap(), activity);
        (activity, policy)
    };
    let service = SqliteActivityService::open(directory.database()).unwrap();
    assert_eq!(
        service
            .capability_policy(7, &activity.id.to_uppercase())
            .unwrap(),
        Some(policy.clone())
    );
    assert_eq!(
        policy.decision(&Cap::new(Verb::FS_WRITE, Scope::path("/drafts/file"))),
        CapabilityBoundaryDecision::RequireApproval
    );
    assert_eq!(
        policy.decision(&Cap::new(Verb::FS_WRITE, Scope::path("/outside/file"))),
        CapabilityBoundaryDecision::Deny
    );
    assert_eq!(service.get(7, &activity.id).unwrap(), activity);
}

#[test]
fn policy_operations_are_owner_scoped_with_no_root_exemption() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let owners = [0, 7, 8, u32::MAX];
    let records: Vec<_> = owners
        .iter()
        .map(|owner| configured(&service, *owner))
        .collect();
    let missing = uuid::Uuid::new_v4().to_string();
    for (activity, policy) in &records {
        assert_eq!(
            service
                .capability_policy(activity.owner_uid, &activity.id)
                .unwrap(),
            Some(policy.clone())
        );
        for owner in owners
            .into_iter()
            .filter(|owner| *owner != activity.owner_uid)
        {
            for id in [&activity.id, &missing] {
                assert!(matches!(
                    service.capability_policy(owner, id),
                    Err(ActivityError::NotFound)
                ));
                assert!(matches!(
                    service.set_capability_policy(owner, id, None, draft()),
                    Err(ActivityError::NotFound)
                ));
                assert!(matches!(
                    service.set_capability_policy_enabled(owner, id, 1, false),
                    Err(ActivityError::NotFound)
                ));
            }
        }
    }
}

#[test]
fn canonical_activity_ids_and_input_validation_precede_policy_mutation() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let activity = service.create(7, activity_draft()).unwrap();
    let id = uuid::Uuid::parse_str(&activity.id).unwrap();
    let first = service
        .set_capability_policy(7, &id.urn().to_string(), None, draft())
        .unwrap();
    assert_eq!(first.activity_id, activity.id);
    assert_eq!(
        service
            .capability_policy(7, &id.simple().to_string())
            .unwrap(),
        Some(first.clone())
    );
    for invalid in ["", "not-a-uuid", "../db", "uuid\0"] {
        assert!(matches!(
            service.capability_policy(7, invalid),
            Err(ActivityError::Invalid(_))
        ));
        assert!(matches!(
            service.set_capability_policy(7, invalid, None, draft()),
            Err(ActivityError::Invalid(_))
        ));
    }
    for revision in [0, u64::MAX] {
        assert!(matches!(
            service.set_capability_policy(7, &activity.id, Some(revision), draft()),
            Err(ActivityError::Invalid(_))
        ));
        assert!(matches!(
            service.set_capability_policy_enabled(7, &activity.id, revision, false),
            Err(ActivityError::Invalid(_))
        ));
    }
    let invalid = CapabilityPolicyDraft {
        rules: vec![ActivityCapabilityRule {
            verb: Verb::FS_WRITE,
            mode: CapabilityRuleMode::Normal,
            scopes: vec![Scope::Wild],
        }],
    };
    assert!(matches!(
        service.set_capability_policy(7, &activity.id, Some(1), invalid),
        Err(ActivityError::Invalid(_))
    ));
    assert_eq!(
        service.capability_policy(7, &activity.id).unwrap(),
        Some(first)
    );
    assert_eq!(service.get(7, &activity.id).unwrap(), activity);
}

#[test]
fn cas_edits_never_implicitly_enable_or_reset_and_empty_rules_remain_an_explicit_policy() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let (activity, first) = configured(&service, 7);
    assert!(matches!(
        service.set_capability_policy(7, &activity.id, None, draft()),
        Err(ActivityError::Conflict(_))
    ));
    let disabled = service
        .set_capability_policy_enabled(7, &activity.id, 1, false)
        .unwrap();
    assert_eq!(disabled.revision, 2);
    assert!(!disabled.enabled);
    assert_eq!(disabled.rules, first.rules);
    for result in [
        service.set_capability_policy(7, &activity.id, Some(1), draft()),
        service.set_capability_policy_enabled(7, &activity.id, 1, true),
    ] {
        assert!(matches!(result, Err(ActivityError::Conflict(_))));
    }
    let empty = service
        .set_capability_policy(
            7,
            &activity.id,
            Some(2),
            CapabilityPolicyDraft { rules: vec![] },
        )
        .unwrap();
    assert_eq!(empty.revision, 3);
    assert!(!empty.enabled);
    assert!(empty.rules.is_empty());
    assert_eq!(empty.created_at, first.created_at);
    assert!(empty.updated_at >= disabled.updated_at);
    assert_eq!(
        empty.decision(&Cap::unscoped(Verb::UI_NOTIFY)),
        CapabilityBoundaryDecision::Deny
    );
    let enabled = service
        .set_capability_policy_enabled(7, &activity.id, 3, true)
        .unwrap();
    assert_eq!(enabled.revision, 4);
    assert!(enabled.enabled);
    assert_eq!(
        enabled.decision(&Cap::unscoped(Verb::UI_NOTIFY)),
        CapabilityBoundaryDecision::Normal
    );
    let repeated = service
        .set_capability_policy_enabled(7, &activity.id, 4, true)
        .unwrap();
    assert_eq!(repeated.revision, 5);
    assert_eq!(repeated.rules, enabled.rules);
    let absent = service.create(7, activity_draft()).unwrap();
    assert!(matches!(
        service.set_capability_policy(7, &absent.id, Some(1), draft()),
        Err(ActivityError::Conflict(_))
    ));
    assert!(service.capability_policy(7, &absent.id).unwrap().is_none());
    assert_eq!(service.get(7, &activity.id).unwrap(), activity);
}

#[test]
fn terminal_activities_can_revoke_but_cannot_configure_or_enable_capability_policy() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    for state in [
        ActivityState::Active,
        ActivityState::Paused,
        ActivityState::Completed,
        ActivityState::Cancelled,
    ] {
        let (mut activity, _) = configured(&service, 7);
        if state != ActivityState::Active {
            activity = service
                .transition(
                    7,
                    &activity.id,
                    state,
                    (state == ActivityState::Completed).then(|| "Explicit completion".into()),
                )
                .unwrap();
        }
        if matches!(state, ActivityState::Completed | ActivityState::Cancelled) {
            assert!(matches!(
                service.set_capability_policy(7, &activity.id, Some(1), draft()),
                Err(ActivityError::Conflict(_))
            ));
            assert!(matches!(
                service.set_capability_policy_enabled(7, &activity.id, 1, true),
                Err(ActivityError::Conflict(_))
            ));
        } else {
            assert_eq!(
                service
                    .set_capability_policy(7, &activity.id, Some(1), draft())
                    .unwrap()
                    .revision,
                2
            );
        }
        let before = service.capability_policy(7, &activity.id).unwrap().unwrap();
        let disabled = service
            .set_capability_policy_enabled(7, &activity.id, before.revision, false)
            .unwrap();
        assert!(!disabled.enabled);
        assert_eq!(disabled.revision, before.revision + 1);
        assert_eq!(
            disabled.decision(&Cap::unscoped(Verb::UI_NOTIFY)),
            CapabilityBoundaryDecision::Deny
        );
        assert_eq!(service.get(7, &activity.id).unwrap(), activity);
    }
    let terminal = service.create(7, activity_draft()).unwrap();
    service
        .transition(7, &terminal.id, ActivityState::Cancelled, None)
        .unwrap();
    assert!(matches!(
        service.set_capability_policy(7, &terminal.id, None, draft()),
        Err(ActivityError::Conflict(_))
    ));
    assert!(service
        .capability_policy(7, &terminal.id)
        .unwrap()
        .is_none());
}

#[test]
fn capability_edits_do_not_touch_execution_policies_reservations_or_activity_metadata() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let activity = service.create(7, activity_draft()).unwrap();
    service
        .set_execution_limits(
            7,
            &activity.id,
            None,
            ExecutionLimitsDraft {
                max_attempts: 3,
                max_turns_per_attempt: 10,
                expires_at: "2099-01-01T00:00:00Z".into(),
            },
        )
        .unwrap();
    let reservation = service
        .reserve_execution(
            7,
            &activity.id,
            &uuid::Uuid::new_v4().to_string(),
            "test-job",
            None,
        )
        .unwrap()
        .unwrap();
    let execution = service.execution_limits(7, &activity.id).unwrap();
    let reservations = rows(&service.lock().unwrap(), "activity_execution_reservations");
    let policy = service
        .set_capability_policy(7, &activity.id, None, draft())
        .unwrap();
    let disabled = service
        .set_capability_policy_enabled(7, &activity.id, policy.revision, false)
        .unwrap();
    service
        .set_capability_policy(
            7,
            &activity.id,
            Some(disabled.revision),
            CapabilityPolicyDraft { rules: vec![] },
        )
        .unwrap();
    assert_eq!(
        service.execution_limits(7, &activity.id).unwrap(),
        execution
    );
    assert_eq!(
        rows(&service.lock().unwrap(), "activity_execution_reservations"),
        reservations
    );
    assert_eq!(
        service
            .reserve_execution(7, &activity.id, &reservation.id, "test-job", None)
            .unwrap(),
        Some(reservation)
    );
    assert_eq!(service.get(7, &activity.id).unwrap(), activity);
}

#[test]
fn concurrent_creates_and_updates_have_one_winner_without_overwriting_cas_state() {
    let directory = TestDirectory::new();
    let first = SqliteActivityService::open(directory.database()).unwrap();
    let second = SqliteActivityService::open(directory.database()).unwrap();
    let activity = first.create(7, activity_draft()).unwrap();
    let id = activity.id.clone();
    let barrier = Arc::new(Barrier::new(2));
    let other_barrier = Arc::clone(&barrier);
    let worker = std::thread::spawn(move || {
        other_barrier.wait();
        second.set_capability_policy(7, &id, None, draft())
    });
    barrier.wait();
    let results = [
        first.set_capability_policy(7, &activity.id, None, draft()),
        worker.join().unwrap(),
    ];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(ActivityError::Conflict(_))))
            .count(),
        1
    );
    assert_eq!(
        first
            .capability_policy(7, &activity.id)
            .unwrap()
            .unwrap()
            .revision,
        1
    );

    let second = SqliteActivityService::open(directory.database()).unwrap();
    let id = activity.id.clone();
    let other_barrier = Arc::clone(&barrier);
    let worker = std::thread::spawn(move || {
        other_barrier.wait();
        second.set_capability_policy_enabled(7, &id, 1, false)
    });
    barrier.wait();
    let results = [
        first.set_capability_policy(
            7,
            &activity.id,
            Some(1),
            CapabilityPolicyDraft { rules: vec![] },
        ),
        worker.join().unwrap(),
    ];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(ActivityError::Conflict(_))))
            .count(),
        1
    );
    assert_eq!(
        first
            .capability_policy(7, &activity.id)
            .unwrap()
            .unwrap()
            .revision,
        2
    );
    assert_eq!(first.get(7, &activity.id).unwrap(), activity);
}

#[test]
fn revision_overflow_is_an_explicit_error_without_enabling_or_resetting_policy() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let (activity, _) = configured(&service, 7);
    service
        .lock()
        .unwrap()
        .execute(
            "UPDATE activity_capability_policies SET revision = ?1, enabled = 0",
            [i64::MAX],
        )
        .unwrap();
    let expected = service.capability_policy(7, &activity.id).unwrap().unwrap();
    assert_eq!(expected.revision, u64::try_from(i64::MAX).unwrap());
    assert!(matches!(
        service.set_capability_policy(7, &activity.id, Some(expected.revision), draft()),
        Err(ActivityError::Conflict(_))
    ));
    assert!(matches!(
        service.set_capability_policy_enabled(7, &activity.id, expected.revision, true),
        Err(ActivityError::Conflict(_))
    ));
    assert_eq!(
        service.capability_policy(7, &activity.id).unwrap(),
        Some(expected)
    );
}

#[test]
fn corrupted_policy_rows_fail_reads_and_writes_without_silent_repair() {
    for change in [
        "revision = 0",
        "revision = -1",
        "enabled = 2",
        "rules_json = 'not JSON'",
        "rules_json = '{}'",
        "created_at = 'not a timestamp'",
        "updated_at = '1900-01-01T00:00:00.000000000Z'",
        "updated_at = '2099-01-01T00:00:00+00:00'",
    ] {
        let service = SqliteActivityService::open_in_memory().unwrap();
        let (activity, _) = configured(&service, 7);
        service
            .lock()
            .unwrap()
            .pragma_update(None, "ignore_check_constraints", true)
            .unwrap();
        service
            .lock()
            .unwrap()
            .execute_batch(&format!("UPDATE activity_capability_policies SET {change}"))
            .unwrap();
        let before = rows(&service.lock().unwrap(), "activity_capability_policies");
        assert!(
            matches!(
                service.capability_policy(7, &activity.id),
                Err(ActivityError::Corrupt(_))
            ),
            "{change}"
        );
        assert!(
            matches!(
                service.set_capability_policy(7, &activity.id, Some(1), draft()),
                Err(ActivityError::Corrupt(_))
            ),
            "{change}"
        );
        assert!(
            matches!(
                service.set_capability_policy_enabled(7, &activity.id, 1, false),
                Err(ActivityError::Corrupt(_))
            ),
            "{change}"
        );
        assert_eq!(
            rows(&service.lock().unwrap(), "activity_capability_policies"),
            before
        );
    }
}

#[test]
fn persisted_rules_must_remain_canonical_bounded_and_strict_even_inside_scope_objects() {
    let invalid_rules = [
        serde_json::json!([{"verb":"not.a.verb","mode":"deny","scopes":[]}]),
        serde_json::json!([{"verb":"fs.write","mode":"normal","scopes":[{"kind":"wild"}]}]),
        serde_json::json!([{"verb":"fs.write","mode":"normal","scopes":[{"kind":"host","value":"example.invalid"}]}]),
        serde_json::json!([{"verb":"fs.write","mode":"normal","scopes":[{"kind":"path","value":"/drafts/\n"}]}]),
        serde_json::json!([{"verb":"fs.write","mode":"deny","scopes":[{"kind":"path","value":"/drafts/**"}]}]),
        serde_json::json!([{"verb":"fs.write","mode":"normal","scopes":[]}]),
        serde_json::json!([{"verb":"fs.write","mode":"normal","scopes":[{"kind":"path","value":"/drafts/**","source":"forged"}]}]),
        serde_json::json!([{"verb":"net.dial","mode":"normal","scopes":[{"kind":"host","value":"EXAMPLE.INVALID"}]}]),
        serde_json::json!([{"verb":"fs.write","mode":"normal","scopes":[{"kind":"path","value":"/z/**"},{"kind":"path","value":"/a/**"}]}]),
        serde_json::json!([{"verb":"fs.delete","mode":"deny","scopes":[]},{"verb":"fs.delete","mode":"deny","scopes":[]}]),
    ];
    for invalid in invalid_rules {
        let service = SqliteActivityService::open_in_memory().unwrap();
        let (activity, _) = configured(&service, 7);
        let text = invalid.to_string();
        service
            .lock()
            .unwrap()
            .execute(
                "UPDATE activity_capability_policies SET rules_json = ?1",
                [&text],
            )
            .unwrap();
        assert!(
            matches!(
                service.capability_policy(7, &activity.id),
                Err(ActivityError::Corrupt(_))
            ),
            "{text}"
        );
        assert!(matches!(
            service.set_capability_policy(7, &activity.id, Some(1), draft()),
            Err(ActivityError::Corrupt(_))
        ));
        assert_eq!(
            service
                .lock()
                .unwrap()
                .query_row(
                    "SELECT rules_json FROM activity_capability_policies",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            text
        );
    }
    let service = SqliteActivityService::open_in_memory().unwrap();
    let (activity, _) = configured(&service, 7);
    service
        .lock()
        .unwrap()
        .pragma_update(None, "ignore_check_constraints", true)
        .unwrap();
    service
        .lock()
        .unwrap()
        .execute(
            "UPDATE activity_capability_policies SET rules_json = ?1",
            [" ".repeat(MAX_POLICY_BYTES + 1)],
        )
        .unwrap();
    assert!(matches!(
        service.capability_policy(7, &activity.id),
        Err(ActivityError::Corrupt(_))
    ));
}

#[test]
fn duplicate_json_fields_are_corruption_not_a_last_value_wins_policy() {
    for text in [
        r#"[{"verb":"fs.write","mode":"deny","mode":"normal","scopes":[{"kind":"path","value":"/drafts/**"}]}]"#,
        r#"[{"verb":"fs.write","mode":"normal","scopes":[{"kind":"path","value":"/private/**","value":"/drafts/**"}]}]"#,
    ] {
        let service = SqliteActivityService::open_in_memory().unwrap();
        let (activity, _) = configured(&service, 7);
        service
            .lock()
            .unwrap()
            .execute(
                "UPDATE activity_capability_policies SET rules_json = ?1",
                [text],
            )
            .unwrap();
        assert!(matches!(
            service.capability_policy(7, &activity.id),
            Err(ActivityError::Corrupt(_))
        ));
        assert!(matches!(
            service.set_capability_policy(7, &activity.id, Some(1), draft()),
            Err(ActivityError::Corrupt(_))
        ));
    }
}

#[test]
fn ignored_rewritten_or_lifecycle_changing_writes_roll_back_instead_of_reporting_success() {
    for trigger in [
        "CREATE TRIGGER ignore_policy BEFORE INSERT ON activity_capability_policies
         BEGIN SELECT RAISE(IGNORE); END;",
        "CREATE TRIGGER rewrite_policy AFTER INSERT ON activity_capability_policies
         BEGIN UPDATE activity_capability_policies SET enabled = 0; END;",
        "CREATE TRIGGER change_goal AFTER INSERT ON activity_capability_policies
         BEGIN UPDATE activities SET goal = 'unexpected mutation' WHERE id = NEW.activity_id; END;",
    ] {
        let service = SqliteActivityService::open_in_memory().unwrap();
        let activity = service.create(7, activity_draft()).unwrap();
        service.lock().unwrap().execute_batch(trigger).unwrap();
        assert!(matches!(
            service.set_capability_policy(7, &activity.id, None, draft()),
            Err(ActivityError::Corrupt(_))
        ));
        assert!(service
            .capability_policy(7, &activity.id)
            .unwrap()
            .is_none());
        assert_eq!(service.get(7, &activity.id).unwrap(), activity);
    }
    let service = SqliteActivityService::open_in_memory().unwrap();
    let (activity, expected) = configured(&service, 7);
    service
        .lock()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER ignore_update BEFORE UPDATE ON activity_capability_policies
         BEGIN SELECT RAISE(IGNORE); END;",
        )
        .unwrap();
    assert!(matches!(
        service.set_capability_policy(7, &activity.id, Some(1), draft()),
        Err(ActivityError::Corrupt(_))
    ));
    assert!(matches!(
        service.set_capability_policy_enabled(7, &activity.id, 1, false),
        Err(ActivityError::Corrupt(_))
    ));
    assert_eq!(
        service.capability_policy(7, &activity.id).unwrap(),
        Some(expected)
    );
}

#[test]
fn storage_errors_and_poisoned_locks_are_not_success_shaped_policy_absence() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let (activity, expected) = configured(&service, 7);
    service
        .lock()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER refuse_update BEFORE UPDATE ON activity_capability_policies
         BEGIN SELECT RAISE(ABORT, 'policy write failed'); END;",
        )
        .unwrap();
    assert!(matches!(
        service.set_capability_policy(7, &activity.id, Some(1), draft()),
        Err(ActivityError::Database(_))
    ));
    assert_eq!(
        service.capability_policy(7, &activity.id).unwrap(),
        Some(expected)
    );
    let clone = service.clone();
    assert!(std::thread::spawn(move || {
        let _guard = clone.conn.lock().unwrap();
        panic!("poison capability policy connection");
    })
    .join()
    .is_err());
    assert!(matches!(
        service.capability_policy(7, &activity.id),
        Err(ActivityError::Poisoned)
    ));
    assert!(matches!(
        service.set_capability_policy(7, &activity.id, Some(1), draft()),
        Err(ActivityError::Poisoned)
    ));
    assert!(matches!(
        service.set_capability_policy_enabled(7, &activity.id, 1, false),
        Err(ActivityError::Poisoned)
    ));
}

const LEGACY_TABLES: &[&str] = &[
    "activities",
    "activity_receipts",
    "activity_object_state",
    "activity_execution_limits",
    "activity_execution_reservations",
];

fn schema_four_database(path: &Path) -> Vec<Activity> {
    let mut activities = Vec::new();
    {
        let service = SqliteActivityService::open(path).unwrap();
        for (owner, state) in [
            (0, ActivityState::Active),
            (7, ActivityState::Paused),
            (7, ActivityState::Completed),
            (8, ActivityState::Cancelled),
        ] {
            let mut input = activity_draft();
            let reference = "app://sample/record?id=legacy";
            input.resources.push(ActivityResource {
                label: "Legacy object".into(),
                reference: reference.into(),
            });
            let mut activity = service.create(owner, input).unwrap();
            let receipt =
                unverified_receipt(&service, owner, &activity.id, receipt_report()).unwrap();
            service
                .record_object_state(
                    owner,
                    &activity.id,
                    ObjectStateDraft {
                        id: uuid::Uuid::new_v4().to_string(),
                        reference: reference.into(),
                        content: ObjectStateContent::AppReport {
                            receipt_id: receipt.id,
                        },
                        observed_at: None,
                        valid_until: None,
                        supersedes: None,
                    },
                )
                .unwrap();
            service
                .set_execution_limits(
                    owner,
                    &activity.id,
                    None,
                    ExecutionLimitsDraft {
                        max_attempts: 3,
                        max_turns_per_attempt: 10,
                        expires_at: "2099-01-01T00:00:00Z".into(),
                    },
                )
                .unwrap();
            service
                .reserve_execution(
                    owner,
                    &activity.id,
                    &uuid::Uuid::new_v4().to_string(),
                    "legacy-job",
                    Some(20),
                )
                .unwrap();
            if state != ActivityState::Active {
                activity = service
                    .transition(
                        owner,
                        &activity.id,
                        state,
                        (state == ActivityState::Completed).then(|| "Legacy confirmation".into()),
                    )
                    .unwrap();
            }
            activities.push(activity);
        }
        assert!(rows(&service.lock().unwrap(), "activity_capability_policies").is_empty());
    }
    let conn = Connection::open(path).unwrap();
    conn.execute_batch("DROP TABLE activity_capability_policies; PRAGMA user_version = 4;")
        .unwrap();
    activities
}

#[test]
fn schema_four_migration_preserves_all_activity_history_and_execution_accounting_fields() {
    let directory = TestDirectory::new();
    let path = directory.database();
    let activities = schema_four_database(&path);
    let before: Vec<_> = {
        let conn = Connection::open(&path).unwrap();
        LEGACY_TABLES
            .iter()
            .map(|table| rows(&conn, table))
            .collect()
    };
    let service = SqliteActivityService::open(&path).unwrap();
    assert_eq!(SCHEMA_VERSION, 1);
    assert_eq!(DATABASE_SCHEMA_VERSION, 5);
    {
        let conn = service.lock().unwrap();
        assert_eq!(
            conn.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            5
        );
        validate_schema(&conn).unwrap();
        for (table, original) in LEGACY_TABLES.iter().zip(&before) {
            assert_eq!(rows(&conn, table), *original);
        }
    }
    for activity in &activities {
        assert_eq!(
            service.get(activity.owner_uid, &activity.id).unwrap(),
            *activity
        );
        assert!(service
            .capability_policy(activity.owner_uid, &activity.id)
            .unwrap()
            .is_none());
        assert_eq!(
            service
                .execution_limits(activity.owner_uid, &activity.id)
                .unwrap()
                .unwrap()
                .used_attempts,
            1
        );
        assert_eq!(
            service
                .receipts(activity.owner_uid, &activity.id, 100)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            service
                .object_state(activity.owner_uid, &activity.id, None, 100)
                .unwrap()
                .len(),
            1
        );
    }
    let active = &activities[0];
    let policy = service
        .set_capability_policy(active.owner_uid, &active.id, None, draft())
        .unwrap();
    drop(service);
    let reopened = SqliteActivityService::open(&path).unwrap();
    assert_eq!(
        reopened
            .capability_policy(active.owner_uid, &active.id)
            .unwrap(),
        Some(policy)
    );
    for (table, original) in LEGACY_TABLES.iter().zip(&before) {
        assert_eq!(rows(&reopened.lock().unwrap(), table), *original);
    }
}

#[test]
fn failed_schema_five_migration_rolls_back_creation_and_preserves_legacy_corruption() {
    for corruption in [
        "UPDATE activities SET title = ' untrimmed '",
        "UPDATE activity_receipts SET report_json = '{}'",
        "UPDATE activity_execution_reservations SET owner_uid = 999",
    ] {
        let directory = TestDirectory::new();
        let path = directory.database();
        schema_four_database(&path);
        let before: Vec<_> = {
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "foreign_keys", false).unwrap();
            conn.execute_batch(corruption).unwrap();
            LEGACY_TABLES
                .iter()
                .map(|table| rows(&conn, table))
                .collect()
        };
        assert!(
            matches!(
                SqliteActivityService::open(&path),
                Err(ActivityError::Corrupt(_))
            ),
            "{corruption}"
        );
        let conn = Connection::open(&path).unwrap();
        assert_eq!(
            conn.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            4
        );
        assert!(!exists(&conn, "activity_capability_policies"));
        for (table, original) in LEGACY_TABLES.iter().zip(&before) {
            assert_eq!(rows(&conn, table), *original);
        }
    }
}

#[test]
fn schema_creation_conflicts_do_not_replace_existing_data_or_advance_the_version() {
    let directory = TestDirectory::new();
    let path = directory.database();
    schema_four_database(&path);
    let before: Vec<_> = {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE activity_capability_policies (sentinel TEXT);
             INSERT INTO activity_capability_policies VALUES ('preserve');",
        )
        .unwrap();
        LEGACY_TABLES
            .iter()
            .map(|table| rows(&conn, table))
            .collect()
    };
    assert!(matches!(
        SqliteActivityService::open(&path),
        Err(ActivityError::Database(_))
    ));
    let conn = Connection::open(&path).unwrap();
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        4
    );
    assert_eq!(
        conn.query_row(
            "SELECT sentinel FROM activity_capability_policies",
            [],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        "preserve"
    );
    for (table, original) in LEGACY_TABLES.iter().zip(&before) {
        assert_eq!(rows(&conn, table), *original);
    }
}

#[test]
fn composite_owner_foreign_keys_reject_cross_owner_rows_and_detect_corruption_on_reopen() {
    let directory = TestDirectory::new();
    let path = directory.database();
    let activity = {
        let service = SqliteActivityService::open(&path).unwrap();
        let (activity, _) = configured(&service, 7);
        assert!(service
            .lock()
            .unwrap()
            .execute("UPDATE activity_capability_policies SET owner_uid = 0", [])
            .is_err());
        activity
    };
    {
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "foreign_keys", false).unwrap();
        conn.execute("UPDATE activity_capability_policies SET owner_uid = 0", [])
            .unwrap();
    }
    assert!(matches!(
        SqliteActivityService::open(&path),
        Err(ActivityError::Corrupt(_))
    ));
    let conn = Connection::open(&path).unwrap();
    assert_eq!(load_activity(&conn, 7, &activity.id).unwrap(), activity);
    assert_eq!(
        conn.query_row(
            "SELECT owner_uid FROM activity_capability_policies",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
}

#[test]
fn schema_five_rejects_missing_owner_identity_or_foreign_key_definitions_even_when_empty() {
    for ddl in [
        MIGRATE_TO_V5.replace("PRIMARY KEY(owner_uid, activity_id),", ""),
        MIGRATE_TO_V5.replace(
            ",\n    FOREIGN KEY(owner_uid, activity_id) REFERENCES activities(owner_uid, id)",
            "",
        ),
    ] {
        let directory = TestDirectory::new();
        let path = directory.database();
        schema_four_database(&path);
        let before: Vec<_> = {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(&ddl).unwrap();
            conn.pragma_update(None, "user_version", 5).unwrap();
            LEGACY_TABLES
                .iter()
                .map(|table| rows(&conn, table))
                .collect()
        };
        assert!(matches!(
            SqliteActivityService::open(&path),
            Err(ActivityError::Corrupt(_))
        ));
        let conn = Connection::open(&path).unwrap();
        for (table, original) in LEGACY_TABLES.iter().zip(&before) {
            assert_eq!(rows(&conn, table), *original);
        }
    }
}
