use super::*;
use serde_json::json;

fn metadata() -> Value {
    json!({
        "id": "activity-1",
        "owner_uid": 1000,
        "title": "Prepare a release",
        "goal": "Publish a verified release",
        "completion_criteria": "Tests and review",
        "boundaries": "Ask before publishing",
        "resources": [{"label": "Notes", "reference": "file:notes", "future": true}],
        "state": "active",
        "completion_note": null,
        "created_at": "2026-09-10T12:00:00Z",
        "updated_at": "2026-09-10T12:00:00Z",
        "private_future_field": "hidden"
    })
}

fn object_state_value() -> Value {
    json!({
        "schema": 1, "activity_id": "11111111-1111-4111-8111-111111111111",
        "entries": [{
            "id": "22222222-2222-4222-8222-222222222222",
            "activity_id": "11111111-1111-4111-8111-111111111111",
            "owner_uid": 1000, "recorded_at": "2026-09-11T12:00:00Z",
            "source": "caller_reported", "draft": {
                "id": "22222222-2222-4222-8222-222222222222",
                "reference": "app://kv/entry?id=release.status",
                "content": {"kind": "user_statement", "text": "Caller report"},
                "observed_at": null, "valid_until": null, "supersedes": null
            },
            "receipt": null, "superseded_by": null, "validity": "unknown"
        }]
    })
}

#[test]
fn activity_object_state_translation_keeps_all_classifications_without_truth_claims() {
    use cos_agent_protocol::{ObjectStateContent, ObjectStateSource};
    for content in [
        json!({"kind": "user_statement", "text": "<script>inert</script>"}),
        json!({"kind": "agent_inference", "text": "Caller-classified inference"}),
        json!({"kind": "relation", "relation": "related_to", "target": "app://kv/entry?id=x", "note": ""}),
        json!({"kind": "relation", "relation": "depends_on", "target": "app://kv/entry?id=x", "note": "Planning"}),
        json!({"kind": "relation", "relation": "derived_from", "target": "app://kv/entry?id=x", "note": "Planning"}),
        json!({"kind": "retracted", "reason": "Mistaken report"}),
    ] {
        let mut value = object_state_value();
        value["entries"][0]["draft"]["content"] = content;
        if value["entries"][0]["draft"]["content"]["kind"] == "retracted" {
            value["entries"][0]["draft"]["supersedes"] =
                json!("33333333-3333-4333-8333-333333333333");
        }
        value["private_future_data"] = json!({"hidden": true});
        value["entries"][0]["verified"] = json!(true);
        value["entries"][0]["app_data"] = json!("never forwarded");
        let response = object_state(value).unwrap();
        assert_eq!(response.entries[0].source, ObjectStateSource::CallerReported);
        let entry = object_state_entry(serde_json::to_value(&response.entries[0]).unwrap()).unwrap();
        assert!(entry.matches_activity(&response.activity_id));
        if matches!(entry.draft.content, ObjectStateContent::Retracted { .. }) {
            assert!(entry.draft.supersedes.is_some());
        }
        let encoded = serde_json::to_value(response).unwrap();
        assert!(encoded.get("private_future_data").is_none());
        assert!(encoded["entries"][0].get("verified").is_none());
        assert!(encoded["entries"][0].get("app_data").is_none());
    }
}

#[test]
fn activity_object_state_translation_reuses_bounded_receipt_report_without_replacement_data() {
    use cos_agent_protocol::ActivityReceiptOutcome;
    let mut value = object_state_value();
    value["entries"][0]["draft"]["content"] = json!({
        "kind": "app_report", "receipt_id": "44444444-4444-4444-8444-444444444444"
    });
    value["entries"][0]["receipt"] = json!({
        "id": "44444444-4444-4444-8444-444444444444", "app_id": "kv",
        "operation": "get", "package_digest": "caller-digest", "outcome": "indeterminate",
        "result": {
            "kind": "json", "sha256": "caller-output-digest", "bytes": 9000,
            "preview": "{\"verified\":true}", "preview_truncated": true
        },
        "error": "Reported uncertainty", "raw_app_data": {"not": "forwarded"}
    });
    let response = object_state(value.clone()).unwrap();
    let report = response.entries[0].receipt.as_ref().unwrap();
    assert_eq!(report.outcome, ActivityReceiptOutcome::Indeterminate);
    assert!(report.result.as_ref().unwrap().preview_truncated);
    assert_eq!(report.result.as_ref().unwrap().preview, "{\"verified\":true}");
    assert_eq!(report.error.as_deref(), Some("Reported uncertainty"));
    let encoded = serde_json::to_value(&response).unwrap();
    assert!(encoded["entries"][0]["receipt"].get("raw_app_data").is_none());
    value["entries"][0]["receipt"]["outcome"] = json!("os_confirmed");
    assert!(object_state(value).is_err());
}

