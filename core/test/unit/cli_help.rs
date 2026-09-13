use super::*;

#[test]
fn activity_capability_policy_commands_have_explicit_non_model_schemas() {
    let schemas = activity_schemas();
    for command in [
        "capability-policy",
        "set-capability-policy",
        "enable-capability-policy",
        "disable-capability-policy",
    ] {
        let schema = schemas
            .iter()
            .find(|schema| schema.command == command)
            .unwrap();
        assert!(schema
            .example
            .starts_with(&format!("cos activity {command} ")));
        let help = command_schema_value("activity", command).unwrap();
        assert_eq!(help["model_callable"], false);
        assert!(help["model_tool"].is_null());
        assert_eq!(help["schema_available"], true);
        assert!(!help["parameters"].as_array().unwrap().is_empty());
    }
    let set = command_schema_value("activity", "set-capability-policy").unwrap();
    let parameters = set["parameters"].to_string();
    assert!(parameters.contains("--policy"));
    assert!(parameters.contains("--expected-revision"));
    assert!(!parameters.contains("--owner"));
    assert!(!parameters.contains("--enabled"));
    for command in ["enable-capability-policy", "disable-capability-policy"] {
        let schema = command_schema_value("activity", command).unwrap();
        assert!(schema["parameters"]
            .to_string()
            .contains("--expected-revision"));
    }
}
