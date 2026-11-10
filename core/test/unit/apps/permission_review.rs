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
            effects: Vec::new(),
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

#[test]
fn gui_review_discloses_each_execution_target_purpose_and_unbound_need() {
    let mut value = serde_json::to_value(manifest()).unwrap();
    value["runtime"] = json!("binary");
    value["entry"] = json!("bin/command");
    value["desktop"] = json!({
        "entry":"bin/window","exec":"open","needs":[],
        "entries":{"applet":{"entry":"bin/widget","exec":"panel open","name":{"en":"Independent widget"},"needs":[{
            "verb":"sys.observe","scope":{"kind":"fixed","scope":{"kind":"name","value":"audio"}},
            "why":{"en":"Show widget audio status"}
        },{
            "verb":"fs.read","scope":{"kind":"from-arg","arg":"path"},"why":{"en":"Unbound widget file"}
        }]}}
    });
    let original = Manifest::from_json(&value.to_string()).unwrap();
    let review = PermissionReview::from_manifest(&original).unwrap();
    assert_eq!(review.gui_entries.len(), 2);
    assert!(review.gui_entries[0].needs.is_empty());
    assert_eq!(review.gui_entries[1].entry, "bin/widget");
    assert_eq!(review.gui_entries[1].exec, "panel open");
    assert_eq!(review.gui_entries[1].unbound_needs, [1]);
    let permission = review.permissions.iter().find(|permission| permission.verb == Verb::SYS_OBSERVE).unwrap();
    assert_eq!(permission.uses.len(), 1);
    assert_eq!(permission.uses[0].entry, "gui:applet");
    assert_eq!(permission.uses[0].purpose.en_str(), "Show widget audio status");
    let text = review.format_for_review().unwrap();
    assert!(text.contains("bin/window") && text.contains("bin/widget"));
    assert!(text.contains("Show widget audio status") && text.contains("Unsupported/unbound"));
    for field in ["entry", "exec", "needs"] {
        let mut changed = value.clone();
        changed["desktop"]["entries"]["applet"][field] = match field {
            "entry" => json!("bin/other-widget"),
            "exec" => json!("other literal selector"),
            _ => json!([]),
        };
        let changed = Manifest::from_json(&changed.to_string()).unwrap();
        assert_ne!(review.contract_digest, PermissionReview::from_manifest(&changed).unwrap().contract_digest, "{field}");
    }
    assert!(!review.permissions_granted);
}

#[test]
fn legacy_desktop_review_digest_remains_compatible_when_new_fields_are_absent() {
    let mut manifest = manifest();
    manifest.desktop = Some(serde_json::from_value(json!({"exec":"open"})).unwrap());
    let review = PermissionReview::from_manifest(&manifest).unwrap();
    let mut legacy_contract = review.contract();
    legacy_contract["desktop"].as_object_mut().unwrap().remove("entry");
    legacy_contract["desktop"].as_object_mut().unwrap().remove("needs");
    legacy_contract["desktop"].as_object_mut().unwrap().remove("entries");
    assert!(legacy_contract.get("gui_entries").is_none());
    let digest = format!("sha256:{:x}", Sha256::digest(serde_json::to_vec(&legacy_contract).unwrap()));
    assert_eq!(review.contract_digest, digest);
    assert!(review.gui_entries[0].legacy_operation_needs);
}

#[test]
fn owner_path_review_retains_declared_purpose_and_displays_root_resolved_scope_without_granting() {
    let manifest = Manifest::from_json(r#"{
        "id":"owner-review","version":"1","name":{"en":"Owner review"},
        "desktop":{"needs":[{"verb":"fs.delete","scope":{"kind":"owner-path","path":"Desktop/**"},
            "why":{"en":"Delete the owner's selected desktop files"}}]}
    }"#).unwrap();
    let mut review = PermissionReview::from_manifest(&manifest).unwrap();
    let digest = review.contract_digest.clone();
    assert!(review.permissions[0].resolved_scope.is_none());
    assert!(review.gui_entries[0].unbound_needs.is_empty());
    review.resolve_owner_paths(&crate::caps::args::PathContext { home:"/home/alice".into(), cwd:None }).unwrap();
    assert_eq!(review.permissions[0].resolved_scope, Some(Scope::path("/home/alice/Desktop/**")));
    assert_eq!(review.permissions[0].uses[0].purpose.en_str(), "Delete the owner's selected desktop files");
    assert_eq!(review.contract_digest, digest);
    assert!(!review.permissions_granted);
}

#[test]
fn optional_gui_review_preserves_purpose_classification_and_contract_digest() {
    let mut value = serde_json::json!({
        "id":"optional-review","version":"1","name":{"en":"Optional review"},
        "desktop":{"needs":[{"verb":"ui.panel","scope":{"kind":"fixed","scope":{"kind":"wild"}},"why":{"en":"Required panel display"}}],
            "optional_needs":[{"verb":"sys.identity","scope":{"kind":"fixed","scope":{"kind":"name","value":"accounts"}},"why":{"en":"Optional management"}}],
            "entries":{"applet":{"entry":"widget.py","exec":"panel","name":{"en":"Widget"},"needs":[],
                "optional_needs":[{"verb":"fs.read","scope":{"kind":"from-arg","arg":"path"},"why":{"en":"Unbound optional selection"}}]}}
        }
    });
    let review = PermissionReview::from_manifest(&Manifest::from_json(&value.to_string()).unwrap()).unwrap();
    let management = review.permissions.iter().find(|permission| permission.verb == Verb::SYS_IDENTITY).unwrap();
    assert_eq!(management.uses[0].entry, "gui:(default):optional");
    assert!(management.uses[0].optional);
    assert_eq!(management.uses[0].purpose.en_str(), "Optional management");
    assert_eq!(review.gui_entries[0].needs[0].verb, Verb::UI_PANEL);
    assert_eq!(review.gui_entries[1].unbound_optional_needs, [0]);
    assert!(review.format_for_review().unwrap().contains("startup continues"));
    assert!(!review.permissions_granted);
    value["desktop"]["optional_needs"] = json!([]);
    let changed = PermissionReview::from_manifest(&Manifest::from_json(&value.to_string()).unwrap()).unwrap();
    assert_ne!(review.contract_digest, changed.contract_digest);
    let required = value["desktop"]["needs"][0].clone();
    value["desktop"]["needs"] = json!([]);
    value["desktop"]["optional_needs"] = json!([required]);
    let relaxed = PermissionReview::from_manifest(&Manifest::from_json(&value.to_string()).unwrap()).unwrap();
    assert_ne!(changed.contract_digest, relaxed.contract_digest);
}

#[test]
fn a_gui_named_optional_does_not_make_its_required_permissions_optional() {
    let manifest = Manifest::from_json(r#"{
        "id":"required-widget","version":"1","name":{"en":"Widget"},
        "desktop":{"needs":[],"entries":{"optional":{
            "entry":"widget.py","exec":"panel","name":{"en":"Widget"},"needs":[{
                "verb":"fs.read","scope":{"kind":"owner-path","path":"Documents/**"},"why":{"en":"Required documents"}
            }]
        }}}
    }"#).unwrap();
    let mut review = PermissionReview::from_manifest(&manifest).unwrap();
    assert_eq!(review.permissions[0].uses[0].entry, "gui:optional");
    assert!(!review.permissions[0].uses[0].optional);
    assert!(review.resolve_owner_paths(&crate::caps::args::PathContext { home: "/".into(), cwd: None }).is_err());
}
