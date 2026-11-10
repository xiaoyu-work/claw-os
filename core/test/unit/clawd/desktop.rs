use super::*;
use crate::clawd::authority::{
    authority, Audience, AudienceSet, Binding, Decision, Issuance, Issuer, Presentation, Principal,
    Requirement, Subject, Uses,
};
use serde_json::json;

const APP_ID: &str = "com.example.App";
const NATIVE_TARGETS: [(NativeLaunchTarget, &str, &str); 5] = [
    (NativeLaunchTarget::Terminal, "com.clawos.Term", "cosmic-term"),
    (NativeLaunchTarget::Store, "com.clawos.Store", "cosmic-store"),
    (NativeLaunchTarget::Settings, "com.clawos.Settings", "cosmic-settings"),
    (NativeLaunchTarget::Files, "com.clawos.Files", "cosmic-files"),
    (NativeLaunchTarget::Editor, "com.clawos.Edit", "cosmic-edit"),
];

#[test]
fn caller_names_and_human_sessions_do_not_grant_desktop_authority() {
    for (index, app) in [
        Some("cosmic-settings"), Some("cosmic-store"), Some("cosmic-term"),
        Some("cosmic-files"), Some("cosmic-edit"), Some("launcher"),
        Some("desktop-manager"), Some("independent-desktop-client"), None,
    ].into_iter().enumerate() {
        let decision = decision_for_app(app, vec![], &format!("desktop-no-caps-{index}"));
        for (_, app_id, _) in NATIVE_TARGETS {
            assert_eq!(native_launch_target(&decision, "launch", Some(app_id)), None);
            assert!(authorize_caller(&decision, "launch", Some(app_id), &[]).is_err());
        }
        for (action, app_id) in [
            ("list", None), ("focus", None), ("close", None), ("restart", Some(APP_ID)),
        ] {
            assert!(authorize_caller(&decision, action, app_id, &[]).is_err());
        }
        assert!(!crate::clawd::authority::obligation_met(Some(&decision)));
        assert_eq!(decision.app_id(), app);
    }
}

#[test]
fn native_launch_admits_independent_callers_with_only_the_existing_exact_rights() {
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", root.path().join("data"));
    let file = root.path().join("report.txt");
    fs::write(&file, "fixture").unwrap();
    let file = fs::canonicalize(file).unwrap();
    let path = file.to_str().unwrap();

    for (target, app_id, program) in NATIVE_TARGETS {
        let paths = match target {
            NativeLaunchTarget::Files => vec![Some(path)],
            NativeLaunchTarget::Editor => vec![None, Some(path)],
            _ => vec![None],
        };
        for (index, app) in [Some(program), Some("independent-desktop-client"), None]
            .into_iter().enumerate()
        {
            for (path_index, local_path) in paths.iter().copied().enumerate() {
                let mut caps = vec![Cap::new(Verb::PROC_SPAWN, Scope::name(program))];
                if let Some(path) = local_path {
                    let verb = if target == NativeLaunchTarget::Files { Verb::FS_META } else { Verb::FS_READ };
                    caps.push(Cap::new(verb, Scope::path(path)));
                }
                let session = format!("native-{program}-{index}-{path_index}");
                let decision = decision_for_app_with_uses(app, caps.clone(), &session, Uses::Budget(1));
                assert_eq!(native_launch_target(&decision, "launch", Some(app_id)), Some(target));
                assert_eq!(native_launch_target(&decision, "launch", Some(APP_ID)), None);
                assert!(authorize_caller(&decision, "launch", Some(APP_ID), &[]).is_err());
                assert!(authorize_caller(&decision, "launch", Some(app_id), &[]).is_err());
                for (other, other_id, _) in NATIVE_TARGETS {
                    if other != target {
                        assert_eq!(native_launch_target(&decision, "launch", Some(other_id)), None);
                        let other_path = matches!(other, NativeLaunchTarget::Files | NativeLaunchTarget::Editor)
                            .then_some(path);
                        assert!(authorize_native_launch(&decision, other, other_path).is_err());
                    }
                }
                for action in ["list", "focus", "close", "restart"] {
                    let target_id = (action == "restart").then_some(app_id);
                    assert_eq!(native_launch_target(&decision, action, target_id), None);
                    assert!(authorize_caller(&decision, action, target_id, &[]).is_err());
                }
                let proof = authorize_native_launch(&decision, target, local_path).unwrap();
                assert_eq!(proof.spent(), caps.as_slice());
                assert_eq!(decision.app_id(), app);
                assert_eq!(decision.session_id(), Some(session.as_str()));
                assert_eq!(decision.owner_uid(), unsafe { libc::geteuid() });
                assert!(authorize_native_launch(&decision, target, local_path).is_err());
            }
        }
    }
}

