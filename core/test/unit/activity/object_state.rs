use super::*;

const ACTIVITY: &str = "00000000-0000-4000-8000-000000000001";
const ENTRY: &str = "00000000-0000-4000-8000-000000000002";
const REFERENCE: &str = "app://kv/entry?id=release.status";

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

#[test]
fn object_state_cli_uses_the_bounded_shared_draft() {
    let (route, params) = parse(
        "observe",
        &args(&[
            ACTIVITY,
            "--reference",
            REFERENCE,
            "--text",
            "Ready for review",
            "--id",
            ENTRY,
        ]),
    )
    .unwrap();
    assert_eq!(route, Command::ActivityObjectStateRecord);
    assert_eq!(params["id"], ACTIVITY);
    assert_eq!(params["entry"]["id"], ENTRY);
    assert_eq!(params["entry"]["content"]["kind"], "user_statement");
    assert!(params["entry"].get("owner_uid").is_none());
    assert!(params["entry"].get("source").is_none());
    let (_, inferred) = parse(
        "observe",
        &args(&[
            ACTIVITY,
            "--reference",
            REFERENCE,
            "--source",
            "agent_inference",
            "--text",
            "A review might be needed",
            "--supersedes",
            ENTRY,
        ]),
    )
    .unwrap();
    assert_eq!(inferred["entry"]["content"]["kind"], "agent_inference");
    assert_eq!(inferred["entry"]["supersedes"], ENTRY);
    uuid::Uuid::parse_str(inferred["entry"]["id"].as_str().unwrap()).unwrap();
}

#[test]
fn app_reports_relationships_and_retractions_have_distinct_content() {
    let (_, report) = parse(
        "observe",
        &args(&[ACTIVITY, "--reference", REFERENCE, "--receipt", ENTRY]),
    )
    .unwrap();
    assert_eq!(
        report["entry"]["content"],
        json!({"kind":"app_report","receipt_id":ENTRY})
    );
    let (_, relation) = parse(
        "relate",
        &args(&[
            ACTIVITY,
            "--reference",
            REFERENCE,
            "--target",
            "app://kv/entry?id=review",
            "--relation",
            "depends_on",
            "--note",
            "Planning only",
        ]),
    )
    .unwrap();
    assert_eq!(relation["entry"]["content"]["kind"], "relation");
    assert_eq!(relation["entry"]["content"]["relation"], "depends_on");
    let (_, retracted) = parse(
        "retract-object-state",
        &args(&[
            ACTIVITY,
            "--reference",
            REFERENCE,
            "--supersedes",
            ENTRY,
            "--reason",
            "Incorrect",
        ]),
    )
    .unwrap();
    assert_eq!(retracted["entry"]["content"]["kind"], "retracted");
    assert_eq!(retracted["entry"]["supersedes"], ENTRY);
}

#[test]
fn invalid_metadata_fails_before_any_broker_connection() {
    for values in [
        args(&[ACTIVITY, "--reference", REFERENCE]),
        args(&[ACTIVITY, "--reference", REFERENCE, "--text", " "]),
        args(&[ACTIVITY, "--reference", "file:///tmp/a", "--text", "note"]),
        args(&[
            ACTIVITY,
            "--reference",
            REFERENCE,
            "--text",
            "note",
            "--owner",
            "0",
        ]),
        args(&[
            ACTIVITY,
            "--reference",
            REFERENCE,
            "--text",
            "note",
            "--source",
            "os_verified",
        ]),
        args(&[
            ACTIVITY,
            "--reference",
            REFERENCE,
            "--text",
            "note",
            "--receipt",
            ENTRY,
        ]),
        args(&[
            ACTIVITY,
            "--reference",
            REFERENCE,
            "--text",
            "note",
            "--observed-at",
            "2026-09-11T00:00:00Z",
        ]),
        args(&[
            ACTIVITY,
            "--reference",
            REFERENCE,
            "--text",
            "one",
            "--text",
            "two",
        ]),
    ] {
        assert!(parse("observe", &values).is_err(), "{values:?}");
    }
    assert!(parse(
        "retract-object-state",
        &args(&[ACTIVITY, "--reference", REFERENCE, "--reason", "wrong",])
    )
    .is_err());
    assert!(read("x".repeat(16 * 1024 + 1).as_bytes()).is_err());
    assert!(read(br#"{"owner_uid":0,"source":"verified"}"#.as_slice()).is_err());
}
