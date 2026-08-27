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
                    {"name": "root", "kind": "path", "default": "/workspace"},
                    {"name": "output", "kind": "path",
                     "default_from": {
                       "arg": "url",
                       "transform": "url-path-basename",
                       "prefix": "~/",
                       "fallback": "download"
                     }}
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
    assert_eq!(parameters[1]["kind"], "positional");
    assert_eq!(parameters[1]["default"], "/workspace");
    assert_eq!(parameters[2]["type"], "string");
    assert_eq!(parameters[2]["required"], false);
    assert_eq!(parameters[2]["kind"], "positional");
    assert_eq!(parameters[2]["default_from"]["arg"], "url");
    assert_eq!(
        parameters[2]["default_from"]["transform"],
        "url-path-basename"
    );
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
        ["command", "timeout", "shell"]
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

    let slack = load(&["gateway", "slack"]);
    assert_eq!(
        slack
            .operations
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["send", "status"]
    );
}
