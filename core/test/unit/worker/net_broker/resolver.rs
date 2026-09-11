use super::*;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

#[test]
fn nss_output_preserves_tcp_order_port_and_both_address_families() {
    let addresses = parse_addresses(
        b"93.184.216.34 STREAM fixture.test\n\
          93.184.216.34 DGRAM\n\
          93.184.216.34 RAW\n\
          2606:4700:4700::1111 STREAM\n\
          93.184.216.34 STREAM\n",
        587,
    )
    .unwrap();
    assert_eq!(
        addresses,
        [
            "93.184.216.34:587".parse::<SocketAddr>().unwrap(),
            "[2606:4700:4700::1111]:587".parse().unwrap(),
        ]
    );
}

#[test]
fn malformed_empty_and_excessive_nss_output_is_not_a_partial_answer() {
    for bytes in [
        &b""[..],
        b"\xff STREAM\n",
        b"93.184.216.34 STREAM",
        b"not-an-address STREAM\n",
        b"93.184.216.34 UNKNOWN\n",
        b"93.184.216.34 STREAM extra fields\n",
        b"93.184.216.34 DGRAM\n",
        b"93.184.216.34 STREAM\ninvalid\n",
    ] {
        assert!(parse_addresses(bytes, 443).is_err(), "{bytes:?}");
    }
    let mut records = String::new();
    for octet in 1..=MAX_ADDRESSES {
        records.push_str(&format!("1.1.1.{octet} STREAM\n"));
    }
    assert_eq!(
        parse_addresses(records.as_bytes(), 443).unwrap().len(),
        MAX_ADDRESSES
    );
    records.push_str("1.1.1.65 STREAM\n");
    assert!(parse_addresses(records.as_bytes(), 443)
        .unwrap_err()
        .to_string()
        .contains("64"));
    let mut output = Vec::new();
    assert!(drain(
        &mut std::io::Cursor::new(vec![0; OUTPUT_BYTES + 1]),
        &mut output,
        OUTPUT_BYTES
    )
    .is_err());
    assert!(output.len() <= OUTPUT_BYTES);
}

#[test]
fn literal_addresses_need_no_resolver_and_stopped_requests_do_not_start_one() {
    let endpoint = Endpoint::new("93.184.216.34", 465);
    assert_eq!(
        resolve(&endpoint, Duration::from_secs(1), || false).unwrap(),
        ["93.184.216.34:465".parse::<SocketAddr>().unwrap()]
    );
    for endpoint in [endpoint, Endpoint::new("never-query.fixture.test", 443)] {
        assert_eq!(
            resolve(&endpoint, Duration::from_secs(1), || true)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        assert_eq!(
            resolve(&endpoint, Duration::ZERO, || false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
}

#[cfg(target_os = "linux")]
fn identity(lookup: &Lookup) -> (u32, u64) {
    let pid = lookup.child.id();
    (pid, crate::proc::read_start_time_ticks_pub(pid).unwrap())
}

#[cfg(target_os = "linux")]
fn assert_reaped((pid, start): (u32, u64)) {
    assert_ne!(crate::proc::read_start_time_ticks_pub(pid), Some(start));
}

#[cfg(target_os = "linux")]
#[test]
fn cancellation_and_timeout_reap_the_actual_lookup_process() {
    let mut command = Command::new("/usr/bin/sleep");
    command.arg("30");
    let mut lookup = Lookup::spawn(&mut command).unwrap();
    let process = identity(&lookup);
    let stopped = Arc::new(AtomicBool::new(false));
    let stop = stopped.clone();
    let notifier = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(40));
        stop.store(true, Ordering::Release);
    });
    let started = Instant::now();
    let error = lookup
        .collect(started + Duration::from_secs(2), &|| {
            stopped.load(Ordering::Acquire)
        })
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert!(started.elapsed() < Duration::from_secs(1));
    notifier.join().unwrap();
    assert!(lookup.reaped);
    assert_reaped(process);

    let mut command = Command::new("/usr/bin/sleep");
    command.arg("30");
    let mut lookup = Lookup::spawn(&mut command).unwrap();
    let process = identity(&lookup);
    let started = Instant::now();
    let error = lookup
        .collect(started + Duration::from_millis(100), &|| false)
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(started.elapsed() >= Duration::from_millis(100));
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(lookup.reaped);
    assert_reaped(process);
}

#[cfg(target_os = "linux")]
#[test]
fn lookup_drop_and_output_overflow_do_not_abandon_a_child() {
    let mut command = Command::new("/usr/bin/sleep");
    command.arg("30");
    let lookup = Lookup::spawn(&mut command).unwrap();
    let process = identity(&lookup);
    drop(lookup);
    assert_reaped(process);

    for (limit, redirect) in [(OUTPUT_BYTES, ""), (STDERR_BYTES, "1>&2")] {
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            &format!("exec /usr/bin/head -c {} /dev/zero {redirect}", limit + 1),
        ]);
        let mut lookup = Lookup::spawn(&mut command).unwrap();
        let process = identity(&lookup);
        let error = lookup
            .collect(Instant::now() + Duration::from_secs(2), &|| false)
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(lookup.reaped);
        assert_reaped(process);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn closed_pipes_and_failed_lookup_do_not_become_empty_success() {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "exec 1>&- 2>&-; exec /usr/bin/sleep 30"]);
    let mut lookup = Lookup::spawn(&mut command).unwrap();
    let process = identity(&lookup);
    let error = lookup
        .collect(Instant::now() + Duration::from_millis(100), &|| false)
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(lookup.reaped);
    assert_reaped(process);

    let mut command = Command::new("/usr/bin/false");
    let mut lookup = Lookup::spawn(&mut command).unwrap();
    let process = identity(&lookup);
    let error = lookup
        .collect(Instant::now() + Duration::from_secs(2), &|| false)
        .unwrap_err();
    assert!(error.to_string().contains("exited with"), "{error}");
    assert!(lookup.reaped);
    assert_reaped(process);
}
