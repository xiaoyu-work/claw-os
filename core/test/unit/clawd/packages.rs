use super::*;

#[test]
fn native_package_intents_reject_authority_fields_and_noncanonical_targets() {
    let body = json!({
        "session": "synthetic-gui-session", "invocation_id": "a".repeat(32),
        "mode": "submit", "action": "remove", "packages": ["fixture-one", "fixture-two:amd64"],
    });
    let request: super::intent::Request = serde_json::from_value(body.clone()).unwrap();
    assert_eq!(request.names().unwrap(), ["fixture-one", "fixture-two:amd64"]);
    for field in ["owner_uid", "approved", "confirmation", "command", "source", "provider", "path"] {
        let mut invalid = body.clone();
        invalid[field] = json!(true);
        assert!(serde_json::from_value::<super::intent::Request>(invalid).is_err(), "{field}");
    }
    for packages in [
        json!([]), json!(["fixture-two", "fixture-one"]),
        json!(["fixture-one", "fixture-one"]), json!(["/local.deb"]),
        json!(["app/org.example.Fixture/x86_64/stable"]), json!(["*"]),
    ] {
        let mut invalid = body.clone();
        invalid["packages"] = packages;
        let parsed = serde_json::from_value::<super::intent::Request>(invalid);
        assert!(parsed.is_err() || parsed.unwrap().names().is_err());
    }
}

