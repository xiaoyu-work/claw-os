use super::*;
use crate::clawd::authority::{
    authority, Audience, AudienceSet, Binding, Issuance, Issuer, Presentation, Principal,
    Requirement, Subject, Uses,
};
use crate::test_env::TestEnvVarGuard;

fn decision(app: Option<&str>, caps: Vec<Cap>, uses: Uses, include_session: bool) -> Decision {
    let uid = unsafe { libc::geteuid() };
    let pid = std::process::id();
    let session = format!("package-client-{}", uuid::Uuid::new_v4().simple());
    let (_handle, view) = authority()
        .issue(Issuance {
            issuer: Issuer::AppSessionAuthority,
            principal: Principal::of_process(uid, pid).unwrap(),
            binding: Binding::ProcessTree,
            subject: Subject::session(&session).with_app(app.map(str::to_string)),
            audience: AudienceSet::one(Audience::SystemService),
            caps: crate::caps::CapSet::from_caps(caps),
            lifetime: Duration::from_secs(60),
            uses,
            index_session: true,
        })
        .unwrap();
    let row = include_session.then(|| SessionInfo {
        session_id: session.clone(),
        pid,
        command: vec!["package-client-fixture".into()],
        started_at: chrono::Utc::now().to_rfc3339(),
        stdout_path: String::new(),
        stderr_path: String::new(),
        group: None,
        parent: Some("authenticated-parent".into()),
        workdir: None,
        exit_code: None,
        ended_at: None,
        tier: None,
        scope: None,
        priority: None,
        caps: None,
        transient_caps: None,
        role: None,
        app_id: app.map(str::to_string),
        pending_bind: false,
        start_time_ticks: crate::proc::read_start_time_ticks_pub(pid),
        client: crate::session::SessionClient::default(),
    });
    Decision::for_test(
        view,
        "system.package.control",
        Audience::SystemService,
        Presentation {
            uid,
            pid,
            start_time_ticks: crate::proc::read_start_time_ticks_pub(pid),
            audience: Audience::SystemService,
            route: "system.package.control",
            session_id: Some(session),
        },
        row,
        &Requirement::RouteDerived,
    )
}

#[test]
fn independently_authorized_package_clients_keep_their_own_identity() {
    let _lock = crate::test_env::lock_env();
    let directory = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", directory.path().join("data"));
    let uid = unsafe { libc::geteuid() };
    for app in [
        Some("pkg"),
        Some("cosmic-store"),
        Some("independent-package-client"),
        None,
    ] {
        let granted = decision(
            app,
            vec![Cap::new(Verb::SYS_PACKAGE, Scope::name("fixture"))],
            Uses::Budget(1),
            true,
        );
        let (session, proof) = authorize_package_session(&granted, Scope::name("fixture"), uid)
            .expect(
                "the explicitly granted capability, not a product name, permits this operation",
            );
        assert_eq!(session.app_id.as_deref(), app);
        assert_eq!(session.parent.as_deref(), Some("authenticated-parent"));
        assert_eq!(granted.app_id(), app);
        assert_eq!(
            proof.spent(),
            &[Cap::new(Verb::SYS_PACKAGE, Scope::name("fixture"))]
        );
        if app != Some("pkg") {
            assert!(granted.require_app("pkg").is_err());
        }
        assert!(authorize_package_session(&granted, Scope::name("fixture"), uid).is_err());
    }
}

#[test]
fn package_clients_require_exact_live_authority_and_do_not_gain_defaults() {
    let _lock = crate::test_env::lock_env();
    let directory = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", directory.path().join("data"));
    let home = directory.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let uid = unsafe { libc::geteuid() };
    let cap = Cap::new(Verb::SYS_PACKAGE, Scope::name("fixture"));
    assert!(!crate::clawd::system_caps::system_agent_caps(uid, &home).covers(&cap));
    assert!(!crate::clawd::system_caps::local_launcher_ceiling(&home).covers(&cap));

    for app in ["pkg", "cosmic-store", "independent-package-client"] {
        for caps in [
            vec![],
            vec![Cap::new(Verb::SYS_PACKAGE, Scope::name("other-package"))],
            vec![Cap::new(Verb::SYS_PERMISSIONS, Scope::name("manage"))],
        ] {
            let rejected = decision(Some(app), caps, Uses::Budget(1), true);
            assert!(authorize_package_session(&rejected, Scope::name("fixture"), uid).is_err());
            assert!(!crate::clawd::authority::obligation_met(Some(&rejected)));
        }
    }
    let granted = decision(
        Some("independent-package-client"),
        vec![cap.clone()],
        Uses::Budget(1),
        true,
    );
    assert!(authorize_package_session(&granted, Scope::Wild, uid).is_err());
    assert!(authorize_package_session(&granted, Scope::name("fixture:arm64"), uid).is_err());
    let wrong_owner =
        authorize_package_session(&granted, Scope::name("fixture"), uid + 1).unwrap_err();
    assert!(wrong_owner.contains("another owner"), "{wrong_owner}");
    assert!(!crate::clawd::authority::obligation_met(Some(&granted)));
    let (_session, proof) =
        authorize_package_session(&granted, Scope::name("fixture"), uid).unwrap();
    assert_eq!(proof.spent(), std::slice::from_ref(&cap));

    let missing = decision(
        Some("independent-package-client"),
        vec![cap.clone()],
        Uses::Budget(1),
        false,
    );
    assert!(
        authorize_package_session(&missing, Scope::name("fixture"), uid)
            .unwrap_err()
            .contains("no authorized session")
    );
    assert!(!crate::clawd::authority::obligation_met(Some(&missing)));
    let revoked = decision(
        Some("independent-package-client"),
        vec![cap],
        Uses::Unbounded,
        true,
    );
    authority().revoke_session(revoked.session_id().unwrap());
    assert!(authorize_package_session(&revoked, Scope::name("fixture"), uid).is_err());

    let global = decision(
        Some("independent-package-client"),
        vec![Cap::new(Verb::SYS_PACKAGE, Scope::Wild)],
        Uses::Budget(1),
        true,
    );
    let (_session, proof) = authorize_package_session(&global, Scope::Wild, uid).unwrap();
    assert_eq!(proof.spent(), &[Cap::new(Verb::SYS_PACKAGE, Scope::Wild)]);
}

