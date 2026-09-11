use super::*;
use serde_json::{json, Value};

fn manifest() -> Value {
    json!({
        "id": "demo", "version": "1", "name": {"en": "Demo"},
        "objects": {
            "entry": {
                "label": {"en": "Entry"},
                "resolve": {"operation": "get", "id_arg": "key", "revision_arg": "revision"}
            }
        },
        "operations": {
            "get": {
                "label": {"en": "Get"},
                "args": [
                    {"name": "key", "kind": "name", "required": true},
                    {"name": "revision", "kind": "text", "binding": "flag"},
                    {"name": "verbose", "kind": "bool"}
                ],
                "needs": [{
                    "verb": "data.kv.read",
                    "scope": {"kind": "from-arg", "arg": "key"},
                    "why": {"en": "Read only the requested key."}
                }]
            }
        }
    })
}

fn parse(value: Value) -> Result<Manifest, ManifestError> {
    Manifest::from_json(&value.to_string())
}

#[test]
fn object_declarations_are_optional_and_preserve_operation_needs() {
    let declared = parse(manifest()).unwrap();
    let mut legacy = manifest();
    legacy.as_object_mut().unwrap().remove("objects");
    let legacy = parse(legacy).unwrap();
    assert!(legacy.objects.is_empty());
    assert_eq!(declared.objects["entry"].resolve.id_arg, "key");
    assert_eq!(
        serde_json::to_value(&declared.operations).unwrap(),
        serde_json::to_value(&legacy.operations).unwrap()
    );
}

#[test]
fn object_resolvers_reject_missing_or_ambiguous_inputs() {
    for change in [
        json!({"operation": "missing", "id_arg": "key"}),
        json!({"operation": "get", "id_arg": "missing"}),
        json!({"operation": "get", "id_arg": "key", "revision_arg": "key"}),
        json!({"operation": "get", "id_arg": "key", "revision_arg": "missing"}),
        json!({"operation": "get", "id_arg": "key", "grant": "anything"}),
    ] {
        let mut value = manifest();
        value["objects"]["entry"]["resolve"] = change;
        assert!(parse(value).is_err());
    }
    for field in [
        "stdin",
        "unbound",
        "optional-id",
        "repeatable-id",
        "positional-revision",
    ] {
        let mut value = manifest();
        match field {
            "stdin" => value["operations"]["get"]["stdin"] = json!(true),
            "unbound" => value["operations"]["get"]["args"]
                .as_array_mut()
                .unwrap()
                .push(
                    json!({"name": "other", "kind": "name", "required": true, "binding": "flag"}),
                ),
            "optional-id" => value["operations"]["get"]["args"][0]["required"] = json!(false),
            "repeatable-id" => value["operations"]["get"]["args"][0]["repeatable"] = json!(true),
            "positional-revision" => {
                value["operations"]["get"]["args"][1]["binding"] = json!("positional")
            }
            _ => unreachable!(),
        }
        assert!(parse(value).is_err(), "{field}");
    }
}

#[test]
fn object_names_localization_and_descriptors_are_bounded_and_closed() {
    for name in ["", "../entry", "Entry", "entry.path", &"x".repeat(65)] {
        let mut value = manifest();
        let object = value["objects"]["entry"].clone();
        value["objects"] = json!({ name: object });
        assert!(parse(value).is_err(), "{name}");
    }
    let mut value = manifest();
    value["objects"]["entry"]["label"] = json!({"zh-CN": "Entry"});
    assert!(parse(value).is_err());
    let mut value = manifest();
    value["objects"]["entry"]["label"] = json!({"en": "x".repeat(241)});
    assert!(parse(value).is_err());
    let mut value = manifest();
    value["objects"]["entry"]["owner_uid"] = json!(0);
    assert!(parse(value).is_err());
    let mut value = manifest();
    let template = value["objects"]["entry"].clone();
    value["objects"] = json!({});
    for index in 0..65 {
        value["objects"][format!("entry{index}")] = template.clone();
    }
    assert!(parse(value).is_err());
}

#[test]
fn bundled_object_types_reuse_existing_operations_without_new_authority() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    for (app, object_type, operation, id_arg) in [
        ("kv", "entry", "get", "key"),
        ("fs", "file", "stat", "path"),
    ] {
        let path = root.join("apps").join(app).join("app.json");
        let manifest = Manifest::from_json(&std::fs::read_to_string(path).unwrap()).unwrap();
        let resolver = &manifest.objects[object_type].resolve;
        assert_eq!(resolver.operation, operation);
        assert_eq!(resolver.id_arg, id_arg);
        assert_eq!(manifest.operations[operation].needs.len(), 1);
        assert!(resolver.revision_arg.is_none());
    }
}
