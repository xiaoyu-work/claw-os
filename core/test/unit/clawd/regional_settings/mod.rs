use super::*;
use crate::caps::{Cap, CapSet, Scope, Verb};
use crate::clawd::authority::{
    authority, Audience, AudienceSet, Binding, Issuance, Issuer, Presentation, Principal,
    Requirement, Subject, Uses,
};
use serde_json::json;

pub(super) fn request(mut value: Value) -> Request {
    value["session"] = json!(format!("regional-{}", uuid::Uuid::new_v4().simple()));
    Request::decode(value).unwrap()
}

pub(super) fn decision(request: &Request, caps: Vec<Cap>, app: &str, uses: Uses) -> Decision {
    let uid = unsafe { libc::geteuid() };
    authority().revoke_session(request.session());
    let (_, view) = authority()
        .issue(Issuance {
            issuer: Issuer::AppSessionAuthority,
            principal: Principal::of_process(uid, std::process::id()).unwrap(),
            binding: Binding::ProcessTree,
            subject: Subject::session(request.session()).with_app(Some(app.into())),
            audience: AudienceSet::one(Audience::SystemService),
            caps: CapSet::from_caps(caps),
            lifetime: Duration::from_secs(60),
            uses,
            index_session: true,
        })
        .unwrap();
    Decision::for_test(
        view,
        "system.regional-settings.control",
        Audience::SystemService,
        Presentation {
            uid,
            pid: std::process::id(),
            start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
            audience: Audience::SystemService,
            route: "system.regional-settings.control",
            session_id: Some(request.session().into()),
        },
        None,
        &Requirement::RouteDerived,
    )
}

pub(super) fn client() -> ClientIdentity {
    let mut identity = ClientIdentity::unknown();
    identity.uid = Some(unsafe { libc::geteuid() });
    identity.pid = Some(std::process::id());
    identity.start_time_ticks = crate::proc::read_start_time_ticks_pub(std::process::id());
    identity
}

