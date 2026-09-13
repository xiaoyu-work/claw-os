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
fn object_resolver_uses_mcp_command_metadata_without_a_legacy_operation() {
    let mut value = manifest();
    let operation = value["operations"]["get"].clone();
    value["schema_version"] = json!(2);
    value.as_object_mut().unwrap().remove("operations");
    value["mcp"] = json!({"tools":[{
        "name":"demo.get", "summary":{"en":"Read a key"},
        "args":operation["args"], "needs":operation["needs"]
    }]});
    let parsed = parse(value.clone()).unwrap();
    assert!(parsed.operations.is_empty());
    let args = parsed.objects["entry"].resolve.arguments(&parsed).unwrap();
    assert_eq!(args[0].name, "key");
    assert_eq!(parsed.mcp_tool_for_command("get").unwrap().needs.len(), 1);
    value["objects"]["entry"]["resolve"]["operation"] = json!("demo.get");
    assert!(
        parse(value).is_err(),
        "a public command is not a second fuzzy tool selector"
    );
}

#[test]
fn declared_operation_precedence_cannot_be_bypassed_by_an_object_resolver() {
    let mut value = manifest();
    value["schema_version"] = json!(2);
    value["operations"]["get"]["stdin"] = json!(true);
    value["mcp"] = json!({"tools":[{
        "name":"demo.get","summary":{"en":"MCP alternative"},
        "args":[{"name":"key","kind":"name","required":true}]
    }]});
    assert!(
        parse(value).is_err(),
        "an invalid selected operation must not fall back to MCP"
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
fn retired_argument_selectors_cannot_reenter_through_object_metadata() {
    for field in ["trusted_resolver", "default_from"] {
        let mut value = manifest();
        value["operations"]["get"]["args"][0][field] = json!("runtime-selected");
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
