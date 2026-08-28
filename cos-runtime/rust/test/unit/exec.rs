use super::*;

#[test]
fn start_stdin_is_bounded_before_invoking_cos() {
    assert!(matches!(
        start_transient_with_stdin(&["program"], b""),
        Err(StartError::EmptyInput)
    ));
    let input = vec![0_u8; MAX_START_STDIN_BYTES + 1];
    assert!(matches!(
        start_transient_with_stdin(&["program"], &input),
        Err(StartError::InputTooLarge {
            actual,
            limit: MAX_START_STDIN_BYTES
        }) if actual == MAX_START_STDIN_BYTES + 1
    ));
}

#[test]
fn start_arguments_always_protect_child_flags_with_delimiter() {
    assert_eq!(
        start_arguments(&["program", "--stdin", "--child-flag"], false),
        ["--", "program", "--stdin", "--child-flag"]
    );
    assert_eq!(
        start_arguments(&["program", "--child-flag"], true),
        ["--stdin", "--", "program", "--child-flag"]
    );
}

#[test]
fn stop_rejects_dangerous_pids_before_invoking_cos() {
    assert!(matches!(stop_pid(0), Err(StopError::InvalidPid { .. })));
    assert!(matches!(
        stop_pid(i32::MAX as u32 + 1),
        Err(StopError::InvalidPid { .. })
    ));
    assert!(matches!(stop("  "), Err(StopError::EmptyLaunchId)));
}

#[test]
#[cfg(unix)]
fn registered_start_decodes_identity_and_preserves_child_flags() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
    let fake_cos = directory.path().join("fake-cos");
    let capture = directory.path().join("argv");
    std::fs::write(
        &fake_cos,
        concat!(
            "#!/bin/sh\n",
            "printf '%s\\n' \"$@\" > \"$EXEC_TEST_CAPTURE\"\n",
            "printf '%s\\n' '{\"launch_id\":\"launch-1\",\"pid\":42,",
            "\"start_time_ticks\":123,\"command\":[\"program\",\"--stdin\"]}'\n",
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake_cos, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::env::set_var("CLAW_COS_BIN", &fake_cos);
    std::env::set_var("EXEC_TEST_CAPTURE", &capture);

    let handle = start(&["program", "--stdin"]).unwrap();

    std::env::remove_var("CLAW_COS_BIN");
    std::env::remove_var("EXEC_TEST_CAPTURE");
    assert_eq!(handle.launch_id, "launch-1");
    assert_eq!(handle.pid, 42);
    assert_eq!(handle.start_time_ticks, 123);
    assert_eq!(handle.command, ["program", "--stdin"]);
    assert_eq!(
        std::fs::read_to_string(capture)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        ["app", "exec", "start", "--", "program", "--stdin"]
    );
}