#[test]
fn native_launch_rechecks_revocation_for_the_source_session() {
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", root.path().join("data"));
    for (target, app_id, program) in NATIVE_TARGETS {
        let cap = Cap::new(Verb::PROC_SPAWN, Scope::name(program));
        let local_path = matches!(target, NativeLaunchTarget::Files | NativeLaunchTarget::Editor)
            .then_some("/work/report.txt");
        let mut caps = vec![cap];
        if let Some(path) = local_path {
            let verb = if target == NativeLaunchTarget::Files { Verb::FS_META } else { Verb::FS_READ };
            caps.push(Cap::new(verb, Scope::path(path)));
        }
        let app = format!("independent-{program}-client");
        let client = decision_for_app(Some(&app), caps.clone(), &format!("revoked-{program}"));
        let original = decision_for_app(Some(program), caps, &format!("original-{program}"));
        assert_eq!(native_launch_target(&client, "launch", Some(app_id)), Some(target));
        authority().revoke_session(client.session_id().unwrap());
        assert_eq!(native_launch_target(&client, "launch", Some(app_id)), Some(target));
        assert!(authorize_native_launch(&client, target, local_path).is_err());
        assert!(!crate::clawd::authority::obligation_met(Some(&client)));
        let _authorized = authorize_native_launch(&original, target, local_path).unwrap();
        authority().revoke_session(original.session_id().unwrap());
        assert_eq!(native_launch_target(&original, "launch", Some(app_id)), Some(target));
        assert!(authorize_native_launch(&original, target, local_path).is_err());
        assert_eq!(client.app_id(), Some(app.as_str()));
        assert_eq!(original.app_id(), Some(program));
    }
}

#[test]
fn settings_launch_page_is_data_not_path_or_command_option() {
    assert!(settings_launch_args(&[]).unwrap().is_empty());
    validate_action("launch", None, Some("com.clawos.Settings"), &["settings://date-time".into()])
        .unwrap();
    assert_eq!(settings_launch_args(&["settings://date-time".into()]).unwrap(), vec!["date-time"]);
    for uris in [
        vec!["settings://--help".into()], vec!["file:///etc/passwd".into()],
        vec!["settings://../users".into()], vec!["settings://users".into(), "settings://sound".into()],
        vec!["settings://".into()], vec!["settings://a b".into()],
        vec!["settings://users?command=delete".into()], vec!["settings://users\0".into()],
        vec![format!("settings://{}", "x".repeat(65))],
    ] {
        assert!(settings_launch_args(&uris).is_err());
    }
}

#[tokio::test]
async fn settings_launch_requires_exact_original_proc_scope_before_desktop_access() {
    for (index, caps) in [
        vec![], vec![Cap::new(Verb::SYS_OBSERVE, Scope::name("desktop"))],
        vec![Cap::new(Verb::DESKTOP_LAUNCH, Scope::name("com.clawos.Settings"))],
        vec![Cap::new(Verb::PROC_SPAWN, Scope::name("cosmic-store"))],
    ].into_iter().enumerate() {
        let decision = decision_for_app(Some("cosmic-settings"), caps, &format!("settings-denied-{index}"));
        let error = open_settings(&[], &decision, 1000, 1000, PathBuf::from("/nonexistent"), 1)
            .await.unwrap_err();
        assert!(!error.contains("home"), "{error}");
    }
}

