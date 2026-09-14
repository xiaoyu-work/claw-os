use super::*;
use crate::activities::*;
use serde_json::json;

fn document() -> ActivityContinuityDocument {
    ActivityContinuityDocument::build(
        ActivityContinuityLineage {
            id: "00000000-0000-4000-8000-000000000123".into(),
            revision: 7,
        },
        PortableActivityIntent {
            title: "Release".into(),
            goal: "Publish the release".into(),
            completion_criteria: "Reviewed and available".into(),
            boundaries: "Ask before publishing".into(),
        },
        vec![PortableActivityReference {
            label: "Status".into(),
            reference: "app://kv/entry?id=release.status&revision=v1".into(),
        }],
        PortableActivityRules {
            execution_limits: Some(PortableExecutionLimits {
                enabled: false,
                max_attempts: 10,
                max_turns_per_attempt: 5,
                expires_at: "2030-01-01T00:00:00.000000000Z".into(),
            }),
            scheduling: Some(PortableSchedulingPreference {
                priority: ActivitySchedulingPriority::Foreground,
            }),
        },
    )
    .unwrap()
}

#[test]
fn v1_has_an_exact_deterministic_shape_and_round_trips() {
    let document = document();
    let json = String::from_utf8(document.to_json().unwrap()).unwrap();
    let expected = format!(
        concat!(
            r#"{{"kind":"claw_os.activity_continuity","schema_version":1,"lineage":"#,
            r#"{{"id":"00000000-0000-4000-8000-000000000123","revision":7}},"#,
            r#""snapshot":"{}","intent":{{"title":"Release","goal":"Publish the release","#,
            r#""completion_criteria":"Reviewed and available","boundaries":"Ask before publishing"}},"#,
            r#""references":[{{"label":"Status","reference":"app://kv/entry?id=release.status&revision=v1"}}],"#,
            r#""rules":{{"execution_limits":{{"enabled":false,"max_attempts":10,"#,
            r#""max_turns_per_attempt":5,"expires_at":"2030-01-01T00:00:00.000000000Z"}},"#,
            r#""scheduling":{{"priority":"foreground"}}}}}}"#
        ),
        document.snapshot
    );
    assert_eq!(json, expected);
    assert_eq!(
        ActivityContinuityDocument::from_json(json.as_bytes()).unwrap(),
        document
    );
    assert_eq!(document.to_json().unwrap(), document.to_json().unwrap());
}

#[test]
fn kind_version_snapshot_unknown_and_duplicate_fields_fail_closed() {
    let valid = document();
    for (field, value) in [
        ("kind", json!("other")),
        ("schema_version", json!(2)),
        ("snapshot", json!(format!("sha256:{}", "0".repeat(64)))),
    ] {
        let mut changed = serde_json::to_value(&valid).unwrap();
        changed[field] = value;
        assert!(
            ActivityContinuityDocument::from_json(
                serde_json::to_string(&changed).unwrap().as_bytes()
            )
            .is_err(),
            "{field}"
        );
    }
    let mut future = serde_json::to_value(&valid).unwrap();
    future["schema_version"] = json!(2);
    assert!(matches!(
        ActivityContinuityDocument::from_json(
            serde_json::to_string(&future).unwrap().as_bytes()
        ),
        Err(ActivityError::Invalid(message))
            if message.contains("unsupported Activity continuity schema version")
    ));
    let mut unknown = serde_json::to_value(&valid).unwrap();
    unknown["authority"] = json!({"grant":"secret"});
    assert!(ActivityContinuityDocument::from_json(
        serde_json::to_string(&unknown).unwrap().as_bytes()
    )
    .is_err());

    let json = String::from_utf8(valid.to_json().unwrap()).unwrap();
    let duplicate = json.replacen(
        r#""kind":"claw_os.activity_continuity""#,
        r#""kind":"claw_os.activity_continuity","kind":"claw_os.activity_continuity""#,
        1,
    );
    assert!(ActivityContinuityDocument::from_json(duplicate.as_bytes()).is_err());
}

