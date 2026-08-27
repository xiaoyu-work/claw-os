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
              "name": "Files"
            }"#,
    );
    assert_eq!(m.id, "fs");
    assert_eq!(m.runtime, Runtime::Python);
    assert!(m.operations.is_empty());
}

#[test]
fn invalid_id_rejected() {
    let err = Manifest::from_json(r#"{"id":"FS!","version":"0","name":"X"}"#).unwrap_err();
    assert!(matches!(err, ManifestError::InvalidId(_)));
}

#[test]
fn unknown_verb_rejected_at_parse_time() {
    let err = Manifest::from_json(
        r#"{
              "id": "fs",
              "version": "0.1",
              "name": "Files",
              "operations": {
                "x": {
                  "label": "X",
                  "args": [],
                  "needs": [
                    {"verb": "fs.nonsense", "scope": {"kind":"wild"}, "why": "..."}
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
              "name": "Files",
              "operations": {
                "rm": {
                  "label": "Delete",
                  "args": [],
                  "needs": [
                    {"verb": "fs.delete", "scope": {"kind":"from-arg","arg":"path"}, "why": "y"}
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
              "name": "Files",
              "operations": {
                "rm": {
                  "label": "Delete",
                  "args": [{"name": "path", "kind": "text"}],
                  "needs": [
                    {"verb": "fs.delete", "scope": {"kind":"from-arg","arg":"path"}, "why": "y"}
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
              "name": "Files",
              "operations": {
                "x": {
                  "label": "X",
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
              "name": "Files",
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
              "name": "Files",
              "operations": {
                "rm": {
                  "label": "Delete",
                  "args": [{"name": "path", "kind": "path", "required": true}],
                  "needs": [
                    {"verb": "fs.delete",
                     "scope": {"kind":"from-arg","arg":"path"},
                     "why": "Remove the file you specified."}
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
    assert_eq!(
        caps[0][0].scope,
        Scope::path("/home/jay/x.md")
    );
}

#[test]
fn resolve_needs_uses_literal_arg_default() {
    let manifest = parse(
        r#"{
              "id": "fs",
              "version": "0.1",
              "name": "Files",
              "operations": {
                "search": {
                  "label": "Search",
                  "args": [
                    {"name": "query", "kind": "text", "required": true},
                    {"name": "path", "kind": "path", "default": "/workspace"}
                  ],
                  "needs": [
                    {"verb": "fs.read",
                     "scope": {"kind":"from-arg","arg":"path"},
                     "why": "Search the default workspace."}
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
    assert_eq!(
        caps[0][0].scope,
        Scope::path("/workspace")
    );
}

#[test]
fn resolve_needs_uses_validated_default_binding() {
    let manifest = parse(
        r#"{
              "id": "net",
              "version": "0.1",
              "name": "Network",
              "operations": {
                "download": {
                  "label": "Download",
                  "args": [
                    {"name": "url", "kind": "text", "required": true},
                    {"name": "output", "kind": "path",
                     "default_from": {
                       "arg": "url",
                       "transform": "url-path-basename",
                       "prefix": "~/",
                       "fallback": "download"
                     }}
                  ],
                  "needs": [
                    {"verb": "fs.write",
                     "scope": {"kind":"from-arg","arg":"output"},
                     "why": "Write the downloaded file."}
                  ]
                }
              }
            }"#,
    );
    let mut args = BTreeMap::new();
    args.insert(
        "url".to_string(),
        serde_json::json!("https://example.com/releases/archive.tar?download=1"),
    );

    let caps = manifest.resolve_needs("download", &args).unwrap();

    assert_eq!(
        caps[0][0].scope,
        Scope::path("~/archive.tar")
    );

    args.insert(
        "url".to_string(),
        serde_json::json!("https://example.com/releases/"),
    );
    let fallback = manifest.resolve_needs("download", &args).unwrap();
    assert_eq!(
        fallback[0][0].scope,
        Scope::path("~/download")
    );
}

#[test]
fn invalid_arg_defaults_are_rejected() {
    let wrong_type = Manifest::from_json(
        r#"{
              "id": "fs",
              "version": "0.1",
              "name": "Files",
              "operations": {
                "ls": {
                  "label": "List",
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

    let forward_reference = Manifest::from_json(
        r#"{
              "id": "net",
              "version": "0.1",
              "name": "Network",
              "operations": {
                "download": {
                  "label": "Download",
                  "args": [
                    {"name": "output", "kind": "path",
                     "default_from": {"arg": "url"}},
                    {"name": "url", "kind": "text", "required": true}
                  ]
                }
              }
            }"#,
    )
    .unwrap_err();
    assert!(matches!(
        forward_reference,
        ManifestError::ArgDefaultInvalid { .. }
    ));
}

#[test]
fn null_defaults_and_ambiguous_positionals_are_rejected() {
    for arg in [
        r#"{"name":"value","kind":"text","default":null}"#,
        r#"{"name":"value","kind":"text","default_from":null}"#,
    ] {
        let body = format!(
            r#"{{
                "id":"defaults","version":"0.1","name":"Defaults",
                "operations":{{"run":{{"label":"Run","args":[{arg}]}}}}
            }}"#
        );
        assert!(Manifest::from_json(&body).is_err());
    }

    let ambiguous = Manifest::from_json(
        r#"{
            "id":"ambiguous","version":"0.1","name":"Ambiguous",
            "operations":{"run":{"label":"Run","args":[
                {"name":"destination","kind":"name"},
                {"name":"text","kind":"text","required":true}
            ]}}
        }"#,
    )
    .unwrap_err();
    assert!(matches!(
        ambiguous,
        ManifestError::ArgDefaultInvalid { .. }
    ));
}

#[test]
fn session_tool_defaults_feed_arguments_and_capabilities() {
    let manifest = Manifest::from_json(
        r#"{
            "id":"session-defaults","version":"0.1","name":"Session defaults",
            "session":{"tools":[{
                "name":"session-defaults.read","summary":"Read",
                "args":[
                    {"name":"key","kind":"name","default":"primary"},
                    {"name":"limit","kind":"integer","default":10}
                ],
                "needs":[{"verb":"data.kv.read","scope":{"kind":"from-arg","arg":"key"},"why":"Read"}]
            }]}
        }"#,
    )
    .unwrap();
    let resolved = manifest
        .resolve_session_tool_args("session-defaults.read", &BTreeMap::new())
        .unwrap();
    assert_eq!(resolved["key"], serde_json::json!("primary"));
    assert_eq!(resolved["limit"], serde_json::json!(10));
    let caps = manifest
        .resolve_session_tool_needs("session-defaults.read", &BTreeMap::new())
        .unwrap();
    assert_eq!(caps[0][0].scope, Scope::name("primary"));
}

#[test]
fn fixed_path_scopes_reject_environment_placeholders() {
    let error = Manifest::from_json(
        r#"{
            "id":"placeholder","version":"0.1","name":"Placeholder",
            "operations":{"read":{"label":"Read","needs":[{
                "verb":"fs.read",
                "scope":{"kind":"fixed","scope":{"kind":"path","value":"$HOME/data/**"}},
                "why":"Read"
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
            "id":"conditional","version":"0.1","name":"Conditional",
            "operations":{"explain":{"label":"Explain","args":[
                {"name":"file","kind":"path","binding":"flag"},
                {"name":"provider","kind":"name","binding":"flag","default":"local"}
            ],"needs":[
                {"verb":"fs.read","scope":{"kind":"from-arg","arg":"file"},
                 "when":{"kind":"arg-present","arg":"file"},"why":"Read file"},
                {"verb":"secret.read","scope":{"kind":"fixed","scope":{"kind":"name","value":"default/TOKEN"}},
                 "when":{"kind":"arg-equals","arg":"provider","value":"cloud"},"why":"Read token"}
            ]}}
        }"#,
    )
    .unwrap();

    let local = manifest
        .resolve_needs("explain", &BTreeMap::new())
        .unwrap();
    assert_eq!(local, [Vec::new(), Vec::new()]);

    let mut cloud_file = BTreeMap::new();
    cloud_file.insert("file".to_string(), serde_json::json!("/workspace/a.txt"));
    cloud_file.insert("provider".to_string(), serde_json::json!("cloud"));
    let active = manifest.resolve_needs("explain", &cloud_file).unwrap();
    assert_eq!(
        active[0][0].scope,
        Scope::path("/workspace/a.txt")
    );
    assert_eq!(
        active[1][0].scope,
        Scope::name("default/TOKEN")
    );
}

#[test]
fn optional_capability_bindings_require_conditions() {
    let error = Manifest::from_json(
        r#"{
            "id":"unsafe","version":"0.1","name":"Unsafe",
            "operations":{"read":{"label":"Read","args":[
                {"name":"file","kind":"path","binding":"flag"}
            ],"needs":[
                {"verb":"fs.read","scope":{"kind":"from-arg","arg":"file"},"why":"Read"}
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
            "id":"repeat","version":"0.1","name":"Repeat",
            "operations":{"read":{"label":"Read","args":[
                {"name":"path","kind":"path","binding":"flag","repeatable":true}
            ],"needs":[
                {"verb":"fs.read","scope":{"kind":"from-arg","arg":"path"},
                 "when":{"kind":"arg-present","arg":"path"},"why":"Read paths"}
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
        [
            Scope::path("/workspace/a"),
            Scope::path("/workspace/b")
        ]
    );
}

#[test]
fn scope_transforms_derive_exact_parent_and_url_host_resources() {
    let manifest = Manifest::from_json(
        r#"{
            "id":"transforms","version":"0.1","name":"Transforms",
            "operations":{
                "tag":{"label":"Tag","args":[
                    {"name":"path","kind":"path","required":true}
                ],"needs":[
                    {"verb":"fs.write","scope":{"kind":"from-arg","arg":"path",
                     "transform":"parent"},"why":"Write sidecar"}
                ]},
                "fetch":{"label":"Fetch","args":[
                    {"name":"url","kind":"text","required":true}
                ],"needs":[
                    {"verb":"net.dial","scope":{"kind":"from-arg","arg":"url",
                     "transform":"url-host"},"why":"Fetch host"}
                ]}
            }
        }"#,
    )
    .unwrap();
    let tag = manifest
        .resolve_needs(
            "tag",
            &BTreeMap::from([(
                "path".to_string(),
                serde_json::json!("/workspace/note.txt"),
            )]),
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
fn ambiguous_repeatable_declarations_are_rejected() {
    for arg in [
        r#"{"name":"toggle","kind":"bool","repeatable":true}"#,
        r#"{"name":"first","kind":"text","repeatable":true},
            {"name":"later","kind":"text"}"#,
        r#"{"name":"path","kind":"path","repeatable":true,
            "default_from":{"arg":"source"}}"#,
    ] {
        let body = format!(
            r#"{{
                "id":"bad-repeat","version":"0.1","name":"Bad repeat",
                "operations":{{"run":{{"label":"Run","args":[{arg}]}}}}
            }}"#
        );
        assert!(Manifest::from_json(&body).is_err(), "accepted {arg}");
    }
}

#[test]
fn positional_default_gaps_and_alias_conflicts_are_rejected() {
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
              "id":"bad-layout","version":"1","name":"Bad",
              "operations":{{"run":{{"label":"Run","args":{args}}}}}
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
                "id":"scope-shape","version":"0.1","name":"Scope shape",
                "operations":{{"read":{{"label":"Read","args":[
                    {{"name":"path","kind":"path","required":true}}
                ],"needs":[{{"verb":"fs.read","scope":{scope},"why":"Read"}}]}}}}
            }}"#
        );
        assert!(Manifest::from_json(&body).is_err(), "accepted {scope}");
    }

    let condition_with_wrong_payload = r#"{
        "id":"condition-shape","version":"0.1","name":"Condition shape",
        "operations":{"read":{"label":"Read","args":[
            {"name":"path","kind":"path","binding":"flag"}
        ],"needs":[{"verb":"fs.read","scope":{"kind":"from-arg","arg":"path"},
            "when":{"kind":"arg-present","arg":"path","value":"/tmp"},"why":"Read"}]}}
    }"#;
    assert!(Manifest::from_json(condition_with_wrong_payload).is_err());
}

#[test]
fn resolve_needs_with_fixed_scope() {
    let m = parse(
        r#"{
              "id": "log",
              "version": "0.1",
              "name": "Log",
              "operations": {
                "tail": {
                  "label": "Tail logs",
                  "needs": [
                    {"verb": "data.log.read",
                     "scope": {"kind":"fixed","scope":{"kind":"name","value":"system/*"}},
                     "why": "Read recent log lines."}
                  ]
                }
              }
            }"#,
    );
    let caps = m.resolve_needs("tail", &BTreeMap::new()).unwrap();
    assert_eq!(caps[0][0].verb, Verb::DATA_LOG_READ);
    assert_eq!(
        caps[0][0].scope,
        Scope::name("system/*")
    );
}

#[test]
fn resolve_needs_missing_arg_at_runtime_is_error() {
    let m = parse(
        r#"{
              "id": "fs",
              "version": "0.1",
              "name": "Files",
              "operations": {
                "rm": {
                  "label": "Delete",
                  "args": [{"name": "path", "kind": "path", "required": true}],
                  "needs": [
                    {"verb": "fs.delete",
                     "scope": {"kind":"from-arg","arg":"path"},
                     "why": "Remove the file you specified."}
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
    let m = parse(r#"{"id":"x","version":"0","name":"X"}"#);
    assert_eq!(m.runtime, Runtime::Python);
    assert_eq!(m.runtime.default_entry(), "main.py");
}

#[test]
fn full_example_round_trips() {
    let src = r#"{
          "id": "fs",
          "version": "0.2.0",
          "name": "Files",
          "summary": "Browse, read, write, and search files.",
          "icon": "📁",
          "runtime": "python",
          "entry": "main.py",
          "operations": {
            "ls": {
              "label": "List files",
              "summary": "Show the names of files inside a folder.",
              "args": [{"name":"path","kind":"path","required":true}],
              "needs": [
                {"verb":"fs.meta",
                 "scope":{"kind":"from-arg","arg":"path"},
                 "why":"Read directory entries to list files."}
              ]
            },
            "mv": {
              "label": "Move a file",
              "args": [
                {"name":"src","kind":"path","required":true},
                {"name":"dst","kind":"path","required":true}
              ],
              "needs": [
                {"verb":"fs.read",   "scope":{"kind":"from-arg","arg":"src"}, "why":"Read the source file."},
                {"verb":"fs.write",  "scope":{"kind":"from-arg","arg":"dst"}, "why":"Write to the destination."},
                {"verb":"fs.delete", "scope":{"kind":"from-arg","arg":"src"}, "why":"Remove the source after copying."}
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
              "name": "Summarize",
              "ai": {
                "budget": {"monthly_units": 100000},
                "safety": "strict",
                "origins": ["external-content"]
              },
              "operations": {
                "run": {
                  "label": "Summarize text",
                  "needs": [
                    {"verb": "ai.chat.untrusted",
                     "scope": {"kind":"fixed","scope":{"kind":"name","value":"*"}},
                     "why": "Summarize the input text."}
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
              "name": "Rogue",
              "operations": {
                "run": {
                  "label": "Run",
                  "needs": [
                    {"verb": "ai.chat",
                     "scope": {"kind":"fixed","scope":{"kind":"name","value":"*"}},
                     "why": "Talk to a model without declaring a policy."}
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
              "name": "Rogue",
              "ai": {
                "budget": {"monthly_units": 1},
                "safety": "minimal",
                "origins": ["trusted"]
              },
              "operations": {
                "run": {
                  "label": "Run",
                  "needs": [
                    {"verb": "ai.bypass",
                     "scope": {"kind":"fixed","scope":{"kind":"name","value":"*"}},
                     "why": "Skip safety pipeline."}
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
              "name": "Summarize",
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
              "name": "Summarize",
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
              "name": "Summarize",
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
              "name": "Summarize",
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
              "name": "Summarize",
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
              "name": "Summarize",
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
              "name": "Calc"
            }"#,
    )
    .unwrap();
    assert!(m.validate_tools_against_catalog(&[]).is_ok());
}

// -----------------------------------------------------------------
// Session block tests (Phase 11)
// -----------------------------------------------------------------

#[test]
fn session_block_parses_with_minimal_tool() {
    let m = parse(
        r#"{
              "id": "kv",
              "version": "0.1",
              "name": "KV",
              "session": {
                "entry": "server.py",
                "tools": [
                  {
                    "name": "kv.list",
                    "summary": "List keys.",
                    "needs": [
                      {"verb": "data.kv.read",
                       "scope": {"kind":"wild"},
                       "why": "Scan every key."}
                    ]
                  }
                ]
              }
            }"#,
    );
    let session = m.session.expect("session block parsed");
    assert_eq!(session.entry.as_deref(), Some("server.py"));
    assert_eq!(session.transport, SessionTransport::Stdio);
    assert_eq!(session.tools.len(), 1);
    assert_eq!(session.tools[0].name, "kv.list");
}

#[test]
fn session_tool_default_entry_per_runtime() {
    assert_eq!(Runtime::Python.default_session_entry(), "server.py");
    assert_eq!(Runtime::Node.default_session_entry(), "server.js");
}

#[test]
fn session_tool_resolve_needs_from_arg() {
    let m = parse(
        r#"{
              "id": "kv",
              "version": "0.1",
              "name": "KV",
              "session": {
                "tools": [
                  {
                    "name": "kv.get",
                    "summary": "Get a value.",
                    "args": [{"name":"key","kind":"name","required":true}],
                    "needs": [
                      {"verb": "data.kv.read",
                       "scope": {"kind":"from-arg","arg":"key"},
                       "why": "Read the value at the named key."}
                    ]
                  }
                ]
              }
            }"#,
    );
    let mut args = BTreeMap::new();
    args.insert("key".to_string(), serde_json::json!("user/jay"));
    let caps = m.resolve_session_tool_needs("kv.get", &args).unwrap();
    assert_eq!(caps.len(), 1);
    assert_eq!(caps[0][0].verb, Verb::DATA_KV_READ);
    assert_eq!(
        caps[0][0].scope,
        Scope::name("user/jay")
    );
}

#[test]
fn session_repeatable_scope_arg_resolves_every_capability() {
    let manifest = parse(
        r#"{
          "id":"files","version":"0.1","name":"Files",
          "session":{"tools":[{
            "name":"files.read","summary":"Read files",
            "args":[{"name":"path","kind":"path","repeatable":true}],
            "needs":[{"verb":"fs.read","scope":{"kind":"from-arg","arg":"path"},
                      "when":{"kind":"arg-present","arg":"path"},"why":"Read files"}]
          }]}
        }"#,
    );
    let args = BTreeMap::from([(
        "path".to_string(),
        serde_json::json!(["/workspace/a", "/workspace/b"]),
    )]);
    let resolved = manifest
        .resolve_session_tool_needs("files.read", &args)
        .unwrap();
    assert_eq!(resolved[0].len(), 2);
    assert_eq!(resolved[0][0].scope, Scope::path("/workspace/a"));
    assert_eq!(resolved[0][1].scope, Scope::path("/workspace/b"));
}

#[test]
fn session_effective_call_matches_one_shot_argument_semantics() {
    let manifest = parse(
        r#"{
          "id":"session-parity","version":"0.1","name":"Session parity",
          "session":{"tools":[{
            "name":"files.read","summary":"Read files",
            "args":[
              {"name":"path","kind":"path","repeatable":true},
              {"name":"mode","kind":"name","binding":"flag",
               "choices":["safe","fast"],"default":"safe"},
              {"name":"enabled","kind":"bool"}
            ],
            "needs":[
              {"verb":"fs.read","scope":{"kind":"from-arg","arg":"path"},
               "when":{"kind":"arg-present","arg":"path"},"why":"Read files"},
              {"verb":"data.kv.read","scope":{"kind":"fixed",
               "scope":{"kind":"name","value":"enabled"}},
               "when":{"kind":"arg-equals","arg":"enabled","value":true},
               "why":"Read enabled state"}
            ]
          }]}
        }"#,
    );
    let paths = crate::caps::args::PathContext {
        home: "/home/test".into(),
        cwd: Some("/workspace".into()),
    };
    let supplied = BTreeMap::from([(
        "path".to_string(),
        serde_json::json!(["a.txt", "b.txt"]),
    )]);
    let effective = manifest
        .resolve_session_tool_call("files.read", &supplied, &paths)
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
        .resolve_session_tool_call("files.read", &invalid, &paths)
        .is_err());
    let undeclared = BTreeMap::from([
        ("path".to_string(), serde_json::json!(["a.txt"])),
        ("protocol_metadata".to_string(), serde_json::json!("not-an-argument")),
    ]);
    let error = manifest
        .resolve_session_tool_call("files.read", &undeclared, &paths)
        .unwrap_err();
    assert!(error.to_string().contains("unknown argument `protocol_metadata`"));
}

#[test]
fn session_tool_resolve_needs_unknown_tool_errors() {
    let m = parse(
        r#"{
              "id": "kv",
              "version": "0.1",
              "name": "KV",
              "session": { "tools": [] }
            }"#,
    );
    let err = m
        .resolve_session_tool_needs("kv.ghost", &BTreeMap::new())
        .unwrap_err();
    assert!(matches!(err, ManifestError::SessionNeedInvalid { .. }));
}

#[test]
fn session_tool_resolve_needs_no_session_errors() {
    let m = parse(r#"{"id":"kv","version":"0","name":"KV"}"#);
    let err = m
        .resolve_session_tool_needs("kv.get", &BTreeMap::new())
        .unwrap_err();
    match err {
        ManifestError::SessionNeedInvalid { detail, .. } => {
            assert!(detail.contains("no `session` block"));
        }
        other => panic!("expected SessionNeedInvalid, got {other:?}"),
    }
}

#[test]
fn session_tool_invalid_name_rejected() {
    let err = Manifest::from_json(
        r#"{
              "id": "kv",
              "version": "0.1",
              "name": "KV",
              "session": {
                "tools": [
                  {"name": "KV.Get", "summary": "Get"}
                ]
              }
            }"#,
    )
    .unwrap_err();
    assert!(matches!(err, ManifestError::SessionToolInvalidName { .. }));
}

#[test]
fn session_duplicate_tool_name_rejected() {
    let err = Manifest::from_json(
        r#"{
              "id": "kv",
              "version": "0.1",
              "name": "KV",
              "session": {
                "tools": [
                  {"name": "kv.get", "summary": "Get"},
                  {"name": "kv.get", "summary": "Get again"}
                ]
              }
            }"#,
    )
    .unwrap_err();
    assert!(matches!(err, ManifestError::SessionDuplicateTool { .. }));
}

#[test]
fn session_need_refs_undeclared_arg_rejected() {
    let err = Manifest::from_json(
        r#"{
              "id": "kv",
              "version": "0.1",
              "name": "KV",
              "session": {
                "tools": [
                  {
                    "name": "kv.get",
                    "summary": "Get",
                    "args": [],
                    "needs": [
                      {"verb": "data.kv.read",
                       "scope": {"kind":"from-arg","arg":"key"},
                       "why": "Read."}
                    ]
                  }
                ]
              }
            }"#,
    )
    .unwrap_err();
    assert!(matches!(
        err,
        ManifestError::SessionNeedRefsUndeclaredArg { .. }
    ));
}

#[test]
fn session_need_binding_to_text_arg_rejected() {
    let err = Manifest::from_json(
        r#"{
              "id": "kv",
              "version": "0.1",
              "name": "KV",
              "session": {
                "tools": [
                  {
                    "name": "kv.get",
                    "summary": "Get",
                    "args": [{"name":"key","kind":"text"}],
                    "needs": [
                      {"verb": "data.kv.read",
                       "scope": {"kind":"from-arg","arg":"key"},
                       "why": "Read."}
                    ]
                  }
                ]
              }
            }"#,
    )
    .unwrap_err();
    assert!(matches!(
        err,
        ManifestError::SessionNeedArgKindMismatch { .. }
    ));
}

#[test]
fn session_ai_verb_without_policy_rejected() {
    let err = Manifest::from_json(
        r#"{
              "id": "summarize",
              "version": "0.1",
              "name": "Summarize",
              "session": {
                "tools": [
                  {
                    "name": "summarize.run",
                    "summary": "Summarize text.",
                    "needs": [
                      {"verb": "ai.chat",
                       "scope": {"kind":"wild"},
                       "why": "Call the model."}
                    ]
                  }
                ]
              }
            }"#,
    )
    .unwrap_err();
    assert!(matches!(
        err,
        ManifestError::SessionAiNeedMissingPolicy { .. }
    ));
}

#[test]
fn desktop_block_parses_with_defaults() {
    let m = parse(
        r#"{
              "id": "notes",
              "version": "0.1",
              "name": "Notes",
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
              "name": "Widget",
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
              "name": "Notes",
              "desktop": {
                "exec": "--ui",
                "name": "My Notes",
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
              "name": "Notes",
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
        "{\"id\":\"notes\",\"version\":\"0.1\",\"name\":\"Notes\",\
             \"desktop\":{\"exec\":\"--gui\\nExec=evil\"}}",
    )
    .unwrap_err();
    assert!(matches!(
        err,
        ManifestError::DesktopInvalid { field: "exec", .. }
    ));
}
