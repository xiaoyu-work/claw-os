use super::*;

#[test]
fn only_connect_is_accepted() {
    assert_eq!(
        parse_connect("CONNECT api.example.com:443 HTTP/1.1\r\nHost: x\r\n\r\n"),
        Some("api.example.com:443".to_string())
    );
    // Absolute-form requests would make this an open forward proxy.
    assert_eq!(
        parse_connect("GET http://api.example.com/ HTTP/1.1\r\n\r\n"),
        None
    );
    assert_eq!(parse_connect("CONNECT api.example.com:443\r\n\r\n"), None);
    assert_eq!(parse_connect(""), None);
}

#[test]
fn connect_targets_are_normalised_and_bounded() {
    assert_eq!(
        parse_connect("connect API.Example.COM:443 HTTP/1.1\r\n\r\n"),
        Some("api.example.com:443".to_string())
    );
    let long = format!("CONNECT {}:443 HTTP/1.1\r\n\r\n", "a".repeat(400));
    assert_eq!(parse_connect(&long), None);
    assert_eq!(
        parse_connect("CONNECT user@api.example.com:443 HTTP/1.1\r\n\r\n"),
        None
    );
}

#[test]
fn endpoint_matching_is_exact() {
    let allowed = vec![
        Endpoint::new("api.example.com", 443),
        Endpoint::new("files.example.com", 8443),
    ];
    assert!(match_endpoint("api.example.com:443", &allowed).is_some());
    // Same host, different port.
    assert!(match_endpoint("api.example.com:80", &allowed).is_none());
    // Suffix and prefix games.
    assert!(match_endpoint("evil-api.example.com:443", &allowed).is_none());
    assert!(match_endpoint("api.example.com.evil.test:443", &allowed).is_none());
    assert!(match_endpoint("api.example.com:443:443", &allowed).is_none());
}

#[test]
fn loopback_link_local_and_metadata_addresses_are_blocked() {
    for blocked in [
        "127.0.0.1",
        "127.1.2.3",
        "0.0.0.0",
        "10.1.2.3",
        "172.16.0.1",
        "192.168.1.1",
        "169.254.169.254",
        "100.64.0.1",
        "192.0.0.1",
        "198.18.0.1",
        "224.0.0.1",
        "240.0.0.1",
        "::1",
        "::",
        "fe80::1",
        "fc00::1",
        "fd00::1",
        "ff02::1",
        "::ffff:127.0.0.1",
        "::ffff:169.254.169.254",
    ] {
        let ip: std::net::IpAddr = blocked.parse().expect(blocked);
        assert!(!is_globally_routable(ip), "{blocked} must be blocked");
    }
}

#[test]
fn public_addresses_are_allowed() {
    for allowed in ["93.184.216.34", "1.1.1.1", "2606:4700:4700::1111"] {
        let ip: std::net::IpAddr = allowed.parse().expect(allowed);
        assert!(is_globally_routable(ip), "{allowed} must be allowed");
    }
}

#[test]
fn a_name_resolving_to_a_blocked_address_is_refused() {
    // `localhost` is the smallest reliable rebinding stand-in: it
    // resolves, and every answer is loopback.
    let error = resolve_public(&Endpoint::new("localhost", 443));
    assert!(error.is_err(), "loopback name must not resolve to a tunnel");
}

#[cfg(unix)]
#[test]
fn a_connect_outside_the_grant_is_refused_without_dialling() {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("egress.sock");
    let uid = unsafe { libc::geteuid() };
    let endpoint = EgressEndpoint::start(
        socket.clone(),
        vec![Endpoint::new("api.example.com", 443)],
        uid,
    )
    .expect("start");

    let mut stream = UnixStream::connect(endpoint.socket_path()).expect("connect");
    stream
        .write_all(b"CONNECT 169.254.169.254:80 HTTP/1.1\r\nHost: metadata\r\n\r\n")
        .expect("write");
    let mut response = String::new();
    let _ = stream.read_to_string(&mut response);
    assert!(response.starts_with("HTTP/1.1 403"), "{response}");

    let mut stream = UnixStream::connect(endpoint.socket_path()).expect("connect");
    stream
        .write_all(b"GET http://api.example.com/ HTTP/1.1\r\n\r\n")
        .expect("write");
    let mut response = String::new();
    let _ = stream.read_to_string(&mut response);
    assert!(response.starts_with("HTTP/1.1 405"), "{response}");

    assert!(endpoint.facts()["refused"].as_u64().unwrap_or(0) >= 2);
}

#[cfg(unix)]
#[test]
fn a_wildcard_endpoint_never_reaches_the_listener() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("egress.sock");
    let uid = unsafe { libc::geteuid() };
    let error =
        EgressEndpoint::start(socket, vec![Endpoint::new("*.example.com", 443)], uid).unwrap_err();
    assert!(error.contains("not exact"), "{error}");
}
