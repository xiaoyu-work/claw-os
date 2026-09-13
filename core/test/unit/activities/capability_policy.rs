use super::*;
use serde_json::json;

fn rule(verb: Verb, mode: CapabilityRuleMode, scopes: Vec<Scope>) -> ActivityCapabilityRule {
    ActivityCapabilityRule { verb, mode, scopes }
}

fn draft(rules: Vec<ActivityCapabilityRule>) -> CapabilityPolicyDraft {
    CapabilityPolicyDraft { rules }
}

fn policy(rules: Vec<ActivityCapabilityRule>) -> ActivityCapabilityPolicy {
    ActivityCapabilityPolicy {
        activity_id: uuid::Uuid::new_v4().to_string(),
        owner_uid: 7,
        revision: 1,
        enabled: true,
        rules: draft(rules).canonicalized().unwrap().rules,
        created_at: "2026-01-01T00:00:00.000000000Z".into(),
        updated_at: "2026-01-01T00:00:00.000000000Z".into(),
    }
}

#[test]
fn rule_and_decision_modes_have_only_the_frozen_non_authorizing_wire_values() {
    for (mode, decision, wire) in [
        (
            CapabilityRuleMode::Normal,
            CapabilityBoundaryDecision::Normal,
            "normal",
        ),
        (
            CapabilityRuleMode::RequireApproval,
            CapabilityBoundaryDecision::RequireApproval,
            "require_approval",
        ),
        (
            CapabilityRuleMode::Deny,
            CapabilityBoundaryDecision::Deny,
            "deny",
        ),
    ] {
        assert_eq!(serde_json::to_value(mode).unwrap(), wire);
        assert_eq!(serde_json::to_value(decision).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<CapabilityRuleMode>(json!(wire)).unwrap(),
            mode
        );
        assert_eq!(
            serde_json::from_value::<CapabilityBoundaryDecision>(json!(wire)).unwrap(),
            decision
        );
    }
    for invalid in ["allow", "authorized", "confirmed", "grant", "NORMAL"] {
        assert!(serde_json::from_value::<CapabilityRuleMode>(json!(invalid)).is_err());
        assert!(serde_json::from_value::<CapabilityBoundaryDecision>(json!(invalid)).is_err());
    }
}

#[test]
fn drafts_and_rules_reject_unknown_fields_missing_fields_and_unknown_catalog_verbs() {
    let original = draft(vec![rule(
        Verb::FS_DELETE,
        CapabilityRuleMode::Deny,
        vec![],
    )]);
    for field in [
        "owner_uid",
        "source",
        "revision",
        "enabled",
        "created_at",
        "grant",
        "approved",
    ] {
        let mut value = serde_json::to_value(&original).unwrap();
        value[field] = json!("forged");
        assert!(
            serde_json::from_value::<CapabilityPolicyDraft>(value).is_err(),
            "{field}"
        );
        let mut value = serde_json::to_value(&original.rules[0]).unwrap();
        value[field] = json!("forged");
        assert!(
            serde_json::from_value::<ActivityCapabilityRule>(value).is_err(),
            "rule {field}"
        );
    }
    assert!(serde_json::from_value::<CapabilityPolicyDraft>(json!({})).is_err());
    assert!(serde_json::from_value::<CapabilityPolicyDraft>(json!({"rules":null})).is_err());
    for field in ["verb", "mode", "scopes"] {
        let mut value = serde_json::to_value(&original.rules[0]).unwrap();
        value.as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<ActivityCapabilityRule>(value).is_err(),
            "{field}"
        );
    }
    for verb in ["unknown.verb", "FS.WRITE", "fs.write\0", "*"] {
        let mut value = serde_json::to_value(&original).unwrap();
        value["rules"][0]["verb"] = json!(verb);
        assert!(
            serde_json::from_value::<CapabilityPolicyDraft>(value).is_err(),
            "{verb}"
        );
    }
}