#[test]
fn store_launch_package_is_data_not_executable_or_option() {
    assert!(store_launch_args(&[]).unwrap().is_empty());
    assert_eq!(store_launch_args(&["apt://curl:amd64".into()]).unwrap(), vec!["curl:amd64"]);
    for uris in [
        vec!["apt://-oHook=id".into()], vec!["file:///etc/passwd".into()],
        vec!["apt://../curl".into()], vec!["apt://curl".into(), "apt://wget".into()],
        vec!["apt://a b".into()], vec![format!("apt://{}", "x".repeat(256))],
    ] {
        assert!(store_launch_args(&uris).is_err());
    }
}

#[tokio::test]
async fn store_launch_requires_original_exact_proc_scope_before_desktop_access() {
    for (index, caps) in [
        vec![], vec![Cap::new(Verb::SYS_PACKAGE, Scope::wild())],
        vec![Cap::new(Verb::DESKTOP_LAUNCH, Scope::name("com.clawos.Store"))],
        vec![Cap::new(Verb::PROC_SPAWN, Scope::name("cosmic-term"))],
    ].into_iter().enumerate() {
        let decision = decision_for_app(Some("cosmic-store"), caps, &format!("store-denied-{index}"));
        let error = open_store(&[], &decision, 1000, 1000, PathBuf::from("/nonexistent"), 1)
            .await.unwrap_err();
        assert!(!error.contains("home"), "{error}");
    }
}

#[test]
fn terminal_launch_directory_is_data_not_arguments_or_an_app() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("a #é --command");
    fs::create_dir(&directory).unwrap();
    let uri = url::Url::from_file_path(&directory).unwrap().to_string();
    assert_eq!(terminal_launch_args(&[uri]).unwrap(), vec![
        "--working-directory".to_string(), directory.display().to_string(),
    ]);
    let link = root.path().join("linked-directory");
    std::os::unix::fs::symlink(&directory, &link).unwrap();
    let uri = url::Url::from_directory_path(&link).unwrap().to_string();
    assert_eq!(terminal_launch_args(&[uri]).unwrap()[1], directory.display().to_string());
    assert!(terminal_launch_args(&[]).unwrap().is_empty());
    let file = root.path().join("file");
    fs::write(&file, "fixture").unwrap();
    assert!(terminal_launch_args(&[url::Url::from_file_path(file).unwrap().to_string()]).is_err());
    for uris in [
        vec!["https://example.test".into()],
        vec!["file://example.test/work".into()],
        vec!["file:///work?command=id".into()],
        vec!["file:///work#command".into()],
        vec!["file:///work/a".into(), "file:///work/b".into()],
        vec!["--command".into()],
    ] {
        assert!(terminal_launch_args(&uris).is_err());
    }
}

#[tokio::test]
async fn terminal_launch_requires_original_exact_proc_scope_before_desktop_access() {
    for (index, caps) in [
        vec![],
        vec![Cap::new(Verb::DESKTOP_LAUNCH, Scope::name("com.clawos.Term"))],
        vec![Cap::new(Verb::PROC_SPAWN, Scope::name("other-program"))],
    ].into_iter().enumerate() {
        let decision = decision_for_app(Some("cosmic-term"), caps, &format!("term-denied-{index}"));
        let error = open_terminal(&[], &decision, 1000, 1000, PathBuf::from("/nonexistent"), 1)
            .await.unwrap_err();
        assert!(!error.contains("home"), "{error}");
    }
}

