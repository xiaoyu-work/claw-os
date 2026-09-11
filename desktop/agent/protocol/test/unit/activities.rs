use super::*;
use serde_json::json;

#[test]
fn activity_v1_additions_have_defaults_and_ignore_future_fields() {
    let value = json!({
        "activity": {"id": "a", "title": "Goal", "goal": "A durable goal", "state": "active"},
        "future_field": true
    });
    let detail: ActivityDetailResponse = serde_json::from_value(value).unwrap();
    assert!(detail.jobs.is_empty());
    assert!(detail.sessions.is_empty());
    assert!(detail.pending_approvals.is_empty());
    assert!(detail.approvals_error.is_none());
    assert!(detail.activity.completion_criteria.is_empty());
    assert!(detail.activity.resources.is_empty());
    assert!(detail.activity.completion_note.is_none());
    assert_eq!(
        serde_json::from_value::<ActivityDetailResponse>(serde_json::to_value(&detail).unwrap())
            .unwrap(),
        detail
    );

    let job: ActivityJobView = serde_json::from_value(json!({
        "id": "j", "status": "ok", "future_field": "ignored"
    }))
    .unwrap();
    assert!(job.waiting_on.is_empty());
    assert!(job.response.is_none());
    let resource: ActivityResource = serde_json::from_value(json!({
        "label": "Notes", "reference": "notes.txt", "future_field": "ignored"
    }))
    .unwrap();
    assert_eq!(resource.reference, "notes.txt");
}