#[test]
fn authority_execution_money_history_and_machine_identity_fields_are_impossible() {
    let forbidden = [
        "credentials",
        "secrets",
        "capabilities",
        "capability_policy",
        "grants",
        "approvals",
        "consent",
        "owner_uid",
        "monetary_budget",
        "ledger",
        "reservations",
        "spend",
        "rates",
        "job_ids",
        "session_ids",
        "call_ids",
        "turn_ids",
        "worker_ids",
        "notifications",
        "audit",
        "journal",
        "object_state",
        "receipts",
        "effects",
        "results",
        "execution_reservations",
        "executed_at",
        "completed",
        "completion_note",
        "local_path",
    ];
    for field in forbidden {
        let mut value = serde_json::to_value(document()).unwrap();
        value[field] = json!("forbidden");
        let data = serde_json::to_vec(&value).unwrap();
        assert!(
            ActivityContinuityDocument::from_json(&data).is_err(),
            "{field}"
        );
    }
}

#[test]
fn semantic_references_are_canonical_and_bounded() {
    let mut valid = document();
    valid.references[0].reference = "APP://kv/entry?id=key".into();
    assert!(valid.validate().is_err());

    let mut local = document();
    local.references[0].reference = "/home/user/private.txt".into();
    assert!(local.validate().is_err());

    assert!(
        ActivityContinuityDocument::from_json(&vec![b' '; MAX_CONTINUITY_DOCUMENT_BYTES + 1])
            .is_err()
    );
    let deep = format!(
        "{}{}{}",
        "[".repeat(MAX_JSON_DEPTH + 1),
        "0",
        "]".repeat(MAX_JSON_DEPTH + 1)
    );
    assert!(ActivityContinuityDocument::from_json(deep.as_bytes()).is_err());
}

#[test]
fn lineage_and_rules_reject_noncanonical_or_unbounded_values() {
    let mut value = document();
    value.lineage.id = "AAAAAAAA-0000-4000-8000-000000000123".into();
    assert!(value.validate().is_err());
    let mut value = document();
    value.lineage.revision = 0;
    assert!(value.validate().is_err());
    let mut value = document();
    value.rules.execution_limits.as_mut().unwrap().max_attempts = 1001;
    assert!(value.validate().is_err());
    let mut value = document();
    value.rules.execution_limits.as_mut().unwrap().expires_at = "2030-01-01T00:00:00Z".into();
    assert!(value.validate().is_err());
}

struct LegacyCompatibleService(SqliteActivityService);

impl ActivityService for LegacyCompatibleService {
    fn create(&self, owner_uid: u32, draft: ActivityDraft) -> Result<Activity, ActivityError> {
        self.0.create(owner_uid, draft)
    }

    fn get(&self, owner_uid: u32, id: &str) -> Result<Activity, ActivityError> {
        self.0.get(owner_uid, id)
    }

    fn list(
        &self,
        owner_uid: u32,
        state: Option<ActivityState>,
        limit: usize,
    ) -> Result<Vec<Activity>, ActivityError> {
        self.0.list(owner_uid, state, limit)
    }

    fn update(
        &self,
        owner_uid: u32,
        id: &str,
        patch: ActivityPatch,
    ) -> Result<Activity, ActivityError> {
        self.0.update(owner_uid, id, patch)
    }

    fn add_resource(
        &self,
        owner_uid: u32,
        id: &str,
        resource: ActivityResource,
    ) -> Result<Activity, ActivityError> {
        self.0.add_resource(owner_uid, id, resource)
    }

    fn transition(
        &self,
        owner_uid: u32,
        id: &str,
        state: ActivityState,
        completion_note: Option<String>,
    ) -> Result<Activity, ActivityError> {
        self.0.transition(owner_uid, id, state, completion_note)
    }