#[test]
fn bounds_are_exact_for_rules_scopes_and_serialized_utf8_bytes() {
    assert!(crate::caps::CATALOG.len() > MAX_RULES);
    let mut many = draft(
        crate::caps::CATALOG
            .iter()
            .take(MAX_RULES)
            .map(|meta| rule(meta.verb, CapabilityRuleMode::Deny, vec![]))
            .collect(),
    );
    many.validate().unwrap();
    many.rules.push(rule(
        crate::caps::CATALOG[MAX_RULES].verb,
        CapabilityRuleMode::Deny,
        vec![],
    ));
    assert!(many.validate().is_err());
    let mut scopes = draft(vec![rule(
        Verb::SECRET_READ,
        CapabilityRuleMode::Normal,
        (0..MAX_SCOPES)
            .map(|index| Scope::name(format!("namespace/key-{index}")))
            .collect(),
    )]);
    scopes.validate().unwrap();
    scopes.rules[0].scopes.push(Scope::name("namespace/extra"));
    assert!(scopes.validate().is_err());

    let mut sized = draft(vec![rule(
        Verb::SECRET_READ,
        CapabilityRuleMode::Normal,
        vec![Scope::name("")],
    )]);
    let overhead = serde_json::to_vec(&sized).unwrap().len();
    let available = MAX_POLICY_BYTES - overhead;
    let value = format!(
        "{}{}",
        "\u{e9}".repeat(available / 2),
        "x".repeat(available % 2)
    );
    sized.rules[0].scopes = vec![Scope::name(value.clone())];
    assert_eq!(serde_json::to_vec(&sized).unwrap().len(), 16 * 1024);
    sized.validate().unwrap();
    sized.rules[0].scopes = vec![Scope::name(format!("{value}x"))];
    assert!(sized.validate().is_err());
    sized.rules[0].scopes = vec![Scope::name("\"".repeat(available))];
    assert!(
        sized.validate().is_err(),
        "JSON escaping also counts toward the serialized limit"
    );
}

#[test]
fn duplicate_verbs_and_inconsistent_deny_or_non_deny_scope_lists_are_invalid() {
    for mode in [
        CapabilityRuleMode::Normal,
        CapabilityRuleMode::RequireApproval,
    ] {
        assert!(draft(vec![rule(Verb::FS_WRITE, mode, vec![])])
            .validate()
            .is_err());
    }
    assert!(draft(vec![rule(
        Verb::FS_WRITE,
        CapabilityRuleMode::Deny,
        vec![Scope::path("/drafts/**")]
    )])
    .validate()
    .is_err());
    let original = rule(
        Verb::FS_WRITE,
        CapabilityRuleMode::Normal,
        vec![Scope::path("/drafts/**")],
    );
    for duplicate in [
        original.clone(),
        rule(Verb::FS_WRITE, CapabilityRuleMode::Deny, vec![]),
    ] {
        assert!(draft(vec![original.clone(), duplicate]).validate().is_err());
    }
    let empty = draft(vec![]);
    empty.validate().unwrap();
    assert_eq!(serde_json::to_value(empty).unwrap(), json!({"rules":[]}));
}

#[test]
fn raw_wild_and_mismatched_scopes_cannot_widen_resource_addressing_verbs() {
    for (verb, valid) in [
        (Verb::FS_WRITE, Scope::path("/drafts/**")),
        (Verb::NET_DIAL, Scope::host("*.example.invalid:443")),
        (Verb::SECRET_READ, Scope::name("namespace/**")),
    ] {
        draft(vec![rule(verb, CapabilityRuleMode::Normal, vec![valid])])
            .validate()
            .unwrap();
        assert!(draft(vec![rule(
            verb,
            CapabilityRuleMode::Normal,
            vec![Scope::Wild]
        )])
        .validate()
        .is_err());
    }
    for (verb, scope) in [
        (Verb::FS_WRITE, Scope::host("example.invalid")),
        (Verb::NET_DIAL, Scope::name("example.invalid")),
        (Verb::SECRET_READ, Scope::path("/secrets/**")),
        (Verb::PROC_SIGNAL, Scope::name("self")),
        (Verb::UI_NOTIFY, Scope::self_ref("self")),
    ] {
        assert!(
            draft(vec![rule(verb, CapabilityRuleMode::Normal, vec![scope])])
                .validate()
                .is_err()
        );
    }
    for (verb, scope) in [
        (Verb::UI_NOTIFY, Scope::Wild),
        (Verb::PROC_SIGNAL, Scope::Wild),
        (Verb::PROC_SIGNAL, Scope::self_ref("self.children")),
    ] {
        draft(vec![rule(
            verb,
            CapabilityRuleMode::RequireApproval,
            vec![scope],
        )])
        .validate()
        .unwrap();
    }
}

