use super::*;
use crate::clawd::authority::{
    authority, Audience, AudienceSet, Binding, Decision, Issuance, Issuer, Presentation, Principal,
    Requirement, Subject, Uses,
};
use serde_json::json;

const APP_ID: &str = "com.example.App";

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
    let store = authority();
    store.clear_for_test();
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("report.txt");
    std::fs::write(&file, b"report").unwrap();
    let uri = url::Url::from_file_path(&file).unwrap().to_string();
    let requested = requested_caps("launch", Some(APP_ID), &[uri]).unwrap();
    let decision = decision_for_app(
        Some("launcher"),
        vec![Cap::new(Verb::DESKTOP_LAUNCH, Scope::name(APP_ID))],
        "desktop-launch-missing-file",
    );

    authorize_caller(&decision, "launch").unwrap();
    assert!(decision.require_all(&requested).is_err());
    store.clear_for_test();
}

#[test]
fn desktop_actions_reject_cross_app_authority() {
    let store = authority();
    store.clear_for_test();

    let launch_caps = requested_caps("launch", Some(APP_ID), &[]).unwrap();
    let launcher = decision_for_app(
        Some("launcher"),
        launch_caps.clone(),
        "desktop-launcher-launch",
    );
    authorize_caller(&launcher, "launch").unwrap();
    let _authorized = launcher.require_all(&launch_caps).unwrap();

    for (action, app_id) in [
        ("list", None),
        ("focus", None),
        ("close", None),
        ("restart", Some(APP_ID)),
    ] {
        let caps = requested_caps(action, app_id, &[]).unwrap();
        let decision = decision_for_app(
            Some("launcher"),
            caps.clone(),
            &format!("desktop-launcher-{action}"),
        );
        assert!(authorize_caller(&decision, action).is_err());
    }

    let manager_launch = decision_for_app(
        Some("desktop-manager"),
        launch_caps.clone(),
        "desktop-manager-launch",
    );
    assert!(authorize_caller(&manager_launch, "launch").is_err());

    let manager_list_caps = requested_caps("list", None, &[]).unwrap();
    let manager_list = decision_for_app(
        Some("desktop-manager"),
        manager_list_caps.clone(),
        "desktop-manager-list",
    );
    authorize_caller(&manager_list, "list").unwrap();
    let _authorized = manager_list.require_all(&manager_list_caps).unwrap();

    let manager_restart_caps = requested_caps("restart", Some(APP_ID), &[]).unwrap();
    let manager_restart = decision_for_app(
        Some("desktop-manager"),
        manager_restart_caps.clone(),
        "desktop-manager-restart",
    );
    authorize_caller(&manager_restart, "restart").unwrap();
    let _authorized = manager_restart.require_all(&manager_restart_caps).unwrap();

    let agent = decision_for_app(None, launch_caps.clone(), "desktop-agent-launch");
    assert!(authorize_caller(&agent, "launch").is_err());

    store.clear_for_test();
}

#[test]
fn display_validation_rejects_paths_and_options() {
    assert!(valid_wayland_display("wayland-0"));
    assert!(!valid_wayland_display("../wayland-0"));
    assert!(valid_display(":0"));
    assert!(!valid_display("-display"));
}

fn decision_for_app(app_id: Option<&str>, caps: Vec<Cap>, session: &str) -> Decision {
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
            uses: Uses::Unbounded,
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