#[test]
fn files_local_uri_accepts_directories_without_widening_editor_uris() {
    let root = tempfile::tempdir().unwrap();
    let directory = fs::canonicalize(root.path()).unwrap();
    let uri = url::Url::from_file_path(&directory).unwrap().to_string();
    assert_eq!(canonical_local_uri_path(&uri, true).unwrap(), Some(directory.display().to_string()));
    assert_eq!(files_reveal_path(&[uri.clone()]).unwrap(), directory.display().to_string());
    assert!(canonical_file_uri_path(&uri).is_err());
    assert!(editor_launch_path(&[uri]).is_err());
    for uris in [
        vec![],
        vec!["https://example.test".into()],
        vec!["file://example.test/work".into()],
        vec!["file:///work/a".into(), "file:///work/b".into()],
    ] {
        assert!(files_reveal_path(&uris).is_err());
    }
    let link = directory.join("link");
    std::os::unix::fs::symlink(&directory, &link).unwrap();
    let uri = url::Url::from_file_path(link).unwrap().to_string();
    assert!(canonical_local_uri_path(&uri, true).is_err());
}

#[tokio::test]
async fn files_reveal_requires_both_exact_capabilities_before_desktop_access() {
    let root = tempfile::tempdir().unwrap();
    let directory = fs::canonicalize(root.path()).unwrap();
    let uri = url::Url::from_file_path(&directory).unwrap().to_string();
    for (index, caps) in [
        vec![Cap::new(Verb::FS_META, Scope::path(directory.to_str().unwrap()))],
        vec![Cap::new(Verb::PROC_SPAWN, Scope::name("cosmic-files"))],
        vec![Cap::new(Verb::DESKTOP_LAUNCH, Scope::name("com.clawos.Files"))],
        vec![Cap::new(Verb::FS_META, Scope::path("/not-the-requested-directory")),
             Cap::new(Verb::PROC_SPAWN, Scope::name("cosmic-files"))],
        vec![Cap::new(Verb::FS_READ, Scope::path(directory.to_str().unwrap())),
             Cap::new(Verb::PROC_SPAWN, Scope::name("cosmic-files"))],
        vec![Cap::new(Verb::FS_META, Scope::path(directory.to_str().unwrap())),
             Cap::new(Verb::PROC_SPAWN, Scope::name("cosmic-edit"))],
    ].into_iter().enumerate() {
        let decision = decision_for_app(Some("independent-files-client"), caps, &format!("files-denied-{index}"));
        let error = reveal_files(&[uri.clone()], &decision, 1000, 1000, directory.clone(), 1)
            .await.unwrap_err();
        assert!(!error.contains("home"), "{error}");
        assert!(!crate::clawd::authority::obligation_met(Some(&decision)));
    }
}

#[test]
fn editor_launch_accepts_only_one_optional_canonical_local_file() {
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let file = root.path().join("report.txt");
    fs::write(&file, "fixture").unwrap();
    let file = fs::canonicalize(file).unwrap();
    let uri = url::Url::from_file_path(&file).unwrap().to_string();
    assert_eq!(editor_launch_path(&[]).unwrap(), None);
    assert_eq!(editor_launch_path(&[uri.clone()]).unwrap(), Some(file.display().to_string()));
    for uris in [
        vec!["https://example.test".into()],
        vec!["file://example.test/report.txt".into()],
        vec![format!("{uri}?command=id")],
        vec![uri.clone(), uri],
    ] {
        assert!(editor_launch_path(&uris).is_err());
    }
}

