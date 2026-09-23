use super::*;
use cos_agent_protocol::{
    CapabilityPolicyDraft, CapabilityPolicyMode, CapabilityPolicyRule, CapabilityPolicyScope,
};

#[test]
fn capability_policy_transport_keeps_identity_in_url_and_cas_in_typed_body() {
    let endpoint = endpoint(2, 2);
    let id = "activity/id?not-query";
    let (request, selected) = capability_policy_get_request(&endpoint, id).unwrap();
    let get = request.build().unwrap();
    assert_eq!(selected, ProtocolVersion(2));
    assert_eq!(get.method(), reqwest::Method::GET);
    assert_eq!(get.url().path_segments().unwrap().count(), 4);
    assert!(get.url().path().ends_with("/capability-policy"));
    assert!(get.url().query().is_none());
    assert!(get.body().is_none());
    assert!(get.headers().contains_key(reqwest::header::AUTHORIZATION));
    assert_eq!(get.headers()[PROTOCOL_VERSION_HEADER], "2");
    let body = ActivityCapabilityPolicySetRequest {
        expected_revision: None,
        policy: CapabilityPolicyDraft {
            rules: vec![CapabilityPolicyRule {
                verb: "fs.delete".into(),
                mode: CapabilityPolicyMode::Deny,
                scopes: Vec::new(),
            }],
        },
    };
    let (request, _) = capability_policy_set_request(&endpoint, id, &body).unwrap();
    let set = request.build().unwrap();
    assert_eq!(set.method(), reqwest::Method::POST);
    assert_eq!(set.url(), get.url());
    let sent: serde_json::Value =
        serde_json::from_slice(set.body().unwrap().as_bytes().unwrap()).unwrap();
    assert_eq!(
        sent,
        serde_json::json!({
            "expected_revision": null, "policy": {"rules": [{"verb": "fs.delete", "mode": "deny", "scopes": []}]}
        })
    );
    for field in [
        "id",
        "owner_uid",
        "revision",
        "approved",
        "grant",
        "enabled",
    ] {
        assert!(sent.get(field).is_none());
    }
}

#[test]
fn capability_policy_transport_preserves_u64_revision_and_explicit_scope_types() {
    let endpoint = endpoint(2, 2);
    let body = ActivityCapabilityPolicyEnabledRequest {
        expected_revision: u64::MAX - 1,
        enabled: false,
    };
    let (request, _) = capability_policy_enabled_request(&endpoint, "activity", &body).unwrap();
    let request = request.build().unwrap();
    assert_eq!(
        request.url().path(),
        "/api/activities/activity/capability-policy/enabled"
    );
    assert_eq!(request.method(), reqwest::Method::POST);
    assert_eq!(request.headers()[PROTOCOL_VERSION_HEADER], "2");
    assert!(
        request
            .headers()
            .contains_key(reqwest::header::AUTHORIZATION)
    );
    let encoded = request.body().unwrap().as_bytes().unwrap();
    assert_eq!(
        serde_json::from_slice::<ActivityCapabilityPolicyEnabledRequest>(encoded).unwrap(),
        body
    );
    let value: serde_json::Value = serde_json::from_slice(encoded).unwrap();
    assert_eq!(value["expected_revision"].as_u64(), Some(u64::MAX - 1));
    assert_eq!(value.as_object().unwrap().len(), 2);
    assert_eq!(
        serde_json::to_value(CapabilityPolicyScope::SelfRef {
            value: "self".into()
        })
        .unwrap(),
        serde_json::json!({"kind": "self-ref", "value": "self"})
    );
}