    fn record_receipt(
        &self,
        owner_uid: u32,
        activity_id: &str,
        report: ReceiptReport,
        declaration: Option<ReceiptDeclaration>,
        declaration_error: Option<String>,
    ) -> Result<ActivityReceipt, ActivityError> {
        self.0.record_receipt(
            owner_uid,
            activity_id,
            report,
            declaration,
            declaration_error,
        )
    }

    fn receipts(
        &self,
        owner_uid: u32,
        activity_id: &str,
        limit: usize,
    ) -> Result<Vec<ActivityReceipt>, ActivityError> {
        self.0.receipts(owner_uid, activity_id, limit)
    }

    fn record_object_state(
        &self,
        owner_uid: u32,
        activity_id: &str,
        draft: ObjectStateDraft,
    ) -> Result<ObjectStateEntry, ActivityError> {
        self.0.record_object_state(owner_uid, activity_id, draft)
    }

    fn object_state(
        &self,
        owner_uid: u32,
        activity_id: &str,
        reference: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ObjectStateEntry>, ActivityError> {
        self.0
            .object_state(owner_uid, activity_id, reference, limit)
    }

    fn execution_limits(
        &self,
        owner_uid: u32,
        activity_id: &str,
    ) -> Result<Option<ActivityExecutionLimits>, ActivityError> {
        self.0.execution_limits(owner_uid, activity_id)
    }

    fn set_execution_limits(
        &self,
        owner_uid: u32,
        activity_id: &str,
        expected_revision: Option<u64>,
        draft: ExecutionLimitsDraft,
    ) -> Result<ActivityExecutionLimits, ActivityError> {
        self.0
            .set_execution_limits(owner_uid, activity_id, expected_revision, draft)
    }

    fn set_execution_limits_enabled(
        &self,
        owner_uid: u32,
        activity_id: &str,
        expected_revision: u64,
        enabled: bool,
    ) -> Result<ActivityExecutionLimits, ActivityError> {
        self.0
            .set_execution_limits_enabled(owner_uid, activity_id, expected_revision, enabled)
    }

    fn reserve_execution(
        &self,
        owner_uid: u32,
        activity_id: &str,
        attempt_id: &str,
        job_id: &str,
        requested_max_turns: Option<u32>,
    ) -> Result<Option<ExecutionReservation>, ActivityError> {
        self.0.reserve_execution(
            owner_uid,
            activity_id,
            attempt_id,
            job_id,
            requested_max_turns,
        )
    }

    fn capability_policy(
        &self,
        owner_uid: u32,
        activity_id: &str,
    ) -> Result<Option<ActivityCapabilityPolicy>, ActivityError> {
        self.0.capability_policy(owner_uid, activity_id)
    }

    fn set_capability_policy(
        &self,
        owner_uid: u32,
        activity_id: &str,
        expected_revision: Option<u64>,
        draft: CapabilityPolicyDraft,
    ) -> Result<ActivityCapabilityPolicy, ActivityError> {
        self.0
            .set_capability_policy(owner_uid, activity_id, expected_revision, draft)
    }

    fn set_capability_policy_enabled(
        &self,
        owner_uid: u32,
        activity_id: &str,
        expected_revision: u64,
        enabled: bool,
    ) -> Result<ActivityCapabilityPolicy, ActivityError> {
        self.0
            .set_capability_policy_enabled(owner_uid, activity_id, expected_revision, enabled)
    }
}

#[test]
fn continuity_trait_additions_have_compatibility_defaults() {
    let service = LegacyCompatibleService(SqliteActivityService::open_in_memory().unwrap());
    let activity = service
        .create(
            7,
            ActivityDraft {
                title: "Legacy".into(),
                goal: "Keep compiling".into(),
                completion_criteria: String::new(),
                boundaries: String::new(),
                resources: vec![],
            },
        )
        .unwrap();
    assert!(matches!(
        service.export_continuity(7, &activity.id),
        Err(ActivityError::Invalid(message)) if message.contains("unsupported")
    ));
    assert!(matches!(
        service.import_continuity(7, ActivityExecutionPlacement::Local, document()),
        Err(ActivityError::Invalid(message)) if message.contains("unsupported")
    ));
}
