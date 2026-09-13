use super::*;

const ID: &str = "00000000-0000-4000-8000-000000000001";
const DRAFT: &str = r#"{"rules":[{"verb":"fs.delete","mode":"deny","scopes":[]}]}"#;

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

#[test]
fn capability_policy_commands_preserve_shared_shapes_and_explicit_revisions() {
    let (command, params) = parse("capability-policy", &args(&[ID])).unwrap();
    assert_eq!(command, Command::ActivityCapabilityPolicyGet);
    assert_eq!(params, json!({"id": ID}));

    for revision in [None, Some("2")] {
        let mut values = args(&[ID, "--policy", DRAFT]);
        if let Some(revision) = revision {
            values.extend(args(&["--expected-revision", revision]));
        }
        let (command, params) = parse("set-capability-policy", &values).unwrap();
        assert_eq!(command, Command::ActivityCapabilityPolicySet);
        assert_eq!(params["id"], ID);
        assert_eq!(
            params["policy"],
            serde_json::from_str::<Value>(DRAFT).unwrap()
        );
        assert!(params.get("owner_uid").is_none());
        assert!(params.get("enabled").is_none());
        if revision.is_some() {
            assert_eq!(params["expected_revision"], 2);
        } else {
            assert!(params.get("expected_revision").is_none());
        }
    }

    for (name, enabled) in [
        ("enable-capability-policy", true),
        ("disable-capability-policy", false),
    ] {
        let (command, params) = parse(name, &args(&[ID, "--expected-revision", "3"])).unwrap();
        assert_eq!(command, Command::ActivityCapabilityPolicyEnabled);
        assert_eq!(
            params,
            json!({"id": ID, "expected_revision": 3, "enabled": enabled})
        );
    }
}

#[test]
fn capability_policy_drafts_use_bounded_closed_broker_validation() {
    for draft in [
        json!({"rules": []}),
        json!({"rules": [{
            "verb": "fs.read", "mode": "normal",
            "scopes": [{"kind": "path", "value": "/home/user/project/**"}],
        }]}),
        json!({"rules": [{
            "verb": "net.dial", "mode": "require_approval",
            "scopes": [{"kind": "host", "value": "example.com:443"}],
        }]}),
    ] {
        assert!(parse(
            "set-capability-policy",
            &args(&[ID, "--policy", &draft.to_string()])
        )
        .is_ok());
    }
    for draft in [
        json!([]),
        json!({"rules": [], "enabled": false}),
        json!({"rules": [], "owner_uid": 0}),
        json!({"rules": [{"verb": "not.a.capability", "mode": "deny", "scopes": []}]}),
        json!({"rules": [{"verb": "fs.read", "mode": "allow", "scopes": []}]}),
        json!({"rules": [{"verb": "fs.read", "mode": "normal", "scopes": []}]}),
        json!({"rules": [{"verb": "fs.delete", "mode": "deny",
            "scopes": [{"kind": "path", "value": "/home/user/**"}]}]}),
        json!({"rules": [{"verb": "fs.read", "mode": "normal", "scopes": [{"kind": "wild"}]}]}),
        json!({"rules": [{"verb": "net.dial", "mode": "normal",
            "scopes": [{"kind": "path", "value": "/home/user/**"}]}]}),
        json!({"rules": [{"verb": "fs.read", "mode": "normal",
            "scopes": [{"kind": "path", "value": "/home/user/\nsecret"}]}]}),
        json!({"rules": [
            {"verb": "fs.delete", "mode": "deny", "scopes": []},
            {"verb": "fs.delete", "mode": "deny", "scopes": []},
        ]}),
        json!({"rules": vec![json!({"verb": "fs.delete", "mode": "deny", "scopes": []}); 65]}),
        json!({"rules": [{"verb": "fs.read", "mode": "normal",
            "scopes": vec![json!({"kind": "path", "value": "/home/user/**"}); 33]}]}),
    ] {
        assert!(
            parse(
                "set-capability-policy",
                &args(&[ID, "--policy", &draft.to_string()])
            )
            .is_err(),
            "{draft}"
        );
    }
}

#[test]
fn capability_policy_flags_and_raw_json_fail_before_broker_access() {
    for values in [
        args(&[]),
        args(&["../not-an-activity", "--policy", DRAFT]),
        args(&[ID]),
        args(&[ID, "--policy"]),
        args(&[ID, "--policy", "{"]),
        args(&[ID, "--policy", DRAFT, "--policy", DRAFT]),
        args(&[ID, "--policy", DRAFT, "--owner", "0"]),
        args(&[ID, "--policy", DRAFT, "--enabled", "true"]),
        args(&[ID, "--policy", DRAFT, "--revision", "1"]),
        args(&[ID, "--policy", DRAFT, "--expected-revision", "0"]),
        args(&[ID, "--policy", DRAFT, "--expected-revision", "-1"]),
        args(&[
            ID,
            "--policy",
            DRAFT,
            "--expected-revision",
            "18446744073709551616",
        ]),
        args(&[
            ID,
            "--policy",
            DRAFT,
            "--expected-revision",
            "1",
            "--expected-revision",
            "1",
        ]),
        args(&[ID, "--policy", &format!("{DRAFT}{}", " ".repeat(16 * 1024))]),
    ] {
        assert!(
            parse("set-capability-policy", &values).is_err(),
            "{values:?}"
        );
    }
    assert!(parse("capability-policy", &args(&[ID, "--policy", DRAFT])).is_err());
    assert!(parse("enable-capability-policy", &args(&[ID])).is_err());
    assert!(parse(
        "disable-capability-policy",
        &args(&[ID, "--expected-revision", "0"])
    )
    .is_err());
    assert!(super::super::parse("capability-policy", &args(&[ID])).is_ok());
}
