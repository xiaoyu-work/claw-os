use super::*;

#[test]
fn override_cmd_help_shape() {
    let err = override_cmd(&[]).unwrap_err();
    assert!(err.contains("show"));
    assert!(err.contains("path"));
    assert!(err.contains("effective"));
}

#[test]
fn override_cmd_path_returns_user_config_path() {
    let v = override_cmd(&["path".to_string(), "demo-app".to_string()]).expect("path ok");
    let p = v.get("path").and_then(|x| x.as_str()).expect("path field");
    assert!(p.contains("apps"));
    assert!(p.ends_with("demo-app.json"));
}

#[test]
fn override_cmd_show_missing_file_reports_absent() {
    // Mutates process-wide env; serialize with the crate-wide
    // env lock so we don't race with other env-touching tests.
    let _env_lock = crate::test_env::lock_env();
    // Point user-config at an empty tmp dir so the file definitely doesn't exist.
    let tmp = std::env::temp_dir().join(format!("cos-override-cmd-missing-{}", std::process::id()));
    let prev = std::env::var_os("COS_USER_CONFIG_DIR");
    std::env::set_var("COS_USER_CONFIG_DIR", &tmp);
    let v = override_cmd(&["show".to_string(), "never-installed".to_string()]).expect("show ok");
    match prev {
        Some(p) => std::env::set_var("COS_USER_CONFIG_DIR", p),
        None => std::env::remove_var("COS_USER_CONFIG_DIR"),
    }
    assert_eq!(v.get("present").and_then(|x| x.as_bool()), Some(false));
    assert!(v.get("override").is_some_and(|x| x.is_null()));
}

#[test]
fn budget_user_path_returns_ai_budget_path() {
    let v = budget_cmd(&["user".to_string(), "path".to_string()]).expect("path ok");
    let p = v.get("path").and_then(|x| x.as_str()).expect("path field");
    assert!(p.contains("ai"));
    assert!(p.ends_with("budget.json"));
    assert_eq!(v.get("scope").and_then(|x| x.as_str()), Some("user"));
}

#[test]
fn budget_user_show_missing_file_reports_unlimited() {
    // Mutates process-wide env; serialize with the crate-wide
    // env lock so we don't race with other env-touching tests.
    let _env_lock = crate::test_env::lock_env();
    // Empty tmp dirs ⇒ no budget.json (unlimited) and a writable
    // data dir for the SQLite store (the default /var/lib/cos is
    // not writable on dev hosts).
    let tmp = std::env::temp_dir().join(format!("cos-budget-user-show-{}", std::process::id()));
    let cfg_dir = tmp.join("config");
    let data_dir = tmp.join("data");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&data_dir).unwrap();
    let prev_cfg = std::env::var_os("COS_USER_CONFIG_DIR");
    let prev_data = std::env::var_os("COS_DATA_DIR");
    std::env::set_var("COS_USER_CONFIG_DIR", &cfg_dir);
    std::env::set_var("COS_DATA_DIR", &data_dir);
    let v = budget_cmd(&["user".to_string(), "show".to_string()]).expect("show ok");
    match prev_cfg {
        Some(p) => std::env::set_var("COS_USER_CONFIG_DIR", p),
        None => std::env::remove_var("COS_USER_CONFIG_DIR"),
    }
    match prev_data {
        Some(p) => std::env::set_var("COS_DATA_DIR", p),
        None => std::env::remove_var("COS_DATA_DIR"),
    }
    let _ = std::fs::remove_dir_all(&tmp);
    assert_eq!(v.get("scope").and_then(|x| x.as_str()), Some("user"));
    assert_eq!(v.get("unlimited").and_then(|x| x.as_bool()), Some(true));
    assert_eq!(v.get("units_cap").and_then(|x| x.as_u64()), Some(0));
    assert!(v.get("units_available").is_some_and(|x| x.is_null()));
}
