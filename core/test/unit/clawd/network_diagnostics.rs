use super::*;
use crate::clawd::authority::{
    authority, Audience, AudienceSet, Binding, Issuance, Issuer, Presentation, Principal,
    Requirement, Subject, Uses,
};
use crate::test_env::TestEnvVarGuard;

fn decision(app: &str, caps: Vec<Cap>) -> Decision {
    let uid = unsafe { libc::geteuid() };
    let pid = std::process::id();
    let session = format!(
        "network-diagnostic-fixture-{}",
        uuid::Uuid::new_v4().simple()
    );
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
    Decision::for_test(
        view,
        "system.network.diagnose",
        Audience::SystemService,
        Presentation {
            uid,
            pid,
            start_time_ticks: crate::proc::read_start_time_ticks_pub(pid),
            audience: Audience::SystemService,
            route: "system.network.diagnose",
            session_id: Some(session),
        },
        None,
        &Requirement::RouteDerived,
    )
}

fn request(value: Value) -> NetworkDiagnose {
    serde_json::from_value(value).expect("network diagnostic request")
}

#[test]
fn origin_neutral_diagnostics_keep_all_live_exact_host_and_port_rights() {
    let _lock = crate::test_env::lock_env();
    let directory = tempfile::Builder::new()
        .prefix(".origin-neutral-network-diagnostic-")
        .tempdir_in(std::env::current_dir().unwrap())
        .unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", directory.path());
    let _caps = TestEnvVarGuard::set("COS_CAPS_DATA_DIR", directory.path());
    let tcp = prepare(&request(json!({
        "session": "fixture",
        "action": "tcp",
        "target": "example.invalid:443",
        "attempts": 1,
        "timeout_ms": 100,
    })))
    .unwrap();
    let other_port = prepare(&request(json!({
        "session": "fixture",
        "action": "tcp",
        "target": "example.invalid:8443",
        "attempts": 1,
        "timeout_ms": 100,
    })))
    .unwrap();
    let combined = prepare(&request(json!({
        "session": "fixture",
        "action": "diagnose",
        "target": "example.invalid:443",
    })))
    .unwrap();
    for app in ["netdiag", "independent-network-diagnostic-client"] {
        let allowed = decision(app, tcp.capabilities());
        for diagnostic in [&other_port, &combined] {
            let error = authorize_session(&allowed, diagnostic).unwrap_err();
            assert_eq!(
                error.kind,
                crate::clawd::protocol::BrokerErrorKind::Unauthorized
            );
            assert!(!crate::clawd::authority::obligation_met(Some(&allowed)));
        }
        let proof = authorize_session(&allowed, &tcp).unwrap();
        assert_eq!(proof.spent(), tcp.capabilities().as_slice());
        assert_eq!(allowed.app_id(), Some(app));
        assert!(authorize_session(&allowed, &tcp).is_err());
        let complete = decision(app, combined.capabilities());
        let proof = authorize_session(&complete, &combined).unwrap();
        assert_eq!(proof.spent(), combined.capabilities().as_slice());

        for caps in [
            vec![],
            vec![Cap::new(Verb::SYS_OBSERVE, Scope::name("network"))],
            vec![Cap::new(
                Verb::NET_RESOLVE,
                Scope::host("example.invalid:443"),
            )],
            vec![Cap::new(
                Verb::NET_PROBE,
                Scope::host("example.invalid:443"),
            )],
            vec![
                Cap::new(Verb::NET_RESOLVE, Scope::host("other.invalid:443")),
                Cap::new(Verb::NET_PROBE, Scope::host("example.invalid:443")),
            ],
            vec![
                Cap::new(Verb::NET_RESOLVE, Scope::host("example.invalid:443")),
                Cap::new(Verb::NET_PROBE, Scope::host("example.invalid:8443")),
            ],
        ] {
            let denied = decision(app, caps);
            assert!(authorize_session(&denied, &tcp).is_err());
            assert!(!crate::clawd::authority::obligation_met(Some(&denied)));
            authority().revoke_session(denied.session_id().unwrap());
        }
    }
    let revoked = decision("independent-network-diagnostic-client", tcp.capabilities());
    authority().revoke_session(revoked.session_id().unwrap());
    assert!(authorize_session(&revoked, &tcp).is_err());
}

