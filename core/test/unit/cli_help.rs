use super::*;

#[test]
fn merged_help_preserves_main_reviews_stdio_and_all_activity_commands() {
    for namespace in ["review", "activity", "object", "operation"] {
        for command in cli_catalog::command_names(namespace).unwrap() {
            let schema = command_schema_value(namespace, command).unwrap();
            assert_eq!(schema["schema_available"], true, "{namespace} {command}");
            assert_eq!(schema["model_callable"], false, "{namespace} {command}");
        }
    }
    let stdio = command_schema_value("app", "stdio").unwrap();
    assert_eq!(stdio["schema_available"], true);
    assert_eq!(stdio["model_callable"], false);
    assert_eq!(stdio["stdin"], true);
    assert_eq!(stdio["output_format"], "opaque");
    assert_eq!(
        stdio["description"],
        cli_catalog::APP_STDIO_DESCRIPTION
    );
}

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
