use super::*;

fn parse(s: &str) -> Manifest {
    Manifest::from_json(s).expect("manifest should be valid")
}

#[test]
fn minimal_manifest_parses() {
    let m = parse(
        r#"{
              "id": "fs",
              "version": "0.2.0",
              "name": {"en": "Files"}
            }"#,
    );
    assert_eq!(m.id, "fs");
    assert_eq!(m.runtime, Runtime::Python);
    assert!(m.operations.is_empty());
}

#[test]
fn localized_manifest_text_requires_an_explicit_locale_map() {
    let error = Manifest::from_json(r#"{"id":"fs","version":"0.2.0","name":"Files"}"#).unwrap_err();
    assert!(matches!(error, ManifestError::Json(_)));
}

#[test]
fn invalid_id_rejected() {
    let err = Manifest::from_json(r#"{"id":"FS!","version":"0","name": {"en": "X"}}"#).unwrap_err();
    assert!(matches!(err, ManifestError::InvalidId(_)));
}

#[test]
fn unknown_verb_rejected_at_parse_time() {
    let err = Manifest::from_json(
        r#"{
              "id": "fs",
              "version": "0.1",
              "name": {"en": "Files"},
              "operations": {
                "x": {
                  "label": {"en": "X"},
                  "args": [],
                  "needs": [
                    {"verb": "fs.nonsense", "scope": {"kind":"wild"}, "why": {"en": "..."}}
                  ]
                }
              }
            }"#,
    )
    .unwrap_err();
    // Serde error, not validate(): the unknown verb is caught at
    // deserialization time by Verb's manual impl.
    assert!(matches!(err, ManifestError::Json(_)));
}

#[test]
fn need_referencing_undeclared_arg_rejected() {
    let err = Manifest::from_json(
        r#"{
              "id": "fs",
              "version": "0.1",
              "name": {"en": "Files"},
              "operations": {
                "rm": {
                  "label": {"en": "Delete"},
                  "args": [],
                  "needs": [
                    {"verb": "fs.delete", "scope": {"kind":"from-arg","arg":"path"}, "why": {"en": "y"}}
                  ]
                }
              }
            }"#,
    )
    .unwrap_err();
    match err {
        ManifestError::NeedRefsUndeclaredArg { op, idx, arg } => {
            assert_eq!(op, "rm");
            assert_eq!(idx, 0);
            assert_eq!(arg, "path");
        }
        other => panic!("expected NeedRefsUndeclaredArg, got {other:?}"),
    }
}

#[test]
fn need_binding_to_text_arg_rejected() {
    let err = Manifest::from_json(
        r#"{
              "id": "fs",
              "version": "0.1",
              "name": {"en": "Files"},
              "operations": {
                "rm": {
                  "label": {"en": "Delete"},
                  "args": [{"name": "path", "kind": "text"}],
                  "needs": [
                    {"verb": "fs.delete", "scope": {"kind":"from-arg","arg":"path"}, "why": {"en": "y"}}
                  ]
                }
              }
            }"#,
    )
    .unwrap_err();
    assert!(matches!(err, ManifestError::NeedArgKindMismatch { .. }));
}

#[test]
fn duplicate_arg_rejected() {
    let err = Manifest::from_json(
        r#"{
              "id": "fs",
              "version": "0.1",
              "name": {"en": "Files"},
              "operations": {
                "x": {
                  "label": {"en": "X"},
                  "args": [
                    {"name": "p", "kind": "path"},
                    {"name": "p", "kind": "path"}
                  ]
                }
              }
            }"#,
    )
    .unwrap_err();
    assert!(matches!(err, ManifestError::DuplicateArg { .. }));
}

#[test]
fn missing_english_in_top_level_name_rejected() {
    let err = Manifest::from_json(
        r#"{
              "id": "fs",
              "version": "0.1",
              "name": {"zh-CN": "文件"}
            }"#,
    )
    .unwrap_err();
    assert!(matches!(
        err,
        ManifestError::TopLevelTextInvalid { field: "name", .. }
    ));
}

#[test]
fn missing_english_in_op_label_rejected() {
    let err = Manifest::from_json(
        r#"{
              "id": "fs",
              "version": "0.1",
              "name": {"en": "Files"},
              "operations": {
                "ls": {
                  "label": {"zh-CN": "列表"}
                }
              }
            }"#,
    )
    .unwrap_err();
    assert!(matches!(
        err,
        ManifestError::LocalizedTextInvalid { field: "label", .. }
    ));
}

#[test]
fn resolve_needs_substitutes_runtime_arg_value() {
    let m = parse(
        r#"{
              "id": "fs",
              "version": "0.1",
              "name": {"en": "Files"},
              "operations": {
                "rm": {
                  "label": {"en": "Delete"},
                  "args": [{"name": "path", "kind": "path", "required": true}],
                  "needs": [
                    {"verb": "fs.delete",
                     "scope": {"kind":"from-arg","arg":"path"},
                     "why": {"en": "Remove the file you specified."}}
                  ]
                }
              }
            }"#,
    );
    let mut args = BTreeMap::new();
    args.insert("path".to_string(), serde_json::json!("/home/jay/x.md"));
    let caps = m.resolve_needs("rm", &args).unwrap();
    assert_eq!(caps.len(), 1);
    assert_eq!(caps[0][0].verb, Verb::FS_DELETE);
    assert_eq!(caps[0][0].scope, Scope::path("/home/jay/x.md"));
}

#[test]
fn resolve_needs_uses_literal_arg_default() {
    let manifest = parse(
        r#"{
              "id": "fs",
              "version": "0.1",
              "name": {"en": "Files"},
              "operations": {
                "search": {
                  "label": {"en": "Search"},
                  "args": [
                    {"name": "query", "kind": "text", "required": true},
                    {"name": "path", "kind": "path", "default": "/workspace"}
                  ],
                  "needs": [
                    {"verb": "fs.read",
                     "scope": {"kind":"from-arg","arg":"path"},
                     "why": {"en": "Search the default workspace."}}
                  ]
                }
              }
            }"#,
    );
    let mut args = BTreeMap::new();
    args.insert("query".to_string(), serde_json::json!("needle"));

    let caps = manifest.resolve_needs("search", &args).unwrap();

    assert_eq!(caps.len(), 1);
    assert_eq!(caps[0][0].verb, Verb::FS_READ);
    assert_eq!(caps[0][0].scope, Scope::path("/workspace"));
}

