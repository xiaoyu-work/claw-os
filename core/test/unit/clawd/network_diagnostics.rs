use super::*;

fn request(value: Value) -> NetworkDiagnose {
    serde_json::from_value(value).expect("network diagnostic request")
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
