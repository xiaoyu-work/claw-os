use super::*;

#[test]
fn data_dir_respects_env_override() {
    // Use a non-conflicting key path; this is best-effort and skipped
    // if the env is already set (parallel-test safety).
    if env::var_os("COS_DATA_DIR").is_some() {
        return;
    }
    // SAFETY: tests in this module are single-threaded by name uniqueness.
    unsafe {
        env::set_var("COS_DATA_DIR", "/tmp/cos-test-data");
    }
    assert_eq!(data_dir(), PathBuf::from("/tmp/cos-test-data"));
    unsafe {
        env::remove_var("COS_DATA_DIR");
    }
}

#[test]
fn models_dir_lives_under_data_dir() {
    assert!(models_dir().starts_with(data_dir()));
}

#[test]
fn agent_state_dir_lives_under_data_dir() {
    assert!(agent_state_dir().starts_with(data_dir()));
}

// ----- HOME_OVERRIDE task_local --------------------------------------

#[test]
fn no_override_falls_back_to_home_env() {
    assert!(current_home_override().is_none());
}

#[cfg(not(windows))]
#[test]
fn home_override_redirects_user_config_dir() {
    // Snapshot any existing COS_USER_CONFIG_DIR — we must clear it for
    // the override path to be observable.
    let prev_cfg = env::var_os("COS_USER_CONFIG_DIR");
    // SAFETY: single-threaded by test name uniqueness.
    unsafe {
        env::remove_var("COS_USER_CONFIG_DIR");
    }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build current-thread runtime");

    let got = rt.block_on(async {
        with_home_override(PathBuf::from("/tmp/cos-test-home"), async {
            user_config_dir()
        })
        .await
    });
    assert_eq!(got, PathBuf::from("/tmp/cos-test-home/.config/cos"));

    // SAFETY: see above.
    unsafe {
        if let Some(v) = prev_cfg {
            env::set_var("COS_USER_CONFIG_DIR", v);
        }
    }
}

#[cfg(not(windows))]
#[test]
fn home_override_redirects_user_data_dir_and_credentials() {
    let prev_data = env::var_os("COS_USER_DATA_DIR");
    let prev_creds = env::var_os("COS_CREDENTIALS_DIR");
    // SAFETY: single-threaded by test name uniqueness.
    unsafe {
        env::remove_var("COS_USER_DATA_DIR");
        env::remove_var("COS_CREDENTIALS_DIR");
    }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build current-thread runtime");

    let (data, creds, snapshot) = rt.block_on(async {
        with_home_override(PathBuf::from("/tmp/cos-test-creds-home"), async {
            (
                user_data_dir(),
                user_credentials_dir(),
                current_home_override(),
            )
        })
        .await
    });
    assert_eq!(
        data,
        PathBuf::from("/tmp/cos-test-creds-home/.local/share/cos")
    );
    assert_eq!(
        creds,
        PathBuf::from("/tmp/cos-test-creds-home/.local/share/cos/credentials")
    );
    assert_eq!(snapshot.as_deref(), Some(Path::new("/tmp/cos-test-creds-home")));

    // SAFETY: see above.
    unsafe {
        if let Some(v) = prev_data {
            env::set_var("COS_USER_DATA_DIR", v);
        }
        if let Some(v) = prev_creds {
            env::set_var("COS_CREDENTIALS_DIR", v);
        }
    }
}

#[cfg(not(windows))]
#[test]
fn env_override_still_wins_over_home_override() {
    // Explicit env-var overrides (used by tests and multi-tenant
    // overlays) must keep winning even when a HOME override is
    // installed — otherwise we'd break existing test isolation.
    let prev = env::var_os("COS_USER_CONFIG_DIR");
    // SAFETY: see above.
    unsafe {
        env::set_var("COS_USER_CONFIG_DIR", "/tmp/cos-test-env-wins");
    }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build current-thread runtime");

    let got = rt.block_on(async {
        with_home_override(PathBuf::from("/tmp/cos-test-home-loses"), async {
            user_config_dir()
        })
        .await
    });
    assert_eq!(got, PathBuf::from("/tmp/cos-test-env-wins"));

    // SAFETY: see above.
    unsafe {
        match prev {
            Some(v) => env::set_var("COS_USER_CONFIG_DIR", v),
            None => env::remove_var("COS_USER_CONFIG_DIR"),
        }
    }
}

#[cfg(not(windows))]
#[test]
fn home_override_scopes_to_task_only() {
    let prev_cfg = env::var_os("COS_USER_CONFIG_DIR");
    // SAFETY: see above.
    unsafe {
        env::remove_var("COS_USER_CONFIG_DIR");
    }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build current-thread runtime");

    // Outside the scope: no override.
    assert!(current_home_override().is_none());

    // Inside: present. After the scope exits: gone again.
    rt.block_on(async {
        with_home_override(PathBuf::from("/tmp/cos-scope"), async {
            assert_eq!(
                current_home_override().as_deref(),
                Some(Path::new("/tmp/cos-scope"))
            );
        })
        .await;
    });
    assert!(current_home_override().is_none());

    // SAFETY: see above.
    unsafe {
        if let Some(v) = prev_cfg {
            env::set_var("COS_USER_CONFIG_DIR", v);
        }
    }
}
