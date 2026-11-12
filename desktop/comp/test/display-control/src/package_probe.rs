// SPDX-License-Identifier: GPL-3.0-only
// Private common-Host probe, not the native Store executable.

use super::{DEADLINE, Protocol, own_process_group, peer, private_marker};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

const EFFECT: &str = "claw-package-acceptance-side-effect";
const SUPPORT: &str = "claw-app-acceptance-support";
const UNUSED: &str = "claw-native-package-unused";
const PENDING: &str = "claw-native-package-pending";

fn persist(name: &str, value: &serde_json::Value) {
    let path = private_marker(name);
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&path).unwrap();
    file.write_all(&serde_json::to_vec(value).unwrap()).unwrap();
    file.sync_all().unwrap();
    std::fs::File::open(path.parent().unwrap()).unwrap().sync_all().unwrap();
}

fn intent(mode: &str, id: &str, targets: &[&str]) -> Result<serde_json::Value, claw_os_sdk::BridgeError> {
    let mut arguments = vec![
        "__package_intent".to_string(), "--retained".into(), mode.into(), id.into(), "install".into(),
    ];
    arguments.extend(targets.iter().map(|name| (*name).to_string()));
    claw_os_sdk::cos_call_json_with_binary("/usr/local/bin/cos", "package", "intent", arguments)
}

pub(super) fn run() {
    let _display = peer(Protocol::Core);
    assert_eq!(std::env::var("COS_APP_ID").as_deref(), Ok("gui-boundary-test"));
    let ungranted: Vec<usize> = serde_json::from_str(
        &std::env::var("COS_APP_OPTIONAL_NEEDS_UNGRANTED_JSON").unwrap(),
    ).unwrap();
    assert_eq!(ungranted, vec![0, 1, 2, 3]);
    for package in [SUPPORT, EFFECT] {
        let denied = claw_os_sdk::cos_call_json_with_binary(
            "/usr/local/bin/cos", "policy", "check",
            ["__policy", "check", "sys.package", "--name", package],
        ).unwrap();
        assert_ne!(denied["decision"], "allow", "GUI inherited Critical authority");
    }

    let main = "1".repeat(32);
    let unused = "2".repeat(32);
    let pending = "3".repeat(32);
    // This fsync precedes even review dispatch. Nothing cached below is authority.
    persist("package-intent-local.json", &serde_json::json!({
        "invocation_id": main, "action": "install", "packages": [SUPPORT, EFFECT],
        "unused_invocation": unused, "pending_invocation": pending,
        "session": std::env::var("COS_SESSION").unwrap(),
    }));
    let reviews = intent("review", &main, &[SUPPORT, EFFECT]).unwrap();
    assert_eq!(reviews["status"], "permission_required");
    assert_eq!(reviews["approval_requests"].as_array().unwrap().len(), 2);
    assert!(reviews.get("receipt").is_none());
    assert!(intent("review", &main, &[EFFECT]).is_err(), "same invocation changed its target set");
    let unused_review = intent("review", &unused, &[UNUSED]).unwrap();
    let pending_review = intent("review", &pending, &[PENDING]).unwrap();
    assert_eq!(unused_review["status"], "permission_required");
    assert_eq!(pending_review["status"], "permission_required");
    persist("package-reviews.json", &serde_json::json!({
        "main": reviews, "unused": unused_review, "pending": pending_review,
    }));
    let deadline = Instant::now() + Duration::from_secs(40);
    loop {
        let status = intent("status", &main, &[SUPPORT, EFFECT]).unwrap();
        if status["status"] == "permission_ready" {
            assert!(status.get("receipt").is_none());
            break;
        }
        assert_eq!(status["status"], "permission_required");
        assert!(Instant::now() < deadline, "protected Critical decision did not settle");
        std::thread::sleep(Duration::from_millis(20));
    }
    persist("package-permission-ready.json", &serde_json::json!({
        "ready": true, "accepted": false, "applied": false,
    }));
    while !private_marker("package-submit-go").exists() {
        assert!(Instant::now() < deadline, "Root did not finish its no-effect/ready checks");
        std::thread::sleep(Duration::from_millis(20));
    }

    let group = unsafe { libc::getpgrp() };
    let (mut ready, mut report) = UnixStream::pair().unwrap();
    ready.set_read_timeout(Some(DEADLINE)).unwrap();
    let descendant = unsafe { libc::fork() };
    assert!(descendant >= 0);
    if descendant == 0 {
        drop(ready);
        own_process_group(group);
        report.write_all(b"ready").unwrap();
        drop(report);
        loop { std::thread::sleep(Duration::from_secs(1)); }
    }
    drop(report);
    let mut acknowledgement = [0; 5];
    ready.read_exact(&mut acknowledgement).unwrap();
    assert_eq!(&acknowledgement, b"ready");
    assert_eq!(unsafe { libc::getpgid(descendant) }, descendant);
    drop(ready);
    persist("package-descendant.json", &serde_json::json!({
        "descendant": descendant, "own_group": group,
    }));

    // The short-lived frontend uses the real SDK/CLI and then loses its receipt.
    // The surviving window retains only its fsynced pre-dispatch invocation.
    let frontend = unsafe { libc::fork() };
    assert!(frontend >= 0);
    if frontend == 0 {
        own_process_group(group);
        let accepted = intent("submit", &main, &[SUPPORT, EFFECT]).unwrap();
        assert_eq!(accepted["status"], "accepted");
        assert_eq!(accepted["receipt"]["accepted"], true);
        assert_eq!(accepted["receipt"]["completion"], "query_required");
        assert!(accepted.get("transaction").is_none());
        std::process::exit(0);
    }
    let mut status = 0;
    assert_eq!(unsafe { libc::waitpid(frontend, &mut status, 0) }, frontend);
    assert!(libc::WIFEXITED(status));
    assert_eq!(libc::WEXITSTATUS(status), 0);
    persist("package-receipt-discarded.json", &serde_json::json!({
        "invocation_id": main, "frontend_exited": true, "completion": "query_required",
    }));
    loop { std::thread::sleep(Duration::from_secs(1)); }
}