#[test]
fn activity_object_state_translation_rejects_schema_identity_shape_and_claim_mismatches() {
    for (field, replacement) in [
        ("id", json!("different-id")),
        ("activity_id", json!("other-activity")),
        ("source", json!("verified")),
        ("owner_uid", json!(-1)),
        ("recorded_at", json!("")),
        ("validity", json!("fresh")),
        ("validity", json!("expired")),
        ("superseded_by", json!("22222222-2222-4222-8222-222222222222")),
    ] {
        let mut value = object_state_value();
        value["entries"][0][field] = replacement;
        assert!(object_state(value).is_err(), "{field}");
    }
    let mut value = object_state_value();
    value["schema"] = json!(2);
    assert!(object_state(value).is_err());
    let mut value = object_state_value();
    value["entries"][0]["draft"]["content"] = json!({
        "kind": "app_report", "receipt_id": "44444444-4444-4444-8444-444444444444"
    });
    assert!(object_state(value.clone()).is_err());
    assert!(object_state_entry(value["entries"][0].clone()).is_err());
    let mut value = object_state_value();
    value["entries"][0]["draft"]["content"]["text"] = json!("\u{e9}".repeat(2049));
    assert!(object_state(value).is_err());
}

#[test]
fn activity_object_state_translation_keeps_expired_windows_and_immutable_history() {
    use cos_agent_protocol::ObjectStateValidity;
    for validity in ["not_yet_applicable", "within_reported_window", "expired"] {
        let mut value = object_state_value();
        value["entries"][0]["draft"]["observed_at"] = json!("2026-09-09T12:00:00Z");
        value["entries"][0]["draft"]["valid_until"] = json!("2026-09-10T12:00:00Z");
        value["entries"][0]["validity"] = json!(validity);
        value["entries"][0]["superseded_by"] = json!("33333333-3333-4333-8333-333333333333");
        let response = object_state(value).unwrap();
        assert!(response.entries[0].superseded_by.is_some());
        assert_ne!(response.entries[0].validity, ObjectStateValidity::Unknown);
        assert_eq!(response.entries.len(), 1);
    }
}

fn detail_value() -> Value {
    json!({
        "schema": 1,
        "activity": metadata(),
        "jobs": [{
            "id": "job-1", "title": "Check release", "status": "ok",
            "session_id": "session-1",
            "created_at": "2026-09-10T12:00:00Z",
            "finished_at": "2026-09-10T12:01:00Z",
            "response": "Checks passed", "error": null,
            "waiting_on": ["approval-1"], "worker_pid": 10,
            "prompt": "not part of the desktop contract", "owner_uid": 1000
        }],
        "sessions": ["session-1"]
    })
}

#[test]
fn activity_projection_is_explicit_and_resources_remain_inert_text() {
    let value = serde_json::to_value(activity(metadata()).unwrap()).unwrap();
    assert!(value.get("owner_uid").is_none());
    assert!(value.get("private_future_field").is_none());
    assert!(value["resources"][0].get("future").is_none());
    assert_eq!(value["resources"][0]["reference"], "file:notes");
    assert_eq!(value["boundaries"], "Ask before publishing");
}

#[test]
fn successful_jobs_do_not_complete_the_activity() {
    let response = detail(detail_value()).unwrap();
    assert_eq!(response.activity.state, ActivityState::Active);
    assert!(response.activity.completion_note.is_none());
    assert_eq!(response.jobs[0].status, "ok");
    assert_eq!(response.jobs[0].waiting_on, ["approval-1"]);
    let job = serde_json::to_value(&response.jobs[0]).unwrap();
    assert!(job.get("prompt").is_none());
    assert!(job.get("worker_pid").is_none());
    assert!(job.get("owner_uid").is_none());
}