#[test]
fn wire_and_cli_keep_the_closed_contract_and_do_not_accept_caller_authority() {
    let _lock = crate::test_env::lock_env();
    let _session = crate::test_env::TestEnvVarGuard::set("COS_SESSION", "regional-test");
    let command = crate::clawd::routes::Command::SystemRegionalSettingsControl;
    let value =
        cli_request(&[r#"{"action":"static_hostname","hostname":"fixture-host"}"#.into()]).unwrap();
    assert_eq!(value["session"], "regional-test");
    (command.route().decode)(value.clone()).unwrap();
    for key in [
        "owner_uid",
        "app_id",
        "path",
        "command",
        "capabilities",
        "bus",
    ] {
        let mut bad = value.clone();
        bad[key] = json!("caller-data");
        assert!((command.route().decode)(bad).is_err(), "{key}");
    }
    assert!(cli_request(&[value.to_string()]).is_err());
    assert!(cli_request(&[]).is_err());
    assert!(cli_request(&["x".repeat(8193)]).is_err());
    let body = r#"{"action":"static_hostname","hostname":"fixture-host"}"#;
    let exact_limit = format!("{body}{}", " ".repeat(8192 - body.len()));
    cli_request(&[exact_limit.clone()]).unwrap();
    assert!(cli_request(&[format!("{exact_limit} ")]).is_err());
    assert_eq!(command.route().access, crate::clawd::routes::Access::User);
    assert_eq!(command.route().kind, crate::clawd::routes::Kind::Mutation);
    assert_eq!(command.route().budget.max_in_flight, 8);
    assert_eq!(
        command.route().budget.deadline,
        crate::clawd::routes::Deadline::Uninterruptible
    );
}

#[test]
fn regional_audit_projects_only_bounded_declared_identifiers() {
    let body = json!({
        "session":"regional-test",
        "action":"owner_language",
        "languages":"de_DE:de:en",
        "owner_uid":0,
    });
    let command = "system.regional-settings.control";
    let facts = crate::audit_policy::request_facts(command, &body);
    assert_eq!(facts.command, command);
    assert_eq!(facts.params_omitted, 1);
    assert_eq!(facts.params["languages"], "de_DE:de:en");
    assert!(facts.params.get("owner_uid").is_none());
    let mut long = body;
    long["languages"] = json!(format!("{}en", "en_US.UTF-8:".repeat(32)));
    let facts = crate::audit_policy::request_facts(command, &long);
    assert_eq!(facts.params["languages"], crate::audit_policy::UNLOGGABLE);
}

#[test]
fn authority_requires_authenticated_owner_session_and_exact_live_cap_not_an_app_name() {
    let _lock = crate::test_env::lock_env();
    let temp = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", temp.path().join("data"));
    for body in [
        json!({"action":"system_locale","lang":"en_US.UTF-8","region":"de_DE.UTF-8"}),
        json!({"action":"owner_language","languages":"de_DE:de:en"}),
        json!({"action":"static_hostname","hostname":"fixture-host"}),
    ] {
        let request = request(body);
        let cap = request.required_cap();
        for app in ["cosmic-settings", "independently-authorized-app"] {
            let grant = decision(&request, vec![cap.clone()], app, Uses::Unbounded);
            assert!(preflight(&request, &client(), &grant).is_ok());
            assert!(preflight(&request, &ClientIdentity::unknown(), &grant).is_err());
            let mut wrong_owner = client();
            wrong_owner.uid = wrong_owner.uid.map(|uid| uid + 1);
            assert!(preflight(&request, &wrong_owner, &grant).is_err());
        }
        for denied in [
            vec![],
            vec![Cap::new(cap.verb, Scope::name("other"))],
            vec![Cap::new(Verb::SYS_CONFIG, Scope::name("locale"))],
            vec![Cap::new(Verb::SYS_IDENTITY, Scope::name("manage"))],
        ] {
            let grant = decision(&request, denied, "cosmic-settings", Uses::Unbounded);
            assert!(preflight(&request, &client(), &grant).is_err());
        }
        let other = request::Request::decode(json!({
            "session":"another-session","action":"static_hostname","hostname":"fixture-host"
        }))
        .unwrap();
        let grant = decision(&request, vec![cap], "cosmic-settings", Uses::Unbounded);
        assert!(preflight(&other, &client(), &grant).is_err());
    }
}

#[test]
fn full_router_argv_reaches_regional_input_validation_without_a_broker() {
    let _lock = crate::test_env::lock_env();
    let temporary = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let _socket = crate::test_env::TestEnvVarGuard::set(
        "COS_EXTENSION_BROKER_SOCKET",
        temporary.path().join("unreachable.sock"),
    );
    let error = crate::router::dispatch(&[
        "__regional-settings".into(),
        r#"{"action":"static_hostname","hostname":"fixture-host","session":"forged"}"#.into(),
    ])
    .unwrap_err();
    assert!(error.contains("caller-supplied session"), "{error}");
    assert!(!temporary.path().join("unreachable.sock").exists());
}

#[test]
fn provider_rejects_invalid_input_and_missing_identity_before_connecting_to_any_bus() {
    let _lock = crate::test_env::lock_env();
    let temp = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", temp.path().join("data"));
    let request = request(json!({"action":"static_hostname","hostname":"fixture-host"}));
    let grant = decision(
        &request,
        vec![request.required_cap()],
        "any-app",
        Uses::Unbounded,
    );
    let mut malformed = serde_json::to_value(&request).unwrap();
    malformed["owner_uid"] = json!(0);
    assert!(prepare(malformed, &client(), &grant).is_err());
    let error = prepare(
        serde_json::to_value(&request).unwrap(),
        &ClientIdentity::unknown(),
        &grant,
    )
    .unwrap_err();
    assert_eq!(
        error.kind,
        crate::clawd::protocol::BrokerErrorKind::Unauthorized
    );
}

#[test]
fn unavailable_attempt_is_authorized_once_before_the_broker_releases_its_error() {
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let _lock = crate::test_env::lock_env();
    let temp = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", temp.path().join("data"));
    let request = request(json!({"action":"static_hostname","hostname":"fixture-host"}));
    let grant = decision(
        &request,
        vec![request.required_cap()],
        "independently-authorized-app",
        Uses::Budget(1),
    );
    let error = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(control(
            serde_json::to_value(&request).unwrap(),
            &client(),
            &grant,
        ))
        .unwrap_err();
    assert_eq!(
        error.kind,
        crate::clawd::protocol::BrokerErrorKind::Unavailable
    );
    assert!(crate::clawd::authority::obligation_met(Some(&grant)));
    assert!(grant.require(request.required_cap()).is_err());
}

#[test]
fn failed_discovery_cannot_spend_a_revoked_grant_or_manufacture_success_authority() {
    let _lock = crate::test_env::lock_env();
    let temp = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", temp.path().join("data"));
    let request = request(json!({"action":"static_hostname","hostname":"fixture-host"}));
    let grant = decision(
        &request,
        vec![request.required_cap()],
        "independently-authorized-app",
        Uses::Budget(1),
    );
    complete_attempt(&request, &grant, Ok(json!({"status":"applied"}))).unwrap();
    assert!(!crate::clawd::authority::obligation_met(Some(&grant)));
    authority().revoke_session(request.session());
    let error = complete_attempt(
        &request,
        &grant,
        Err(BrokerError::unavailable("fixture discovery failure")),
    )
    .unwrap_err();
    assert_eq!(error.kind, BrokerErrorKind::Unauthorized);
    assert!(!crate::clawd::authority::obligation_met(Some(&grant)));
}

#[test]
fn regional_broker_client_refuses_a_non_root_socket_before_sending_any_bytes() {
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("non-root peer refusal is exercised by the unprivileged suite");
        return;
    }
    use std::io::Read;
    use std::os::unix::net::UnixListener;
    let directory = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let socket = directory.path().join("untrusted.sock");
    for asynchronous in [false, true] {
        let listener = UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut byte = [0];
            assert_eq!(stream.read(&mut byte).unwrap(), 0);
        });
        let request = crate::clawd::protocol::Request::build(
            crate::clawd::routes::Command::SystemRegionalSettingsControl,
            json!({"session":"untrusted","action":"static_hostname","hostname":"fixture-host"}),
        );
        let error = if asynchronous {
            tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(crate::clawd::client::request(&socket, request))
                .unwrap_err()
        } else {
            crate::clawd::client::request_blocking(&socket, request).unwrap_err()
        };
        assert!(!error.may_have_dispatched());
        server.join().unwrap();
        std::fs::remove_file(&socket).unwrap();
    }
}
