use std::os::unix::fs::PermissionsExt;

use super::*;

fn temp_root() -> tempfile::TempDir {
    tempfile::tempdir().expect("tempdir")
}

#[test]
fn first_load_mints_a_private_key() {
    let tmp = temp_root();
    let ring = load_or_create(tmp.path()).expect("keyring");
    assert_eq!(ring.len(), 1);
    assert!(!ring.active_key().is_empty());

    let path = key_path(tmp.path(), ring.active_id());
    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600, "key material must be owner-only");
    let dir_mode = std::fs::metadata(keys_dir(tmp.path()))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(
        dir_mode & 0o077,
        0,
        "the key directory must not be reachable"
    );
}

#[test]
fn a_second_load_reuses_the_same_key() {
    let tmp = temp_root();
    let first = load_or_create(tmp.path()).expect("keyring");
    let second = load_or_create(tmp.path()).expect("keyring");
    assert_eq!(first.active_id(), second.active_id());
    assert_eq!(first.active_key(), second.active_key());
}

#[test]
fn rotation_keeps_old_keys_verifiable() {
    let tmp = temp_root();
    let before = load_or_create(tmp.path()).expect("keyring");
    let old_id = before.active_id().to_string();
    let old_key = before.active_key().to_vec();

    let new_id = rotate(tmp.path()).expect("rotate");
    let after = load_or_create(tmp.path()).expect("keyring");

    assert_eq!(after.active_id(), new_id);
    assert_ne!(after.active_id(), old_id);
    assert_eq!(
        after.verify_key(&old_id),
        Some(old_key.as_slice()),
        "a rotated chain must still verify under the key that signed it"
    );
}

#[test]
fn a_key_another_account_could_read_is_refused() {
    let tmp = temp_root();
    let ring = load_or_create(tmp.path()).expect("keyring");
    let path = key_path(tmp.path(), ring.active_id());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

    let error = load_or_create(tmp.path()).expect_err("group-readable key must fail closed");
    assert!(
        error.to_string().contains("not private"),
        "unexpected error: {error}"
    );
}

#[test]
fn a_symlinked_key_is_refused() {
    let tmp = temp_root();
    let ring = load_or_create(tmp.path()).expect("keyring");
    let path = key_path(tmp.path(), ring.active_id());
    let elsewhere = tmp.path().join("planted.key");
    std::fs::write(&elsewhere, "00".repeat(32)).unwrap();
    std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &path).unwrap();

    let error = load_or_create(tmp.path()).expect_err("a symlinked key must fail closed");
    assert!(
        error.to_string().contains("not a regular file"),
        "unexpected error: {error}"
    );
}

#[test]
fn a_hardlinked_key_is_refused() {
    let tmp = temp_root();
    let ring = load_or_create(tmp.path()).expect("keyring");
    let path = key_path(tmp.path(), ring.active_id());
    let shared = tmp.path().join("shared.key");
    std::fs::hard_link(&path, &shared).unwrap();

    let error = load_or_create(tmp.path()).expect_err("a shared inode must fail closed");
    assert!(
        error.to_string().contains("links"),
        "unexpected error: {error}"
    );
}

#[test]
fn a_world_reachable_key_directory_is_refused() {
    let tmp = temp_root();
    load_or_create(tmp.path()).expect("keyring");
    std::fs::set_permissions(keys_dir(tmp.path()), std::fs::Permissions::from_mode(0o755)).unwrap();

    let error = load_or_create(tmp.path()).expect_err("a reachable key dir must fail closed");
    assert!(
        error.to_string().contains("reachable by other accounts"),
        "unexpected error: {error}"
    );
}

#[test]
fn an_active_file_naming_a_missing_key_fails_closed() {
    let tmp = temp_root();
    let ring = load_or_create(tmp.path()).expect("keyring");
    std::fs::remove_file(key_path(tmp.path(), ring.active_id())).unwrap();

    let error = load_or_create(tmp.path()).expect_err("a missing signing key must fail closed");
    assert!(
        error.to_string().contains("missing from the keyring"),
        "unexpected error: {error}"
    );
}

#[test]
fn a_planted_key_file_is_never_adopted() {
    // `create_new` is the point: minting must fail rather than pick up
    // a file somebody else placed first.
    let tmp = temp_root();
    crate::storage::ensure_private_dir(&keys_dir(tmp.path())).unwrap();
    let planted = keys_dir(tmp.path()).join("00112233445566778.key");
    std::fs::write(&planted, "zz").unwrap();

    let error = load_or_create(tmp.path()).expect_err("an unusable file name must fail closed");
    assert!(
        error.to_string().contains("unusable file name"),
        "unexpected error: {error}"
    );
}