#[test]
fn control_bearing_and_noncanonical_path_scopes_are_rejected_without_expansion() {
    for control in ['\0', '\n', '\r', '\t', '\u{1b}', '\u{7f}', '\u{85}'] {
        for (verb, scope) in [
            (Verb::FS_WRITE, Scope::path(format!("/drafts/{control}"))),
            (
                Verb::NET_DIAL,
                Scope::host(format!("example.invalid{control}")),
            ),
            (
                Verb::SECRET_READ,
                Scope::name(format!("namespace/{control}")),
            ),
            (Verb::PROC_SIGNAL, Scope::self_ref(format!("self{control}"))),
        ] {
            assert!(
                draft(vec![rule(verb, CapabilityRuleMode::Normal, vec![scope])])
                    .validate()
                    .is_err()
            );
        }
    }
    for path in [
        "",
        "**",
        "relative/**",
        "~/drafts/**",
        "$HOME/drafts/**",
        "/$HOME/drafts/**",
        "/drafts/../private/**",
        "/drafts/./file",
        "/drafts//file",
        "/drafts/",
    ] {
        assert!(
            draft(vec![rule(
                Verb::FS_WRITE,
                CapabilityRuleMode::Normal,
                vec![Scope::path(path)]
            )])
            .validate()
            .is_err(),
            "{path}"
        );
    }
    for path in [
        "/",
        "/**",
        "/drafts/**",
        "/drafts/file with spaces",
        "/drafts/\u{e9}",
    ] {
        draft(vec![rule(
            Verb::FS_WRITE,
            CapabilityRuleMode::Normal,
            vec![Scope::path(path)],
        )])
        .validate()
        .unwrap();
    }
}

#[test]
fn canonicalization_sorts_rules_and_scopes_and_only_folds_existing_host_case_equivalence() {
    let input = draft(vec![
        rule(
            Verb::NET_DIAL,
            CapabilityRuleMode::Normal,
            vec![
                Scope::host("B.EXAMPLE.INVALID:443"),
                Scope::host("a.example.invalid:443"),
                Scope::host("b.example.invalid:443"),
                Scope::host("b.example.invalid:0443"),
            ],
        ),
        rule(
            Verb::FS_WRITE,
            CapabilityRuleMode::RequireApproval,
            vec![
                Scope::path("/z/**"),
                Scope::path("/a/**"),
                Scope::path("/z/**"),
            ],
        ),
        rule(
            Verb::SECRET_READ,
            CapabilityRuleMode::Normal,
            vec![Scope::name("Key"), Scope::name("key")],
        ),
    ]);
    let canonical = input.canonicalized().unwrap();
    assert_eq!(
        canonical
            .rules
            .iter()
            .map(|rule| rule.verb)
            .collect::<Vec<_>>(),
        vec![Verb::FS_WRITE, Verb::NET_DIAL, Verb::SECRET_READ]
    );
    assert_eq!(
        canonical.rules[0].scopes,
        vec![Scope::path("/a/**"), Scope::path("/z/**")]
    );
    assert_eq!(
        canonical.rules[1].scopes,
        vec![
            Scope::host("a.example.invalid:443"),
            Scope::host("b.example.invalid:0443"),
            Scope::host("b.example.invalid:443"),
        ]
    );
    assert_eq!(
        canonical.rules[2].scopes,
        vec![Scope::name("Key"), Scope::name("key")]
    );
    assert_eq!(canonical.clone().canonicalized().unwrap(), canonical);
}

#[test]
fn absent_rules_normal_scopes_approval_scopes_and_disabled_policies_have_exact_decisions() {
    let write = Cap::new(Verb::FS_WRITE, Scope::path("/drafts/file"));
    assert_eq!(
        policy(vec![]).decision(&write),
        CapabilityBoundaryDecision::Normal
    );
    for (mode, expected) in [
        (
            CapabilityRuleMode::Normal,
            CapabilityBoundaryDecision::Normal,
        ),
        (
            CapabilityRuleMode::RequireApproval,
            CapabilityBoundaryDecision::RequireApproval,
        ),
    ] {
        let scoped = policy(vec![rule(
            Verb::FS_WRITE,
            mode,
            vec![Scope::path("/drafts/**")],
        )]);
        assert_eq!(scoped.decision(&write), expected);
        assert_eq!(
            scoped.decision(&Cap::new(Verb::FS_WRITE, Scope::path("/private/file"))),
            CapabilityBoundaryDecision::Deny
        );
        assert_eq!(
            scoped.decision(&Cap::new(Verb::FS_WRITE, Scope::host("drafts"))),
            CapabilityBoundaryDecision::Deny
        );
        assert_eq!(
            scoped.decision(&Cap::new(Verb::FS_READ, Scope::path("/private/file"))),
            CapabilityBoundaryDecision::Normal
        );
        let mut disabled = scoped;
        disabled.enabled = false;
        assert_eq!(disabled.decision(&write), CapabilityBoundaryDecision::Deny);
        assert_eq!(
            disabled.decision(&Cap::unscoped(Verb::UI_NOTIFY)),
            CapabilityBoundaryDecision::Deny
        );
    }
    let mut disabled_empty = policy(vec![]);
    disabled_empty.enabled = false;
    assert_eq!(
        disabled_empty.decision(&write),
        CapabilityBoundaryDecision::Deny
    );
}

