// SPDX-License-Identifier: GPL-3.0-only

#[allow(dead_code)]
#[path = "../../selection-read/tests/support/client.rs"]
mod client;

use client::{Peer, Protocol, read_pipe};
use smithay::wayland::selection::SelectionTarget;
use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    time::Duration,
};

const DEADLINE: Duration = Duration::from_secs(5);
const MIME: &str = "text/plain;charset=utf-8";
const CASES: &[(Protocol, SelectionTarget)] = &[
    (Protocol::Core, SelectionTarget::Clipboard),
    (Protocol::Primary, SelectionTarget::Primary),
    (Protocol::Wlr, SelectionTarget::Clipboard),
    (Protocol::Wlr, SelectionTarget::Primary),
    (Protocol::Ext, SelectionTarget::Clipboard),
    (Protocol::Ext, SelectionTarget::Primary),
];

fn peer(protocol: Protocol) -> Peer {
    let display = std::env::var("WAYLAND_DISPLAY").expect("private instance display");
    assert_eq!(display, "/run/cos/app-wayland.sock");
    let mut peer = Peer::from_stream(UnixStream::connect(display).unwrap(), protocol);
    peer.commit();
    assert!(!peer.has_global("wp_security_context_manager_v1"));
    assert!(!peer.has_global("zwlr_layer_shell_v1"));
    peer
}

fn private_marker(name: &str) -> PathBuf {
    let data = PathBuf::from(std::env::var_os("COS_DATA_DIR").expect("App-owned private data"));
    assert!(data.is_absolute());
    data.join(name)
}

fn check_inherited_descriptors() {
    for entry in std::fs::read_dir("/proc/self/fd").unwrap() {
        let entry = entry.unwrap();
        let number: i32 = entry.file_name().to_str().unwrap().parse().unwrap();
        if number <= 2 {
            continue;
        }
        let target = std::fs::read_link(entry.path()).unwrap();
        assert!(
            target.to_string_lossy().ends_with("/fd"),
            "unexpected inherited GUI descriptor {number}: {target:?}"
        );
    }
}

fn seed(target: SelectionTarget) -> &'static [u8] {
    match target {
        SelectionTarget::Clipboard => b"private-clipboard-fixture",
        SelectionTarget::Primary => b"private-primary-fixture",
    }
}

fn zero() {
    for (protocol, target) in [
        (Protocol::Core, SelectionTarget::Clipboard),
        (Protocol::Primary, SelectionTarget::Primary),
    ] {
        let mut peer = peer(protocol);
        assert!(!peer.has_global("zwlr_data_control_manager_v1"));
        assert!(!peer.has_global("ext_data_control_manager_v1"));
        assert_eq!(peer.offer_count(), 0);
        assert_eq!(peer.mime_count(), 0);
        peer.publish_selection(target, b"must-not-become-selection");
        assert!(!peer.has_offer(target));
    }
    let mut source = peer(Protocol::Core);
    source.start_drag_serial(source.pointer_serial(), b"private-unrelated-dnd");
    let mut recipient = peer(Protocol::Core);
    let offer = recipient.dnd_offer();
    let descriptor = recipient.receive(&offer, MIME);
    source.roundtrip();
    assert_eq!(read_pipe(descriptor), b"private-unrelated-dnd");
    assert!(!recipient.has_offer(SelectionTarget::Clipboard));
}

fn read_only() {
    for &(protocol, target) in CASES {
        let mut peer = peer(protocol);
        let offer = peer.offer(target);
        assert_eq!(read_pipe(peer.receive(&offer, MIME)), seed(target));
        peer.publish_selection(target, b"must-not-replace-readable-selection");
        let offer = peer.offer(target);
        assert_eq!(read_pipe(peer.receive(&offer, MIME)), seed(target));
        peer.clear_selection(target);
        let offer = peer.offer(target);
        assert_eq!(read_pipe(peer.receive(&offer, MIME)), seed(target));
    }
}

fn write_only() {
    for &(protocol, target) in CASES {
        let mut peer = peer(protocol);
        assert_eq!(peer.offer_count(), 0);
        peer.publish_selection_with_mimes(
            target,
            b"private-write-only-source",
            &[MIME, "application/x-claw-private-write-witness"],
        );
        peer.roundtrip();
        peer.clear_selection(target);
        assert_eq!(peer.offer_count(), 0);
        assert_eq!(peer.mime_count(), 0);
        assert_eq!(peer.source_sends(), 1);
    }
}

