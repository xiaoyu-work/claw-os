#![cfg(all(feature = "provider", target_os = "linux"))]

use claw_os_sdk::applet::{Client, HistoryPermission};
use std::{
    fs,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    time::Duration,
};
use tokio::time::{sleep, timeout};

#[allow(dead_code)]
mod fixture {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/support/fixture.rs"
    ));
}

#[tokio::test]
async fn cancellation_terminates_the_real_provider_and_its_active_policy_child() {
    struct Subreaper(libc::c_int);
    impl Drop for Subreaper {
        fn drop(&mut self) {
            assert_eq!(
                unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, self.0) },
                0
            );
        }
    }
    // This integration binary has one test; adoption cannot affect other suites.
    let mut original = 0;
    assert_eq!(
        unsafe { libc::prctl(libc::PR_GET_CHILD_SUBREAPER, &mut original) },
        0
    );
    assert_eq!(unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1) }, 0);
    let _subreaper = Subreaper(original);
    let fixture = fixture::Fixture::new();
    fs::write(fixture.root.join("hang-policy"), "").unwrap();
    let client =
        Client::with_binary(fixture.provider(env!("CARGO_BIN_EXE_claw-os-applet-provider")))
            .unwrap();
    let call = tokio::spawn(async move { client.require_history(HistoryPermission::Read).await });
    let child = timeout(Duration::from_secs(3), async {
        loop {
            match fs::read_to_string(fixture.root.join("blocked-policy-pid")) {
                Ok(value) => break value.trim().parse::<libc::pid_t>().unwrap(),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    sleep(Duration::from_millis(10)).await;
                }
                Err(error) => panic!("{error}"),
            }
        }
    })
    .await
    .unwrap();
    let status = fs::read_to_string(format!("/proc/{child}/status")).unwrap();
    let provider: libc::pid_t = status
        .lines()
        .find_map(|line| {
            line.strip_prefix("PPid:")
                .map(|value| value.trim().parse().unwrap())
        })
        .unwrap();
    let group: libc::pid_t = fs::read_to_string(fixture.root.join("provider-group-pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(unsafe { libc::getpgid(child) }, group);
    assert_eq!(unsafe { libc::getpgid(provider) }, group);
    let processes: Vec<_> = [child, provider, group]
        .into_iter()
        .map(|pid| {
            let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
            assert!(raw >= 0);
            let descriptor = unsafe { OwnedFd::from_raw_fd(raw as i32) };
            let mut poll = libc::pollfd {
                fd: descriptor.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            assert_eq!(unsafe { libc::poll(&mut poll, 1, 0) }, 0);
            (pid, descriptor)
        })
        .collect();
    call.abort();
    assert!(call.await.unwrap_err().is_cancelled());
    timeout(Duration::from_secs(3), async {
        loop {
            let mut complete = true;
            for (pid, descriptor) in &processes {
                if *pid != group {
                    let mut status = 0;
                    let waited = unsafe { libc::waitpid(*pid, &mut status, libc::WNOHANG) };
                    if waited == *pid {
                        assert!(libc::WIFSIGNALED(status));
                        assert_eq!(libc::WTERMSIG(status), libc::SIGKILL);
                    } else if waited < 0 {
                        assert_eq!(
                            std::io::Error::last_os_error().raw_os_error(),
                            Some(libc::ECHILD)
                        );
                    }
                }
                let mut poll = libc::pollfd {
                    fd: descriptor.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                let ready = unsafe { libc::poll(&mut poll, 1, 0) };
                assert!(ready >= 0);
                complete &= ready > 0
                    && poll.revents & libc::POLLIN != 0
                    && !std::path::Path::new(&format!("/proc/{pid}")).exists();
            }
            if complete {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the real provider and its policy child must terminate and be reaped");
}
