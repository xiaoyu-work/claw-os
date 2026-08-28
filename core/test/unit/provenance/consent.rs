use super::*;

#[test]
fn the_phrase_names_the_exact_package() {
    let phrase = confirmation_phrase(PackageKind::App, "notes");
    assert_eq!(phrase, "trust unsigned app notes");
    // Not something a stray `y` or a `yes |` pipeline produces.
    assert!(phrase.len() > 8);
    assert_ne!(phrase, "y");
    assert_ne!(phrase, "yes");
    assert_ne!(
        confirmation_phrase(PackageKind::Skill, "notes"),
        confirmation_phrase(PackageKind::App, "notes")
    );
    assert_ne!(
        confirmation_phrase(PackageKind::App, "other"),
        confirmation_phrase(PackageKind::App, "notes")
    );
}

#[cfg(unix)]
#[test]
fn a_non_interactive_process_is_refused() {
    // `cargo test` captures stdio, so this process has no controlling
    // terminal — exactly the shape of a CI runner, an agent subprocess
    // or a model-issued tool call.
    let dir = std::env::temp_dir();
    let err = require_developer_consent(
        PackageKind::App,
        "notes",
        &dir,
        "sha256:00",
        &dir,
        false,
    )
    .unwrap_err();
    assert!(
        matches!(err, ConsentError::NotInteractive { .. }),
        "expected a non-interactive refusal, got {err}"
    );
    assert!(format!("{err}").contains("offline signed developer grant"));
}

#[cfg(unix)]
#[test]
fn auto_yes_does_not_satisfy_consent() {
    let dir = std::env::temp_dir();
    // `--yes` must not change the outcome in any direction.
    let with_yes =
        require_developer_consent(PackageKind::App, "notes", &dir, "sha256:00", &dir, true);
    let without =
        require_developer_consent(PackageKind::App, "notes", &dir, "sha256:00", &dir, false);
    assert!(with_yes.is_err());
    assert!(without.is_err());
    assert_eq!(
        std::mem::discriminant(&with_yes.unwrap_err()),
        std::mem::discriminant(&without.unwrap_err()),
        "--yes must not take a different path"
    );
}

#[cfg(unix)]
#[test]
fn an_active_session_blocks_the_decision() {
    let _guard = crate::test_env::lock_env();
    let _session = crate::test_env::TestEnvVarGuard::set("COS_SESSION", "sess-active");
    // The session check runs after the TTY check, so on a captured
    // stdio test process we assert the guard function itself.
    assert!(active_session().is_some());
    let what = active_session().unwrap();
    assert!(what.contains("COS_SESSION"), "{what}");
}