#[test]
fn verb_denial_is_not_a_semantic_guarantee_against_other_writable_or_executable_capabilities() {
    let denied = policy(vec![rule(
        Verb::FS_DELETE,
        CapabilityRuleMode::Deny,
        vec![],
    )]);
    assert_eq!(
        denied.decision(&Cap::new(Verb::FS_DELETE, Scope::path("/drafts/file"))),
        CapabilityBoundaryDecision::Deny
    );
    for verb in [Verb::FS_WRITE, Verb::FS_EXEC] {
        assert_eq!(
            denied.decision(&Cap::new(verb, Scope::path("/drafts/file"))),
            CapabilityBoundaryDecision::Normal
        );
    }
}

#[test]
fn every_requested_scope_must_fit_one_rule_scope_and_symbolic_partial_covers_are_denied() {
    let cases = [
        (
            Verb::FS_WRITE,
            Scope::path("/drafts/*"),
            Scope::path("/drafts/**"),
            CapabilityBoundaryDecision::Deny,
        ),
        (
            Verb::FS_WRITE,
            Scope::path("/drafts/**"),
            Scope::path("/drafts/2026/**"),
            CapabilityBoundaryDecision::Normal,
        ),
        (
            Verb::FS_WRITE,
            Scope::path("/drafts/**"),
            Scope::path("/drafts-other/**"),
            CapabilityBoundaryDecision::Deny,
        ),
        (
            Verb::FS_WRITE,
            Scope::path("/**"),
            Scope::path("/drafts/*"),
            CapabilityBoundaryDecision::Normal,
        ),
        (
            Verb::SECRET_READ,
            Scope::name("*"),
            Scope::name("**"),
            CapabilityBoundaryDecision::Deny,
        ),
        (
            Verb::SECRET_READ,
            Scope::name("namespace/**"),
            Scope::name("namespace/item*"),
            CapabilityBoundaryDecision::Normal,
        ),
        (
            Verb::SECRET_READ,
            Scope::name("item*"),
            Scope::name("item*"),
            CapabilityBoundaryDecision::Normal,
        ),
        (
            Verb::SECRET_READ,
            Scope::name("item*"),
            Scope::name("item-sub*"),
            CapabilityBoundaryDecision::Deny,
        ),
        (
            Verb::NET_DIAL,
            Scope::host("*:443"),
            Scope::host("**:443"),
            CapabilityBoundaryDecision::Deny,
        ),
        (
            Verb::NET_DIAL,
            Scope::host("**:443"),
            Scope::host("*.example.invalid:443"),
            CapabilityBoundaryDecision::Normal,
        ),
        (
            Verb::NET_DIAL,
            Scope::host("**:443"),
            Scope::host("*.example.invalid:80"),
            CapabilityBoundaryDecision::Deny,
        ),
    ];
    for (verb, scope, requested, expected) in cases {
        let value = policy(vec![rule(verb, CapabilityRuleMode::Normal, vec![scope])]);
        assert_eq!(
            value.decision(&Cap::new(verb, requested.clone())),
            expected,
            "{requested:?}"
        );
    }
    let separate = policy(vec![rule(
        Verb::FS_WRITE,
        CapabilityRuleMode::Normal,
        vec![Scope::path("/drafts/a/**"), Scope::path("/drafts/b/**")],
    )]);
    assert_eq!(
        separate.decision(&Cap::new(Verb::FS_WRITE, Scope::path("/drafts/**"))),
        CapabilityBoundaryDecision::Deny
    );
}

