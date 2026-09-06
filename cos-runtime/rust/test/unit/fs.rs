use super::*;

#[test]
#[cfg(unix)]
fn fs_json_writes_roundtrip_large_text_binary_and_nul_and_reject_overflow() {
    use std::os::unix::fs::PermissionsExt;

    struct RestoreBin(Option<std::ffi::OsString>);
    impl Drop for RestoreBin {
        fn drop(&mut self) {
            match &self.0 {
                Some(value) => std::env::set_var("CLAW_COS_BIN", value),
                None => std::env::remove_var("CLAW_COS_BIN"),
            }
        }
    }

    let dir = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let script = dir.path().join("cos");
    std::fs::write(
        &script,
        "#!/bin/sh\nprintf '%s\\0' \"$@\" > \"$0.args\"\ncat > \"$0.stdin\"\n\
         printf '%s\\n' '{\"ok\":true,\"wire_version\":1,\"data\":{\"path\":\"out\",\"bytes\":0}}'\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let _restore = RestoreBin(std::env::var_os("CLAW_COS_BIN"));
    std::env::set_var("CLAW_COS_BIN", &script);

    let assert_request = |verb: &str, content: &str| {
        let captured = std::fs::read(dir.path().join("cos.args")).unwrap();
        let args: Vec<&[u8]> = captured
            .split(|byte| *byte == 0)
            .filter(|value| !value.is_empty())
            .collect();
        let expected = ["--wire=1", "app", "fs", verb, "--args-stdin"];
        assert_eq!(args, expected.map(str::as_bytes));
        let arguments: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.path().join("cos.stdin")).unwrap()).unwrap();
        assert_eq!(
            arguments,
            serde_json::json!({"path": "--output", "content": content})
        );
    };

    for content in [
        "",
        "--literal flag\nsecond line",
        "Unicode: é",
        "before\0after",
    ] {
        write("--output", content).unwrap();
        assert_request("write", content);
    }
    write_bytes("--output", &[0, 255, 10]).unwrap();
    assert_request("write_bytes", "AP8K");
    write_bytes("--output", &[]).unwrap();
    assert_request("write_bytes", "");

    let large_text = "large é\0\n".repeat(32 * 1024);
    assert!(large_text.len() > 128 * 1024);
    write("--output", &large_text).unwrap();
    assert_request("write", &large_text);
    let binary = vec![255; 256 * 1024];
    write_bytes("--output", &binary).unwrap();
    let arguments: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("cos.stdin")).unwrap()).unwrap();
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    assert_request("write_bytes", &STANDARD.encode(&binary));
    assert_eq!(
        STANDARD
            .decode(arguments["content"].as_str().unwrap())
            .unwrap(),
        binary
    );

    let overhead = serde_json::to_vec(&serde_json::json!({
        "path": "--output", "content": ""
    }))
    .unwrap()
    .len();
    let near_limit = "x".repeat(claw_os_sdk::APP_ARGS_STDIN_MAX_BYTES - overhead);
    write("--output", &near_limit).unwrap();
    assert_request("write", &near_limit);
    std::fs::remove_file(dir.path().join("cos.args")).unwrap();
    assert!(matches!(
        write("--output", &(near_limit + "x")),
        Err(BridgeError::Io(_))
    ));
    assert!(
        !dir.path().join("cos.args").exists(),
        "oversize request spawned cos"
    );
    assert!(matches!(
        write_bytes("--output", &vec![0; claw_os_sdk::APP_ARGS_STDIN_MAX_BYTES]),
        Err(BridgeError::Io(_))
    ));
    assert!(!dir.path().join("cos.args").exists());

    for (truncation_field, succeeds) in [
        ("", true),
        (",\"truncated\":false", true),
        (",\"truncated\":true", false),
    ] {
        std::fs::write(&script, format!(
            "#!/bin/sh\nprintf '%s\\n' '{{\"ok\":true,\"wire_version\":1,\"data\":{{\"base64\":\"AP8K\"{truncation_field}}}}}'\n"
        )).unwrap();
        let result = read_bytes("file");
        if succeeds {
            assert_eq!(result.unwrap(), [0, 255, 10]);
        } else {
            assert!(
                matches!(result, Err(BridgeError::Decode { message, .. }) if message.contains("truncated"))
            );
        }
    }
}
