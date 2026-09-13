use super::*;
use serde_json::{Value, json};

const ACTIVITY: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
const OTHER: &str = "bbbbbbbb-cccc-4ddd-8eee-ffffffffffff";

fn rule() -> CapabilityPolicyRule {
    CapabilityPolicyRule {
        verb: "fs.read".into(),
        mode: CapabilityPolicyMode::Normal,
        scopes: vec![CapabilityPolicyScope::Path {
            value: "/workspace/**".into(),
        }],
    }
}

fn policy() -> ActivityCapabilityPolicy {
    ActivityCapabilityPolicy {
        activity_id: ACTIVITY.into(),
        owner_uid: 1000,
        revision: 5,
        enabled: false,
        rules: vec![rule()],
        created_at: "2026-09-11T12:00:00Z".into(),
        updated_at: "2026-09-11T13:00:00Z".into(),
    }
}

#[test]
fn capability_policy_wire_shapes_preserve_required_null_and_all_scope_kinds() {
    let request = ActivityCapabilityPolicySetRequest {
        expected_revision: None,
        policy: CapabilityPolicyDraft::default(),
    };
    assert_eq!(
        serde_json::to_value(request).unwrap(),
        json!({
            "expected_revision": null, "policy": {"rules": []}
        })
    );
    let response = ActivityCapabilityPolicyResponse {
        schema: 1,
        activity_id: ACTIVITY.into(),
        capability_policy: None,
    };
    assert_eq!(
        serde_json::to_value(&response).unwrap(),
        json!({
            "schema": 1, "activity_id": ACTIVITY, "capability_policy": null
        })
    );
    assert!(response.matches_owner(ACTIVITY, 1000));
    assert!(
        serde_json::from_value::<ActivityCapabilityPolicyResponse>(json!({
            "schema": 1, "activity_id": ACTIVITY
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<ActivityCapabilityPolicySetRequest>(json!({
            "policy": {"rules": []}
        }))
        .is_err()
    );
    for value in [
        json!({"kind": "path", "value": "/workspace/**"}),
        json!({"kind": "host", "value": "example.org:443"}),
        json!({"kind": "name", "value": "project/*"}),
        json!({"kind": "self-ref", "value": "self"}),
        json!({"kind": "wild"}),
    ] {
        let scope: CapabilityPolicyScope = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(scope).unwrap(), value);
    }
    let encoded = serde_json::to_value(policy()).unwrap();
    assert_eq!(encoded.as_object().unwrap().len(), 7);
}

#[test]
fn capability_policy_requests_reject_authority_selectors_and_open_ended_scope_shapes() {
    for field in [
        "id",
        "activity_id",
        "owner_uid",
        "revision",
        "enabled",
        "approved",
        "grant",
        "status",
    ] {
        let mut value = json!({"expected_revision": null, "policy": {"rules": []}});
        value[field] = json!(true);
        assert!(serde_json::from_value::<ActivityCapabilityPolicySetRequest>(value).is_err());
        let mut query = json!({});
        query[field] = json!(true);
        assert!(serde_json::from_value::<ActivityCapabilityPolicyQuery>(query).is_err());
    }
    for field in ["owner_uid", "revision", "rules", "approved", "grant"] {
        let mut value = json!({"expected_revision": 5, "enabled": false});
        value[field] = json!(true);
        assert!(serde_json::from_value::<ActivityCapabilityPolicyEnabledRequest>(value).is_err());
    }
    for value in [
        json!({"kind": "none"}),
        json!({"kind": "self_ref", "value": "self"}),
        json!({"kind": "wild", "value": "*"}),
        json!({"kind": "path"}),
        json!({"kind": "host", "value": ["example.org"]}),
    ] {
        assert!(
            serde_json::from_value::<CapabilityPolicyScope>(value.clone()).is_err(),
            "{value}"
        );
    }
    assert!(serde_json::from_value::<CapabilityPolicyMode>(json!("allow")).is_err());
    assert!(serde_json::from_value::<CapabilityPolicyMode>(json!("approve")).is_err());
    let mut value = serde_json::to_value(rule()).unwrap();
    value["grant"] = json!(true);
    assert!(serde_json::from_value::<CapabilityPolicyRule>(value).is_err());
}

#[test]
fn capability_policy_modes_bounds_and_empty_rules_are_constraints_only() {
    assert!(CapabilityPolicyDraft::default().validate_shape().is_ok());
    for mode in [
        CapabilityPolicyMode::Normal,
        CapabilityPolicyMode::RequireApproval,
    ] {
        let mut draft = CapabilityPolicyDraft {
            rules: vec![CapabilityPolicyRule { mode, ..rule() }],
        };
        assert!(draft.validate_shape().is_ok());
        draft.rules[0].scopes.clear();
        assert!(draft.validate_shape().is_err());
        draft.rules[0].scopes = vec![CapabilityPolicyScope::Wild {}; 32];
        assert!(draft.validate_shape().is_ok());
        draft.rules[0].scopes.push(CapabilityPolicyScope::Wild {});
        assert!(draft.validate_shape().is_err());
    }
    let mut draft = CapabilityPolicyDraft {
        rules: vec![CapabilityPolicyRule {
            verb: "fs.delete".into(),
            mode: CapabilityPolicyMode::Deny,
            scopes: Vec::new(),
        }],
    };
    assert!(draft.validate_shape().is_ok());
    draft.rules[0].scopes.push(CapabilityPolicyScope::Wild {});
    assert!(draft.validate_shape().is_err());
    draft.rules = vec![rule(), rule()];
    assert!(draft.validate_shape().is_err());
    draft.rules = (0..64)
        .map(|index| CapabilityPolicyRule {
            verb: format!("fixture.verb.{index}"),
            mode: CapabilityPolicyMode::Deny,
            scopes: Vec::new(),
        })
        .collect();
    assert!(
        draft.validate_shape().is_ok(),
        "catalogue membership is checked by the broker"
    );
    draft.rules.push(CapabilityPolicyRule {
        verb: "fixture.extra".into(),
        ..rule()
    });
    assert!(draft.validate_shape().is_err());
    draft.rules = vec![CapabilityPolicyRule {
        verb: " ".into(),
        ..rule()
    }];
    assert!(draft.validate_shape().is_err());
}

#[test]
fn capability_policy_complete_json_byte_bound_includes_escaping_and_utf8() {
    let mut draft = CapabilityPolicyDraft {
        rules: vec![rule()],
    };
    draft.rules[0].scopes = vec![CapabilityPolicyScope::Name {
        value: String::new(),
    }];
    let overhead = serde_json::to_vec(&draft).unwrap().len();
    draft.rules[0].scopes = vec![CapabilityPolicyScope::Name {
        value: "x".repeat(MAX_CAPABILITY_POLICY_BYTES - overhead),
    }];
    assert_eq!(
        serde_json::to_vec(&draft).unwrap().len(),
        MAX_CAPABILITY_POLICY_BYTES
    );
    assert!(draft.validate_shape().is_ok());
    draft.rules[0].scopes = vec![CapabilityPolicyScope::Name {
        value: "x".repeat(MAX_CAPABILITY_POLICY_BYTES - overhead + 1),
    }];
    assert!(draft.validate_shape().is_err());
    for value in ["\"".repeat(9000), "\u{e9}".repeat(9000)] {
        draft.rules[0].scopes = vec![CapabilityPolicyScope::Name { value }];
        assert!(serde_json::to_vec(&draft).unwrap().len() > MAX_CAPABILITY_POLICY_BYTES);
        assert!(draft.validate_shape().is_err());
    }
}

#[test]
fn capability_policy_acknowledgements_preserve_exact_rule_meaning_across_ordering() {
    let mut original = policy();
    original.rules[0].scopes.push(CapabilityPolicyScope::Path {
        value: "/notes/**".into(),
    });
    original.rules.push(CapabilityPolicyRule {
        verb: "fs.delete".into(),
        mode: CapabilityPolicyMode::Deny,
        scopes: Vec::new(),
    });
    let request = ActivityCapabilityPolicySetRequest {
        expected_revision: Some(original.revision),
        policy: CapabilityPolicyDraft {
            rules: original.rules.clone(),
        },
    };
    let before = request.clone();
    let mut reply = original.clone();
    reply.revision += 1;
    reply.rules[0].scopes.reverse();
    reply.rules.reverse();
    assert!(reply.matches_set(&ACTIVITY.to_uppercase(), &request));
    assert!(reply.preserves_identity(&original));
    assert_eq!(request, before);
    for mutation in 0..6 {
        let mut changed = reply.clone();
        match mutation {
            0 => changed.rules[0].mode = CapabilityPolicyMode::Normal,
            1 => changed.rules.pop().map(|_| ()).unwrap(),
            2 => {
                changed.rules[1].scopes = vec![CapabilityPolicyScope::Path {
                    value: "/elsewhere/**".into(),
                }]
            }
            3 => changed.rules[1].verb = "fs.write".into(),
            4 => {
                changed.rules[1].scopes[0] = CapabilityPolicyScope::Name {
                    value: "/notes/**".into(),
                }
            }
            _ => changed.rules[1].scopes = vec![CapabilityPolicyScope::Wild {}],
        }
        assert!(!changed.matches_set(ACTIVITY, &request));
    }
}

#[test]
fn capability_policy_acknowledgements_allow_only_documented_host_and_duplicate_normalization() {
    let mut previous = policy();
    previous.rules = vec![CapabilityPolicyRule {
        verb: "net.dial".into(),
        mode: CapabilityPolicyMode::RequireApproval,
        scopes: vec![
            CapabilityPolicyScope::Host {
                value: "EXAMPLE.ORG:443".into(),
            },
            CapabilityPolicyScope::Host {
                value: "example.org:443".into(),
            },
        ],
    }];
    let request = ActivityCapabilityPolicySetRequest {
        expected_revision: Some(previous.revision),
        policy: CapabilityPolicyDraft {
            rules: previous.rules.clone(),
        },
    };
    let mut reply = previous;
    reply.revision += 1;
    reply.rules[0].scopes = vec![CapabilityPolicyScope::Host {
        value: "example.org:443".into(),
    }];
    assert!(reply.matches_set(ACTIVITY, &request));
    reply.rules[0].scopes = vec![CapabilityPolicyScope::Host {
        value: "example.org:8443".into(),
    }];
    assert!(!reply.matches_set(ACTIVITY, &request));
    for (submitted, returned) in [
        (
            CapabilityPolicyScope::Path {
                value: "/Workspace/**".into(),
            },
            CapabilityPolicyScope::Path {
                value: "/workspace/**".into(),
            },
        ),
        (
            CapabilityPolicyScope::Name {
                value: " Project/* ".into(),
            },
            CapabilityPolicyScope::Name {
                value: "Project/*".into(),
            },
        ),
        (
            CapabilityPolicyScope::SelfRef {
                value: "self.A".into(),
            },
            CapabilityPolicyScope::SelfRef {
                value: "self.a".into(),
            },
        ),
    ] {
        let mut request = request.clone();
        request.policy.rules[0].scopes = vec![submitted];
        reply.rules[0].scopes = vec![returned];
        assert!(!reply.matches_set(ACTIVITY, &request));
    }
    let mut controls = CapabilityPolicyDraft {
        rules: vec![rule()],
    };
    controls.rules[0].scopes = vec![CapabilityPolicyScope::Name {
        value: "a\nb".into(),
    }];
    assert!(controls.validate_shape().is_err());
}

#[test]
fn capability_policy_identity_cas_and_enabled_state_are_checked() {
    let mut reply = policy();
    assert!(reply.matches_owner(ACTIVITY, 1000));
    assert!(!reply.matches_owner(OTHER, 1000));
    assert!(!reply.matches_owner(ACTIVITY, 1001));
    let initial = ActivityCapabilityPolicySetRequest {
        expected_revision: None,
        policy: CapabilityPolicyDraft {
            rules: reply.rules.clone(),
        },
    };
    reply.revision = 1;
    reply.enabled = true;
    assert!(reply.matches_set(ACTIVITY, &initial));
    reply.enabled = false;
    assert!(!reply.matches_set(ACTIVITY, &initial));
    let toggle = ActivityCapabilityPolicyEnabledRequest {
        expected_revision: 5,
        enabled: false,
    };
    reply.revision = 6;
    assert!(reply.matches_enabled(ACTIVITY, &toggle));
    reply.revision = 7;
    assert!(!reply.matches_enabled(ACTIVITY, &toggle));
    for revision in [0, u64::MAX] {
        assert!(
            ActivityCapabilityPolicyEnabledRequest {
                expected_revision: revision,
                enabled: false
            }
            .validate_shape()
            .is_err()
        );
    }
    let request = ActivityCapabilityPolicySetRequest {
        expected_revision: Some(u64::MAX - 1),
        policy: CapabilityPolicyDraft::default(),
    };
    assert_eq!(
        serde_json::to_value(&request).unwrap()["expected_revision"].as_u64(),
        Some(u64::MAX - 1)
    );
    assert!(request.validate_shape().is_ok());
}

#[test]
fn capability_policy_invalid_responses_do_not_become_unconfigured_or_authorized() {
    for (field, value) in [
        ("owner_uid", json!(-1)),
        ("enabled", Value::Null),
        ("revision", json!("1")),
    ] {
        let mut data = serde_json::to_value(policy()).unwrap();
        data[field] = value;
        assert!(serde_json::from_value::<ActivityCapabilityPolicy>(data).is_err());
    }
    for field in [
        "rules",
        "owner_uid",
        "revision",
        "enabled",
        "created_at",
        "updated_at",
    ] {
        let mut data = serde_json::to_value(policy()).unwrap();
        data.as_object_mut().unwrap().remove(field);
        assert!(serde_json::from_value::<ActivityCapabilityPolicy>(data).is_err());
    }
    let mut response = ActivityCapabilityPolicyResponse {
        schema: 2,
        activity_id: ACTIVITY.into(),
        capability_policy: Some(policy()),
    };
    assert!(!response.matches_owner(ACTIVITY, 1000));
    response.schema = 1;
    response.capability_policy.as_mut().unwrap().revision = 0;
    assert!(!response.matches_owner(ACTIVITY, 1000));
}