#[test]
fn requested_globs_cannot_widen_path_or_host_scopes_through_normal_or_approval_modes() {
    let allowed_host = Scope::host("*.example.test");
    let wider_host = Scope::host("**.example.test");
    let counterexample = Scope::host("nested.sub.example.test");
    assert!(wider_host.covers(&counterexample));
    assert!(!allowed_host.covers(&counterexample));

    let cases = [
        (
            Verb::FS_WRITE,
            Scope::path("/workspace/*"),
            Scope::path("/workspace/**"),
            Scope::path("/workspace/file"),
        ),
        (
            Verb::NET_DIAL,
            allowed_host,
            wider_host,
            Scope::host("one.example.test"),
        ),
        (
            Verb::NET_DIAL,
            Scope::host("*.example.test:443"),
            Scope::host("**.example.test:443"),
            Scope::host("one.example.test:443"),
        ),
    ];
    for (mode, covered_decision) in [
        (
            CapabilityRuleMode::Normal,
            CapabilityBoundaryDecision::Normal,
        ),
        (
            CapabilityRuleMode::RequireApproval,
            CapabilityBoundaryDecision::RequireApproval,
        ),
    ] {
        for (verb, allowed, wider, literal) in &cases {
            let boundary = policy(vec![rule(*verb, mode, vec![allowed.clone()])]);
            assert_eq!(
                boundary.decision(&Cap::new(*verb, wider.clone())),
                CapabilityBoundaryDecision::Deny,
                "{mode:?}: {allowed:?} must fully contain {wider:?}"
            );
            assert_eq!(
                boundary.decision(&Cap::new(*verb, literal.clone())),
                covered_decision
            );
        }
    }
}

#[test]
fn crate_internal_containment_checks_catalog_kinds_canonical_paths_and_requested_globs() {
    let covers = crate::activities::capability_scope_covers;
    assert!(covers(
        Verb::FS_WRITE,
        &Scope::path("/workspace/**"),
        &Scope::path("/workspace/file"),
    ));
    assert!(!covers(
        Verb::FS_WRITE,
        &Scope::path("/workspace/*"),
        &Scope::path("/workspace/**"),
    ));
    assert!(!covers(
        Verb::NET_DIAL,
        &Scope::host("*.example.test"),
        &Scope::host("**.example.test"),
    ));
    assert!(!covers(
        Verb::FS_WRITE,
        &Scope::path("/workspace/**"),
        &Scope::path("/workspace/../private/file"),
    ));
    assert!(!covers(
        Verb::FS_WRITE,
        &Scope::name("**"),
        &Scope::path("/workspace/file"),
    ));
    assert!(!covers(
        Verb::FS_WRITE,
        &Scope::Wild,
        &Scope::path("/workspace/file"),
    ));
    assert!(covers(Verb::UI_NOTIFY, &Scope::Wild, &Scope::Wild));
    assert!(covers(
        Verb::PROC_SIGNAL,
        &Scope::Wild,
        &Scope::self_ref("self"),
    ));
    assert!(!covers(
        Verb::PROC_SIGNAL,
        &Scope::Wild,
        &Scope::path("/workspace/file"),
    ));
}

