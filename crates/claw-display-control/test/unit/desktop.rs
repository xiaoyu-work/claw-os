use super::*;
use std::os::fd::AsFd;

#[test]
fn desktop_commands_reject_unbound_ids_and_nonliteral_app_targets() {
    assert!(Command::Focus { identifier: 0 }.validate().is_err());
    assert!(Command::CloseApp {
        identifier: 1,
        app_id: "../unbound".into(),
    }
    .validate()
    .is_err());
    assert!(serde_json::from_str::<Command>(
        r#"{"action":"focus","identifier":1,"owner_uid":0}"#
    )
    .is_err());
}

#[test]
fn desktop_response_requires_seals_exact_owner_and_ignores_shared_file_offsets() {
    let bytes = br#"{"windows":[]}"#;
    let descriptor = seal_response(bytes).unwrap();
    assert_eq!(
        unsafe { libc::lseek(descriptor.as_raw_fd(), 0, libc::SEEK_END) },
        bytes.len() as i64,
    );
    let duplicate = descriptor.as_fd().try_clone_to_owned().unwrap();
    assert!(receive_response(duplicate, unsafe { libc::geteuid() } ^ 1).is_err());
    assert_eq!(
        receive_response(descriptor, unsafe { libc::geteuid() }).unwrap(),
        serde_json::json!({"windows":[]}),
    );
    assert!(seal_response(b"[]").is_err());
    let unsealed = tempfile_descriptor();
    assert!(receive_response(unsealed, unsafe { libc::geteuid() }).is_err());
}

fn tempfile_descriptor() -> OwnedFd {
    let raw = unsafe {
        libc::memfd_create(c"unsealed-desktop-test".as_ptr(), libc::MFD_CLOEXEC)
    };
    assert!(raw >= 0);
    let mut file = File::from(unsafe { OwnedFd::from_raw_fd(raw) });
    file.write_all(br#"{"windows":[]}"#).unwrap();
    file.into()
}
