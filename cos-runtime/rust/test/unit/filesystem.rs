use super::*;

#[test]
fn filesystem_requests_are_typed_and_never_app_calls() {
    let input = Request::Write {
        path: "/work/a",
        content: "é\0\n",
    };
    assert_eq!(
        serde_json::to_value(input).unwrap(),
        serde_json::json!({"action":"write","path":"/work/a","content":"é\0\n"})
    );
    assert!(absolute("").is_err());
    assert!(absolute("a\0b").is_err());
    assert!(serde_json::from_value::<ReadResult>(
        serde_json::json!({"path":"/x","content":"partial","truncated":true})
    )
    .is_err());
}

#[test]
#[cfg(unix)]
fn filesystem_bridge_sends_stdin_and_validates_confirmation() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let script = dir.path().join("cos");
    std::fs::write(&script, "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$0.args\"\ncat > \"$0.input\"\nprintf '%s\\n' '{\"ok\":true,\"wire_version\":1,\"data\":{\"path\":\"/work/a\",\"bytes\":4}}'\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let old = std::env::var_os("CLAW_COS_BIN");
    std::env::set_var("CLAW_COS_BIN", &script);
    let result = write("/work/a", "é\0\n");
    match old {
        Some(old) => std::env::set_var("CLAW_COS_BIN", old),
        None => std::env::remove_var("CLAW_COS_BIN"),
    }
    assert_eq!(result.unwrap().bytes, 4);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("cos.args")).unwrap(),
        "--wire=1\n__filesystem\nwrite\n--request-stdin\n"
    );
    let request: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("cos.input")).unwrap()).unwrap();
    assert_eq!(request["content"], "é\0\n");
}