#[tokio::test]
async fn editor_launch_requires_original_proc_and_exact_read_before_desktop_access() {
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let file = root.path().join("report.txt");
    fs::write(&file, "fixture").unwrap();
    let file = fs::canonicalize(file).unwrap();
    let uri = url::Url::from_file_path(&file).unwrap().to_string();
    for (index, caps) in [
        vec![],
        vec![Cap::new(Verb::PROC_SPAWN, Scope::name("cosmic-edit"))],
        vec![Cap::new(Verb::FS_READ, Scope::path(file.to_str().unwrap()))],
        vec![Cap::new(Verb::PROC_SPAWN, Scope::name("cosmic-edit")),
             Cap::new(Verb::FS_READ, Scope::path("/not-the-requested-file"))],
        vec![Cap::new(Verb::PROC_SPAWN, Scope::name("cosmic-files")),
             Cap::new(Verb::FS_READ, Scope::path(file.to_str().unwrap()))],
    ].into_iter().enumerate() {
        let decision = decision_for_app(Some("independent-editor-client"), caps, &format!("editor-denied-{index}"));
        let error = open_editor(&[uri.clone()], &decision, 1000, 1000, root.path().join("missing-home"), 1)
            .await.unwrap_err();
        assert!(error.contains("capability"), "{error}");
        assert!(!crate::clawd::authority::obligation_met(Some(&decision)));
    }
}

#[test]
fn desktop_action_validation_is_strict() {
    validate_action("focus", Some("window-1"), None, &[]).unwrap();
    validate_action("restart", Some("window-1"), Some(APP_ID), &[]).unwrap();
    validate_action("launch", None, Some(APP_ID), &[]).unwrap();
    validate_action(
        "launch",
        None,
        Some(APP_ID),
        &[
            "https://example.test/new-window".to_string(),
            "file:///data/report.txt".to_string(),
        ],
    )
    .unwrap();

    assert!(validate_action("restart", Some("window-1"), Some("*"), &[]).is_err());
    assert!(validate_action("list", Some("window-1"), None, &[]).is_err());
    assert!(validate_action("launch", Some("window-1"), Some(APP_ID), &[]).is_err());
    assert!(validate_action("launch", None, None, &[]).is_err());
    assert!(validate_action("launch", None, Some(".hidden"), &[]).is_err());
    assert!(validate_action("launch", None, Some("_hidden"), &[]).is_err());
    assert!(validate_action("focus", Some("window-1"), None, &["uri".to_string()]).is_err());
}

#[test]
fn desktop_launch_uri_validation_is_bounded() {
    validate_launch_uris(&["https://example.test/report".to_string()]).unwrap();
    let long_uri = format!("https://example.test/{}", "é".repeat(2028));
    validate_launch_uris(&[long_uri]).unwrap();

    assert!(validate_launch_uris(&vec!["x".to_string(); MAX_LAUNCH_URIS + 1]).is_err());
    assert!(validate_launch_uris(&["".to_string()]).is_err());
    assert!(validate_launch_uris(&["line\nbreak".to_string()]).is_err());
    assert!(validate_launch_uris(&["é".repeat(MAX_URI_BYTES / 2 + 1)]).is_err());
    assert!(validate_launch_uris(&["relative/path".to_string()]).is_err());
}

#[test]
fn desktop_wire_bounds_uri_count_and_bytes() {
    let valid = json!({
        "session": "session",
        "action": "launch",
        "app_id": APP_ID,
        "uris": vec!["x"; MAX_LAUNCH_URIS],
    });
    serde_json::from_value::<crate::clawd::wire::requests::DesktopControl>(valid).unwrap();

    let too_many = json!({
        "session": "session",
        "action": "launch",
        "app_id": APP_ID,
        "uris": vec!["x"; MAX_LAUNCH_URIS + 1],
    });
    assert!(
        serde_json::from_value::<crate::clawd::wire::requests::DesktopControl>(too_many).is_err()
    );

    let too_long = json!({
        "session": "session",
        "action": "launch",
        "app_id": APP_ID,
        "uris": ["x".repeat(MAX_URI_BYTES + 1)],
    });
    assert!(
        serde_json::from_value::<crate::clawd::wire::requests::DesktopControl>(too_long).is_err()
    );
}

