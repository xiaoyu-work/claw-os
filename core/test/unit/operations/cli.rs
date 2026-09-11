use super::*;

#[test]
fn preview_cli_keeps_app_arguments_as_data_and_uses_the_shared_broker() {
    let args = ["kv", "get", "--", "--", "--schema"].map(str::to_string);
    let (route, value) = parse("preview", &args).unwrap();
    assert_eq!(route, Command::OperationPreview);
    assert_eq!(value["args"], json!(["--", "--schema"]));
    assert!(value.get("owner_uid").is_none());
    let args = [
        "kv",
        "get",
        "--activity",
        "00000000-0000-4000-8000-000000000001",
        "--",
        "key",
    ]
    .map(str::to_string);
    let (route, value) = parse("preview", &args).unwrap();
    assert_eq!(route, Command::ActivityOperationPreview);
    assert_eq!(value["args"], json!(["key"]));
    assert!(parse("preview", &["kv".into(), "get".into(), "key".into()]).is_err());
    assert!(parse("execute", &args).is_err());
}
