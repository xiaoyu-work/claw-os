use super::*;
use crate::clawd::authority::{
    authority, Audience, AudienceSet, Binding, Issuance, Issuer, Presentation, Principal,
    Requirement, Subject, Uses,
};
use crate::test_env::TestEnvVarGuard;

fn decision(app: &str, caps: Vec<Cap>, include_session: bool) -> Decision {
    let uid = unsafe { libc::geteuid() };
    let pid = std::process::id();
    let session = format!("network-fixture-{}", uuid::Uuid::new_v4().simple());
    let (_, view) = authority()
        .issue(Issuance {
            issuer: Issuer::AppSessionAuthority,
            principal: Principal::of_process(uid, pid).unwrap(),
            binding: Binding::ProcessTree,
            subject: Subject::session(&session).with_app(Some(app.to_string())),
            audience: AudienceSet::one(Audience::SystemService),
            caps: crate::caps::CapSet::from_caps(caps),
            lifetime: Duration::from_secs(60),
            uses: Uses::Budget(1),
            index_session: true,
        })
        .unwrap();
    let row = include_session.then(|| SessionInfo {
        session_id: session.clone(),
        pid,
        command: vec!["network-client-fixture".into()],
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
        app_id: Some(app.to_string()),
        pending_bind: false,
        start_time_ticks: crate::proc::read_start_time_ticks_pub(pid),
        client: crate::session::SessionClient::default(),
    });
    Decision::for_test(
        view,
        "system.network.control",
        Audience::SystemService,
        Presentation {
            uid,
            pid,
            start_time_ticks: crate::proc::read_start_time_ticks_pub(pid),
            audience: Audience::SystemService,
            route: "system.network.control",
            session_id: Some(session),
        },
        row,
        &Requirement::RouteDerived,
    )
}

#[test]
fn origin_neutral_network_keeps_session_and_exact_credential_authority() {
    let _lock = crate::test_env::lock_env();
    let directory = tempfile::Builder::new()
        .prefix(".origin-neutral-network-")
        .tempdir_in(std::env::current_dir().unwrap())
        .unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", directory.path());
    let _caps = TestEnvVarGuard::set("COS_CAPS_DATA_DIR", directory.path());
    let wifi = Cap::new(Verb::NET_MANAGE, Scope::name("wifi"));
    let secret = Cap::new(Verb::SECRET_READ, Scope::name("default/wifi_psk"));
    let requested = vec![wifi.clone(), secret.clone()];
    for app in ["network-manager", "independent-network-client"] {
        let allowed = decision(app, requested.clone(), true);
        assert!(authorize_session(
            &allowed,
            Verb::NET_MANAGE,
            Scope::name("wifi"),
            Some("other/wifi_psk")
        )
        .is_err());
        assert!(authorize_session(
            &allowed,
            Verb::NET_MANAGE,
            Scope::name("vpn"),
            Some("default/wifi_psk")
        )
        .is_err());
        assert!(!crate::clawd::authority::obligation_met(Some(&allowed)));
        let (session, proof) = authorize_session(
            &allowed,
            Verb::NET_MANAGE,
            Scope::name("wifi"),
            Some("default/wifi_psk"),
        )
        .unwrap();
        assert_eq!(proof.spent(), requested.as_slice());
        assert_eq!(session.app_id.as_deref(), Some(app));
        assert_eq!(session.parent.as_deref(), Some("authenticated-parent"));
        assert_eq!(Some(session.session_id.as_str()), allowed.session_id());
        assert_eq!(allowed.app_id(), Some(app));

        for caps in [
            vec![],
            vec![wifi.clone()],
            vec![secret.clone()],
            vec![
                Cap::new(Verb::SYS_OBSERVE, Scope::name("network")),
                secret.clone(),
            ],
            vec![
                Cap::new(Verb::NET_MANAGE, Scope::name("vpn")),
                secret.clone(),
            ],
            vec![
                wifi.clone(),
                Cap::new(Verb::SECRET_READ, Scope::name("default/other")),
            ],
        ] {
            let denied = decision(app, caps, true);
            assert!(authorize_session(
                &denied,
                Verb::NET_MANAGE,
                Scope::name("wifi"),
                Some("default/wifi_psk")
            )
            .is_err());
            assert!(!crate::clawd::authority::obligation_met(Some(&denied)));
            authority().revoke_session(denied.session_id().unwrap());
        }
    }
    let missing = decision("independent-network-client", requested.clone(), false);
    assert!(authorize_session(
        &missing,
        Verb::NET_MANAGE,
        Scope::name("wifi"),
        Some("default/wifi_psk")
    )
    .unwrap_err()
    .contains("no authorized session"));
    let revoked = decision("independent-network-client", requested, true);
    authority().revoke_session(revoked.session_id().unwrap());
    assert!(authorize_session(
        &revoked,
        Verb::NET_MANAGE,
        Scope::name("wifi"),
        Some("default/wifi_psk")
    )
    .is_err());
}

#[test]
fn terse_parser_handles_escaped_colons() {
    assert_eq!(
        split_terse(r"*:Cafe\:Guest:aa\:bb:11:54 Mbit/s:70:WPA2"),
        vec!["*", "Cafe:Guest", "aa:bb", "11", "54 Mbit/s", "70", "WPA2"]
    );
}

#[test]
fn action_validation_is_strict() {
    validate_action("wifi-connect", Some("Cafe"), None, Some("default/wifi_psk")).unwrap();
    validate_action("airplane", None, Some("on"), None).unwrap();
    assert!(validate_action("wifi-toggle", None, Some("maybe"), None).is_err());
    assert!(validate_action("vpn-up", Some("--help"), None, None).is_err());
}

#[test]
fn connection_categories_are_disjoint() {
    assert!(connection_type_matches("802-11-wireless", "wifi"));
    assert!(connection_type_matches("wifi", "wifi"));
    assert!(!connection_type_matches("802-3-ethernet", "wifi"));
    assert!(connection_type_matches("vpn", "vpn"));
    assert!(connection_type_matches("wireguard", "vpn"));
    assert!(!connection_type_matches("802-11-wireless", "vpn"));
}