#[test]
fn removed_derived_defaults_are_rejected() {
    let error = Manifest::from_json(
        r#"{
              "id": "net",
              "version": "0.1",
              "name": {"en": "Network"},
              "operations": {
                "download": {
                  "label": {"en": "Download"},
                  "args": [
                    {"name": "url", "kind": "text", "required": true},
                    {"name": "output", "kind": "path",
                     "default_from": {"arg": "url"}}
                  ]
                }
              }
            }"#,
    )
    .unwrap_err();
    assert!(matches!(error, ManifestError::Json(_)));
}

#[test]
fn invalid_arg_defaults_are_rejected() {
    let wrong_type = Manifest::from_json(
        r#"{
              "id": "fs",
              "version": "0.1",
              "name": {"en": "Files"},
              "operations": {
                "ls": {
                  "label": {"en": "List"},
                  "args": [{"name": "path", "kind": "path", "default": 42}]
                }
              }
            }"#,
    )
    .unwrap_err();
    assert!(matches!(
        wrong_type,
        ManifestError::ArgDefaultInvalid { .. }
    ));

    let required_default = Manifest::from_json(
        r#"{
              "id": "net",
              "version": "0.1",
              "name": {"en": "Network"},
              "operations": {
                "download": {
                  "label": {"en": "Download"},
                  "args": [
                    {"name": "output", "kind": "path",
                     "required": true, "default": "/tmp/output"}
                  ]
                }
              }
            }"#,
    )
    .unwrap_err();
    assert!(matches!(
        required_default,
        ManifestError::ArgDefaultInvalid { .. }
    ));
}

#[test]
fn null_defaults_and_ambiguous_positionals_are_rejected() {
    for arg in [r#"{"name":"value","kind":"text","default":null}"#] {
        let body = format!(
            r#"{{
                "id":"defaults","version":"0.1","name": {{"en": "Defaults"}},
                "operations":{{"run":{{"label": {{"en": "Run"}},"args":[{arg}]}}}}
            }}"#
        );
        assert!(Manifest::from_json(&body).is_err());
    }

    let ambiguous = Manifest::from_json(
        r#"{
            "id":"ambiguous","version":"0.1","name": {"en": "Ambiguous"},
            "operations":{"run":{"label": {"en": "Run"},"args":[
                {"name":"destination","kind":"name"},
                {"name":"text","kind":"text","required":true}
            ]}}
        }"#,
    )
    .unwrap_err();
    assert!(matches!(ambiguous, ManifestError::ArgDefaultInvalid { .. }));
}

#[test]
fn mcp_tool_defaults_feed_arguments_and_capabilities() {
    let manifest = Manifest::from_json(
        r#"{
              "schema_version": 2,
            "id":"session-defaults","version":"0.1","name": {"en": "Session defaults"},
            "mcp":{"tools":[{
                "name":"session-defaults.read","summary": {"en": "Read"},
                "args":[
                    {"name":"key","kind":"name","default":"primary"},
                    {"name":"limit","kind":"integer","default":10}
                ],
                "needs":[{"verb":"data.kv.read","scope":{"kind":"from-arg","arg":"key"},"why": {"en": "Read"}}]
            }]}
        }"#,
    )
    .unwrap();
    let resolved = manifest
        .resolve_mcp_tool_args("session-defaults.read", &BTreeMap::new())
        .unwrap();
    assert_eq!(resolved["key"], serde_json::json!("primary"));
    assert_eq!(resolved["limit"], serde_json::json!(10));
    let caps = manifest
        .resolve_mcp_tool_needs("session-defaults.read", &BTreeMap::new())
        .unwrap();
    assert_eq!(caps[0][0].scope, Scope::name("primary"));
}

#[test]
fn fixed_path_scopes_reject_environment_placeholders() {
    let error = Manifest::from_json(
        r#"{
            "id":"placeholder","version":"0.1","name": {"en": "Placeholder"},
            "operations":{"read":{"label": {"en": "Read"},"needs":[{
                "verb":"fs.read",
                "scope":{"kind":"fixed","scope":{"kind":"path","value":"$HOME/data/**"}},
                "why": {"en": "Read"}
            }]}}
        }"#,
    )
    .unwrap_err();
    assert!(matches!(error, ManifestError::NeedInvalid { .. }));
}

#[test]
fn conditional_needs_skip_only_explicit_inactive_cases() {
    let manifest = Manifest::from_json(
        r#"{
            "id":"conditional","version":"0.1","name": {"en": "Conditional"},
            "operations":{"explain":{"label": {"en": "Explain"},"args":[
                {"name":"file","kind":"path","binding":"flag"},
                {"name":"provider","kind":"name","binding":"flag","default":"local"}
            ],"needs":[
                {"verb":"fs.read","scope":{"kind":"from-arg","arg":"file"},
                 "when":{"kind":"arg-present","arg":"file"},"why": {"en": "Read file"}},
                {"verb":"secret.read","scope":{"kind":"fixed","scope":{"kind":"name","value":"default/TOKEN"}},
                 "when":{"kind":"arg-equals","arg":"provider","value":"cloud"},"why": {"en": "Read token"}}
            ]}}
        }"#,
    )
    .unwrap();

    let local = manifest.resolve_needs("explain", &BTreeMap::new()).unwrap();
    assert_eq!(local, [Vec::new(), Vec::new()]);

    let mut cloud_file = BTreeMap::new();
    cloud_file.insert("file".to_string(), serde_json::json!("/workspace/a.txt"));
    cloud_file.insert("provider".to_string(), serde_json::json!("cloud"));
    let active = manifest.resolve_needs("explain", &cloud_file).unwrap();
    assert_eq!(active[0][0].scope, Scope::path("/workspace/a.txt"));
    assert_eq!(active[1][0].scope, Scope::name("default/TOKEN"));
}

#[test]
fn optional_capability_bindings_require_conditions() {
    let error = Manifest::from_json(
        r#"{
            "id":"unsafe","version":"0.1","name": {"en": "Unsafe"},
            "operations":{"read":{"label": {"en": "Read"},"args":[
                {"name":"file","kind":"path","binding":"flag"}
            ],"needs":[
                {"verb":"fs.read","scope":{"kind":"from-arg","arg":"file"},"why": {"en": "Read"}}
            ]}}
        }"#,
    )
    .unwrap_err();
    assert!(matches!(error, ManifestError::NeedInvalid { .. }));
}