#[test]
fn detail_previews_are_unicode_safe_and_bounded() {
    let mut value = detail_value();
    value["jobs"][0]["response"] = json!("é".repeat(PREVIEW_CHARS + 10));
    value["jobs"][0]["error"] = json!("失".repeat(PREVIEW_CHARS + 10));
    let response = detail(value).unwrap();
    for preview in [&response.jobs[0].response, &response.jobs[0].error] {
        let preview = preview.as_ref().unwrap();
        assert_eq!(preview.chars().count(), PREVIEW_CHARS + 1);
        assert!(preview.ends_with('…'));
    }
}

#[test]
fn unsupported_schema_and_malformed_metadata_are_not_success() {
    assert!(list(json!({"schema": 2, "activities": []})).is_err());
    let mut value = detail_value();
    value["schema"] = json!(2);
    assert!(detail(value).is_err());
    let mut value = metadata();
    value["state"] = json!("succeeded");
    assert!(activity(value).is_err());
    let mut value = metadata();
    value["id"] = json!("");
    assert!(activity(value).is_err());
    assert!(work(json!({"id": "", "status": "queued"})).is_err());
}

#[test]
fn approval_projection_is_limited_to_associated_sessions() {
    let response = detail(detail_value()).unwrap();
    let approvals = approvals(
        json!({"requests": [
            {"id": "a", "session": "session-1", "verb": "fs.write", "reason": "Publish",
             "owner_uid": 1000, "meta": {"label": "Write files", "risk": "high"}},
            {"id": "b", "session": "unrelated", "verb": "fs.write", "reason": "Unrelated"},
            {"id": "c", "session": "", "verb": "fs.write", "reason": "Unassociated"}
        ]}),
        &response,
    )
    .unwrap();
    assert_eq!(approvals.len(), 1);
    assert_eq!(approvals[0].id, "a");
    assert_eq!(approvals[0].label, "Write files");
    assert!(
        serde_json::to_value(&approvals[0])
            .unwrap()
            .get("owner_uid")
            .is_none()
    );
    assert!(super::approvals(json!({"invalid": []}), &response).is_err());
}

#[test]
fn work_acknowledgement_keeps_identity_without_private_job_payloads() {
    let response = work(json!({
        "id": "job-1", "status": "queued", "session_id": "session-1",
        "activity_id": "activity-1", "owner_uid": 1000, "prompt": "hidden", "worker_pid": 10
    }))
    .unwrap();
    let value = serde_json::to_value(response).unwrap();
    assert_eq!(value["activity_id"], "activity-1");
    assert!(value.get("prompt").is_none());
    assert!(value.get("owner_uid").is_none());
    assert!(value.get("worker_pid").is_none());
    assert!(value.get("state").is_none());
}

fn object_envelope() -> Value {
    json!({
        "schema": 1, "activity_id": "activity-1", "owner_uid": 0,
        "objects": [
            {
                "label": "Release status", "reference": "app://kv/entry?id=release.status",
                "status": "declared", "error": null,
                "description": {
                    "object": {"app_id": "kv", "object_type": "entry", "object_id": "release.status"},
                    "reference": "app://kv/entry?id=release.status",
                    "app_name": "Key/value", "app_version": "1.0",
                    "object_label": "Entry", "object_summary": "A key/value entry",
                    "invocation": {"app_id": "kv", "operation": "get", "args": ["--key", "release.status"]},
                    "provenance": {"publisher": "verified upstream"},
                    "exists": true, "readable": true, "data": {"must": "not leak"}
                }
            },
            {
                "label": "Unavailable", "reference": "app://missing/entry?id=x",
                "status": "unavailable", "description": null, "error": "publisher revoked"
            },
            {
                "label": "Malformed", "reference": "app:malformed",
                "status": "invalid", "description": null, "error": "invalid object reference"
            }
        ]
    })
}

