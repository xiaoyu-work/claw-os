use super::*;

#[test]
fn output_is_kept_up_to_the_ceiling_and_reported_as_truncated() {
    let payload = vec![b'x'; 4096];
    let (kept, truncated) = read_bounded(payload.as_slice(), 1024);
    assert_eq!(kept.len(), 1024);
    assert!(truncated);

    let (kept, truncated) = read_bounded(payload.as_slice(), 8192);
    assert_eq!(kept.len(), 4096);
    assert!(!truncated);
}

#[test]
fn outcome_facts_describe_the_run_not_its_output() {
    let output = WorkerOutput {
        status: exit_status(0),
        stdout: b"secret token".to_vec(),
        stderr: Vec::new(),
        stdout_truncated: true,
        stderr_truncated: false,
        timed_out: true,
    };
    let facts = output.audit_facts().to_string();
    assert!(!facts.contains("secret"), "{facts}");
    assert!(facts.contains("\"stdout_bytes\":12"), "{facts}");
    assert!(facts.contains("\"timed_out\":true"), "{facts}");
}

#[cfg(unix)]
fn exit_status(code: i32) -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    std::process::ExitStatus::from_raw(code << 8)
}

#[cfg(not(unix))]
fn exit_status(_code: i32) -> std::process::ExitStatus {
    std::process::Command::new("cmd")
        .args(["/c", "exit 0"])
        .status()
        .expect("status")
}

#[cfg(unix)]
#[test]
fn a_worker_that_never_exits_is_killed_at_the_deadline() {
    let mut child = std::process::Command::new("/bin/sh")
        .args(["-c", "sleep 30"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn");
    let killed = std::sync::atomic::AtomicBool::new(false);
    let (_, timed_out) = wait_bounded(&mut child, Some(Duration::from_millis(200)), || {
        killed.store(true, std::sync::atomic::Ordering::SeqCst);
    })
    .expect("wait");
    assert!(timed_out);
    assert!(killed.load(std::sync::atomic::Ordering::SeqCst));
}

#[cfg(unix)]
#[test]
fn a_worker_that_exits_in_time_is_not_reported_as_timed_out() {
    let mut child = std::process::Command::new("/bin/sh")
        .args(["-c", "exit 3"])
        .spawn()
        .expect("spawn");
    let (status, timed_out) =
        wait_bounded(&mut child, Some(Duration::from_secs(10)), || {}).expect("wait");
    assert!(!timed_out);
    assert_eq!(status.code(), Some(3));
}