#[test]
fn repeatable_scope_arguments_resolve_one_capability_per_value() {
    let manifest = Manifest::from_json(
        r#"{
            "id":"repeat","version":"0.1","name": {"en": "Repeat"},
            "operations":{"read":{"label": {"en": "Read"},"args":[
                {"name":"path","kind":"path","binding":"flag","repeatable":true}
            ],"needs":[
                {"verb":"fs.read","scope":{"kind":"from-arg","arg":"path"},
                 "when":{"kind":"arg-present","arg":"path"},"why": {"en": "Read paths"}}
            ]}}
        }"#,
    )
    .unwrap();
    let args = BTreeMap::from([(
        "path".to_string(),
        serde_json::json!(["/workspace/a", "/workspace/b"]),
    )]);
    let resolved = manifest.resolve_needs("read", &args).unwrap();
    assert_eq!(resolved.len(), 1);
    assert_eq!(
        resolved[0]
            .iter()
            .map(|cap| cap.scope.clone())
            .collect::<Vec<_>>(),
        [Scope::path("/workspace/a"), Scope::path("/workspace/b")]
    );
}

#[test]
fn scope_transforms_derive_exact_parent_and_url_host_resources() {
    let manifest = Manifest::from_json(
        r#"{
            "id":"transforms","version":"0.1","name": {"en": "Transforms"},
            "operations":{
                "tag":{"label": {"en": "Tag"},"args":[
                    {"name":"path","kind":"path","required":true}
                ],"needs":[
                    {"verb":"fs.write","scope":{"kind":"from-arg","arg":"path",
                     "transform":"parent"},"why": {"en": "Write sidecar"}}
                ]},
                "fetch":{"label": {"en": "Fetch"},"args":[
                    {"name":"url","kind":"text","required":true}
                ],"needs":[
                    {"verb":"net.dial","scope":{"kind":"from-arg","arg":"url",
                     "transform":"url-host"},"why": {"en": "Fetch host"}}
                ]}
            }
        }"#,
    )
    .unwrap();
    let tag = manifest
        .resolve_needs(
            "tag",
            &BTreeMap::from([("path".to_string(), serde_json::json!("/workspace/note.txt"))]),
        )
        .unwrap();
    assert_eq!(tag[0][0].scope, Scope::path("/workspace"));
    let fetch = manifest
        .resolve_needs(
            "fetch",
            &BTreeMap::from([(
                "url".to_string(),
                serde_json::json!("https://api.example.test/v1?q=1"),
            )]),
        )
        .unwrap();
    assert_eq!(fetch[0][0].scope, Scope::host("api.example.test:443"));
    for (url, expected) in [
        ("https://api.example.test:8443/v1", "api.example.test:8443"),
        ("http://[2001:db8::1]:8080/", "[2001:db8::1]:8080"),
        ("http://[2001:db8::2]/", "[2001:db8::2]:80"),
        ("ftp://files.example.test:2121/", "files.example.test:2121"),
        ("ftp://files.example.test:21/", "files.example.test:21"),
    ] {
        let fetch = manifest
            .resolve_needs(
                "fetch",
                &BTreeMap::from([("url".to_string(), serde_json::json!(url))]),
            )
            .unwrap();
        assert_eq!(fetch[0][0].scope, Scope::host(expected));
    }
    let unsupported = manifest.resolve_needs(
        "fetch",
        &BTreeMap::from([(
            "url".to_string(),
            serde_json::json!("ftp://files.example.test/archive"),
        )]),
    );
    assert!(unsupported.is_err());
}

#[test]
fn python_and_rust_share_url_host_scope_vectors() {
    let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let vectors: Vec<serde_json::Value> = serde_json::from_str(
        &std::fs::read_to_string(repository.join("apps/_shared/url_host_scope_vectors.json"))
            .unwrap(),
    )
    .unwrap();
    let manifest = Manifest::from_json(
        r#"{
            "id":"url-vectors","version":"1","name": {"en": "URL vectors"},
            "operations":{"fetch":{"label": {"en": "Fetch"},"args":[
                {"name":"url","kind":"text","required":true}
            ],"needs":[{"verb":"net.dial","scope":{
                "kind":"from-arg","arg":"url","transform":"url-host"
            },"why": {"en": "Fetch URL"}}]}}
        }"#,
    )
    .unwrap();
    let paths = crate::caps::args::PathContext {
        home: "/home/test".into(),
        cwd: Some("/workspace".into()),
    };
    for vector in vectors {
        let url = vector["url"].as_str().unwrap();
        let resolved = manifest.resolve_operation_call(
            "fetch",
            &BTreeMap::from([("url".to_string(), serde_json::json!(url))]),
            &paths,
        );
        if vector["error"].as_bool().unwrap_or(false) {
            assert!(resolved.is_err(), "accepted {url}");
        } else {
            let canonical_url = vector["canonical_url"].as_str().unwrap();
            let expected = vector["scope"].as_str().unwrap();
            let resolved = resolved.unwrap();
            assert_eq!(resolved.values["url"], canonical_url, "{url}");
            assert_eq!(resolved.needs[0][0].scope, Scope::host(expected), "{url}");
        }
    }
}

#[test]
fn destructive_confirmation_is_required_and_true_before_capability_resolution() {
    let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let manifest = Manifest::from_json(
        &std::fs::read_to_string(repository.join("apps/firewall-manager/app.json")).unwrap(),
    )
    .unwrap();
    let paths = crate::caps::args::PathContext {
        home: "/home/test".into(),
        cwd: Some("/workspace".into()),
    };
    assert!(manifest
        .resolve_operation_call("clear", &BTreeMap::new(), &paths)
        .is_err());
    assert!(manifest
        .resolve_operation_call(
            "clear",
            &BTreeMap::from([("confirm".to_string(), serde_json::json!(false))]),
            &paths,
        )
        .is_err());
    let confirmed = manifest
        .resolve_operation_call(
            "clear",
            &BTreeMap::from([("confirm".to_string(), serde_json::json!(true))]),
            &paths,
        )
        .unwrap();
    assert_eq!(confirmed.values["confirm"], serde_json::json!(true));
    assert_eq!(confirmed.needs[0][0].verb, Verb::NET_FIREWALL);
}

#[test]
fn usb_authorize_conditionally_requires_true_confirmation_before_authority() {
    let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let manifest = Manifest::from_json(
        &std::fs::read_to_string(repository.join("apps/usb-guard/app.json")).unwrap(),
    )
    .unwrap();
    let paths = crate::caps::args::PathContext {
        home: "/home/test".into(),
        cwd: Some("/workspace".into()),
    };
    let base = |state| {
        BTreeMap::from([
            ("device".to_string(), serde_json::json!("1-2")),
            ("state".to_string(), serde_json::json!(state)),
        ])
    };
    let enabled = manifest
        .resolve_operation_call("authorize", &base("on"), &paths)
        .unwrap();
    assert!(!enabled.values.contains_key("confirm"));
    assert_eq!(enabled.needs[0][0].verb, Verb::DEVICE_USB);
    let mut unnecessary = base("on");
    unnecessary.insert("confirm".to_string(), serde_json::json!(true));
    assert!(manifest
        .resolve_operation_call("authorize", &unnecessary, &paths)
        .is_err());

    assert!(manifest
        .resolve_operation_call("authorize", &base("off"), &paths)
        .is_err());
    let mut denied = base("off");
    denied.insert("confirm".to_string(), serde_json::json!(false));
    assert!(manifest
        .resolve_operation_call("authorize", &denied, &paths)
        .is_err());
    denied.insert("confirm".to_string(), serde_json::json!(true));
    let disabled = manifest
        .resolve_operation_call("authorize", &denied, &paths)
        .unwrap();
    assert_eq!(disabled.needs[0][0].verb, Verb::DEVICE_USB);
}

