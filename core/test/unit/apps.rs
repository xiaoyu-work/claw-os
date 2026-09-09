use super::*;

mod app_sources {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/support/app_sources.rs"));
}

#[test]
fn operation_schema_preserves_literal_defaults() {
    let manifest = Manifest::from_json(
        r#"{
              "id": "defaults",
              "version": "0.1",
              "name": {"en": "Defaults"},
              "operations": {
                "run": {
                  "label": {"en": "Run"},
                  "args": [
                    {"name": "url", "kind": "text", "required": true},
                    {"name": "root", "kind": "path", "binding": "flag",
                     "default": "/workspace"},
                    {"name": "output", "kind": "path", "default": "~/download"},
                    {"name": "verbose", "kind": "bool", "default": false}
                  ]
                }
              }
            }"#,
    )
    .unwrap();

    let schema = operation_schema(&manifest.operations["run"]);
    let parameters = schema["parameters"].as_array().unwrap();

    assert_eq!(parameters[1]["type"], "string");
    assert_eq!(parameters[1]["required"], false);
    assert_eq!(parameters[1]["kind"], "flag");
    assert_eq!(parameters[1]["binding"], "flag");
    assert_eq!(parameters[1]["default"], "/workspace");
    assert_eq!(parameters[2]["type"], "string");
    assert_eq!(parameters[2]["required"], false);
    assert_eq!(parameters[2]["kind"], "positional");
    assert_eq!(parameters[2]["default"], "~/download");
    assert_eq!(parameters[3]["type"], "boolean");
    assert_eq!(parameters[3]["binding"], "flag");
}