#[cfg(target_os = "linux")]
#[test]
fn native_package_window_registry_bounds_and_exact_reuse() {
    use super::gui::{IntentLedger, ReviewIdentity};
    use crate::clawd::package_transactions::Operation;
    let mut registry = IntentLedger::default();
    for index in 0..32 {
        let package = format!("fixture-{index}");
        registry.reserve("root-window", &format!("{index}"),
            Operation::Install { package: package.clone() },
            vec![Cap::new(Verb::SYS_PACKAGE, Scope::name(package))]).unwrap();
    }
    registry.entries.get_mut("0").unwrap().targets[0].identity =
        ReviewIdentity::Tracked("ap-partially-approved".into());
    let operation = Operation::Install { package: "fixture-0".into() };
    let required = vec![Cap::new(Verb::SYS_PACKAGE, Scope::name("fixture-0"))];
    registry.reserve("root-window", "0", operation.clone(), required.clone()).unwrap();
    assert_eq!(registry.entries.len(), 32);
    assert_eq!(registry.target_count, 32);
    assert_eq!(registry.entries["0"].targets[0].identity,
        ReviewIdentity::Tracked("ap-partially-approved".into()));
    assert!(registry.reserve("root-window", "33", operation.clone(), required.clone()).is_err());
    assert!(registry.reserve("another-window", "0", operation, required.clone()).is_err());
    assert!(registry.reserve("root-window", "0",
        Operation::Remove { package: "fixture-0".into() }, required).is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn native_package_window_partial_creation_and_close_never_reset_quota() {
    use super::gui::{IntentLedger, ReviewIdentity};
    use crate::clawd::package_transactions::Operation;
    let names = (0..128).map(|index| format!("fixture-{index:03}")).collect::<Vec<_>>();
    let required = names.iter().map(|name| Cap::new(Verb::SYS_PACKAGE, Scope::name(name))).collect::<Vec<_>>();
    let operation = Operation::install_packages(names).unwrap();
    let mut registry = IntentLedger::default();
    registry.reserve("root-window", "batch", operation.clone(), required.clone()).unwrap();
    registry.entries.get_mut("batch").unwrap().targets[0].identity = ReviewIdentity::Tracked("ap-known".into());
    registry.entries.get_mut("batch").unwrap().targets[1].identity = ReviewIdentity::Unconfirmed;
    registry.reserve("root-window", "batch", operation.clone(), required.clone()).unwrap();
    assert_eq!(registry.target_count, 128);
    assert!(registry.reserve("root-window", "another",
        Operation::Install { package: "fixture-extra".into() },
        vec![Cap::new(Verb::SYS_PACKAGE, Scope::name("fixture-extra"))]).is_err());
    let first = registry.fence();
    assert!(registry.reserve("root-window", "batch", operation, required).is_err());
    let retry = registry.fence();
    assert_eq!(first[0].1.targets[0].identity, retry[0].1.targets[0].identity);
    assert_eq!(retry[0].1.targets[1].identity, ReviewIdentity::Unconfirmed);
    assert!(!registry.retired);
    assert_eq!(registry.entries.len(), 1);
    assert_eq!(registry.target_count, 128);
}
use crate::clawd::authority::{
    authority, Audience, AudienceSet, Binding, Issuance, Issuer, Presentation, Principal,
    Requirement, Subject, Uses,
};
use crate::test_env::TestEnvVarGuard;

fn decision(app: Option<&str>, caps: Vec<Cap>, uses: Uses, include_session: bool) -> Decision {
    decision_for_route("system.package.control", app, caps, uses, include_session)
}

fn decision_for_route(
    route: &'static str,
    app: Option<&str>,
    caps: Vec<Cap>,
    uses: Uses,
    include_session: bool,
) -> Decision {
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
        route,
        Audience::SystemService,
        Presentation {
            uid,
            pid,
            start_time_ticks: crate::proc::read_start_time_ticks_pub(pid),
            audience: Audience::SystemService,
            route,
            session_id: Some(session),
        },
        row,
        &Requirement::RouteDerived,
    )
}

#[test]
fn package_queries_require_live_read_scopes_not_a_product_name_or_package_mutation_grant() {
    let _lock = crate::test_env::lock_env();
    let directory = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("COS_CAPS_DATA_DIR", directory.path());
    let owner = unsafe { libc::geteuid() };
    for app in [Some("cosmic-store"), Some("independent-package-client"), None] {
        for (body, scope) in [
            (json!({"session": "fixture", "action": "installed"}), "packages"),
            (json!({"session": "fixture", "action": "search", "query": "editor", "limit": 25}), "packages"),
            (json!({"session": "fixture", "action": "show", "name": "fixture-one:amd64"}), "fixture-one:amd64"),
            (json!({"session": "fixture", "action": "available", "name": "fixture-one:amd64"}), "fixture-one:amd64"),
            (json!({"session": "fixture", "action": "available", "name": "FixtureCommand"}), "FixtureCommand"),
        ] {
            let request = serde_json::from_value(body).unwrap();
            let cap = Cap::new(Verb::SYS_OBSERVE, Scope::name(scope));
            let grant = |caps| decision_for_route(
                "system.package.query", app, caps, Uses::Budget(1), true,
            );
            let allowed = grant(vec![cap.clone()]);
            assert!(super::query::authorize(&allowed, owner + 1, &request).is_err());
            assert_eq!(super::query::authorize(&allowed, owner, &request).unwrap().spent(), std::slice::from_ref(&cap));
            assert!(super::query::authorize(&allowed, owner, &request).is_err());
            for caps in [
                Vec::new(),
                vec![Cap::new(Verb::SYS_OBSERVE, Scope::name("hardware"))],
                vec![Cap::new(Verb::SYS_PACKAGE, Scope::name(scope))],
            ] {
                assert!(super::query::authorize(&grant(caps), owner, &request).is_err());
            }
            let revoked = grant(vec![cap]);
            authority().revoke_session(revoked.session_id().unwrap());
            assert!(super::query::authorize(&revoked, owner, &request).is_err());
        }
    }
}

#[test]
fn package_query_wire_rejects_authority_paths_mutation_actions_and_unbounded_search() {
    let route = crate::clawd::routes::Command::SystemPackageQuery.route();
    let body = json!({"session": "fixture", "action": "search", "query": "-oHook=id", "limit": 25});
    let parse = |value| -> Result<Cap, String> {
        let value = (route.decode)(value).map_err(|_| "invalid typed package query".to_string())?;
        let request = serde_json::from_value(value).map_err(|_| "invalid package query".to_string())?;
        super::query::required_cap(&request)
    };
    assert!(parse(body.clone()).is_ok());
    for field in ["owner_uid", "path", "command", "provider", "approved", "socket", "packages", "offset"] {
        let mut invalid = body.clone();
        invalid[field] = json!("caller-selected");
        assert!((route.decode)(invalid).is_err(), "{field}");
    }
    for (field, value) in [
        ("action", json!("install")), ("limit", json!(0)), ("limit", json!(101)),
        ("query", json!("")), ("query", json!("x\nid")), ("name", json!("fixture-one")),
    ] {
        let mut invalid = body.clone();
        invalid[field] = value;
        assert!(parse(invalid).is_err(), "{field}");
    }
    let mut too_long = body;
    too_long["query"] = json!("x".repeat(513));
    assert!((route.decode)(too_long).is_err());
    for name in ["-oHook=id", "../fixture", "/local.deb", "*", "x\0y"] {
        assert!(parse(json!({"session": "fixture", "action": "show", "name": name})).is_err());
    }
    for name in ["", "-oHook=id", "../fixture", "/usr/bin/fixture", ".", "..", "x\nid"] {
        assert!(parse(json!({"session": "fixture", "action": "available", "name": name})).is_err());
    }
}

#[test]
fn package_query_projection_preserves_catalog_shapes_and_excludes_noninstalled_selections() {
    let result = super::query::search_result("fixture", 1,
        "fixture-one - First summary\nfixture-two - Second summary\n").unwrap();
    assert_eq!(result["results"], json!([{"name": "fixture-one", "summary": "First summary"}]));
    assert_eq!(result["count"], 1);
    assert_eq!(result["truncated"], true);
    let result = super::query::show_result("fixture-one:amd64",
        "Package: fixture-one\nVersion: 2.0\nDescription: First summary\n Long description\nHomepage: https://example.test\n\nPackage: fixture-one\nVersion: 1.0\nDescription: Older\n").unwrap();
    assert_eq!(result["version"], "2.0");
    assert_eq!(result["summary"], "First summary");
    assert_eq!(result["description"], "Long description");
    assert!(super::query::show_result("other", "Package: fixture-one\nVersion: 2.0\n").is_err());
    assert!(super::query::show_result("fixture-one", "").is_err());
    let result = super::query::installed_result(
        "fixture-one:amd64\tinstalled\t2.0\nfixture-old\tconfig-files\t1.0\nfixture-half\thalf-configured\t3.0\n",
    ).unwrap();
    assert_eq!(result["packages"], json!(["fixture-one:amd64"]));
    assert_eq!(result["versions"], json!({"fixture-one:amd64": "2.0"}));
    assert!(super::query::installed_result("malformed\n").is_err());
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

#[test]
fn package_receipt_queries_do_not_accept_owner_or_execution_selectors() {
    use crate::clawd::authority::SubjectSource;
    use crate::clawd::routes::Command;
    for command in [Command::SystemPackageStatus, Command::SystemPackageRecover] {
        let route = command.route();
        assert_eq!(route.authority.subject, SubjectSource::Peer);
        for field in [
            "owner_uid",
            "session",
            "caps",
            "command",
            "executable",
            "path",
        ] {
            let mut params = json!({"request_id":"a".repeat(32)});
            (route.decode)(params.clone()).unwrap();
            params[field] = json!("caller-selection");
            assert!((route.decode)(params).is_err(), "{field}");
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn retained_package_gate_requires_complete_apt_v3_removal_metadata() {
    use crate::clawd::package_transactions::check_hook_fixture;
    assert!(check_hook_fixture(vec!["/protected/archive.deb".into()]).is_err());
    let complete = vec![
        "VERSION 3".into(),
        "APT::Architecture=amd64".into(),
        "".into(),
        "fixture 1.0 amd64 none > - - none **REMOVE**".into(),
    ];
    assert_eq!(check_hook_fixture(complete.clone()).unwrap(), 1);
    let mut malformed = complete;
    malformed[3] = "fixture 1.0 amd64 none > - amd64 none **REMOVE**".into();
    assert!(check_hook_fixture(malformed).is_err());
}

#[test]
fn concrete_package_reads_keep_held_versions_and_separate_foreign_architectures() {
    let rows = "hold ok installed\t2.0\tamd64\ninstall ok installed\t1.0\ti386\n";
    let native = select_package_status(rows, "amd64").unwrap().unwrap();
    assert_eq!(
        native,
        ("hold ok installed".into(), "2.0".into(), "amd64".into())
    );
    assert_eq!(
        select_package_status(rows, "i386").unwrap().unwrap().1,
        "1.0"
    );
    assert!(select_package_status(rows, "arm64").unwrap().is_none());
    assert!(
        select_package_status("install ok installed\t2.0\tamd64\nmalformed\n", "amd64").is_err()
    );
}

#[cfg(target_os = "linux")]
#[test]
fn package_impact_follows_installed_shared_dependencies_not_every_app_package() {
    let rows = [
        "support\t1.0\tall\tinstalled\t\t\tshared-v1\t",
        "parser\t1.0\tall\tinstalled\tshared-v1 (>= 1)\t\tparser-v1\t",
        "app-a\t1.0\tall\tinstalled\tparser-v1\t\t\t",
        "app-b\t1.0\tall\tinstalled\tunrelated\t\t\t",
        "unrelated\t1.0\tall\tinstalled\t\t\t\t",
    ];
    let (closure, uncertain) =
        crate::clawd::package_transactions::dependency_fixture(&rows, &["support:all"]).unwrap();
    assert!(closure.contains(&"app-a:all".into()));
    assert!(!closure.contains(&"app-b:all".into()));
    assert!(!uncertain);
}

#[cfg(target_os = "linux")]
#[test]
fn ambiguous_shared_providers_are_not_reported_as_precise_package_impact() {
    let rows = [
        "support-one\t1.0\tall\tinstalled\t\t\tshared-v1\t",
        "support-two\t1.0\tall\tinstalled\t\t\tshared-v1\t",
        "app-a\t1.0\tall\tinstalled\tshared-v1\t\t\t",
    ];
    let (closure, uncertain) =
        crate::clawd::package_transactions::dependency_fixture(&rows, &["support-one:all"])
            .unwrap();
    assert!(closure.contains(&"app-a:all".into()));
    assert!(
        uncertain,
        "alternative providers cannot silently authorize a precise stop set"
    );
}
