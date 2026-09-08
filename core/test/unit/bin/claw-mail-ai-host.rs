use super::*;
use std::ffi::OsString;

#[test]
fn native_identity_is_consumed_not_forwarded_to_python() {
    let args = python_arguments([OsString::from("claw-mail-ai@claw.os")].into_iter()).unwrap();
    assert_eq!(
        args,
        [
            OsString::from("-I"),
            OsString::from("/usr/lib/cos/apps/mail-ai/native_host.py")
        ]
    );
}

#[test]
fn native_launcher_rejects_missing_other_and_extra_arguments() {
    for args in [
        vec![],
        vec![OsString::from("other@example.invalid")],
        vec![OsString::from("claw-mail-ai@claw.os"), OsString::from("--probe")],
    ] {
        assert!(python_arguments(args.into_iter()).is_err());
    }
}