#[test]
fn pure_matching_preserves_path_root_host_ports_and_literal_self_scope_contracts() {
    for (pattern, target, expected) in [
        ("/", "/", CapabilityBoundaryDecision::Normal),
        ("/*", "/", CapabilityBoundaryDecision::Deny),
        ("/**", "/", CapabilityBoundaryDecision::Normal),
        ("/drafts/*", "/drafts/a", CapabilityBoundaryDecision::Normal),
        ("/drafts/*", "/drafts/a/b", CapabilityBoundaryDecision::Deny),
        ("/drafts/**", "/drafts", CapabilityBoundaryDecision::Normal),
        (
            "/drafts/**",
            "/drafts/a/b",
            CapabilityBoundaryDecision::Normal,
        ),
    ] {
        assert_eq!(
            policy(vec![rule(
                Verb::FS_WRITE,
                CapabilityRuleMode::Normal,
                vec![Scope::path(pattern)]
            )])
            .decision(&Cap::new(Verb::FS_WRITE, Scope::path(target))),
            expected,
            "{pattern} -> {target}"
        );
    }
    let host = policy(vec![rule(
        Verb::NET_DIAL,
        CapabilityRuleMode::RequireApproval,
        vec![Scope::host("*.example.invalid:443")],
    )]);
    assert_eq!(
        host.decision(&Cap::new(
            Verb::NET_DIAL,
            Scope::host("API.EXAMPLE.INVALID:443")
        )),
        CapabilityBoundaryDecision::RequireApproval
    );
    assert_eq!(
        host.decision(&Cap::new(
            Verb::NET_DIAL,
            Scope::host("api.example.invalid:80")
        )),
        CapabilityBoundaryDecision::Deny
    );
    let own = policy(vec![rule(
        Verb::PROC_SIGNAL,
        CapabilityRuleMode::Normal,
        vec![Scope::self_ref("self.children.*")],
    )]);
    assert_eq!(
        own.decision(&Cap::new(
            Verb::PROC_SIGNAL,
            Scope::self_ref("self.children.*")
        )),
        CapabilityBoundaryDecision::Normal
    );
    assert_eq!(
        own.decision(&Cap::new(
            Verb::PROC_SIGNAL,
            Scope::self_ref("self.children.1")
        )),
        CapabilityBoundaryDecision::Deny
    );
    let canonical_wild = policy(vec![rule(
        Verb::PROC_SIGNAL,
        CapabilityRuleMode::RequireApproval,
        vec![Scope::Wild],
    )]);
    assert_eq!(
        canonical_wild.decision(&Cap::new(Verb::PROC_SIGNAL, Scope::self_ref("self"))),
        CapabilityBoundaryDecision::RequireApproval
    );
    assert_eq!(
        canonical_wild.decision(&Cap::new(Verb::PROC_SIGNAL, Scope::name("self"))),
        CapabilityBoundaryDecision::Deny
    );
    assert_eq!(
        policy(vec![rule(
            Verb::UI_NOTIFY,
            CapabilityRuleMode::RequireApproval,
            vec![Scope::Wild]
        )])
        .decision(&Cap::unscoped(Verb::UI_NOTIFY)),
        CapabilityBoundaryDecision::RequireApproval
    );
}

#[test]
#[cfg(unix)]
fn decisions_do_not_resolve_filesystem_aliases_or_depend_on_app_or_credential_data() {
    use std::os::unix::fs::symlink;
    struct Directory(PathBuf);
    impl Drop for Directory {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).expect("remove capability policy test directory");
        }
    }
    let root =
        Directory(std::env::temp_dir().join(format!("capability-policy-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir_all(root.0.join("real")).unwrap();
    let alias = root.0.join("alias");
    let real = root.0.join("real");
    let boundary = policy(vec![rule(
        Verb::FS_WRITE,
        CapabilityRuleMode::Normal,
        vec![Scope::path(format!("{}/**", alias.display()))],
    )]);
    let request = Cap::new(
        Verb::FS_WRITE,
        Scope::path(format!("{}/file", real.display())),
    );
    assert_eq!(
        boundary.decision(&request),
        CapabilityBoundaryDecision::Deny
    );
    symlink(&real, &alias).unwrap();
    assert_eq!(
        boundary.decision(&request),
        CapabilityBoundaryDecision::Deny
    );
    std::fs::remove_file(&alias).unwrap();
    assert_eq!(
        boundary.decision(&request),
        CapabilityBoundaryDecision::Deny
    );
    let secrets = policy(vec![rule(
        Verb::SECRET_READ,
        CapabilityRuleMode::RequireApproval,
        vec![Scope::name("never-installed/never-created")],
    )]);
    assert_eq!(
        secrets.decision(&Cap::new(
            Verb::SECRET_READ,
            Scope::name("never-installed/never-created")
        )),
        CapabilityBoundaryDecision::RequireApproval
    );
}

#[test]
fn policy_output_has_only_the_frozen_server_fields_and_rules() {
    let value = policy(vec![rule(
        Verb::FS_DELETE,
        CapabilityRuleMode::Deny,
        vec![],
    )]);
    let encoded = serde_json::to_value(&value).unwrap();
    assert_eq!(
        encoded,
        json!({
            "activity_id":value.activity_id, "owner_uid":7, "revision":1, "enabled":true,
            "rules":[{"verb":"fs.delete","mode":"deny","scopes":[]}],
            "created_at":value.created_at, "updated_at":value.updated_at,
        })
    );
    assert_eq!(
        serde_json::from_value::<ActivityCapabilityPolicy>(encoded.clone()).unwrap(),
        value
    );
    let mut forged = encoded;
    forged["grant"] = json!("not authority");
    assert!(serde_json::from_value::<ActivityCapabilityPolicy>(forged).is_err());
}