#[test]
fn package_restore_must_still_match_its_recorded_inverse() {
    let _lock = crate::test_env::lock_env();
    let directory = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", directory.path());
    let session = crate::session::create("private package inverse fixture").unwrap();
    let seq = crate::session::record_mutation(
        &session,
        MutationRecord::new(Mutation::SystemPackage {
            package: "fixture:amd64".into(),
            previous_version: Some("1.2-3".into()),
            was_held: true,
        }),
    )
    .unwrap();
    validate_restore_record(session.as_str(), seq, "fixture:amd64", Some("1.2-3"), true).unwrap();
    for (package, version, held) in [
        ("other-package", Some("1.2-3"), true),
        ("fixture:arm64", Some("1.2-3"), true),
        ("fixture:amd64", Some("1.2-4"), true),
        ("fixture:amd64", None, true),
        ("fixture:amd64", Some("1.2-3"), false),
    ] {
        assert!(validate_restore_record(session.as_str(), seq, package, version, held).is_err());
    }
    assert!(validate_restore_record(
        session.as_str(),
        seq + 1,
        "fixture:amd64",
        Some("1.2-3"),
        true
    )
    .is_err());
}

#[test]
fn package_routes_keep_their_subjects_and_reject_caller_authority() {
    use crate::clawd::authority::SubjectSource;
    use crate::clawd::routes::Command;

    for command in [
        Command::SystemPackageInstall,
        Command::SystemPackageControl,
        Command::SystemPackageRestore,
    ] {
        let route = command.route();
        let expected = if command == Command::SystemPackageRestore {
            SubjectSource::PeerSession
        } else {
            SubjectSource::Session
        };
        assert_eq!(route.authority.subject, expected);
        for key in ["owner_uid", "caps", "app_id", "command"] {
            let mut params = json!({
                "session":"fixture-session", "package":"fixture",
            });
            if command == Command::SystemPackageControl {
                params["action"] = json!("install");
            } else if command == Command::SystemPackageRestore {
                params["mutation_session"] = json!("fixture-task");
                params["mutation_seq"] = json!(1);
                params["was_held"] = json!(false);
            }
            (route.decode)(params.clone()).unwrap();
            params[key] = json!("caller-forgery");
            assert!((route.decode)(params).is_err(), "{key}");
        }
    }
}

#[test]
fn accepts_debian_package_names_versions_and_arch_qualifiers() {
    for name in ["bash", "libssl3", "python3-venv", "g++", "curl:amd64"] {
        validate_package_name(name).unwrap();
    }
    for version in ["1.2.3-1", "2:1.0~rc1+deb12u1"] {
        validate_version(version).unwrap();
    }
}

#[test]
fn rejects_options_paths_and_invalid_versions() {
    for name in ["", "-oDpkg::Pre-Invoke::=id", "../bash", "bash=1.0", "Bash"] {
        assert!(validate_package_name(name).is_err(), "{name:?} should fail");
    }
    for version in ["", "--option", "1.0 /tmp"] {
        assert!(
            validate_version(version).is_err(),
            "{version:?} should fail"
        );
    }
}

#[test]
fn global_and_versioned_action_shapes_are_strict() {
    validate_action("update-index", None, None).unwrap();
    validate_action("upgrade-all", None, None).unwrap();
    validate_action("install-version", Some("curl"), Some("8.0-1")).unwrap();
    assert!(validate_action("update-index", Some("curl"), None).is_err());
    assert!(validate_action("install-version", Some("curl"), None).is_err());
    assert!(validate_action("remove", Some("curl"), Some("8.0")).is_err());
}
