use super::*;

const ID: &str = "00000000-0000-4000-8000-000000000001";

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

#[test]
fn priority_commands_use_closed_shared_routes_and_exact_revisions() {
    let (command, params) = parse("priority", &args(&[ID])).unwrap();
    assert_eq!(command, Command::ActivitySchedulingPolicyGet);
    assert_eq!(params, json!({"id":ID}));

    let (command, params) = parse(
        "set-priority",
        &args(&[ID, "--priority", "foreground"]),
    )
    .unwrap();
    assert_eq!(command, Command::ActivitySchedulingPolicySet);
    assert_eq!(params, json!({"id":ID,"priority":"foreground"}));

    let (_, params) = parse(
        "set-priority",
        &args(&[
            ID,
            "--priority",
            "background",
            "--expected-revision",
            "3",
        ]),
    )
    .unwrap();
    assert_eq!(
        params,
        json!({"id":ID,"priority":"background","expected_revision":3})
    );
    assert!(params.get("owner_uid").is_none());
    assert!(params.get("preempt").is_none());
}

#[test]
fn priority_flags_reject_authority_fields_and_open_ended_values() {
    for values in [
        args(&[]),
        args(&["../foreign"]),
        args(&[ID]),
        args(&[ID, "--priority"]),
        args(&[ID, "--priority", "urgent"]),
        args(&[ID, "--priority", "foreground", "--priority", "background"]),
        args(&[ID, "--priority", "foreground", "--expected-revision", "0"]),
        args(&[ID, "--priority", "foreground", "--owner", "0"]),
        args(&[ID, "--priority", "foreground", "--preempt", "true"]),
    ] {
        assert!(parse("set-priority", &values).is_err(), "{values:?}");
    }
    assert!(parse("priority", &args(&[ID, "--priority", "standard"])).is_err());
    assert!(super::super::parse("priority", &args(&[ID])).is_ok());
}
