use super::*;

#[test]
fn native_store_cannot_borrow_pkg_transaction_identity_even_with_package_caps() {
    use crate::clawd::authority::{
        authority, Audience, AudienceSet, Binding, Decision, Issuance, Issuer,
        Presentation, Principal, Requirement, Subject, Uses,
    };
    let uid = unsafe { libc::geteuid() };
    let pid = std::process::id();
    for app in ["cosmic-store", "launcher", "exec"] {
        let session = format!("package-identity-{app}");
        let (_handle, view) = authority().issue(Issuance {
            issuer: Issuer::AppSessionAuthority,
            principal: Principal::of_process(uid, pid).unwrap(),
            binding: Binding::ProcessTree,
            subject: Subject::session(&session).with_app(Some(app.to_string())),
            audience: AudienceSet::one(Audience::SystemService),
            caps: crate::caps::CapSet::from_caps(vec![Cap::new(Verb::SYS_PACKAGE, Scope::Wild)]),
            lifetime: Duration::from_secs(60), uses: Uses::Unbounded, index_session: true,
        }).unwrap();
        let decision = Decision::for_test(
            view, "system.package.control", Audience::SystemService,
            Presentation {
                uid, pid, start_time_ticks: crate::proc::read_start_time_ticks_pub(pid),
                audience: Audience::SystemService, route: "system.package.control",
                session_id: Some(session),
            },
            None, &Requirement::RouteDerived,
        );
        assert!(authorize_package_session(&decision, Scope::name("fixture"), true).is_err());
        assert!(decision.require_app("pkg").is_err());
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