#[test]
fn object_translation_retains_status_diagnostics_and_only_presentation_fields() {
    let response = objects(object_envelope()).unwrap();
    assert_eq!(response.activity_id, "activity-1");
    assert_eq!(response.objects[0].status, ActivityObjectStatus::Declared);
    assert_eq!(
        response.objects[1].error.as_deref(),
        Some("publisher revoked")
    );
    assert_eq!(response.objects[2].reference, "app:malformed");
    assert_eq!(
        response.objects[2].error.as_deref(),
        Some("invalid object reference")
    );
    let encoded = serde_json::to_value(&response).unwrap();
    assert!(encoded.get("owner_uid").is_none());
    for field in ["provenance", "exists", "readable", "data"] {
        assert!(encoded["objects"][0]["description"].get(field).is_none());
    }
    assert_eq!(
        encoded["objects"][0]["description"]["invocation"]["args"],
        json!(["--key", "release.status"])
    );
}

#[test]
fn object_translation_refuses_unknown_schemas_and_inconsistent_declarations() {
    let mut value = object_envelope();
    value["schema"] = json!(2);
    assert!(objects(value).is_err());
    let mut value = object_envelope();
    value["activity_id"] = json!("");
    assert!(objects(value).is_err());
    let mut value = object_envelope();
    value["objects"][0]["status"] = json!("readable");
    assert!(objects(value).is_err());
    let mut value = object_envelope();
    value["objects"][0]["description"] = Value::Null;
    assert!(objects(value).is_err());
    let mut value = object_envelope();
    value["objects"][0]["description"]["invocation"]["app_id"] = json!("other-app");
    assert!(objects(value).is_err());
    let mut value = object_envelope();
    value["objects"][0]["status"] = json!("unavailable");
    assert!(objects(value).is_err());
}

#[test]
fn object_translation_preserves_noncanonical_resources_and_broker_canonical_descriptions() {
    let mut value = object_envelope();
    value["objects"][0]["reference"] = json!("app://kv/entry?id=release%2Estatus");
    let response = objects(value).unwrap();
    assert_eq!(response.objects[0].status, ActivityObjectStatus::Declared);
    assert_eq!(
        response.objects[0].reference,
        "app://kv/entry?id=release%2Estatus"
    );
    assert_eq!(
        response.objects[0].description.as_ref().unwrap().reference,
        "app://kv/entry?id=release.status",
    );
}

fn operation_preview_value() -> Value {
    json!({
        "schema": 1, "app_id": "fs", "app_name": "Files", "app_version": "1",
        "package_digest": "sha256:fixture", "operation": "write", "operation_label": "Write",
        "effects_declared": true,
        "effects": [{
            "kind": "update", "label": "App-declared write", "recovery": "reversible",
            "target_arg": "path", "target_kind": "path",
            "requested_targets": ["~/requested/../path;$(data)"], "target_state": "requested"
        }],
        "unresolved_arguments": ["credential"],
        "authorization_checked": false, "executed": false, "effects_confirmed": false,
        "notes": ["Recovery is not guaranteed"]
    })
}

#[test]
fn operation_preview_projection_preserves_requested_metadata_not_argument_values() {
    let mut value = operation_preview_value();
    value["args"] = json!(["--content", "private non-resource value"]);
    value["owner_uid"] = json!(0);
    value["credentials"] = json!({"token": "private non-resource value"});
    value["effects"][0]["confirmed_target"] = json!("/canonical/not-claimed");
    let preview = operation_preview(value).unwrap();
    assert_eq!(
        preview.effects[0].requested_targets,
        ["~/requested/../path;$(data)"]
    );
    assert_eq!(preview.unresolved_arguments, ["credential"]);
    assert_eq!(preview.notes, ["Recovery is not guaranteed"]);
    let encoded = serde_json::to_value(preview).unwrap();
    for key in ["args", "owner_uid", "credentials"] {
        assert!(encoded.get(key).is_none());
    }
    assert!(encoded["effects"][0].get("confirmed_target").is_none());
    assert!(!encoded.to_string().contains("private non-resource value"));
}

#[test]
fn operation_preview_projection_rejects_execution_authorization_and_schema_claims() {
    for field in ["authorization_checked", "executed", "effects_confirmed"] {
        let mut value = operation_preview_value();
        value[field] = json!(true);
        assert!(operation_preview(value).is_err());
    }
    let mut value = operation_preview_value();
    value["schema"] = json!(2);
    assert!(operation_preview(value).is_err());
    let mut value = operation_preview_value();
    value["effects"][0]["target_state"] = json!("canonical");
    assert!(operation_preview(value).is_err());
}

