use super::*;

fn with_tmp_config_dir<R>(label: &str, f: impl FnOnce() -> R) -> R {
    let tmp = std::env::temp_dir().join(format!(
        "cos-user-budget-test-{label}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&tmp);
    let prev = std::env::var_os("COS_USER_CONFIG_DIR");
    std::env::set_var("COS_USER_CONFIG_DIR", &tmp);
    let out = f();
    match prev {
        Some(v) => std::env::set_var("COS_USER_CONFIG_DIR", v),
        None => std::env::remove_var("COS_USER_CONFIG_DIR"),
    }
    let _ = fs::remove_dir_all(&tmp);
    out
}

#[test]
fn missing_file_is_unlimited() {
    with_tmp_config_dir("missing", || {
        let b = load().unwrap();
        assert_eq!(b.monthly_units, 0);
        assert!(b.is_unlimited());
    });
}

#[test]
fn empty_file_is_unlimited() {
    with_tmp_config_dir("empty", || {
        let path = config_path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "").unwrap();
        let b = load().unwrap();
        assert!(b.is_unlimited());
    });
}

#[test]
fn explicit_zero_is_unlimited() {
    with_tmp_config_dir("zero", || {
        let path = config_path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, r#"{"monthly_units": 0}"#).unwrap();
        let b = load().unwrap();
        assert!(b.is_unlimited());
    });
}

#[test]
fn nonzero_cap_loads() {
    with_tmp_config_dir("cap", || {
        let path = config_path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, r#"{"monthly_units": 1234567}"#).unwrap();
        let b = load().unwrap();
        assert_eq!(b.monthly_units, 1234567);
        assert!(!b.is_unlimited());
    });
}

#[test]
fn unknown_field_is_ignored() {
    with_tmp_config_dir("extra", || {
        let path = config_path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{"monthly_units": 42, "future_usd_axis": 99.99}"#,
        )
        .unwrap();
        let b = load().unwrap();
        assert_eq!(b.monthly_units, 42);
    });
}

#[test]
fn malformed_file_errors() {
    with_tmp_config_dir("bad", || {
        let path = config_path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{not json").unwrap();
        assert!(load().is_err());
    });
}

#[test]
fn user_budget_bucket_is_reserved_id() {
    // Sentinel must contain a character (underscore) that App ids
    // cannot. Apps validate to alphanumeric-plus-hyphen.
    assert!(USER_BUDGET_BUCKET.starts_with("__"));
    assert!(USER_BUDGET_BUCKET.ends_with("__"));
}
