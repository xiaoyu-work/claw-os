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