#[test]
fn fs_mcp_tools_declare_their_optional_path_defaults() {
    let fs = Manifest::from_json(
        &std::fs::read_to_string(app_sources::app_dir("fs").join("app.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        mcp_tool_for_command(&fs, "ls").unwrap().args[0].default,
        Some(serde_json::json!("."))
    );
    assert_eq!(
        mcp_tool_for_command(&fs, "search").unwrap().args[1].default,
        Some(serde_json::json!("/workspace"))
    );
}

#[test]
fn bundled_apps_declare_their_optional_path_defaults() {
    let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let net = Manifest::from_json(
        &std::fs::read_to_string(repository.join("apps/net/app.json")).unwrap(),
    )
    .unwrap();
    let output = &net.operations["download"].args[1];
    assert!(output.required);
    assert_eq!(
        output.effective_binding(),
        crate::caps::manifest::ArgBinding::Positional
    );
    assert!(output.default.is_none());
}

#[test]
fn bundled_python_entries_do_not_own_operation_schemas() {
    fn inspect(dir: &std::path::Path, duplicates: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.is_dir() {
                inspect(&path, duplicates);
            } else if path.extension().and_then(|extension| extension.to_str()) == Some("py") {
                let source = std::fs::read_to_string(&path).unwrap();
                if source.contains("def _schema(") || source.contains("__schema__") {
                    duplicates.push(path.display().to_string());
                }
            }
        }
    }

    let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let mut duplicates = Vec::new();
    inspect(&repository.join("apps"), &mut duplicates);
    assert!(
        duplicates.is_empty(),
        "app.json is the sole operation schema owner; duplicate runtime schemas: {duplicates:?}"
    );
}

#[test]
fn known_first_party_schema_drift_is_resolved_in_manifests() {
    let exec = Manifest::from_json(
        &std::fs::read_to_string(app_sources::app_dir("exec").join("app.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        exec.operations["run"]
            .args
            .iter()
            .map(|arg| arg.name.as_str())
            .collect::<Vec<_>>(),
        ["command", "arguments", "timeout", "shell"]
    );
    assert_eq!(
        exec.operations["script"]
            .args
            .iter()
            .map(|arg| arg.name.as_str())
            .collect::<Vec<_>>(),
        ["code", "lang", "file", "timeout"]
    );
    assert_eq!(exec.operations["which"].args[0].name, "name");
    assert_eq!(exec.operations["stop"].args[0].name, "pid");
    let timeout = &exec.operations["run"].args[2];
    assert_eq!(timeout.kind, crate::caps::manifest::ArgKind::Integer);
    assert_eq!(
        timeout.effective_binding(),
        crate::caps::manifest::ArgBinding::Flag
    );
    assert_eq!(timeout.default, Some(serde_json::json!(300)));
    assert_eq!(
        exec.operations["script"].args[1].default,
        Some(serde_json::json!("bash"))
    );

    let fs = Manifest::from_json(
        &std::fs::read_to_string(app_sources::app_dir("fs").join("app.json")).unwrap(),
    )
    .unwrap();
    assert!(is_mcp_only_cli(&fs));
    for command in ["rename", "move", "copy", "read_bytes", "write_bytes"] {
        assert!(
            mcp_tool_for_command(&fs, command).is_ok(),
            "fs manifest omitted `{command}`"
        );
    }
    assert_eq!(mcp_tool_for_command(&fs, "rename").unwrap().needs.len(), 2);
    assert_eq!(mcp_tool_for_command(&fs, "copy").unwrap().needs.len(), 2);
    assert_eq!(
        mcp_tool_for_command(&fs, "read_bytes").unwrap().args[1].effective_binding(),
        crate::caps::manifest::ArgBinding::Flag
    );
}

#[test]
fn slack_schema_preserves_outbound_only_operations() {
    let slack = Manifest::from_json(
        &std::fs::read_to_string(app_sources::app_dir("gateway/slack").join("app.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        slack.operations.keys().map(String::as_str).collect::<Vec<_>>(),
        ["send", "status"]
    );
}

#[test]
fn bundled_conditional_capabilities_are_exact() {
    let active = |caps: Vec<Vec<crate::caps::Cap>>| caps.into_iter().flatten().collect::<Vec<_>>();

    let calendar = Manifest::from_json(
        &std::fs::read_to_string(app_sources::app_dir("calendar").join("app.json")).unwrap(),
    )
    .unwrap();
    let local = active(
        calendar
            .resolve_needs(
                "today",
                &BTreeMap::from([("provider".to_string(), serde_json::json!("local"))]),
            )
            .unwrap(),
    );
    assert!(local
        .iter()
        .all(|cap| cap.verb != crate::caps::Verb::SECRET_READ));
    assert!(local
        .iter()
        .all(|cap| cap.verb != crate::caps::Verb::NET_DIAL));
    let google = active(
        calendar
            .resolve_needs(
                "today",
                &BTreeMap::from([("provider".to_string(), serde_json::json!("google"))]),
            )
            .unwrap(),
    );
    assert!(google.iter().any(|cap| {
        cap.verb == crate::caps::Verb::SECRET_READ
            && cap.scope == crate::caps::Scope::name("default/GOOGLE_ACCESS_TOKEN")
    }));
    assert!(google.iter().any(|cap| {
        cap.verb == crate::caps::Verb::NET_DIAL
            && cap.scope == crate::caps::Scope::host("www.googleapis.com")
    }));
    assert!(google
        .iter()
        .all(|cap| cap.verb != crate::caps::Verb::DATA_DB_READ));
    assert!(!google
        .iter()
        .any(|cap| { cap.scope == crate::caps::Scope::name("default/MICROSOFT_ACCESS_TOKEN") }));

    let doc = Manifest::from_json(
        &std::fs::read_to_string(app_sources::app_dir("doc").join("app.json")).unwrap(),
    )
    .unwrap();
    let stdin = active(doc.resolve_needs("summarize", &BTreeMap::new()).unwrap());
    assert!(stdin
        .iter()
        .all(|cap| cap.verb != crate::caps::Verb::FS_READ));
    let file = active(
        doc.resolve_needs(
            "summarize",
            &BTreeMap::from([("file".to_string(), serde_json::json!("/workspace/a.md"))]),
        )
        .unwrap(),
    );
    assert!(file.iter().any(|cap| {
        cap.verb == crate::caps::Verb::FS_READ
            && cap.scope == crate::caps::Scope::path("/workspace/a.md")
    }));
}

#[test]
fn network_manager_mcp_capabilities_are_exact() {
    use crate::caps::{Cap, Scope, Verb};

    let network = Manifest::from_json(
        &std::fs::read_to_string(app_sources::app_dir("network-manager").join("app.json")).unwrap(),
    )
    .unwrap();
    assert!(network.operations.is_empty());
    for credential in [None, Some("wifi/home")] {
        let mut args = BTreeMap::from([("ssid".to_string(), serde_json::json!("Cafe"))]);
        let mut expected = vec![Cap::new(Verb::NET_MANAGE, Scope::name("wifi"))];
        if let Some(reference) = credential {
            args.insert("credential".to_string(), serde_json::json!(reference));
            expected.push(Cap::new(Verb::SECRET_READ, Scope::name(reference)));
        }
        let caps = network
            .resolve_mcp_tool_needs("network-manager.wifi-connect", &args)
            .unwrap()
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        assert_eq!(caps, expected);
    }
    for (tool, arg, verb, scope) in [
        ("status", None, Verb::SYS_OBSERVE, "network"),
        ("wifi-list", None, Verb::SYS_OBSERVE, "network"),
        ("connection-list", None, Verb::SYS_OBSERVE, "network"),
        ("vpn-list", None, Verb::SYS_OBSERVE, "network"),
        ("wifi-toggle", Some(("state", "off")), Verb::NET_MANAGE, "wifi"),
        ("airplane", Some(("state", "on")), Verb::NET_MANAGE, "airplane"),
        ("wifi-disconnect", Some(("device", "wlan0")), Verb::NET_MANAGE, "wifi"),
        ("wifi-forget", Some(("connection", "Cafe")), Verb::NET_MANAGE, "wifi"),
        ("vpn-up", Some(("profile", "work")), Verb::NET_MANAGE, "vpn"),
        ("vpn-down", Some(("profile", "work")), Verb::NET_MANAGE, "vpn"),
    ] {
        let args = arg
            .into_iter()
            .map(|(name, value)| (name.to_string(), serde_json::json!(value)))
            .collect();
        let caps = network
            .resolve_mcp_tool_needs(&format!("network-manager.{tool}"), &args)
            .unwrap()
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        assert_eq!(caps, [Cap::new(verb, Scope::name(scope))], "{tool}");
    }
}

#[test]
fn search_mcp_provider_capabilities_are_exact() {
    use crate::caps::{Scope, Verb};

    let search = Manifest::from_json(
        &std::fs::read_to_string(app_sources::app_dir("search").join("app.json")).unwrap(),
    )
    .unwrap();
    assert!(search.operations.is_empty());
    for tool in ["search.web", "search.image"] {
        for (provider, host, secrets) in [
            (
                "brave",
                "api.search.brave.com",
                vec![Scope::name("default/BRAVE_SEARCH_API_KEY")],
            ),
            (
                "google",
                "www.googleapis.com",
                vec![
                    Scope::name("default/GOOGLE_SEARCH_API_KEY"),
                    Scope::name("default/GOOGLE_SEARCH_ENGINE_ID"),
                ],
            ),
        ] {
            let caps = search
                .resolve_mcp_tool_needs(
                    tool,
                    &BTreeMap::from([
                        ("provider".to_string(), serde_json::json!(provider)),
                        ("query".to_string(), serde_json::json!("claw")),
                    ]),
                )
                .unwrap()
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();
            let scopes = |verb| {
                caps.iter()
                    .filter(|cap| cap.verb == verb)
                    .map(|cap| cap.scope.clone())
                    .collect::<Vec<_>>()
            };
            assert_eq!(scopes(Verb::SECRET_READ), secrets, "{tool}: {provider}");
            assert_eq!(scopes(Verb::NET_DIAL), [Scope::host(host)], "{tool}: {provider}");
            assert!(caps.iter().all(|cap| cap.scope != Scope::Wild));
        }
    }
}

#[test]
fn bundled_schema_exposes_repeatables_choices_and_stdin() {
    let load = |id: &str| {
        Manifest::from_json(
            &std::fs::read_to_string(app_sources::app_dir(id).join("app.json")).unwrap(),
        )
        .unwrap()
    };

    let net = load("net");
    let schema = operation_schema(&net.operations["fetch"]);
    let header = schema["parameters"]
        .as_array()
        .unwrap()
        .iter()
        .find(|arg| arg["name"] == "header")
        .unwrap();
    assert_eq!(header["type"], "array");
    assert_eq!(header["items"]["type"], "string");
    assert_eq!(header["repeatable"], true);
    let download = operation_schema(&net.operations["download"]);
    assert_eq!(download["parameters"][1]["required"], true);
    assert_eq!(download["parameters"][1]["binding"], "positional");

    let calendar = Manifest::from_json(
        &std::fs::read_to_string(app_sources::app_dir("calendar").join("app.json")).unwrap(),
    )
    .unwrap();
    let schema = operation_schema(&calendar.operations["today"]);
    assert_eq!(
        schema["parameters"][0]["enum"],
        serde_json::json!(["local", "google", "outlook"])
    );

    let doc = load("doc");
    assert_eq!(
        operation_schema(&doc.operations["summarize"])["stdin"],
        true
    );
}

#[test]
fn googlechat_schema_preserves_recipient_flag_binding() {
    let googlechat = Manifest::from_json(
        &std::fs::read_to_string(app_sources::app_dir("gateway/googlechat").join("app.json"))
            .unwrap(),
    )
    .unwrap();
    let schema = operation_schema(&googlechat.operations["send"]);
    assert_eq!(schema["parameters"][1]["binding"], "flag");
}

#[test]
fn usb_authorize_schema_preserves_conditional_confirmation() {
    let manifest = Manifest::from_json(
        &std::fs::read_to_string(app_sources::app_dir("usb-guard").join("app.json")).unwrap(),
    )
    .unwrap();
    assert!(is_mcp_only_cli(&manifest));
    let authorize = tool_schema(mcp_tool_for_command(&manifest, "authorize").unwrap());
    assert_eq!(
        authorize["parameters"][2]["required_when"],
        serde_json::json!({"kind":"arg-equals","arg":"state","value":"off"})
    );
    assert_eq!(authorize["parameters"][2]["binding"], "flag");
    assert_eq!(authorize["parameters"][2]["required"], false);
    assert_eq!(authorize["parameters"][2]["enum"], serde_json::json!([true]));
    assert_eq!(authorize["stdin"], false);
}

#[test]
fn mcp_only_cli_command_lookup_and_schema() {
    use crate::caps::manifest::ArgBinding;
    let manifest = Manifest::from_json(
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
                    "args": [
                      {"name": "key", "kind": "text", "required": true, "binding": "positional"},
                      {"name": "raw", "kind": "bool", "binding": "flag"}
                    ]
                  }
                ]
              }
            }"#,
    )
    .unwrap();

    // The staged gate: no operations + an mcp service => MCP-only CLI dispatch.
    assert!(is_mcp_only_cli(&manifest));

    // Exact `<app_id>.<command>` lookup; a non-matching command is rejected
    // rather than guessed, and the bare tool name is not a command.
    let tool = mcp_tool_for_command(&manifest, "get").unwrap();
    assert_eq!(tool.name, "kv.get");
    assert_eq!(tool.args[0].effective_binding(), ArgBinding::Positional);
    assert_eq!(tool.args[1].effective_binding(), ArgBinding::Flag);
    assert!(mcp_tool_for_command(&manifest, "missing").is_err());
    assert!(mcp_tool_for_command(&manifest, "kv.get").is_err());

    // The CLI schema is derived from the tool and carries `binding` metadata,
    // and MCP tools take no piped stdin in this foundation.
    let schema = tool_schema(tool);
    assert_eq!(schema["parameters"][0]["binding"], "positional");
    assert_eq!(schema["parameters"][1]["binding"], "flag");
    assert_eq!(schema["stdin"], false);
}

#[test]
fn notify_product_cli_aliases_input_bindings_and_distinct_grants_are_preserved() {
    use crate::caps::{Cap, Scope, Verb};
    let manifest = Manifest::from_json(
        &std::fs::read_to_string(app_sources::app_dir("notify").join("app.json")).unwrap(),
    )
    .unwrap();
    assert!(is_mcp_only_cli(&manifest));
    let send = mcp_tool_for_command(&manifest, "send").unwrap();
    let list = mcp_tool_for_command(&manifest, "list").unwrap();
    assert_eq!(send.name, "notify.send");
    assert_eq!(list.name, "notify.list");
    assert!(mcp_tool_for_command(&manifest, "post").is_err());
    assert!(mcp_tool_for_command(&manifest, "close").is_err());
    let values = crate::caps::args::bind_cli_args(
        &send.args,
        &["Plain message".into(), "--urgent".into()],
    )
    .unwrap();
    assert_eq!(values["message"], "Plain message");
    assert_eq!(values["urgent"], true);
    assert_eq!(
        manifest
            .resolve_mcp_tool_needs(&send.name, &values)
            .unwrap()
            .into_iter()
            .flatten()
            .collect::<Vec<_>>(),
        vec![Cap::unscoped(Verb::UI_NOTIFY)]
    );
    let values = crate::caps::args::bind_cli_args(&list.args, &[]).unwrap();
    assert_eq!(values["limit"], 20);
    assert_eq!(
        manifest
            .resolve_mcp_tool_needs(&list.name, &values)
            .unwrap()
            .into_iter()
            .flatten()
            .collect::<Vec<_>>(),
        vec![Cap::new(Verb::DATA_INBOX_READ, Scope::Wild)]
    );
    assert_eq!(tool_schema(send)["parameters"][0]["binding"], "positional");
    assert_eq!(tool_schema(send)["parameters"][1]["binding"], "flag");
    assert_eq!(tool_schema(list)["parameters"][0]["type"], "integer");
    assert_eq!(tool_schema(list)["parameters"][0]["binding"], "flag");
    for args in [
        vec!["--owner-uid".into(), "0".into()],
        vec!["--limit".into(), "true".into()],
    ] {
        assert!(crate::caps::args::bind_cli_args(&list.args, &args).is_err());
    }
    let paths = crate::caps::args::PathContext {
        home: "/home/tester".into(),
        cwd: None,
    };
    for field in [
        "owner_uid",
        "source",
        "session",
        "session_id",
        "task_id",
        "confirm",
    ] {
        assert!(manifest
            .resolve_mcp_tool_call(
                &list.name,
                &std::collections::BTreeMap::from([(field.into(), json!("forged"))]),
                &paths,
            )
            .is_err());
    }
}

#[test]
fn an_operation_app_is_not_treated_as_mcp_only() {
    // Staged migration: an App that still declares operations keeps the legacy
    // CLI dispatch even when it also exposes an mcp service.
    let manifest = Manifest::from_json(
        r#"{
              "schema_version": 2,
              "id": "hybrid",
              "version": "0.1",
              "name": {"en": "Hybrid"},
              "operations": {"say": {"label": {"en": "Say"}}},
              "mcp": {"tools": [{"name": "hybrid.say", "summary": {"en": "Say"}}]}
            }"#,
    )
    .unwrap();
    assert!(!is_mcp_only_cli(&manifest));
}