#[test]
fn invalid_conditional_requiredness_is_rejected() {
    for args in [
        r#"[
          {"name":"mode","kind":"name","required":true},
          {"name":"confirm","kind":"bool","required":true,
           "required_when":{"kind":"arg-equals","arg":"mode","value":"off"}}
        ]"#,
        r#"[
          {"name":"confirm","kind":"bool",
           "required_when":{"kind":"arg-equals","arg":"mode","value":"off"}},
          {"name":"mode","kind":"name","required":true}
        ]"#,
        r#"[
          {"name":"mode","kind":"name","required":true},
          {"name":"confirm","kind":"bool","default":false,
           "required_when":{"kind":"arg-equals","arg":"mode","value":"off"}}
        ]"#,
        r#"[
          {"name":"mode","kind":"name","required":true},
          {"name":"confirm","kind":"bool",
           "required_when":{"kind":"arg-equals","arg":"mode","value":false}}
        ]"#,
    ] {
        let body = format!(
            r#"{{
              "id":"bad-condition","version":"1","name": {{"en": "Bad"}},
              "operations":{{"run":{{"label": {{"en": "Run"}},"args":{args}}}}}
            }}"#
        );
        assert!(Manifest::from_json(&body).is_err(), "accepted {args}");
    }
}

#[test]
fn ambiguous_repeatable_declarations_are_rejected() {
    for arg in [
        r#"{"name":"toggle","kind":"bool","repeatable":true}"#,
        r#"{"name":"first","kind":"text","repeatable":true},
            {"name":"later","kind":"text"}"#,
    ] {
        let body = format!(
            r#"{{
                "id":"bad-repeat","version":"0.1","name": {{"en": "Bad repeat"}},
                "operations":{{"run":{{"label": {{"en": "Run"}},"args":[{arg}]}}}}
            }}"#
        );
        assert!(Manifest::from_json(&body).is_err(), "accepted {arg}");
    }
}

#[test]
fn positional_default_gaps_and_removed_aliases_are_rejected() {
    for args in [
        r#"[
          {"name":"optional","kind":"text"},
          {"name":"later","kind":"text","default":"value"}
        ]"#,
        r#"[
          {"name":"first","kind":"text","binding":"flag","aliases":["-x"]},
          {"name":"second","kind":"text","binding":"flag","aliases":["-x"]}
        ]"#,
        r#"[
          {"name":"text","kind":"text","repeatable":true},
          {"name":"target","kind":"name","binding":"flag","positional_alias":true}
        ]"#,
        r#"[
          {"name":"text","kind":"text","required":false},
          {"name":"target","kind":"name","binding":"flag","positional_alias":true}
        ]"#,
    ] {
        let body = format!(
            r#"{{
              "id":"bad-layout","version":"1","name": {{"en": "Bad"}},
              "operations":{{"run":{{"label": {{"en": "Run"}},"args":{args}}}}}
            }}"#
        );
        assert!(Manifest::from_json(&body).is_err(), "accepted {args}");
    }
}

#[test]
fn scope_binding_discriminators_reject_missing_and_unknown_payloads() {
    for scope in [
        r#"{"kind":"wild","arg":"path"}"#,
        r#"{"kind":"from-arg-map","arg":"path"}"#,
        r#"{"kind":"fixed","scope":{"kind":"path","value":"/tmp"},"values":{}}"#,
    ] {
        let body = format!(
            r#"{{
                "id":"scope-shape","version":"0.1","name": {{"en": "Scope shape"}},
                "operations":{{"read":{{"label": {{"en": "Read"}},"args":[
                    {{"name":"path","kind":"path","required":true}}
                ],"needs":[{{"verb":"fs.read","scope":{scope},"why": {{"en": "Read"}}}}]}}}}
            }}"#
        );
        assert!(Manifest::from_json(&body).is_err(), "accepted {scope}");
    }

    let condition_with_wrong_payload = r#"{
        "id":"condition-shape","version":"0.1","name": {"en": "Condition shape"},
        "operations":{"read":{"label": {"en": "Read"},"args":[
            {"name":"path","kind":"path","binding":"flag"}
        ],"needs":[{"verb":"fs.read","scope":{"kind":"from-arg","arg":"path"},
            "when":{"kind":"arg-present","arg":"path","value":"/tmp"},"why": {"en": "Read"}}]}}
    }"#;
    assert!(Manifest::from_json(condition_with_wrong_payload).is_err());
}

#[test]
fn resolve_needs_with_fixed_scope() {
    let m = parse(
        r#"{
              "id": "log",
              "version": "0.1",
              "name": {"en": "Log"},
              "operations": {
                "tail": {
                  "label": {"en": "Tail logs"},
                  "needs": [
                    {"verb": "data.log.read",
                     "scope": {"kind":"fixed","scope":{"kind":"name","value":"system/*"}},
                     "why": {"en": "Read recent log lines."}}
                  ]
                }
              }
            }"#,
    );
    let caps = m.resolve_needs("tail", &BTreeMap::new()).unwrap();
    assert_eq!(caps[0][0].verb, Verb::DATA_LOG_READ);
    assert_eq!(caps[0][0].scope, Scope::name("system/*"));
}

#[test]
fn resolve_needs_missing_arg_at_runtime_is_error() {
    let m = parse(
        r#"{
              "id": "fs",
              "version": "0.1",
              "name": {"en": "Files"},
              "operations": {
                "rm": {
                  "label": {"en": "Delete"},
                  "args": [{"name": "path", "kind": "path", "required": true}],
                  "needs": [
                    {"verb": "fs.delete",
                     "scope": {"kind":"from-arg","arg":"path"},
                     "why": {"en": "Remove the file you specified."}}
                  ]
                }
              }
            }"#,
    );
    let err = m.resolve_needs("rm", &BTreeMap::new()).unwrap_err();
    match err {
        ManifestError::NeedInvalid { op, detail, .. } => {
            assert_eq!(op, "rm");
            assert!(detail.contains("required"));
        }
        other => panic!("expected NeedInvalid, got {other:?}"),
    }
}