#[test]
fn launch_arguments_keep_option_shaped_values_as_data() {
    let uris = vec![
        "--new-window".to_string(),
        "file:///data/report.txt".to_string(),
    ];
    assert_eq!(
        gtk4_launch_args(APP_ID, &uris),
        vec!["--", APP_ID, "--new-window", "file:///data/report.txt",]
    );
}

#[test]
fn requested_capabilities_are_action_specific() {
    assert_eq!(
        requested_caps("list", None, &[]).unwrap(),
        vec![Cap::new(Verb::SYS_OBSERVE, Scope::name("desktop"))]
    );
    assert_eq!(
        requested_caps("launch", Some(APP_ID), &[]).unwrap(),
        vec![Cap::new(Verb::DESKTOP_LAUNCH, Scope::name(APP_ID))]
    );
    assert_eq!(
        requested_caps("restart", Some(APP_ID), &[]).unwrap(),
        vec![
            Cap::new(Verb::DESKTOP_WINDOW, Scope::name("control")),
            Cap::new(Verb::DESKTOP_LAUNCH, Scope::name(APP_ID)),
        ]
    );
}

#[test]
fn local_file_launch_requires_exact_read_authority() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("report.txt");
    std::fs::write(&file, b"report").unwrap();
    let uri = url::Url::from_file_path(&file).unwrap().to_string();
    let canonical = std::fs::canonicalize(&file).unwrap();

    assert_eq!(
        canonicalize_launch_uris(std::slice::from_ref(&uri)).unwrap(),
        vec![uri.clone()]
    );
    assert_eq!(
        requested_caps("launch", Some(APP_ID), &[uri]).unwrap(),
        vec![
            Cap::new(Verb::DESKTOP_LAUNCH, Scope::name(APP_ID)),
            Cap::new(
                Verb::FS_READ,
                Scope::path(canonical.to_string_lossy().into_owned()),
            ),
        ]
    );
}

#[test]
fn local_file_launch_rejects_unsafe_paths() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("report.txt");
    let link = directory.path().join("report-link.txt");
    std::fs::write(&file, b"report").unwrap();
    std::os::unix::fs::symlink(&file, &link).unwrap();

    let remote = "file://example.test/report.txt".to_string();
    let missing = url::Url::from_file_path(directory.path().join("missing.txt"))
        .unwrap()
        .to_string();
    let directory_uri = url::Url::from_directory_path(directory.path())
        .unwrap()
        .to_string();
    let link_uri = url::Url::from_file_path(link).unwrap().to_string();
    let canonical_uri = url::Url::from_file_path(&file).unwrap().to_string();
    let noncanonical = canonical_uri.replace("/report.txt", "/./report.txt");

    for uri in [remote, missing, directory_uri, link_uri, noncanonical] {
        assert!(canonicalize_launch_uris(&[uri]).is_err());
    }
}

#[test]
fn launch_authority_must_include_every_local_file() {
    let _lock = crate::test_env::lock_env();
    let directory = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", directory.path().join("data"));
    let file = directory.path().join("report.txt");
    let other_file = directory.path().join("other.txt");
    std::fs::write(&file, b"report").unwrap();
    std::fs::write(&other_file, b"other").unwrap();
    let file = fs::canonicalize(file).unwrap();
    let other_file = fs::canonicalize(other_file).unwrap();
    let uris = vec![
        url::Url::from_file_path(&file).unwrap().to_string(),
        url::Url::from_file_path(&other_file).unwrap().to_string(),
    ];
    let launch = Cap::new(Verb::DESKTOP_LAUNCH, Scope::name(APP_ID));
    let read = Cap::new(Verb::FS_READ, Scope::path(file.to_str().unwrap()));
    let other_read = Cap::new(Verb::FS_READ, Scope::path(other_file.to_str().unwrap()));
    for (index, caps) in [
        vec![launch.clone()],
        vec![launch.clone(), read.clone()],
        vec![launch.clone(), read.clone(), Cap::new(Verb::FS_READ, Scope::path("/not-the-requested-file"))],
        vec![Cap::new(Verb::DESKTOP_LAUNCH, Scope::name("com.example.Other")), read.clone(), other_read.clone()],
    ].into_iter().enumerate() {
        let decision = decision_for_app(
            Some("independent-desktop-client"), caps, &format!("desktop-missing-file-{index}"),
        );
        assert!(authorize_caller(&decision, "launch", Some(APP_ID), &uris).is_err());
        assert!(!crate::clawd::authority::obligation_met(Some(&decision)));
    }
    let decision = decision_for_app(
        Some("independent-desktop-client"),
        vec![launch, read, other_read],
        "desktop-all-files",
    );
    let proof = authorize_caller(&decision, "launch", Some(APP_ID), &uris).unwrap();
    assert_eq!(proof.spent(), requested_caps("launch", Some(APP_ID), &uris).unwrap().as_slice());
}