fn both() {
    for &(protocol, target) in CASES {
        let mut source = peer(protocol);
        source.publish_selection(target, b"private-client-source-payload");
        let mut recipient = peer(protocol);
        let offer = recipient.offer(target);
        let descriptor = recipient.receive(&offer, MIME);
        source.roundtrip();
        assert_eq!(read_pipe(descriptor), b"private-client-source-payload");
        assert_eq!(source.source_sends(), 1);
    }
}

fn own_process_group(parent_group: libc::pid_t) {
    assert_eq!(unsafe { libc::setpgid(0, 0) }, 0);
    assert_eq!(unsafe { libc::getpgrp() }, unsafe { libc::getpid() });
    assert_ne!(unsafe { libc::getpgrp() }, parent_group);
}

fn revoke() {
    let mut peer = peer(Protocol::Wlr);
    let offer = peer.offer(SelectionTarget::Clipboard);
    let retained = peer.receive(&offer, MIME);
    let group = unsafe { libc::getpgrp() };
    let (mut ready, mut report) = UnixStream::pair().unwrap();
    ready.set_read_timeout(Some(DEADLINE)).unwrap();
    let child = unsafe { libc::fork() };
    assert!(child >= 0);
    if child == 0 {
        drop(ready);
        own_process_group(group);
        let _peer = self::peer(Protocol::Core);
        report.write_all(b"ready").unwrap();
        drop(report);
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }
    drop(report);
    let mut confirmation = [0; 5];
    ready.read_exact(&mut confirmation).unwrap();
    assert_eq!(&confirmation, b"ready");
    drop(ready);
    assert_eq!(unsafe { libc::getpgid(child) }, child);
    std::fs::write(private_marker("revoke-ready"), b"ready").unwrap();
    // Different process groups retain the connection and transfer descriptor.
    // Only Root instance/cgroup retirement, not a process-group kill, can finish.
    let _retained = retained;
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn aliases_refused() {
    for path in [
        "/run/gui-public/owner-bus.sock",
        "/run/gui-public/owner-bus-alias.sock",
        "/run/gui-public/ordinary-wayland.sock",
        "/tmp/.X11-unix/X0",
        "/run/user/62050/bus",
    ] {
        let error =
            UnixStream::connect(path).expect_err("raw owner/display transport must be denied");
        assert_eq!(error.raw_os_error(), Some(libc::EACCES), "{path}: {error}");
    }
    assert!(std::env::var_os("DISPLAY").is_none());
    assert!(std::env::var_os("WAYLAND_SOCKET").is_none());
    assert!(std::env::var_os("CLAW_DISPLAY_CONTROL_FD").is_none());
    let _peer = peer(Protocol::Core);
}

fn aliases() {
    std::fs::write(private_marker("aliases-ready"), b"ready").unwrap();
    let ready = PathBuf::from("/run/gui-public/ready");
    let until = std::time::Instant::now() + DEADLINE;
    while !ready.exists() {
        assert!(
            std::time::Instant::now() < until,
            "private alias fixture deadline"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    aliases_refused();
    let group = unsafe { libc::getpgrp() };
    let child = unsafe { libc::fork() };
    assert!(child >= 0);
    if child == 0 {
        own_process_group(group);
        aliases_refused();
        unsafe { libc::_exit(0) }
    }
    let mut status = 0;
    assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
    assert!(libc::WIFEXITED(status));
    assert_eq!(libc::WEXITSTATUS(status), 0);
}

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    assert_eq!(args.len(), 4);
    assert_eq!(args[0], "surface");
    assert_eq!(args[2], "literal ; quote' --flag");
    assert_eq!(args[3], "");
    assert_ne!(unsafe { libc::getuid() }, 0);
    assert_eq!(std::env::var("COS_WORKER_SANDBOX").as_deref(), Ok("1"));
    assert_eq!(std::env::var("LANG").as_deref(), Ok("fr_CA.UTF-8"));
    assert_eq!(std::env::var("LANGUAGE").as_deref(), Ok("fr:en"));
    assert_eq!(std::env::var("LC_TIME").as_deref(), Ok("C.UTF-8"));
    assert!(std::env::var_os("LC_ALL").is_none());
    check_inherited_descriptors();
    match args[1].as_str() {
        "zero" => zero(),
        "read" => read_only(),
        "write" => write_only(),
        "both" => both(),
        "revoke" => revoke(),
        "aliases" => aliases(),
        _ => panic!("unknown private GUI probe mode"),
    }
    println!(
        "{}",
        serde_json::json!({"private_gui_probe": args[1], "passed": true, "argv": args})
    );
}
