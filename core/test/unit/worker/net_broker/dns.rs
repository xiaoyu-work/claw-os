use super::*;
use std::collections::BTreeSet;
use std::ffi::CString;
use std::net::UdpSocket;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::time::Instant;

struct PrivateDns {
    _directory: tempfile::TempDir,
    mounts: Vec<CString>,
}

impl PrivateDns {
    fn new() -> Self {
        assert_eq!(
            unsafe { libc::geteuid() },
            0,
            "private DNS fixture needs Root"
        );
        for namespace in ["net", "mnt"] {
            assert_ne!(
                std::fs::read_link(format!("/proc/self/ns/{namespace}")).unwrap(),
                std::fs::read_link(format!("/proc/1/ns/{namespace}")).unwrap(),
                "never replace NSS configuration or listen for DNS outside a private namespace",
            );
        }
        let root = CString::new("/").unwrap();
        assert_eq!(
            unsafe {
                libc::mount(
                    std::ptr::null(),
                    root.as_ptr(),
                    std::ptr::null(),
                    libc::MS_REC | libc::MS_PRIVATE,
                    std::ptr::null(),
                )
            },
            0,
            "make fixture mounts private: {}",
            std::io::Error::last_os_error()
        );
        let mut fixture = Self {
            _directory: tempfile::tempdir().unwrap(),
            mounts: Vec::new(),
        };
        fixture.bind("/etc/nsswitch.conf", b"hosts: files dns\n");
        fixture.bind(
            "/etc/resolv.conf",
            b"nameserver 127.0.0.1\noptions timeout:10 attempts:2\n",
        );
        fixture.bind(
            "/etc/hosts",
            b"127.0.0.1 localhost\n\
              93.184.216.34 public.fixture.test mixed.fixture.test\n\
              2606:4700:4700::1111 public.fixture.test\n\
              127.0.0.1 mixed.fixture.test\n",
        );
        fixture
    }

    fn bind(&mut self, target: &str, bytes: &[u8]) {
        let source = self._directory.path().join(self.mounts.len().to_string());
        std::fs::write(&source, bytes).unwrap();
        let source = CString::new(source.as_os_str().as_bytes()).unwrap();
        let target = std::fs::canonicalize(target).unwrap();
        let target = CString::new(target.as_os_str().as_bytes()).unwrap();
        assert_eq!(
            unsafe {
                libc::mount(
                    source.as_ptr(),
                    target.as_ptr(),
                    std::ptr::null(),
                    libc::MS_BIND,
                    std::ptr::null(),
                )
            },
            0,
            "mount private NSS input: {}",
            std::io::Error::last_os_error()
        );
        self.mounts.push(target);
    }
}

impl Drop for PrivateDns {
    fn drop(&mut self) {
        for target in self.mounts.iter().rev() {
            if unsafe { libc::umount2(target.as_ptr(), libc::MNT_DETACH) } != 0 {
                eprintln!(
                    "private DNS fixture unmount failed: {}",
                    std::io::Error::last_os_error()
                );
            }
        }
    }
}

fn child_processes() -> BTreeSet<u32> {
    let mut children = BTreeSet::new();
    for task in std::fs::read_dir("/proc/self/task").unwrap() {
        let path = task.unwrap().path().join("children");
        let contents = match std::fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => panic!("read fixture child identities: {error}"),
        };
        children.extend(
            contents
                .split_whitespace()
                .map(|pid| pid.parse::<u32>().unwrap()),
        );
    }
    children
}