#[test]
fn runtime_default_is_python() {
    let m = parse(r#"{"id":"x","version":"0","name": {"en": "X"}}"#);
    assert_eq!(m.runtime, Runtime::Python);
    assert_eq!(m.runtime.default_entry(), "main.py");
}

#[test]
fn full_example_round_trips() {
    let src = r#"{
          "id": "fs",
          "version": "0.2.0",
          "name": {"en": "Files"},
          "summary": {"en": "Browse, read, write, and search files."},
          "icon": "📁",
          "runtime": "python",
          "entry": "main.py",
          "operations": {
            "ls": {
              "label": {"en": "List files"},
              "summary": {"en": "Show the names of files inside a folder."},
              "args": [{"name":"path","kind":"path","required":true}],
              "needs": [
                {"verb":"fs.meta",
                 "scope":{"kind":"from-arg","arg":"path"},
                 "why": {"en": "Read directory entries to list files."}}
              ]
            },
            "mv": {
              "label": {"en": "Move a file"},
              "args": [
                {"name":"src","kind":"path","required":true},
                {"name":"dst","kind":"path","required":true}
              ],
              "needs": [
                {"verb":"fs.read",   "scope":{"kind":"from-arg","arg":"src"}, "why": {"en": "Read the source file."}},
                {"verb":"fs.write",  "scope":{"kind":"from-arg","arg":"dst"}, "why": {"en": "Write to the destination."}},
                {"verb":"fs.delete", "scope":{"kind":"from-arg","arg":"src"}, "why": {"en": "Remove the source after copying."}}
              ]
            }
          }
        }"#;
    let m = Manifest::from_json(src).unwrap();
    let json = serde_json::to_string(&m).unwrap();
    let back = Manifest::from_json(&json).unwrap();
    assert_eq!(back.id, m.id);
    assert_eq!(back.operations.len(), m.operations.len());
    assert_eq!(back.operations["mv"].needs.len(), 3);
}

// ---------------------------------------------------------------
// AI policy block
// ---------------------------------------------------------------

#[test]
fn ai_block_with_valid_policy_parses() {
    let m = Manifest::from_json(
        r#"{
              "id": "summarize",
              "version": "0.1",
              "name": {"en": "Summarize"},
              "ai": {
                "budget": {"monthly_units": 100000},
                "safety": "strict",
                "origins": ["external-content"]
              },
              "operations": {
                "run": {
                  "label": {"en": "Summarize text"},
                  "needs": [
                    {"verb": "ai.chat.untrusted",
                     "scope": {"kind":"fixed","scope":{"kind":"name","value":"*"}},
                     "why": {"en": "Summarize the input text."}}
                  ]
                }
              }
            }"#,
    )
    .unwrap();
    let policy = m.ai.as_ref().unwrap();
    assert_eq!(policy.safety, AiSafety::Strict);
    assert_eq!(policy.origins, vec![PromptOrigin::ExternalContent]);
    assert_eq!(policy.budget.monthly_units, 100000);
}

#[test]
fn ai_need_without_ai_block_rejected() {
    let err = Manifest::from_json(
        r#"{
              "id": "rogue",
              "version": "0.1",
              "name": {"en": "Rogue"},
              "operations": {
                "run": {
                  "label": {"en": "Run"},
                  "needs": [
                    {"verb": "ai.chat",
                     "scope": {"kind":"fixed","scope":{"kind":"name","value":"*"}},
                     "why": {"en": "Talk to a model without declaring a policy."}}
                  ]
                }
              }
            }"#,
    )
    .unwrap_err();
    match err {
        ManifestError::AiNeedMissingPolicy { op, verb, .. } => {
            assert_eq!(op, "run");
            assert_eq!(verb, "ai.chat");
        }
        other => panic!("expected AiNeedMissingPolicy, got {other:?}"),
    }
}

#[test]
fn ai_bypass_rejected_for_apps() {
    let err = Manifest::from_json(
        r#"{
              "id": "rogue",
              "version": "0.1",
              "name": {"en": "Rogue"},
              "ai": {
                "budget": {"monthly_units": 1},
                "safety": "minimal",
                "origins": ["trusted"]
              },
              "operations": {
                "run": {
                  "label": {"en": "Run"},
                  "needs": [
                    {"verb": "ai.bypass",
                     "scope": {"kind":"fixed","scope":{"kind":"name","value":"*"}},
                     "why": {"en": "Skip safety pipeline."}}
                  ]
                }
              }
            }"#,
    )
    .unwrap_err();
    assert!(matches!(
        err,
        ManifestError::AiBypassNotAllowedForApps { .. }
    ));
}

#[test]
fn ai_block_with_empty_origins_rejected() {
    let err = Manifest::from_json(
        r#"{
              "id": "summarize",
              "version": "0.1",
              "name": {"en": "Summarize"},
              "ai": {
                "budget": {"monthly_units": 1},
                "safety": "strict",
                "origins": []
              }
            }"#,
    )
    .unwrap_err();
    assert!(matches!(err, ManifestError::AiPolicyNoOrigins));
}

#[test]
fn ai_origins_default_to_trusted() {
    let m = Manifest::from_json(
        r#"{
              "id": "summarize",
              "version": "0.1",
              "name": {"en": "Summarize"},
              "ai": {
                "budget": {"monthly_units": 1},
                "safety": "strict"
              }
            }"#,
    )
    .unwrap();
    let policy = m.ai.as_ref().unwrap();
    assert_eq!(policy.origins, vec![PromptOrigin::Trusted]);
}

#[test]
fn ai_tools_default_to_empty_list() {
    let m = Manifest::from_json(
        r#"{
              "id": "summarize",
              "version": "0.1",
              "name": {"en": "Summarize"},
              "ai": {
                "budget": {"monthly_units": 1},
                "safety": "strict",
                "origins": ["trusted"]
              }
            }"#,
    )
    .unwrap();
    let policy = m.ai.as_ref().unwrap();
    assert!(policy.tools.is_empty());
}

#[test]
fn ai_tools_duplicate_entry_rejected() {
    let err = Manifest::from_json(
        r#"{
              "id": "summarize",
              "version": "0.1",
              "name": {"en": "Summarize"},
              "ai": {
                "budget": {"monthly_units": 1},
                "safety": "strict",
                "origins": ["trusted"],
                "tools": ["fs.read_text", "kv.get", "fs.read_text"]
              }
            }"#,
    )
    .unwrap_err();
    assert!(matches!(err, ManifestError::AiDuplicateTool { ref name } if name == "fs.read_text"));
}