#[test]
fn target_parser_canonicalizes_domains_and_ipv6() {
    let domain = parse_target("Example.COM.:8443", true).unwrap();
    assert_eq!(domain.host, "example.com");
    assert_eq!(domain.display, "example.com:8443");
    assert_eq!(domain.port, 8443);

    let ipv6 = parse_target("[2001:0db8::1]:443", true).unwrap();
    assert_eq!(ipv6.host, "2001:db8::1");
    assert_eq!(ipv6.display, "[2001:db8::1]:443");
}

#[test]
fn tcp_targets_require_an_explicit_port() {
    assert!(parse_target("example.com", true)
        .unwrap_err()
        .contains("explicit host:port"));
    assert!(parse_target("2001:db8::1", true)
        .unwrap_err()
        .contains("explicit host:port"));
}

#[test]
fn tcp_preparation_derives_only_exact_target_capabilities() {
    let prepared = prepare(&request(json!({
        "session": "session-1",
        "action": "tcp",
        "target": "Example.COM:443",
        "attempts": 3,
        "timeout_ms": 5000,
    })))
    .unwrap();
    assert_eq!(
        prepared.capabilities(),
        vec![
            Cap::new(Verb::NET_RESOLVE, Scope::host("Example.COM:443")),
            Cap::new(Verb::NET_PROBE, Scope::host("Example.COM:443")),
        ]
    );
}

#[test]
fn probe_budget_is_bounded() {
    assert!(validate_probe_options(5, 4_000).is_ok());
    assert!(validate_probe_options(5, 4_001)
        .unwrap_err()
        .contains("must not exceed"));
    assert!(validate_probe_options(0, 1_000)
        .unwrap_err()
        .contains("attempts"));
}

#[test]
fn action_fields_are_closed() {
    assert!(prepare(&request(json!({
        "session": "session-1",
        "action": "interfaces",
        "target": "example.com",
    })))
    .unwrap_err()
    .contains("does not accept target"));
    assert!(prepare(&request(json!({
        "session": "session-1",
        "action": "dns",
        "target": "example.com",
        "attempts": 1,
    })))
    .unwrap_err()
    .contains("does not accept attempts"));
}

#[test]
fn proc_route_parser_preserves_default_route_details() {
    let routes = parse_routes(
        "Iface Destination Gateway Flags RefCnt Use Metric Mask MTU Window IRTT\n\
         eth0 00000000 0102A8C0 0003 0 0 100 00000000 0 0 0\n",
    )
    .unwrap();
    assert_eq!(
        routes,
        vec![RouteRecord {
            interface: "eth0".to_string(),
            destination: "0.0.0.0".to_string(),
            gateway: "192.168.2.1".to_string(),
            mask: "0.0.0.0".to_string(),
            default: true,
            metric: 100,
        }]
    );
}

#[test]
fn proc_route_parser_rejects_malformed_input() {
    assert!(parse_routes("").unwrap_err().contains("header"));
    assert!(parse_routes(
        "Iface Destination Gateway Flags RefCnt Use Metric Mask MTU Window IRTT\n\
         eth0 00000000\n",
    )
    .unwrap_err()
    .contains("row 2"));
    assert!(parse_routes(
        "Iface Destination Gateway Flags RefCnt Use Metric Mask MTU Window IRTT\n\
         eth0 INVALID 0102A8C0 0003 0 0 100 00000000 0 0 0\n",
    )
    .unwrap_err()
    .contains("IPv4"));
}

#[test]
fn resolved_addresses_preserve_preferred_order_and_fail_on_overflow() {
    let preferred: SocketAddr = "[2001:db8::1]:443".parse().unwrap();
    let fallback: SocketAddr = "192.0.2.10:443".parse().unwrap();
    assert_eq!(
        bounded_unique_addresses([preferred, fallback, preferred]).unwrap(),
        vec![preferred, fallback]
    );

    let too_many = (1..=65)
        .map(|last| SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, last)), DEFAULT_PORT));
    assert!(bounded_unique_addresses(too_many)
        .unwrap_err()
        .contains("more than"));
}