#[test]
#[ignore = "requires Root in private mount/network namespaces with loopback up"]
fn retiring_endpoint_cancels_a_pending_dns_lookup() {
    let _fixture = PrivateDns::new();
    let server = UdpSocket::bind(("127.0.0.1", 53)).unwrap();
    server
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let before = child_processes();
    let directory = tempfile::tempdir().unwrap();
    let mut endpoint = EgressEndpoint::start(
        directory.path().join("egress.sock"),
        vec![Endpoint::new("stalled.fixture.test", 443)],
        unsafe { libc::geteuid() },
    )
    .unwrap();
    let mut app = UnixStream::connect(endpoint.socket_path()).unwrap();
    app.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    app.write_all(b"CONNECT stalled.fixture.test:443 HTTP/1.1\r\n\r\n")
        .unwrap();
    let mut query = [0_u8; 512];
    let received = server.recv(&mut query).unwrap();
    assert!(received > 12);
    assert!(query[12..received].starts_with(b"\x07stalled\x07fixture\x04test\0"));
    let resolving: Vec<_> = child_processes()
        .difference(&before)
        .map(|pid| (*pid, crate::proc::read_start_time_ticks_pub(*pid).unwrap()))
        .collect();
    assert_eq!(resolving.len(), 1, "one owned NSS resolver must be active");
    assert_eq!(
        std::fs::read_link(format!("/proc/{}/exe", resolving[0].0)).unwrap(),
        std::fs::canonicalize("/usr/bin/getent").unwrap()
    );
    assert!(
        std::fs::read_to_string(format!("/proc/{}/status", resolving[0].0))
            .unwrap()
            .lines()
            .any(|line| line == "NoNewPrivs:\t1")
    );
    let started = Instant::now();
    endpoint
        .retire(started + Duration::from_secs(1))
        .expect("retirement must cancel the pending NSS lookup, not wait for its DNS timeout");
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(endpoint.stats.inflight.load(Ordering::Acquire), 0);
    assert_eq!(app.read(&mut [0_u8]).unwrap(), 0);
    for (pid, start) in resolving {
        assert_ne!(
            crate::proc::read_start_time_ticks_pub(pid),
            Some(start),
            "resolver {pid} must be terminated and reaped before retirement succeeds"
        );
    }
}

#[test]
#[ignore = "requires Root in private mount/network namespaces with loopback up"]
fn resolver_preserves_complete_system_nss_answers_and_public_address_policy() {
    use std::net::ToSocketAddrs;

    let _fixture = PrivateDns::new();
    let endpoint = Endpoint::new("public.fixture.test", 587);
    let mut expected = Vec::new();
    for address in (endpoint.host.as_str(), endpoint.port)
        .to_socket_addrs()
        .unwrap()
    {
        if !expected.contains(&address) {
            expected.push(address);
        }
    }
    assert!(expected.iter().any(SocketAddr::is_ipv4));
    assert!(expected.iter().any(SocketAddr::is_ipv6));
    let actual = resolver::resolve(&endpoint, RESOLVE_DEADLINE, || false).unwrap();
    assert_eq!(actual, expected);

    let (downstream, _app) = UnixStream::pair().unwrap();
    let lifetime = Arc::new(lifetime::Lifetime::default());
    let tunnel = lifetime.track(&downstream).unwrap();
    assert_eq!(resolve_public(&endpoint, &tunnel).unwrap(), expected[0]);
    let error = resolve_public(&Endpoint::new("mixed.fixture.test", 443), &tunnel).unwrap_err();
    assert!(error.contains("blocked address"), "{error}");
    assert!(
        child_processes().is_empty(),
        "completed lookup children must be reaped"
    );
}

#[test]
#[ignore = "requires Root in private mount/network namespaces with loopback up"]
fn a_pending_dns_lookup_keeps_its_deadline_and_reaps_the_resolver() {
    let _fixture = PrivateDns::new();
    let server = UdpSocket::bind(("127.0.0.1", 53)).unwrap();
    server
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let before = child_processes();
    let (completed, result) = std::sync::mpsc::channel();
    let lookup = std::thread::spawn(move || {
        let started = Instant::now();
        let result = resolver::resolve(
            &Endpoint::new("deadline.fixture.test", 443),
            Duration::from_millis(500),
            || false,
        );
        completed.send((started.elapsed(), result)).unwrap();
    });
    let mut query = [0_u8; 512];
    let received = server.recv(&mut query).unwrap();
    assert!(received > 12);
    assert!(query[12..received].starts_with(b"\x08deadline\x07fixture\x04test\0"));
    let (elapsed, outcome) = result.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(outcome.unwrap_err().kind(), std::io::ErrorKind::TimedOut);
    assert!(elapsed >= Duration::from_millis(500));
    assert!(elapsed < Duration::from_secs(2));
    lookup.join().unwrap();
    assert_eq!(child_processes(), before);
}