#[test]
fn desktop_actions_admit_independently_authorized_sessions_without_rebinding_identity() {
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", root.path().join("data"));
    for (index, app) in [
        Some("independent-desktop-client"), Some("launcher"), Some("desktop-manager"),
        Some("cosmic-settings"), None,
    ].into_iter().enumerate() {
        for (action, app_id) in [
            ("launch", Some(APP_ID)), ("list", None), ("focus", None),
            ("close", None), ("restart", Some(APP_ID)),
        ] {
            let caps = requested_caps(action, app_id, &[]).unwrap();
            let session = format!("desktop-{index}-{action}");
            let decision = decision_for_app_with_uses(app, caps.clone(), &session, Uses::Budget(1));
            let proof = authorize_caller(&decision, action, app_id, &[]).unwrap();
            assert_eq!(proof.spent(), caps.as_slice());
            assert_eq!(decision.app_id(), app);
            assert_eq!(decision.session_id(), Some(session.as_str()));
            assert_eq!(decision.owner_uid(), unsafe { libc::geteuid() });
            assert!(authorize_caller(&decision, action, app_id, &[]).is_err());
        }
    }
}

#[test]
fn desktop_actions_still_require_the_exact_action_and_restart_target_scopes() {
    for (index, (action, app_id, caps)) in [
        ("list", None, vec![Cap::new(Verb::SYS_OBSERVE, Scope::name("network"))]),
        ("focus", None, vec![Cap::new(Verb::DESKTOP_WINDOW, Scope::name("other"))]),
        ("close", None, vec![Cap::new(Verb::SYS_OBSERVE, Scope::name("desktop"))]),
        ("launch", Some(APP_ID), vec![Cap::new(Verb::PROC_SPAWN, Scope::name("cosmic-term"))]),
        ("launch", Some(APP_ID), vec![Cap::new(Verb::DESKTOP_LAUNCH, Scope::name("com.example.Other"))]),
        ("restart", Some(APP_ID), vec![Cap::new(Verb::DESKTOP_WINDOW, Scope::name("control"))]),
        ("restart", Some(APP_ID), vec![Cap::new(Verb::DESKTOP_LAUNCH, Scope::name(APP_ID))]),
        ("restart", Some(APP_ID), vec![
            Cap::new(Verb::DESKTOP_WINDOW, Scope::name("control")),
            Cap::new(Verb::DESKTOP_LAUNCH, Scope::name("com.example.Other")),
        ]),
    ].into_iter().enumerate() {
        let decision = decision_for_app(
            Some("independent-desktop-client"), caps, &format!("desktop-wrong-scope-{index}"),
        );
        assert!(authorize_caller(&decision, action, app_id, &[]).is_err());
        assert!(!crate::clawd::authority::obligation_met(Some(&decision)));
    }
}

