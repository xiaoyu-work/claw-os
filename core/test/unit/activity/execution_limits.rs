use super::*;

const ID: &str = "00000000-0000-4000-8000-000000000001";

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

#[test]
fn execution_limit_commands_forward_only_limits_and_expected_revision() {
    let (command, params) = parse(
        "set-execution-limits",
        &args(&[
            ID,
            "--max-attempts",
            "10",
            "--max-turns",
            "5",
            "--expires-at",
            "2099-01-01T00:00:00Z",
        ]),
    )
    .unwrap();
    assert_eq!(command, Command::ActivityExecutionLimitsSet);
    assert_eq!(params["id"], ID);
    assert_eq!(params["limits"]["max_attempts"], 10);
    assert_eq!(params["limits"]["max_turns_per_attempt"], 5);
    assert!(params.get("owner_uid").is_none());
    assert!(params.get("expected_revision").is_none());
    for (name, enabled) in [
        ("enable-execution-limits", true),
        ("disable-execution-limits", false),
    ] {
        let (command, params) = parse(name, &args(&[ID, "--revision", "2"])).unwrap();
        assert_eq!(command, Command::ActivityExecutionLimitsEnabled);
        assert_eq!(
            params,
            json!({"id":ID,"expected_revision":2,"enabled":enabled})
        );
    }
}

#[test]
fn invalid_execution_limits_fail_before_requesting_the_broker() {
    for values in [
        args(&[ID]),
        args(&[
            ID,
            "--max-attempts",
            "0",
            "--max-turns",
            "1",
            "--expires-at",
            "2099-01-01T00:00:00Z",
        ]),
        args(&[
            ID,
            "--max-attempts",
            "1001",
            "--max-turns",
            "1",
            "--expires-at",
            "2099-01-01T00:00:00Z",
        ]),
        args(&[
            ID,
            "--max-attempts",
            "1",
            "--max-turns",
            "101",
            "--expires-at",
            "2099-01-01T00:00:00Z",
        ]),
        args(&[
            ID,
            "--max-attempts",
            "1",
            "--max-turns",
            "1",
            "--expires-at",
            "not-a-time",
        ]),
        args(&[ID, "--owner", "0"]),
    ] {
        assert!(
            parse("set-execution-limits", &values).is_err(),
            "{values:?}"
        );
    }
    assert!(parse("disable-execution-limits", &args(&[ID])).is_err());
    assert!(parse("enable-execution-limits", &args(&[ID, "--revision", "0"])).is_err());
    assert!(parse("execution-limits", &args(&[ID, "--max-attempts", "100"])).is_err());
}