#[test]
fn ai_tools_unknown_name_rejected_against_catalog() {
    let m = Manifest::from_json(
        r#"{
              "id": "summarize",
              "version": "0.1",
              "name": {"en": "Summarize"},
              "ai": {
                "budget": {"monthly_units": 1},
                "safety": "strict",
                "origins": ["trusted"],
                "tools": ["fs.read_text", "fs.unicorn"]
              }
            }"#,
    )
    .unwrap();
    let err = m
        .validate_tools_against_catalog(&["fs.read_text", "kv.get"])
        .unwrap_err();
    assert!(matches!(err, ManifestError::AiUnknownTool { ref name } if name == "fs.unicorn"));
}

#[test]
fn ai_tools_known_names_pass_catalog_check() {
    let m = Manifest::from_json(
        r#"{
              "id": "summarize",
              "version": "0.1",
              "name": {"en": "Summarize"},
              "ai": {
                "budget": {"monthly_units": 1},
                "safety": "strict",
                "origins": ["trusted"],
                "tools": ["fs.read_text", "kv.get"]
              }
            }"#,
    )
    .unwrap();
    assert!(m
        .validate_tools_against_catalog(&["fs.read_text", "fs.list", "kv.get"])
        .is_ok());
}

#[test]
fn manifest_without_ai_block_skips_tool_catalog_check() {
    let m = Manifest::from_json(
        r#"{
              "id": "calc",
              "version": "0.1",
              "name": {"en": "Calc"}
            }"#,
    )
    .unwrap();
    assert!(m.validate_tools_against_catalog(&[]).is_ok());
}

// -----------------------------------------------------------------
// MCP service block tests
// -----------------------------------------------------------------

#[test]
fn mcp_block_parses_with_minimal_tool() {
    let m = parse(
        r#"{
              "schema_version": 2,
              "id": "kv",
              "version": "0.1",
              "name": {"en": "KV"},
              "mcp": {
                "entry": "server.py",
                "tools": [
                  {
                    "name": "kv.list",
                    "summary": {"en": "List keys."},
                    "needs": [
                      {"verb": "data.kv.read",
                       "scope": {"kind":"wild"},
                       "why": {"en": "Scan every key."}}
                    ]
                  }
                ]
              }
            }"#,
    );
    let service = m.mcp.expect("mcp block parsed");
    assert_eq!(service.entry.as_deref(), Some("server.py"));
    assert_eq!(service.transport, McpTransport::Stdio);
    assert_eq!(service.tools.len(), 1);
    assert_eq!(service.tools[0].name, "kv.list");
}

#[test]
fn mcp_first_service_parses_lifecycle_access_and_tools() {
    let manifest = parse(
        r#"{
              "schema_version": 2,
              "id": "email",
              "version": "1.0.0",
              "name": {"en": "Email"},
              "mcp": {
                "entry": "server.py",
                "lifecycle": "always-on",
                "access": {
                  "system_agent": true,
                  "apps": ["crm"],
                  "external_agents": false
                },
                "tools": [
                  {
                    "name": "email.search",
                    "summary": {"en": "Search mail."},
                    "args": [{"name":"query","kind":"text","required":true}]
                  }
                ]
              }
            }"#,
    );
    assert_eq!(manifest.schema_version, Some(2));
    let service = manifest.mcp.as_ref().expect("MCP service");
    assert_eq!(service.entry.as_deref(), Some("server.py"));
    assert_eq!(service.lifecycle, McpLifecycle::AlwaysOn);
    assert!(service.access.system_agent);
    assert_eq!(service.access.apps, vec!["crm"]);
    assert!(!service.access.external_agents);
    assert_eq!(service.tools[0].name, "email.search");
}

#[test]
fn mcp_first_service_uses_restrictive_caller_defaults() {
    let manifest = parse(
        r#"{
              "schema_version": 2,
              "id": "email",
              "version": "1.0.0",
              "name": {"en": "Email"},
              "mcp": {"tools":[{"name":"email.status","summary": {"en": "Status"}}]}
            }"#,
    );
    let service = manifest.mcp.as_ref().expect("MCP service");
    assert_eq!(service.lifecycle, McpLifecycle::Lazy);
    assert!(service.access.system_agent);
    assert!(service.access.apps.is_empty());
    assert!(!service.access.external_agents);
}

#[test]
fn mcp_first_service_requires_at_least_one_tool() {
    let error = Manifest::from_json(
        r#"{
              "schema_version": 2,
              "id": "email",
              "version": "1.0.0",
              "name": {"en": "Email"},
              "mcp": {"tools":[]}
            }"#,
    )
    .unwrap_err();
    assert!(matches!(error, ManifestError::McpNoTools));
}

#[test]
fn manifest_and_mcp_objects_reject_unknown_fields() {
    for body in [
        r#"{"id":"closed","version":"1","name": {"en": "Closed"},"unknown":true}"#,
        r#"{"id":"closed","version":"1","name": {"en": "Closed"},"operations":{"run":{"label": {"en": "Run"},"unknown":true}}}"#,
        r#"{"schema_version":2,"id":"closed","version":"1","name": {"en": "Closed"},"mcp":{"unknown":true,"tools":[{"name":"closed.run","summary": {"en": "Run"}}]}}"#,
        r#"{"schema_version":2,"id":"closed","version":"1","name": {"en": "Closed"},"mcp":{"tools":[{"name":"closed.run","summary": {"en": "Run"},"unknown":true}]}}"#,
    ] {
        assert!(matches!(
            Manifest::from_json(body),
            Err(ManifestError::Json(_))
        ));
    }
}

#[test]
fn mcp_tool_args_reject_cli_bindings() {
    let error = Manifest::from_json(
        r#"{
              "schema_version": 2,
              "id": "closed",
              "version": "1",
              "name": {"en": "Closed"},
              "mcp": {"tools":[{
                "name":"closed.run",
                "summary": {"en": "Run"},
                "args":[{"name":"value","kind":"text","binding":"flag"}]
              }]}
            }"#,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        ManifestError::McpArgBindingNotAllowed { .. }
    ));
}

#[test]
fn mcp_first_service_requires_schema_version_two() {
    let error = Manifest::from_json(
        r#"{
              "id": "email",
              "version": "1.0.0",
              "name": {"en": "Email"},
              "mcp": {"tools":[]}
            }"#,
    )
    .unwrap_err();
    assert!(matches!(error, ManifestError::McpSchemaVersion));
}

#[test]
fn a_removed_session_block_is_rejected() {
    let error = Manifest::from_json(
        r#"{
              "id": "kv",
              "version": "0.1",
              "name": {"en": "KV"},
              "session": {
                "entry": "server.py",
                "tools": [{"name": "kv.list", "summary": {"en": "List keys."}}]
              }
            }"#,
    )
    .unwrap_err();
    assert!(matches!(error, ManifestError::RemovedSessionField));
}

