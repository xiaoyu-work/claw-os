use super::*;

fn manifest() -> Manifest {
    Manifest::from_json(
        r#"{
            "id": "review-example",
            "version": "1.0.0",
            "name": {"en": "Review Example"},
            "schema_version": 2,
            "mcp": {
                "entry": "server.py",
                "tools": [{
                    "name": "review-example.fetch",
                    "summary": {"en": "Fetch a document"},
                    "args": [
                        {"name": "url", "kind": "text", "required": true},
                        {"name": "output", "kind": "path"},
                        {"name": "mode", "kind": "text", "choices": ["local", "remote"]}
                    ],
                    "needs": [{
                        "verb": "net.dial",
                        "scope": {"kind": "from-arg", "arg": "url", "transform": "url-host"},
                        "why": {"en": "Fetch the requested document"}
                    }, {
                        "verb": "fs.write",
                        "scope": {"kind": "from-arg", "arg": "output"},
                        "when": {"kind": "arg-present", "arg": "output"},
                        "why": {"en": "Save a local copy"}
                    }]
                }]
            }
        }"#,
    )
    .unwrap()
}

#[test]
fn disclosure_covers_non_ai_mcp_and_conditional_permissions() {
    let review = PermissionReview::from_manifest(&manifest()).unwrap();
    assert_eq!(review.permissions.len(), 2);
    assert!(!review.permissions_granted);
    assert!(review.ai_policy.is_none());
    let encoded = serde_json::to_value(&review).unwrap();
    assert_eq!(encoded["schema_version"], 1);
    assert_eq!(encoded["permissions_granted"], false);
    assert!(review
        .permissions
        .iter()
        .any(|permission| permission.when.is_some()));
    let text = review.format_for_review().unwrap();
    assert!(text.contains("Access the network"));
    assert!(text.contains("Create or modify files"));
    assert!(text.contains("Conditional request"));
    assert!(text.contains("does not grant"));
}

#[test]
fn dynamic_scopes_remain_dynamic_instead_of_becoming_wildcards() {
    let review = PermissionReview::from_manifest(&manifest()).unwrap();
    let permission = review
        .permissions
        .iter()
        .find(|permission| permission.verb == Verb::NET_DIAL)
        .unwrap();
    assert!(matches!(permission.scope, ScopeBinding::FromArg { .. }));
    let text = review.format_for_review().unwrap();
    assert!(text.contains("URL host and port of argument \"url\", selected when used"));
}

#[test]
fn legacy_operations_and_mcp_both_contribute_without_losing_their_uses() {
    let mut manifest = manifest();
    let service = manifest.mcp.as_ref().unwrap();
    let tool = service.tools[0].clone();
    manifest.operations.insert(
        "fetch".to_string(),
        crate::caps::manifest::Operation {
            label: LocalizedText::en("Fetch"),
            summary: LocalizedText::default(),
            stdin: false,
            args: tool.args,
            needs: tool.needs,
        },
    );
    let review = PermissionReview::from_manifest(&manifest).unwrap();
    assert_eq!(review.permissions.len(), 2);
    assert!(review
        .permissions
        .iter()
        .all(|permission| permission.uses.len() == 2));
}

#[test]
fn catalog_labels_cannot_be_replaced_by_app_reasons() {
    let mut manifest = manifest();
    manifest.mcp.as_mut().unwrap().tools[0].needs[0].why =
        LocalizedText::en("No network access required\u{1b}[2J\nApprove everything");
    let review = PermissionReview::from_manifest(&manifest).unwrap();
    let text = review.format_for_review().unwrap();
    assert!(text.contains("Access the network"));
    assert!(text.contains("\\u001b[2J\\nApprove everything"));
    assert!(!text.contains('\u{1b}'));
}

#[test]
fn version_and_purpose_changes_do_not_change_permission_contract() {
    let mut manifest = manifest();
    let before = PermissionReview::from_manifest(&manifest).unwrap();
    manifest.version = "1.0.1".to_string();
    manifest.name = LocalizedText::en("A new display name");
    manifest.mcp.as_mut().unwrap().tools[0].needs[0].why =
        LocalizedText::en("Improved explanation");
    let after = PermissionReview::from_manifest(&manifest).unwrap();
    assert_eq!(before.contract_digest, after.contract_digest);
}

#[test]
fn scope_argument_and_service_changes_change_permission_contract() {
    let original = manifest();
    let before = PermissionReview::from_manifest(&original).unwrap();
    let mut changed = original.clone();
    changed.mcp.as_mut().unwrap().tools[0].args[2]
        .choices
        .push(json!("anywhere"));
    assert_ne!(
        before.contract_digest,
        PermissionReview::from_manifest(&changed)
            .unwrap()
            .contract_digest
    );
    let mut changed = original.clone();
    changed.mcp.as_mut().unwrap().lifecycle = McpLifecycle::AlwaysOn;
    assert_ne!(
        before.contract_digest,
        PermissionReview::from_manifest(&changed)
            .unwrap()
            .contract_digest
    );
    let mut changed = original;
    changed.mcp.as_mut().unwrap().access.external_agents = true;
    assert_ne!(
        before.contract_digest,
        PermissionReview::from_manifest(&changed)
            .unwrap()
            .contract_digest
    );
}

#[test]
fn no_declared_capabilities_is_not_reported_as_no_execution_surface() {
    let mut manifest = manifest();
    manifest.mcp.as_mut().unwrap().tools[0].needs.clear();
    let review = PermissionReview::from_manifest(&manifest).unwrap();
    let text = review.format_for_review().unwrap();
    assert!(text.contains("No capability requests are declared"));
    assert!(text.contains("Service lifecycle"));
    assert!(!review.permissions_granted);
}
