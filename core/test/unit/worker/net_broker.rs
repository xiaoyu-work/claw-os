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

#[cfg(unix)]
#[test]
fn a_name_resolving_to_a_blocked_address_is_refused() {
    // `localhost` is the smallest reliable rebinding stand-in: it
    // resolves, and every answer is loopback.
    let (downstream, _app) = std::os::unix::net::UnixStream::pair().unwrap();
    let lifetime = Arc::new(lifetime::Lifetime::default());
    let tunnel = lifetime.track(&downstream).unwrap();
    let error = resolve_public(&Endpoint::new("localhost", 443), &tunnel).unwrap_err();
    assert!(error.contains("blocked address"), "{error}");
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

#[cfg(target_os = "linux")]
fn stalled_listener(ip: std::net::Ipv4Addr) -> (std::net::TcpListener, TcpStream, SocketAddr) {
    let listener = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::STREAM,
        Some(socket2::Protocol::TCP),
    )
    .unwrap();
    listener.bind(&SocketAddr::from((ip, 0)).into()).unwrap();
    listener.listen(0).unwrap();
    let address = listener.local_addr().unwrap().as_socket().unwrap();
    let queued = TcpStream::connect_timeout(&address, Duration::from_secs(2)).unwrap();
    (listener.into(), queued, address)
}

#[cfg(target_os = "linux")]
fn wait_for_pending_connect(address: SocketAddr) {
    let IpAddr::V4(ip) = address.ip() else {
        panic!("the private pending-connect fixture uses IPv4");
    };
    let remote = format!(
        "{:08X}:{:04X}",
        u32::from_le_bytes(ip.octets()),
        address.port()
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        let sockets = std::fs::read_to_string("/proc/net/tcp").unwrap();
        if sockets.lines().any(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            fields.get(2) == Some(&remote.as_str()) && fields.get(3) == Some(&"02")
        }) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no real SYN-SENT connection to {address}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn stopping_a_tunnel_interrupts_its_real_pending_connect() {
    use std::os::unix::net::UnixStream;
    use std::time::Instant;

    let (_listener, _queued, address) = stalled_listener(std::net::Ipv4Addr::LOCALHOST);
    let (downstream, mut app) = UnixStream::pair().unwrap();
    app.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let lifetime = Arc::new(lifetime::Lifetime::default());
    let tunnel = lifetime.track(&downstream).unwrap();
    let (completed, result) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let outcome = tunnel.connect(&address, CONNECT_DEADLINE);
        drop(tunnel);
        completed.send(outcome).unwrap();
    });
    wait_for_pending_connect(address);
    let started = Instant::now();
    lifetime.stop().unwrap();
    let error = result
        .recv_timeout(Duration::from_secs(1))
        .unwrap()
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    assert!(started.elapsed() < Duration::from_secs(1));
    worker.join().unwrap();
    assert_eq!(app.read(&mut [0_u8]).unwrap(), 0);
    assert!(lifetime.track(&app).is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn pending_connect_keeps_its_original_timeout_without_retrying() {
    use std::os::unix::net::UnixStream;
    use std::time::Instant;

    let (_listener, _queued, address) = stalled_listener(std::net::Ipv4Addr::LOCALHOST);
    let (downstream, _app) = UnixStream::pair().unwrap();
    let lifetime = Arc::new(lifetime::Lifetime::default());
    let tunnel = lifetime.track(&downstream).unwrap();
    let started = Instant::now();
    let error = tunnel
        .connect(&address, Duration::from_millis(100))
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(started.elapsed() >= Duration::from_millis(100));
    assert!(started.elapsed() < Duration::from_secs(2));
    let next = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let connected = tunnel
        .connect(&next.local_addr().unwrap(), Duration::from_secs(2))
        .unwrap();
    let (_remote, _) = next.accept().unwrap();
    drop(connected);
    drop(tunnel);
    lifetime.stop().unwrap();
}

#[cfg(unix)]
#[test]
fn a_retired_tunnel_never_starts_an_upstream_connection() {
    use std::os::unix::net::UnixStream;

    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let (downstream, _app) = UnixStream::pair().unwrap();
    let lifetime = Arc::new(lifetime::Lifetime::default());
    let tunnel = lifetime.track(&downstream).unwrap();
    lifetime.stop().unwrap();
    let error = tunnel
        .connect(&listener.local_addr().unwrap(), CONNECT_DEADLINE)
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[cfg(unix)]
#[test]
fn a_refused_upstream_remains_a_connection_error() {
    use std::os::unix::net::UnixStream;

    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let (downstream, _app) = UnixStream::pair().unwrap();
    let lifetime = Arc::new(lifetime::Lifetime::default());
    let tunnel = lifetime.track(&downstream).unwrap();
    let error = tunnel.connect(&address, CONNECT_DEADLINE).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::ConnectionRefused);
    let next = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let connected = tunnel
        .connect(&next.local_addr().unwrap(), Duration::from_secs(2))
        .unwrap();
    let (_remote, _) = next.accept().unwrap();
    drop(connected);
    drop(tunnel);
    lifetime.stop().unwrap();
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires Root in a private network namespace with 93.184.216.34/32 on loopback"]
fn retiring_endpoint_cancels_a_pending_tcp_connect() {
    use std::os::unix::net::UnixStream;

    assert_ne!(
        std::fs::read_link("/proc/self/ns/net").unwrap(),
        std::fs::read_link("/proc/1/ns/net").unwrap(),
        "never exercise the public-address fixture on the real network",
    );
    let (_listener, _queued, address) = stalled_listener(std::net::Ipv4Addr::new(93, 184, 216, 34));
    let directory = tempfile::tempdir().unwrap();
    let mut endpoint = EgressEndpoint::start(
        directory.path().join("egress.sock"),
        vec![Endpoint::new(address.ip().to_string(), address.port())],
        unsafe { libc::geteuid() },
    )
    .unwrap();
    let mut app = UnixStream::connect(endpoint.socket_path()).unwrap();
    app.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    write!(app, "CONNECT {address} HTTP/1.1\r\nHost: {address}\r\n\r\n").unwrap();
    wait_for_pending_connect(address);
    let started = std::time::Instant::now();
    endpoint
        .retire(started + Duration::from_secs(1))
        .expect("retirement must not wait for the twenty-second TCP connect deadline");
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(endpoint.stats.inflight.load(Ordering::Acquire), 0);
    assert_eq!(app.read(&mut [0_u8]).unwrap(), 0);
    endpoint.retire(std::time::Instant::now()).unwrap();
}

#[cfg(target_os = "linux")]
mod dns {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/worker/net_broker/dns.rs"
    ));
}