#[test]
fn mcp_first_service_rejects_invalid_callers() {
    let invalid = Manifest::from_json(
        r#"{
              "schema_version": 2,
              "id": "email",
              "version": "1.0.0",
              "name": {"en": "Email"},
              "mcp": {
                "access": {"apps":["Bad/App"]},
                "tools":[{"name":"email.status","summary": {"en": "Status"}}]
              }
            }"#,
    )
    .unwrap_err();
    assert!(matches!(invalid, ManifestError::McpAccessInvalidApp { .. }));

    let duplicate = Manifest::from_json(
        r#"{
              "schema_version": 2,
              "id": "email",
              "version": "1.0.0",
              "name": {"en": "Email"},
              "mcp": {
                "access": {"apps":["crm", "crm"]},
                "tools":[{"name":"email.status","summary": {"en": "Status"}}]
              }
            }"#,
    )
    .unwrap_err();
    assert!(matches!(
        duplicate,
        ManifestError::McpAccessDuplicateApp { .. }
    ));

    let unknown_field = Manifest::from_json(
        r#"{
              "schema_version": 2,
              "id": "email",
              "version": "1.0.0",
              "name": {"en": "Email"},
              "mcp": {
                "access": {"systemAgent":false},
                "tools":[{"name":"email.status","summary": {"en": "Status"}}]
              }
            }"#,
    )
    .unwrap_err();
    assert!(matches!(unknown_field, ManifestError::Json(_)));
}

#[test]
fn mcp_tool_default_entry_per_runtime() {
    assert_eq!(Runtime::Python.default_mcp_entry(), "server.py");
    assert_eq!(Runtime::Node.default_mcp_entry(), "server.js");
}

#[test]
fn mcp_tool_resolve_needs_from_arg() {
    let m = parse(
        r#"{
              "schema_version": 2,
              "id": "kv",
              "version": "0.1",
              "name": {"en": "KV"},
              "mcp": {
                "tools": [
                  {
                    "name": "kv.get",
                    "summary": {"en": "Get a value."},
                    "args": [{"name":"key","kind":"name","required":true}],
                    "needs": [
                      {"verb": "data.kv.read",
                       "scope": {"kind":"from-arg","arg":"key"},
                       "why": {"en": "Read the value at the named key."}}
                    ]
                  }
                ]
              }
            }"#,
    );
    let mut args = BTreeMap::new();
    args.insert("key".to_string(), serde_json::json!("user/jay"));
    let caps = m.resolve_mcp_tool_needs("kv.get", &args).unwrap();
    assert_eq!(caps.len(), 1);
    assert_eq!(caps[0][0].verb, Verb::DATA_KV_READ);
    assert_eq!(caps[0][0].scope, Scope::name("user/jay"));
}

#[test]
fn mcp_repeatable_scope_arg_resolves_every_capability() {
    let manifest = parse(
        r#"{
              "schema_version": 2,
          "id":"files","version":"0.1","name": {"en": "Files"},
          "mcp":{"tools":[{
            "name":"files.read","summary": {"en": "Read files"},
            "args":[{"name":"path","kind":"path","repeatable":true}],
            "needs":[{"verb":"fs.read","scope":{"kind":"from-arg","arg":"path"},
                      "when":{"kind":"arg-present","arg":"path"},"why": {"en": "Read files"}}]
          }]}
        }"#,
    );
    let args = BTreeMap::from([(
        "path".to_string(),
        serde_json::json!(["/workspace/a", "/workspace/b"]),
    )]);
    let resolved = manifest
        .resolve_mcp_tool_needs("files.read", &args)
        .unwrap();
    assert_eq!(resolved[0].len(), 2);
    assert_eq!(resolved[0][0].scope, Scope::path("/workspace/a"));
    assert_eq!(resolved[0][1].scope, Scope::path("/workspace/b"));
}

#[test]
fn mcp_effective_call_matches_one_shot_argument_semantics() {
    let manifest = parse(
        r#"{
              "schema_version": 2,
          "id":"session-parity","version":"0.1","name": {"en": "Session parity"},
          "mcp":{"tools":[{
            "name":"files.read","summary": {"en": "Read files"},
            "args":[
              {"name":"path","kind":"path","repeatable":true},
              {"name":"mode","kind":"name",
               "choices":["safe","fast"],"default":"safe"},
              {"name":"enabled","kind":"bool"}
            ],
            "needs":[
              {"verb":"fs.read","scope":{"kind":"from-arg","arg":"path"},
               "when":{"kind":"arg-present","arg":"path"},"why": {"en": "Read files"}},
              {"verb":"data.kv.read","scope":{"kind":"fixed",
               "scope":{"kind":"name","value":"enabled"}},
               "when":{"kind":"arg-equals","arg":"enabled","value":true},
               "why": {"en": "Read enabled state"}}
            ]
          }]}
        }"#,
    );
    let paths = crate::caps::args::PathContext {
        home: "/home/test".into(),
        cwd: Some("/workspace".into()),
    };
    let supplied = BTreeMap::from([("path".to_string(), serde_json::json!(["a.txt", "b.txt"]))]);
    let effective = manifest
        .resolve_mcp_tool_call("files.read", &supplied, &paths)
        .unwrap();
    assert_eq!(
        effective.values["path"],
        serde_json::json!(["/workspace/a.txt", "/workspace/b.txt"])
    );
    assert_eq!(effective.values["mode"], serde_json::json!("safe"));
    assert_eq!(effective.values["enabled"], serde_json::json!(false));
    assert_eq!(effective.needs[0].len(), 2);
    assert!(effective.needs[1].is_empty());

    let invalid = BTreeMap::from([("mode".to_string(), serde_json::json!("unsafe"))]);
    assert!(manifest
        .resolve_mcp_tool_call("files.read", &invalid, &paths)
        .is_err());
    let undeclared = BTreeMap::from([
        ("path".to_string(), serde_json::json!(["a.txt"])),
        (
            "protocol_metadata".to_string(),
            serde_json::json!("not-an-argument"),
        ),
    ]);
    let error = manifest
        .resolve_mcp_tool_call("files.read", &undeclared, &paths)
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("unknown argument `protocol_metadata`"));
}

#[test]
fn mcp_tool_resolve_needs_unknown_tool_errors() {
    let m = parse(
        r#"{
              "schema_version": 2,
              "id": "kv",
              "version": "0.1",
              "name": {"en": "KV"},
              "mcp": {
                "tools": [{"name":"kv.get","summary": {"en": "Get a value"}}]
              }
            }"#,
    );
    let err = m
        .resolve_mcp_tool_needs("kv.ghost", &BTreeMap::new())
        .unwrap_err();
    assert!(matches!(err, ManifestError::McpNeedInvalid { .. }));
}