#[test]
fn desktop_policy_revocation_tracks_the_source_app_not_the_backend() {
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", root.path().join("data"));
    let uid = unsafe { libc::geteuid() };
    let observe = Cap::new(Verb::SYS_OBSERVE, Scope::name("desktop"));
    let window = Cap::new(Verb::DESKTOP_WINDOW, Scope::name("control"));
    let client = decision_for_app(
        Some("independent-desktop-client"), vec![observe.clone(), window], "desktop-policy-client",
    );
    let manager = decision_for_app(
        Some("desktop-manager"), vec![observe.clone()], "desktop-policy-manager",
    );
    crate::approvals::app_policy::revoke(uid, "independent-desktop-client", observe).unwrap();
    assert!(authorize_caller(&client, "list", None, &[]).unwrap_err().contains("revoked"));
    assert!(!crate::clawd::authority::obligation_met(Some(&client)));
    let _authorized = authorize_caller(&client, "focus", None, &[]).unwrap();
    let _authorized = authorize_caller(&manager, "list", None, &[]).unwrap();
    assert_eq!(client.app_id(), Some("independent-desktop-client"));
    assert_eq!(manager.app_id(), Some("desktop-manager"));
}

#[test]
fn canonical_launch_keeps_its_existing_rights_for_native_app_ids() {
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", root.path().join("data"));
    let file = root.path().join("report.txt");
    fs::write(&file, "fixture").unwrap();
    let file = fs::canonicalize(file).unwrap();
    let uri = url::Url::from_file_path(&file).unwrap().to_string();
    for (target, app_id, _) in NATIVE_TARGETS {
        let mut caps = vec![Cap::new(Verb::DESKTOP_LAUNCH, Scope::name(app_id))];
        let uris = if matches!(target, NativeLaunchTarget::Files | NativeLaunchTarget::Editor) {
            caps.push(Cap::new(Verb::FS_READ, Scope::path(file.to_str().unwrap())));
            vec![uri.clone()]
        } else {
            vec![]
        };
        let decision = decision_for_app(
            Some("independent-launch-client"), caps.clone(), &format!("canonical-{app_id}"),
        );
        assert_eq!(native_launch_target(&decision, "launch", Some(app_id)), None);
        let proof = authorize_caller(&decision, "launch", Some(app_id), &uris).unwrap();
        assert_eq!(proof.spent(), caps.as_slice());
        authority().revoke_session(decision.session_id().unwrap());
        assert!(authorize_caller(&decision, "launch", Some(app_id), &uris).is_err());
    }
}

#[test]
fn display_validation_rejects_paths_and_options() {
    assert!(valid_wayland_display("wayland-0"));
    assert!(!valid_wayland_display("../wayland-0"));
    assert!(valid_display(":0"));
    assert!(!valid_display("-display"));
}

fn decision_for_app(app_id: Option<&str>, caps: Vec<Cap>, session: &str) -> Decision {
    decision_for_app_with_uses(app_id, caps, session, Uses::Unbounded)
}

fn decision_for_app_with_uses(
    app_id: Option<&str>,
    caps: Vec<Cap>,
    session: &str,
    uses: Uses,
) -> Decision {
    let store = authority();
    let uid = unsafe { libc::geteuid() };
    let (_handle, view) = store
        .issue(Issuance {
            issuer: Issuer::AppSessionAuthority,
            principal: Principal::of_process(uid, std::process::id()).expect("this process"),
            binding: Binding::ProcessTree,
            subject: Subject::session(session).with_app(app_id.map(str::to_string)),
            audience: AudienceSet::one(Audience::SystemService),
            caps: crate::caps::CapSet::from_caps(caps),
            lifetime: std::time::Duration::from_secs(60),
            uses,
            index_session: true,
        })
        .expect("issue a session grant");
    Decision::for_test(
        view,
        "system.desktop.control",
        Audience::SystemService,
        Presentation {
            uid,
            pid: std::process::id(),
            start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
            audience: Audience::SystemService,
            route: "system.desktop.control",
            session_id: Some(session.to_string()),
        },
        None,
        &Requirement::RouteDerived,
    )
}
