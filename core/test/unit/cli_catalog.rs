use super::*;

#[test]
fn catalogue_contains_every_public_router_namespace() {
    assert_eq!(
        namespace_names(),
        vec![
            "sys",
            "service",
            "checkpoint",
            "credential",
            "cron",
            "triggers",
            "ai",
            "agent",
            "model",
            "engine",
        ]
    );
}

#[test]
fn agent_usage_is_discoverable_and_model_callable() {
    let namespace = namespace_help("agent").expect("agent namespace");
    assert!(namespace["commands"].get("usage").is_some());
    assert_eq!(namespace["model_tools"]["usage"], "cos_usage");

    let command = command_help("agent", "usage").expect("usage command");
    assert_eq!(command["command"], "cos agent usage");
    assert_eq!(command["model_callable"], true);
    assert_eq!(command["model_tool"], "cos_usage");
}

#[test]
fn catalogue_matches_current_model_and_engine_surfaces() {
    assert_eq!(
        command_names("model").unwrap(),
        vec![
            "list",
            "import",
            "load",
            "unload",
            "infer",
            "embed",
            "image",
            "transcribe",
            "translate",
            "speak",
            "status",
            "bench",
            "rm",
        ]
    );
    assert_eq!(
        command_names("engine").unwrap(),
        vec!["list", "update", "activate", "remove", "unpin"]
    );
}

#[test]
fn root_discloses_namespaces_before_commands() {
    let root = overview("test", 3);
    let agent = root["primitives"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == "agent")
        .unwrap();
    assert_eq!(agent["commands_available"], 24);
    assert!(agent.get("commands").is_none());
    assert_eq!(agent["next"], "cos agent");
}

#[test]
fn nested_agent_namespaces_are_discoverable() {
    let budget = nested_commands(&["agent", "budget"]).unwrap();
    assert!(budget.iter().any(|(name, _)| *name == "user"));

    let user = nested_commands(&["agent", "budget", "user"]).unwrap();
    assert_eq!(
        user.into_iter().map(|(name, _)| name).collect::<Vec<_>>(),
        vec!["show", "path"]
    );

    let usage = nested_commands(&["agent", "usage"]).unwrap();
    assert!(usage.iter().any(|(name, _)| *name == "provider"));

    let skill_usage = nested_commands(&["agent", "skills", "usage"]).unwrap();
    assert_eq!(
        skill_usage
            .into_iter()
            .map(|(name, _)| name)
            .collect::<Vec<_>>(),
        vec!["stats", "record", "path", "clear"]
    );
}
