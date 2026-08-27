use super::*;

#[test]
fn operation_schema_preserves_literal_and_bound_defaults() {
    let manifest = Manifest::from_json(
        r#"{
              "id": "defaults",
              "version": "0.1",
              "name": "Defaults",
              "operations": {
                "run": {
                  "label": "Run",
                  "args": [
                    {"name": "url", "kind": "text", "required": true},
                    {"name": "root", "kind": "path", "binding": "flag",
                     "default": "/workspace"},
                    {"name": "output", "kind": "path",
                     "default_from": {
                       "arg": "url",
                       "transform": "url-path-basename",
                       "prefix": "~/",
                       "fallback": "download"
                     }},
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
    assert_eq!(parameters[2]["default_from"]["arg"], "url");
    assert_eq!(
        parameters[2]["default_from"]["transform"],
        "url-path-basename"
    );
    assert_eq!(parameters[3]["type"], "boolean");
    assert_eq!(parameters[3]["binding"], "flag");
}

#[test]
fn bundled_apps_declare_their_optional_path_defaults() {
    let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let load = |id: &str| {
        let path = repository.join("apps").join(id).join("app.json");
        Manifest::from_json(&std::fs::read_to_string(path).unwrap()).unwrap()
    };

    let fs = load("fs");
    assert_eq!(
        fs.operations["ls"].args[0].default,
        Some(serde_json::json!("."))
    );
    assert_eq!(
        fs.operations["search"].args[1].default,
        Some(serde_json::json!("/workspace"))
    );

    let net = load("net");
    let output = &net.operations["download"].args[1];
    let binding = output.default_from.as_ref().unwrap();
    assert_eq!(binding.arg, "url");
    assert_eq!(
        binding.transform,
        crate::caps::manifest::ArgDefaultTransform::UrlPathBasename
    );
    assert_eq!(binding.prefix, "~/");
    assert_eq!(binding.fallback.as_deref(), Some("download"));
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
    let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let load = |path: &[&str]| {
        let path = path
            .iter()
            .fold(repository.join("apps"), |path, component| {
                path.join(component)
            })
            .join("app.json");
        Manifest::from_json(&std::fs::read_to_string(path).unwrap()).unwrap()
    };

    let exec = load(&["exec"]);
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

    let slack = load(&["gateway", "slack"]);
    assert_eq!(
        slack
            .operations
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["send", "status"]
    );

    let fs = load(&["fs"]);
    for operation in ["rename", "move", "copy", "read_bytes", "write_bytes"] {
        assert!(
            fs.operations.contains_key(operation),
            "fs manifest omitted `{operation}`"
        );
    }
    assert_eq!(fs.operations["rename"].needs.len(), 2);
    assert_eq!(fs.operations["copy"].needs.len(), 2);
    assert_eq!(
        fs.operations["read_bytes"].args[1].effective_binding(),
        crate::caps::manifest::ArgBinding::Flag
    );

    let mail = load(&["mail-ai"]);
    for (operation, required) in [
        ("summarize", "body"),
        ("smart_reply", "thread"),
        ("smart_compose", "intent"),
        ("translate", "text"),
        ("chat", "question"),
    ] {
        assert!(
            mail.operations[operation]
                .args
                .iter()
                .any(|arg| arg.name == required && arg.required),
            "mail-ai `{operation}` omitted required flag `{required}`"
        );
    }
    assert_eq!(mail.operations["triage"].args.len(), 4);
}

#[test]
fn bundled_conditional_capabilities_are_exact() {
    let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let load = |path: &[&str]| {
        let path = path
            .iter()
            .fold(repository.join("apps"), |path, component| {
                path.join(component)
            })
            .join("app.json");
        Manifest::from_json(&std::fs::read_to_string(path).unwrap()).unwrap()
    };
    let active = |caps: Vec<Vec<crate::caps::Cap>>| {
        caps.into_iter().flatten().collect::<Vec<_>>()
    };

    let calendar = load(&["calendar"]);
    let local = active(calendar.resolve_needs("today", &BTreeMap::new()).unwrap());
    assert!(local.iter().all(|cap| cap.verb != crate::caps::Verb::SECRET_READ));
    assert!(local.iter().all(|cap| cap.verb != crate::caps::Verb::NET_DIAL));
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
    assert!(!google.iter().any(|cap| {
        cap.scope == crate::caps::Scope::name("default/MICROSOFT_ACCESS_TOKEN")
    }));

    let doc = load(&["doc"]);
    let stdin = active(doc.resolve_needs("summarize", &BTreeMap::new()).unwrap());
    assert!(stdin.iter().all(|cap| cap.verb != crate::caps::Verb::FS_READ));
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

    let network = load(&["network-manager"]);
    let open_wifi = active(
        network
            .resolve_needs(
                "wifi-connect",
                &BTreeMap::from([("ssid".to_string(), serde_json::json!("guest"))]),
            )
            .unwrap(),
    );
    assert!(open_wifi
        .iter()
        .all(|cap| cap.verb != crate::caps::Verb::SECRET_READ));
    let protected_wifi = active(
        network
            .resolve_needs(
                "wifi-connect",
                &BTreeMap::from([
                    ("ssid".to_string(), serde_json::json!("home")),
                    (
                        "credential".to_string(),
                        serde_json::json!("wifi/home"),
                    ),
                ]),
            )
            .unwrap(),
    );
    assert!(protected_wifi.iter().any(|cap| {
        cap.verb == crate::caps::Verb::SECRET_READ
            && cap.scope == crate::caps::Scope::name("wifi/home")
    }));

    let search = load(&["search"]);
    let brave = active(
        search
            .resolve_needs(
                "web",
                &BTreeMap::from([
                    ("provider".to_string(), serde_json::json!("brave")),
                    ("query".to_string(), serde_json::json!(["claw"])),
                ]),
            )
            .unwrap(),
    );
    let brave_secrets = brave
        .iter()
        .filter(|cap| cap.verb == crate::caps::Verb::SECRET_READ)
        .map(|cap| cap.scope.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        brave_secrets,
        [crate::caps::Scope::name("default/BRAVE_SEARCH_API_KEY")]
    );
    assert!(brave.iter().any(|cap| {
        cap.verb == crate::caps::Verb::NET_DIAL
            && cap.scope == crate::caps::Scope::host("api.search.brave.com")
    }));
    assert!(brave.iter().all(|cap| cap.scope != crate::caps::Scope::Wild));
}

#[test]
fn bundled_schema_exposes_repeatables_choices_and_stdin() {
    let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let load = |id: &str| {
        Manifest::from_json(
            &std::fs::read_to_string(repository.join("apps").join(id).join("app.json"))
                .unwrap(),
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

    let calendar = load("calendar");
    let schema = operation_schema(&calendar.operations["today"]);
    assert_eq!(
        schema["parameters"][0]["enum"],
        serde_json::json!(["local", "google", "outlook"])
    );

    let doc = load("doc");
    assert_eq!(operation_schema(&doc.operations["summarize"])["stdin"], true);
}
