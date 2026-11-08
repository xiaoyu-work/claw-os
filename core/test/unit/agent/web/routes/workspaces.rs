use super::*;

#[test]
fn workspace_revisions_preserve_exact_sqlite_integer_precision() {
    let mut request = json!({"expected_revision":"9007199254740993"});
    decimal_revision(&mut request).unwrap();
    assert_eq!(
        request["expected_revision"].as_u64(),
        Some(9007199254740993)
    );
    assert_eq!(
        project(json!({"workspace":{"revision":9007199254740993u64},"notes":[{"revision":1}]})),
        json!({"workspace":{"revision":"9007199254740993"},"notes":[{"revision":"1"}]})
    );
}

#[test]
fn workspace_actions_refuse_authority_and_scope_selectors() {
    let body = json!({"request":{"action":"create","draft":{"name":"Research","owner_uid":1000}}});
    assert!(serde_json::from_value::<crate::workspaces::ControlRequest>(body).is_err());
    for revision in ["0", "01", "-1", "9223372036854775808"] {
        assert!(decimal_revision(&mut json!({"expected_revision":revision})).is_err());
    }
}
