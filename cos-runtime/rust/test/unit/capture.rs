use super::*;
use std::os::unix::fs::PermissionsExt;

#[test]
fn capture_uses_typed_sdk_stdin_and_validates_before_contacting_os() {
    let dir = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let script = dir.path().join("cos");
    std::fs::write(
        &script,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$0.args\"\ncat > \"$0.input\"\ncat \"$0.reply\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let old = std::env::var_os("CLAW_COS_BIN");
    std::env::set_var("CLAW_COS_BIN", &script);
    for path in ["", "relative", "/work/\0bad"] {
        assert!(screenshot(path, true).is_err());
    }
    assert!(!dir.path().join("cos.args").exists());
    let cases = [
        (
            serde_json::json!({"cancelled":false,"path":"/work/shots/shot.png"}),
            true,
        ),
        (serde_json::json!({"cancelled":true,"path":null}), true),
        (
            serde_json::json!({"cancelled":true,"path":"/work/shots/shot.png"}),
            false,
        ),
        (
            serde_json::json!({"cancelled":false,"path":"/other/shot.png"}),
            false,
        ),
        (
            serde_json::json!({"cancelled":false,"path":"/work/shots/shot.txt"}),
            false,
        ),
        (serde_json::json!({"cancelled":false,"path":null}), false),
    ];
    for (data, ok) in cases {
        std::fs::write(
            dir.path().join("cos.reply"),
            serde_json::json!({"ok":true,"wire_version":1,"data":data}).to_string(),
        )
        .unwrap();
        assert_eq!(screenshot("/work/shots", false).is_ok(), ok);
    }
    std::fs::write(
        dir.path().join("cos.reply"),
        r#"{"ok":false,"wire_version":1,"error":{"code":"denied","message":"fixture denied"}}"#,
    )
    .unwrap();
    assert!(screenshot("/work/shots", false).is_err());
    match old {
        Some(old) => std::env::set_var("CLAW_COS_BIN", old),
        None => std::env::remove_var("CLAW_COS_BIN"),
    }
    assert_eq!(
        std::fs::read_to_string(dir.path().join("cos.args")).unwrap(),
        "--wire=1\n__capture\nscreenshot\n--request-stdin\n"
    );
    let request: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("cos.input")).unwrap()).unwrap();
    assert_eq!(
        request,
        serde_json::json!({"directory":"/work/shots","modal":false})
    );
}
