use super::*;

struct DataDirGuard {
    previous: Option<std::ffi::OsString>,
    _temp: tempfile::TempDir,
}

impl Drop for DataDirGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => std::env::set_var("COS_DATA_DIR", value),
            None => std::env::remove_var("COS_DATA_DIR"),
        }
    }
}

fn isolated_data_dir() -> DataDirGuard {
    let temp = tempfile::tempdir().unwrap();
    let previous = std::env::var_os("COS_DATA_DIR");
    std::env::set_var("COS_DATA_DIR", temp.path());
    DataDirGuard {
        previous,
        _temp: temp,
    }
}

#[test]
fn signed_access_token_round_trips_and_binds_uid() {
    let _lock = crate::test_env::lock_env();
    let _data = isolated_data_dir();
    let bootstrap = load_or_generate_token().unwrap();
    ensure_signing_key().unwrap();
    let issued = exchange_bootstrap_token(&bootstrap, 1001, Some(300)).unwrap();
    let verified = verify_access_token(&issued.access_token, 1001).unwrap();
    assert_eq!(verified.uid, 1001);
    assert!(verify_access_token(&issued.access_token, 1002).is_err());
    assert_eq!(issued.expires_in, 300);
}

#[test]
fn rotating_keys_invalidates_existing_access_tokens() {
    let _lock = crate::test_env::lock_env();
    let _data = isolated_data_dir();
    let bootstrap = load_or_generate_token().unwrap();
    ensure_signing_key().unwrap();
    let issued = exchange_bootstrap_token(&bootstrap, 1001, None).unwrap();
    let new_bootstrap = rotate_tokens().unwrap();
    assert_ne!(bootstrap, new_bootstrap);
    assert!(verify_access_token(&issued.access_token, 1001).is_err());
    assert!(exchange_bootstrap_token(&bootstrap, 1001, None).is_err());
    assert!(exchange_bootstrap_token(&new_bootstrap, 1001, None).is_ok());
}

#[test]
fn bootstrap_secret_must_be_full_strength_hex() {
    let _lock = crate::test_env::lock_env();
    let _data = isolated_data_dir();
    assert!(persist_token("abcd").is_err());
    assert!(persist_token(&"g".repeat(64)).is_err());
}
