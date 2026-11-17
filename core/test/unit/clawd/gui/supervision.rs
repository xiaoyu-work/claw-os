use super::*;
use std::os::fd::AsRawFd;

struct HeldChild(Child);

impl Drop for HeldChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn held_child() -> HeldChild {
    HeldChild(
        Command::new("/bin/cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    )
}

#[test]
fn natural_child_exit_between_poll_and_provider_health_keeps_its_real_status() {
    let mut child = held_child();
    let pidfd = ProcessIdentity::child(&child.0).unwrap().pidfd().unwrap();
    assert!(child.0.try_wait().unwrap().is_none());
    drop(child.0.stdin.take());
    let mut exited = libc::pollfd {
        fd: pidfd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis();
        assert!(
            remaining > 0,
            "owned child did not exit after its input closed"
        );
        match unsafe { libc::poll(&mut exited, 1, i32::try_from(remaining).unwrap()) } {
            1 => break,
            -1 if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted => {}
            result => panic!("owned child exit poll failed: {result}"),
        }
    }
    let status = health_while_running(
        &mut child.0,
        Err("native filesystem instance process ended or changed".into()),
    )
    .unwrap()
    .expect("the exact owned child exited before the provider result");
    assert!(status.success());
}

#[test]
fn provider_failure_cannot_complete_a_still_running_child() {
    let mut child = held_child();
    let error = health_while_running(
        &mut child.0,
        Err("native filesystem lease is revoked or expired".into()),
    )
    .unwrap_err();
    assert_eq!(error, "native filesystem lease is revoked or expired");
    assert!(child.0.try_wait().unwrap().is_none());
}
