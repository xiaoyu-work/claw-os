use super::*;

#[test]
fn activity_trigger_options_are_discoverable_without_model_or_owner_authority() {
    for command in cli_catalog::command_names("triggers").unwrap() {
        let schema = command_schema_value("triggers", command).unwrap();
        assert_eq!(schema["schema_available"], true, "{command}");
        assert_eq!(schema["model_callable"], false, "{command}");
        assert!(schema["model_tool"].is_null());
    }
    for command in ["add", "list"] {
        let schema = command_schema_value("triggers", command).unwrap();
        let fields = schema["parameters"].as_array().unwrap();
        let activity = fields
            .iter()
            .find(|field| field["name"] == "--activity")
            .unwrap();
        assert_eq!(activity["type"], "uuid");
        assert_eq!(activity["kind"], "flag");
        assert_eq!(activity["required"], false);
        assert!(fields
            .iter()
            .all(|field| field["name"] != "--owner" && field["name"] != "--caps"));
    }
    let add = command_schema_value("triggers", "add").unwrap();
    let fields = add["parameters"].as_array().unwrap();
    let required: Vec<_> = fields
        .iter()
        .filter(|field| field["required"] == true)
        .map(|field| field["name"].as_str().unwrap())
        .collect();
    assert_eq!(required, vec!["--id", "--prompt"]);
    assert!(add["parameters"]
        .to_string()
        .contains("finite execution limits"));
}

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
    assert_eq!(stdio["description"], cli_catalog::APP_STDIO_DESCRIPTION);
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

#[test]
fn activity_monetary_budget_help_is_explicit_and_non_model_callable() {
    for command in [
        "monetary-budget",
        "set-monetary-budget",
        "enable-monetary-budget",
        "disable-monetary-budget",
    ] {
        let help = command_schema_value("activity", command).unwrap();
        assert_eq!(help["schema_available"], true);
        assert_eq!(help["model_callable"], false);
        assert!(help["model_tool"].is_null());
        assert!(!help["parameters"].to_string().contains("--owner"));
    }
    let set = command_schema_value("activity", "set-monetary-budget").unwrap();
    let parameters = set["parameters"].to_string();
    for flag in [
        "--max-total-microusd",
        "--input-microusd-per-million-tokens",
        "--output-microusd-per-million-tokens",
        "--max-output-tokens-per-turn",
    ] {
        assert!(parameters.contains(flag), "{flag}");
    }
    assert!(set["description"].as_str().unwrap().contains("preserving"));
}
