use super::*;

const ID: &str = "00000000-0000-4000-8000-000000000001";

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

#[test]
fn create_uses_the_shared_broker_contract() {
    let (route, params) = parse(
        "create",
        &args(&[
            "Release v2",
            "--goal",
            "Publish on Friday",
            "--criteria",
            "Release has been reviewed",
            "--boundaries",
            "Ask before publishing",
            "--resource",
            "Draft=/home/user/release.md",
        ]),
    )
    .unwrap();
    assert_eq!(route, Command::ActivityCreate);
    assert_eq!(params["title"], "Release v2");
    assert_eq!(params["completion_criteria"], "Release has been reviewed");
    assert_eq!(params["resources"][0]["reference"], "/home/user/release.md");
    assert!(params.get("owner_uid").is_none());
}

#[test]
fn invalid_cli_requests_fail_before_connecting() {
    for (command, values) in [
        ("create", args(&["Release v2"])),
        ("create", args(&["Release", "--goal", ""])),
        ("list", args(&["--owner-uid", "0"])),
        ("list", args(&["--state", "invented"])),
        ("show", args(&["../foreign"])),
        ("show", args(&[ID, "--limit", "0"])),
        ("show", args(&[ID, "--limit", "101"])),
        ("update", args(&[ID])),
        ("update", args(&[ID, "--resource", "not-a-reference"])),
        ("pause", args(&[ID, "--approve", "true"])),
        ("run", args(&[ID, "--max-turns", "0"])),
    ] {
        assert!(parse(command, &values).is_err(), "{command}: {values:?}");
    }
}

#[test]
fn completion_is_an_explicit_user_statement() {
    assert!(parse("complete", &args(&[ID])).is_err());
    assert!(parse("complete", &args(&[ID, "--note", "  "])).is_err());
    let (route, params) = parse(
        "complete",
        &args(&[ID, "--note", "I reviewed and published the release"]),
    )
    .unwrap();
    assert_eq!(route, Command::ActivityTransition);
    assert_eq!(params["state"], "completed");
    assert_eq!(
        params["completion_note"],
        "I reviewed and published the release"
    );
}

#[test]
fn lifecycle_commands_only_choose_a_broker_transition() {
    for (command, state) in [
        ("pause", "paused"),
        ("resume", "active"),
        ("cancel", "cancelled"),
    ] {
        let (route, params) = parse(command, &args(&[ID])).unwrap();
        assert_eq!(route, Command::ActivityTransition);
        assert_eq!(params, json!({"id": ID, "state": state}));
    }
}

#[test]
fn run_can_continue_a_session_or_use_the_activity_goal() {
    let (route, params) = parse("run", &args(&[ID])).unwrap();
    assert_eq!(route, Command::ActivityRun);
    assert_eq!(params, json!({"id": ID}));

    let (_, params) = parse(
        "run",
        &args(&[
            ID,
            "Prepare a draft",
            "--session",
            "session-1",
            "--max-turns",
            "4",
        ]),
    )
    .unwrap();
    assert_eq!(params["prompt"], "Prepare a draft");
    assert_eq!(params["session_id"], "session-1");
    assert_eq!(params["max_turns"], 4);
    assert!(parse("run", &args(&[ID, "one", "two"])).is_err());
}

#[test]
fn resources_are_explicit_references_not_files_to_execute() {
    let (_, params) = parse(
        "update",
        &args(&[
            ID,
            "--resource",
            "Draft=relative draft.md",
            "--resource",
            "Build=job:123",
        ]),
    )
    .unwrap();
    assert_eq!(params["resources"].as_array().unwrap().len(), 2);
    let (_, cleared) = parse("update", &args(&[ID, "--clear-resources"])).unwrap();
    assert_eq!(cleared["resources"], json!([]));
    assert!(parse(
        "update",
        &args(&[ID, "--resource", "Draft=file", "--clear-resources"]),
    )
    .is_err());
}

#[test]
fn every_activity_command_has_headless_help_and_schema() {
    for command in crate::cli_catalog::command_names("activity").unwrap() {
        let output = crate::router::dispatch(&args(&["activity", command, "--schema"]))
            .unwrap()
            .unwrap();
        let schema: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(schema["schema_available"], true, "{command}");
        assert_eq!(schema["model_callable"], false, "{command}");
    }
}
