use super::*;
use serde_json::json;

const SPACE: &str = "11111111-1111-4111-8111-111111111111";
const LINK: &str = "22222222-2222-4222-8222-222222222222";
const JOB: &str = "33333333-3333-4333-8333-333333333333";
const NOTE: &str = "44444444-4444-4444-8444-444444444444";

#[test]
fn human_workspace_preview_requires_link_and_exact_closed_target() {
    let direct = json!({"id":SPACE,"request":{"command":"note","note_id":NOTE}});
    assert!(serde_json::from_value::<ReadRequest>(direct.clone()).is_ok());
    let mut linked = direct;
    linked["request"]["link_id"] = json!(LINK);
    assert!(serde_json::from_value::<ReadRequest>(linked.clone()).is_err());
    linked["target"] = json!({"kind":"job","id":JOB});
    let request: ReadRequest = serde_json::from_value(linked.clone()).unwrap();
    assert_eq!(request.target, Some(ReadTarget::Job { id: JOB.into() }));
    linked["request"].as_object_mut().unwrap().remove("link_id");
    assert!(serde_json::from_value::<ReadRequest>(linked).is_err());

    for target in [
        json!({"kind":"job","id":JOB,"owner_uid":1001}),
        json!({"kind":"conversation","id":"ses_example","job_id":JOB}),
        json!({"kind":"all","id":JOB}),
    ] {
        assert!(serde_json::from_value::<ReadRequest>(
            json!({"id":SPACE,"request":{"command":"note","note_id":NOTE,"link_id":LINK},"target":target})
        ).is_err());
    }
}

#[test]
fn human_http_preview_keeps_binding_and_revision_on_each_page() {
    let body = json!({
        "request":{"command":"source","path":"/home/example/file.txt","link_id":LINK,
            "offset":20,"max_chars":20,"revision":"a".repeat(64)},
        "target":{"kind":"job","id":JOB}
    });
    let request = serde_json::from_value::<ReadBody>(body.clone())
        .unwrap()
        .into_request(SPACE.into())
        .unwrap();
    assert_eq!(request.request.link_id(), Some(LINK));
    assert_eq!(request.target, Some(ReadTarget::Job { id: JOB.into() }));
    let encoded = serde_json::to_value(request).unwrap();
    assert_eq!(encoded["request"]["offset"], 20);
    assert_eq!(encoded["request"]["revision"], "a".repeat(64));
    let mut unknown = body;
    unknown["owner_uid"] = json!(1001);
    assert!(serde_json::from_value::<ReadBody>(unknown).is_err());
    assert!(serde_json::from_value::<WorkspaceRead>(encoded["request"].clone()).is_ok());
    let mut model_request = encoded["request"].clone();
    model_request["target"] = encoded["target"].clone();
    assert!(serde_json::from_value::<WorkspaceRead>(model_request).is_err());
}

#[test]
fn empty_workspace_commands_do_not_ignore_human_preview_envelopes_or_selectors() {
    for command in ["context", "index"] {
        assert!(serde_json::from_value::<WorkspaceRead>(json!({"command":command})).is_ok());
        for (field, value) in [
            ("target", json!({"kind":"job","id":JOB})),
            ("workspace_id", json!(SPACE)),
            ("owner_uid", json!(1001)),
            (
                "request",
                json!({"command":"note","note_id":NOTE,"link_id":LINK}),
            ),
        ] {
            let mut body = json!({"command":command});
            body[field] = value;
            assert!(serde_json::from_value::<WorkspaceRead>(body.clone()).is_err());
            assert!(serde_json::from_value::<ReadBody>(body).is_err());
        }
    }
}

#[test]
fn note_preview_preserves_origin_and_never_relabels_evidence() {
    let value = json!({"note":{"id":NOTE,"workspace_id":SPACE,"title":"Audience","content":"Beginners",
        "source":"owner_note","source_job_id":null,"revision":1,
        "created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"},
        "imported":true,"source_workspace_id":SPACE,"evidence":"owner_note_or_caller_report_not_current_fact"});
    serde_json::from_value::<NotePreview>(value.clone())
        .unwrap()
        .validate()
        .unwrap();
    for (field, replacement) in [
        ("source_workspace_id", json!(JOB)),
        ("evidence", json!("owner_confirmed_current_fact")),
    ] {
        let mut invalid = value.clone();
        invalid[field] = replacement;
        assert!(serde_json::from_value::<NotePreview>(invalid)
            .unwrap()
            .validate()
            .is_err());
    }
}