#[test]
fn operation_preview_missing_effects_remain_unknown_without_read_only_inference() {
    let mut value = operation_preview_value();
    value["effects_declared"] = json!(false);
    value["effects"] = json!([]);
    value["operation"] = json!("delete");
    let preview = operation_preview(value).unwrap();
    assert!(!preview.effects_declared);
    assert!(preview.effects.is_empty());
    assert_eq!(preview.operation, "delete");
    assert!(preview.is_metadata_only());
}

fn receipt_envelope() -> Value {
    json!({
        "schema": 1, "activity_id": "activity-1",
        "receipts": [{
            "id": "receipt-1", "activity_id": "activity-1", "owner_uid": 1000,
            "received_at": "2026-09-10T21:00:00Z", "source": "caller_reported",
            "report": {
                "id": "report-1", "app_id": "kv", "operation": "get",
                "package_digest": "reported-package", "outcome": "returned",
                "result": {
                    "kind": "text", "sha256": "reported-output", "bytes": 4096,
                    "preview": "**applied** [open](https://invalid.example) <script>data</script>",
                    "preview_truncated": true
                },
                "error": "[REDACTED] caller error"
            },
            "declaration": null,
            "declaration_error": "package changed or revoked"
        }]
    })
}

#[test]
fn receipt_translation_preserves_recording_time_redacted_content_and_declaration_diagnostics() {
    let response = receipts(receipt_envelope()).unwrap();
    let receipt = &response.receipts[0];
    assert_eq!(receipt.received_at, "2026-09-10T21:00:00Z");
    assert_eq!(
        receipt.declaration_error.as_deref(),
        Some("package changed or revoked")
    );
    assert!(receipt.declaration.is_none());
    assert_eq!(
        receipt.report.error.as_deref(),
        Some("[REDACTED] caller error")
    );
    let result = receipt.report.result.as_ref().unwrap();
    assert_eq!(result.bytes, 4096);
    assert_eq!(result.sha256, "reported-output");
    assert_eq!(
        result.preview,
        "**applied** [open](https://invalid.example) <script>data</script>"
    );
    assert!(result.preview_truncated);
    assert!(
        serde_json::to_value(receipt)
            .unwrap()
            .get("owner_uid")
            .is_none()
    );
}

#[test]
fn receipt_translation_rejects_stronger_sources_outcomes_and_mixed_activity_records() {
    for source in ["os_confirmed", "verified"] {
        let mut value = receipt_envelope();
        value["receipts"][0]["source"] = json!(source);
        assert!(receipts(value).is_err());
    }
    let mut value = receipt_envelope();
    value["receipts"][0]["report"]["outcome"] = json!("applied");
    assert!(receipts(value).is_err());
    let mut value = receipt_envelope();
    value["receipts"][0]["activity_id"] = json!("other-activity");
    assert!(receipts(value).is_err());
    let mut value = receipt_envelope();
    value["receipts"][0]["id"] = json!("");
    assert!(receipts(value).is_err());
    let mut value = receipt_envelope();
    value["schema"] = json!(2);
    assert!(receipts(value).is_err());
}

#[test]
fn receipt_translation_keeps_matched_declarations_separate_from_execution_claims() {
    let mut value = receipt_envelope();
    value["receipts"][0]["declaration_error"] = Value::Null;
    value["receipts"][0]["declaration"] = json!({
        "app_version": "1", "operation_label": "Get entry",
        "effects": [{"kind": "read", "label": "App-declared read", "recovery": "unknown", "target_arg": "key"}],
        "execution_verified": true
    });
    value["receipts"][0]["effects_confirmed"] = json!(true);
    let response = receipts(value).unwrap();
    let encoded = serde_json::to_value(&response.receipts[0]).unwrap();
    assert_eq!(encoded["source"], "caller_reported");
    assert_eq!(encoded["report"]["outcome"], "returned");
    assert_eq!(encoded["declaration"]["effects"][0]["target_arg"], "key");
    assert!(encoded.get("effects_confirmed").is_none());
    assert!(encoded["declaration"].get("execution_verified").is_none());
}