#[test]
fn mcp_tool_resolve_needs_without_mcp_block_errors() {
    let m = parse(r#"{"id":"kv","version":"0","name": {"en": "KV"}}"#);
    let err = m
        .resolve_mcp_tool_needs("kv.get", &BTreeMap::new())
        .unwrap_err();
    match err {
        ManifestError::McpNeedInvalid { detail, .. } => {
            assert!(detail.contains("no `mcp` block"));
        }
        other => panic!("expected McpNeedInvalid, got {other:?}"),
    }
}

#[test]
fn mcp_tool_invalid_name_rejected() {
    let err = Manifest::from_json(
        r#"{
              "schema_version": 2,
              "id": "kv",
              "version": "0.1",
              "name": {"en": "KV"},
              "mcp": {
                "tools": [
                  {"name": "KV.Get", "summary": {"en": "Get"}}
                ]
              }
            }"#,
    )
    .unwrap_err();
    assert!(matches!(err, ManifestError::McpToolInvalidName { .. }));
}

#[test]
fn mcp_duplicate_tool_name_rejected() {
    let err = Manifest::from_json(
        r#"{
              "schema_version": 2,
              "id": "kv",
              "version": "0.1",
              "name": {"en": "KV"},
              "mcp": {
                "tools": [
                  {"name": "kv.get", "summary": {"en": "Get"}},
                  {"name": "kv.get", "summary": {"en": "Get again"}}
                ]
              }
            }"#,
    )
    .unwrap_err();
    assert!(matches!(err, ManifestError::McpDuplicateTool { .. }));
}

#[test]
fn mcp_need_refs_undeclared_arg_rejected() {
    let err = Manifest::from_json(
        r#"{
              "schema_version": 2,
              "id": "kv",
              "version": "0.1",
              "name": {"en": "KV"},
              "mcp": {
                "tools": [
                  {
                    "name": "kv.get",
                    "summary": {"en": "Get"},
                    "args": [],
                    "needs": [
                      {"verb": "data.kv.read",
                       "scope": {"kind":"from-arg","arg":"key"},
                       "why": {"en": "Read."}}
                    ]
                  }
                ]
              }
            }"#,
    )
    .unwrap_err();
    assert!(matches!(
        err,
        ManifestError::McpNeedRefsUndeclaredArg { .. }
    ));
}

#[test]
fn mcp_need_binding_to_text_arg_rejected() {
    let err = Manifest::from_json(
        r#"{
              "schema_version": 2,
              "id": "kv",
              "version": "0.1",
              "name": {"en": "KV"},
              "mcp": {
                "tools": [
                  {
                    "name": "kv.get",
                    "summary": {"en": "Get"},
                    "args": [{"name":"key","kind":"text"}],
                    "needs": [
                      {"verb": "data.kv.read",
                       "scope": {"kind":"from-arg","arg":"key"},
                       "why": {"en": "Read."}}
                    ]
                  }
                ]
              }
            }"#,
    )
    .unwrap_err();
    assert!(matches!(err, ManifestError::McpNeedArgKindMismatch { .. }));
}

#[test]
fn mcp_ai_verb_without_policy_rejected() {
    let err = Manifest::from_json(
        r#"{
              "schema_version": 2,
              "id": "summarize",
              "version": "0.1",
              "name": {"en": "Summarize"},
              "mcp": {
                "tools": [
                  {
                    "name": "summarize.run",
                    "summary": {"en": "Summarize text."},
                    "needs": [
                      {"verb": "ai.chat",
                       "scope": {"kind":"wild"},
                       "why": {"en": "Call the model."}}
                    ]
                  }
                ]
              }
            }"#,
    )
    .unwrap_err();
    assert!(matches!(err, ManifestError::McpAiNeedMissingPolicy { .. }));
}

#[test]
fn desktop_block_parses_with_defaults() {
    let m = parse(
        r#"{
              "id": "notes",
              "version": "0.1",
              "name": {"en": "Notes"},
              "desktop": {}
            }"#,
    );
    let d = m.desktop.expect("desktop block present");
    assert_eq!(d.exec, "--gui");
    assert!(!d.single_instance);
    assert!(!d.panel_applet);
    assert!(d.categories.is_empty());
}

#[test]
fn desktop_panel_applet_round_trips() {
    let m = parse(
        r#"{
              "id": "widget",
              "version": "0.1",
              "name": {"en": "Widget"},
              "desktop": {
                "panel_applet": true
              }
            }"#,
    );
    assert!(
        m.desktop
            .as_ref()
            .expect("desktop block present")
            .panel_applet
    );

    let json = serde_json::to_string(&m).expect("serialize manifest");
    let back = Manifest::from_json(&json).expect("parse serialized manifest");
    assert!(
        back.desktop
            .as_ref()
            .expect("desktop block present")
            .panel_applet
    );
}

#[test]
fn desktop_block_full_parses() {
    let m = parse(
        r#"{
              "id": "notes",
              "version": "0.1",
              "name": {"en": "Notes"},
              "desktop": {
                "exec": "--ui",
                "name": {"en": "My Notes"},
                "icon": "notes",
                "categories": ["Utility", "TextEditor"],
                "mime_types": ["text/markdown"],
                "single_instance": true
              }
            }"#,
    );
    let d = m.desktop.expect("desktop block present");
    assert_eq!(d.exec, "--ui");
    assert_eq!(d.name.unwrap().en_str(), "My Notes");
    assert_eq!(d.categories, vec!["Utility", "TextEditor"]);
    assert_eq!(d.mime_types, vec!["text/markdown"]);
    assert!(d.single_instance);
}

#[test]
fn desktop_rejects_category_with_separator() {
    let err = Manifest::from_json(
        r#"{
              "id": "notes",
              "version": "0.1",
              "name": {"en": "Notes"},
              "desktop": { "categories": ["Utility;Evil"] }
            }"#,
    )
    .unwrap_err();
    assert!(matches!(
        err,
        ManifestError::DesktopInvalid {
            field: "categories",
            ..
        }
    ));
}

#[test]
fn desktop_rejects_control_char_in_exec() {
    let err = Manifest::from_json(
        "{\"id\":\"notes\",\"version\":\"0.1\",\"name\":{\"en\":\"Notes\"},\
             \"desktop\":{\"exec\":\"--gui\\nExec=evil\"}}",
    )
    .unwrap_err();
    assert!(matches!(
        err,
        ManifestError::DesktopInvalid { field: "exec", .. }
    ));
}