#[test]
fn activity_requests_cannot_supply_owner_or_authority() {
    assert!(
        serde_json::from_value::<ActivityCreateRequest>(json!({
            "title": "Goal", "goal": "Do something", "owner_uid": 0
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<ActivityUpdateRequest>(json!({
            "goal": "Do something", "owner_uid": 0
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<ActivityTransitionRequest>(json!({
            "state": "completed", "completion_note": "Confirmed", "owner_uid": 0
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<ActivityRunRequest>(json!({
            "session_id": "s", "owner_uid": 0
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<ActivityRunRequest>(json!({
            "caps": ["fs:write:*"]
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<ActivityListQuery>(json!({
            "owner_uid": 0
        }))
        .is_err()
    );
}

#[test]
fn minimal_requests_keep_defaults_and_explicit_completion_note() {
    let create: ActivityCreateRequest =
        serde_json::from_value(json!({"title": "Goal", "goal": "Do something"})).unwrap();
    assert!(create.resources.is_empty());
    assert_eq!(
        serde_json::to_value(ActivityRunRequest::default()).unwrap(),
        json!({})
    );
    assert_eq!(
        serde_json::to_value(ActivityUpdateRequest::default()).unwrap(),
        json!({})
    );
    let transition = ActivityTransitionRequest {
        state: ActivityState::Completed,
        completion_note: Some("I verified the result".into()),
    };
    assert_eq!(
        serde_json::to_value(transition).unwrap(),
        json!({"state": "completed", "completion_note": "I verified the result"})
    );
    assert!(serde_json::from_value::<ActivityState>(json!("success")).is_err());
}

#[test]
fn job_acknowledgement_has_no_activity_lifecycle_or_worker_state() {
    let work: ActivityWorkResponse = serde_json::from_value(json!({
        "id": "job", "status": "ok", "owner_uid": 1000,
        "worker_pid": 100, "state": "completed"
    }))
    .unwrap();
    let value = serde_json::to_value(work).unwrap();
    assert!(value.get("state").is_none());
    assert!(value.get("owner_uid").is_none());
    assert!(value.get("worker_pid").is_none());
    assert!(value["activity_id"].is_null());
}

#[test]
fn object_attachment_sends_components_without_owner_uri_or_execution_fields() {
    let value = json!({
        "label": "Release status",
        "object": {"app_id": "kv", "object_type": "entry", "object_id": " a/b?x=1&y=2 "}
    });
    let request: ActivityObjectAttachRequest = serde_json::from_value(value.clone()).unwrap();
    assert!(request.object.revision.is_none());
    assert_eq!(serde_json::to_value(&request).unwrap(), value);
    for field in ["owner_uid", "reference", "invocation", "caps"] {
        let mut invalid = value.clone();
        invalid[field] = json!("not caller authority");
        assert!(serde_json::from_value::<ActivityObjectAttachRequest>(invalid).is_err());
    }
    assert!(
        serde_json::from_value::<ActivityObjectAttachRequest>(json!({
            "label": "Raw URI", "object": "app://kv/entry?id=x"
        }))
        .is_err()
    );
}

#[test]
fn object_presentation_v1_defaults_preserve_additive_compatibility() {
    let response: ActivityObjectsResponse = serde_json::from_value(json!({
        "activity_id": "a", "future": true,
        "objects": [{"label": "Missing", "reference": "app://missing/entry?id=x", "status": "unavailable"}]
    })).unwrap();
    assert!(response.objects[0].description.is_none());
    assert!(response.objects[0].error.is_none());
    let description: AppObjectDescription = serde_json::from_value(json!({
        "object": {"app_id": "kv", "object_type": "entry", "object_id": "x", "future": true},
        "reference": "app://kv/entry?id=x",
        "invocation": {"app_id": "kv", "operation": "get"},
        "provenance": {"publisher": "not in the desktop DTO"}
    }))
    .unwrap();
    assert!(description.object.revision.is_none());
    assert!(description.invocation.args.is_empty());
    assert!(description.object_summary.is_empty());
    let encoded = serde_json::to_value(&description).unwrap();
    assert!(encoded.get("provenance").is_none());
    assert!(encoded["object"].get("future").is_none());
    assert_eq!(
        serde_json::from_value::<AppObjectDescription>(encoded).unwrap(),
        description
    );
}

#[test]
fn object_descriptions_do_not_accept_arbitrary_json_arguments_or_access_claims() {
    assert!(
        serde_json::from_value::<AppObjectInvocation>(json!({
            "app_id": "kv", "operation": "get", "args": [{"execute": true}]
        }))
        .is_err()
    );
    assert!(serde_json::from_value::<ActivityObjectStatus>(json!("readable")).is_err());
    assert_eq!(
        serde_json::to_value(ActivityResource {
            label: "File".into(),
            reference: "notes.txt".into(),
        })
        .unwrap(),
        json!({"label": "File", "reference": "notes.txt"})
    );
}

fn operation_preview_value() -> serde_json::Value {
    json!({
        "schema": 1, "app_id": "fs", "app_name": "Files", "app_version": "1",
        "package_digest": "sha256:fixture", "operation": "write", "operation_label": "Write",
        "effects_declared": true,
        "effects": [{
            "kind": "update", "label": "Requested write", "recovery": "unknown",
            "target_arg": "path", "target_kind": "path",
            "requested_targets": ["../requested;path"], "target_state": "requested"
        }],
        "unresolved_arguments": ["credential"],
        "authorization_checked": false, "executed": false, "effects_confirmed": false,
        "notes": ["Manifest metadata only"]
    })
}

#[test]
fn operation_preview_requests_keep_opaque_argv_without_owner_or_execution_controls() {
    let invocation = AppObjectInvocation {
        app_id: "fs".into(),
        operation: "write".into(),
        args: vec![
            "../requested;path".into(),
            "--content".into(),
            "private $() text".into(),
        ],
    };
    let request = ActivityOperationPreviewRequest::from(&invocation);
    assert_eq!(request.args, invocation.args);
    let value = serde_json::to_value(&request).unwrap();
    assert_eq!(
        serde_json::from_value::<ActivityOperationPreviewRequest>(value.clone()).unwrap(),
        request
    );
    for field in ["owner_uid", "id", "execute", "approve", "grant"] {
        let mut value = value.clone();
        value[field] = json!(true);
        assert!(serde_json::from_value::<ActivityOperationPreviewRequest>(value).is_err());
    }
    assert!(
        serde_json::from_value::<ActivityOperationPreviewRequest>(json!({
            "app_id": "fs", "operation": "write", "args": [{"not": "argv"}]
        }))
        .is_err()
    );
}

#[test]
fn operation_preview_contract_retains_closed_metadata_and_explicit_false_flags() {
    let mut value = operation_preview_value();
    value["args"] = json!(["private value"]);
    value["credential"] = json!("private value");
    let preview: ActivityOperationPreview = serde_json::from_value(value).unwrap();
    assert!(preview.is_metadata_only());
    let encoded = serde_json::to_value(&preview).unwrap();
    assert!(encoded.get("args").is_none());
    assert!(encoded.get("credential").is_none());
    assert_eq!(
        serde_json::from_value::<ActivityOperationPreview>(encoded).unwrap(),
        preview
    );
    for field in [
        "authorization_checked",
        "executed",
        "effects_confirmed",
        "effects_declared",
        "effects",
    ] {
        let mut value = operation_preview_value();
        value.as_object_mut().unwrap().remove(field);
        assert!(serde_json::from_value::<ActivityOperationPreview>(value).is_err());
    }
}

#[test]
fn operation_preview_enums_accept_only_declared_contract_values() {
    for kind in ["read", "create", "update", "delete", "external", "execute"] {
        assert!(serde_json::from_value::<AppEffectKind>(json!(kind)).is_ok());
    }
    for recovery in [
        "not_applicable",
        "reversible",
        "compensatable",
        "irreversible",
        "unknown",
    ] {
        assert!(serde_json::from_value::<AppEffectRecovery>(json!(recovery)).is_ok());
    }
    for state in ["requested", "unspecified", "unresolved"] {
        assert!(serde_json::from_value::<AppEffectTargetState>(json!(state)).is_ok());
    }
    assert!(serde_json::from_value::<AppEffectRecovery>(json!("guaranteed")).is_err());
    assert!(serde_json::from_value::<AppEffectTargetState>(json!("canonical")).is_err());
}

fn receipt_value() -> serde_json::Value {
    json!({
        "id": "receipt-1", "activity_id": "activity-1",
        "received_at": "2026-09-10T21:00:00Z", "source": "caller_reported",
        "report": {
            "id": "report-1", "app_id": "kv", "operation": "get",
            "package_digest": "reported-package-digest", "outcome": "returned",
            "result": {
                "kind": "json", "sha256": "reported-output-digest", "bytes": 900,
                "preview": "{\"outcome\":\"applied\",\"os_confirmed\":true}",
                "preview_truncated": true
            },
            "error": null
        }
    })
}

#[test]
fn receipt_protocol_preserves_caller_reports_and_defaults_unavailable_declarations() {
    let receipt: ActivityReceiptView = serde_json::from_value(receipt_value()).unwrap();
    assert_eq!(receipt.source, ActivityReceiptSource::CallerReported);
    assert_eq!(receipt.report.outcome, ActivityReceiptOutcome::Returned);
    assert!(receipt.declaration.is_none());
    assert!(receipt.declaration_error.is_none());
    assert!(receipt.report.result.as_ref().unwrap().preview_truncated);
    let encoded = serde_json::to_value(&receipt).unwrap();
    assert_eq!(
        serde_json::from_value::<ActivityReceiptView>(encoded).unwrap(),
        receipt
    );
}

#[test]
fn receipt_protocol_rejects_unknown_source_outcome_and_result_claims() {
    for source in ["os_confirmed", "verified", "broker_attested"] {
        let mut value = receipt_value();
        value["source"] = json!(source);
        assert!(serde_json::from_value::<ActivityReceiptView>(value).is_err());
    }
    for outcome in ["applied", "verified", "goal_completed", "ok"] {
        let mut value = receipt_value();
        value["report"]["outcome"] = json!(outcome);
        assert!(serde_json::from_value::<ActivityReceiptView>(value).is_err());
    }
    let mut value = receipt_value();
    value["report"]["result"]["kind"] = json!("trusted_html");
    assert!(serde_json::from_value::<ActivityReceiptView>(value).is_err());
    let mut value = receipt_value();
    value.as_object_mut().unwrap().remove("source");
    assert!(serde_json::from_value::<ActivityReceiptView>(value).is_err());
    let mut value = receipt_value();
    value["report"]["result"]
        .as_object_mut()
        .unwrap()
        .remove("preview_truncated");
    assert!(serde_json::from_value::<ActivityReceiptView>(value).is_err());
}

#[test]
fn receipt_protocol_drops_owner_and_additive_attestation_claims() {
    let mut value = receipt_value();
    value["owner_uid"] = json!(0);
    value["os_confirmed"] = json!(true);
    value["report"]["verified"] = json!(true);
    value["report"]["result"]["attestation"] = json!("not part of this contract");
    let receipt: ActivityReceiptView = serde_json::from_value(value).unwrap();
    let encoded = serde_json::to_value(receipt).unwrap();
    assert!(encoded.get("owner_uid").is_none());
    assert!(encoded.get("os_confirmed").is_none());
    assert!(encoded["report"].get("verified").is_none());
    assert!(encoded["report"]["result"].get("attestation").is_none());
    assert_eq!(encoded["source"], "caller_reported");
}

#[test]
fn receipt_query_is_owner_free_and_collection_scope_is_explicit() {
    assert!(serde_json::from_value::<ActivityReceiptsQuery>(json!({"owner_uid": 0})).is_err());
    let receipt: ActivityReceiptView = serde_json::from_value(receipt_value()).unwrap();
    let mut response = ActivityReceiptsResponse {
        schema: 1,
        activity_id: "activity-1".into(),
        receipts: vec![receipt],
    };
    assert!(response.matches_activity("activity-1"));
    assert!(!response.matches_activity("other"));
    response.receipts[0].activity_id = "other".into();
    assert!(!response.matches_activity("activity-1"));
}
