// SPDX-License-Identifier: GPL-3.0-only

#[allow(dead_code)]
#[path = "../../selection-read/tests/support/client.rs"]
mod client;

mod package_probe;

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
    let panel = std::env::args()
        .nth(2)
        .is_some_and(|mode| mode.starts_with("panel") || mode == "metadata-subroot");
    assert_eq!(peer.has_global("zwlr_layer_shell_v1"), panel);
    for denied in [
        "zwp_virtual_keyboard_manager_v1",
        "zwlr_virtual_pointer_manager_v1",
        "zwp_input_method_manager_v2",
        "ext_session_lock_manager_v1",
        "ext_image_copy_capture_manager_v1",
    ] {
        assert!(!peer.has_global(denied), "{denied}");
    }
    peer
}

fn panel() -> Peer {
    let mut peer = peer(Protocol::Core);
    peer.create_layer_surface();
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

fn file_manager(args: &[String]) {
    let vectors: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../../crates/claw-display-login/test/fixtures/file_manager_intents.json"
    )))
    .unwrap();
    assert!(std::env::var("COS_APP_ID").ok().as_deref() == vectors["app_id"].as_str());
    assert_eq!(std::env::var("COS_APP_GUI").as_deref(), Ok("1"));
    assert_eq!(std::env::var("COS_WORKER_SANDBOX").as_deref(), Ok("1"));
    assert_eq!(unsafe { libc::getuid() }, 62050);
    check_inherited_descriptors();
    let call = vectors["calls"]
        .as_array()
        .unwrap()
        .iter()
        .find(|call| {
            let expected: Vec<_> = call["uris"]
                .as_array()
                .unwrap()
                .iter()
                .map(|uri| uri.as_str().unwrap())
                .collect();
            args.iter().map(String::as_str).eq(expected)
        })
        .expect("private FileManager1 argv was not preserved");
    let name = call["name"].as_str().unwrap();
    let encoded: Vec<String> = serde_json::from_str(&std::env::var("COS_ARGS_JSON").unwrap()).unwrap();
    assert!(
        encoded == args,
        "private FileManager1 GUI envelope changed argv"
    );
    let surface = peer(Protocol::Core);
    assert_eq!(surface.offer_count(), 0);
    assert_eq!(surface.mime_count(), 0);
    for denied in [
        "zwlr_layer_shell_v1",
        "zwlr_data_control_manager_v1",
        "ext_data_control_manager_v1",
    ] {
        assert!(!surface.has_global(denied));
    }
    for path in [
        "/run/user/62050/bus",
        "/run/gui-public/file-manager-bus/bus",
    ] {
        let error = UnixStream::connect(path).expect_err("GUI App reached the raw user bus");
        assert_eq!(error.raw_os_error(), Some(libc::EACCES));
    }
    if name == "hold" {
        let group = unsafe { libc::getpgrp() };
        let (mut ready, mut report) = UnixStream::pair().unwrap();
        ready.set_read_timeout(Some(DEADLINE)).unwrap();
        let child = unsafe { libc::fork() };
        assert!(child >= 0);
        if child == 0 {
            drop(ready);
            own_process_group(group);
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
        assert_eq!(unsafe { libc::getpgid(child) }, child);
    }
    std::fs::write(
        private_marker(&format!("file-manager-{name}-ready.json")),
        serde_json::to_vec(&serde_json::json!({
            "passed": true, "literal_argv": true, "raw_bus_denied": true,
            "name": name, "argument_count": args.len(), "retained_descendant": name == "hold",
        }))
        .unwrap(),
    )
    .unwrap();
    let until = std::time::Instant::now() + Duration::from_secs(15);
    while name == "hold" || !private_marker(&format!("file-manager-{name}-release")).exists() {
        assert!(
            name == "hold" || std::time::Instant::now() < until,
            "private GUI release deadline"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    if name == "failure" {
        std::process::exit(7);
    }
    println!("private FileManager1 signed GUI completed");
}

fn home_metadata(subroot: bool) {
    use std::fs::File;
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    let home = PathBuf::from(std::env::var_os("COS_OWNER_HOME").expect("authenticated Home context"));
    let private = PathBuf::from(std::env::var_os("HOME").expect("private App Home"));
    assert_ne!(home, private);
    assert_eq!(private, PathBuf::from(std::env::var_os("COS_DATA_DIR").unwrap()));
    assert!(!std::path::Path::new("/dev/fuse").exists());
    let root = if subroot { home.join("Projects/review") } else { home.clone() };
    let prefix = if subroot { "subroot-metadata" } else { "metadata" };
    let names = std::fs::read_dir(&root).unwrap().map(|entry| entry.unwrap().file_name()).collect::<Vec<_>>();
    assert!(names.iter().any(|name| name == "metadata-visible"));
    for protected in [b".ssh".as_slice(), b"metadata-symlink", b"metadata-hardlink"] {
        assert!(!names.iter().any(|name| name.as_bytes() == protected));
    }
    let visible = root.join("metadata-visible");
    assert_eq!(std::fs::metadata(&visible).unwrap().len(), b"metadata is not content\0\xff".len() as u64);
    assert!(File::open(&visible).is_err());
    assert!(std::fs::write(&visible, b"metadata never grants write").is_err());
    assert!(std::fs::write(root.join("metadata-new-file"), b"no creation authority").is_err());
    assert!(std::fs::metadata(root.join(".ssh/secret")).is_err());
    assert!(std::fs::metadata(root.join(".config/cos")).is_err());
    let directory = File::open(&root).unwrap();
    let mut retained_entries = std::fs::read_dir(&root).unwrap();
    let saved_entry = retained_entries.next().map(|entry| entry.unwrap().file_name());
    assert!(saved_entry.is_some());
    assert!(directory.metadata().unwrap().is_dir());
    assert_eq!(directory.metadata().unwrap().ino(), 1, "inode 1 must be precisely the declared root");
    if subroot {
        for path in [home.join("metadata-visible"), home.join("Projects/metadata-outside"),
            root.join("../metadata-outside")] {
            assert!(std::fs::metadata(path).is_err(), "metadata escaped the declared subroot");
        }
    }
    let mut filesystem: libc::statfs = unsafe { std::mem::zeroed() };
    assert_eq!(unsafe { libc::fstatfs(directory.as_raw_fd(), &mut filesystem) }, 0);
    assert_eq!(filesystem.f_type as u64, 0x6573_5546,
        "the inner App's opened Home descriptor must be a real FUSE filesystem");
    let mut capacity: libc::statvfs = unsafe { std::mem::zeroed() };
    assert_eq!(unsafe { libc::fstatvfs(directory.as_raw_fd(), &mut capacity) }, 0);
    assert!(capacity.f_blocks > 0 && capacity.f_bsize > 0 && capacity.f_frsize > 0);
    std::fs::write(private_marker(&format!("{prefix}-statfs.json")), serde_json::to_vec(&serde_json::json!({
        "blocks":capacity.f_blocks, "bsize":capacity.f_bsize,
        "frsize":capacity.f_frsize, "namelen":capacity.f_namemax,
    })).unwrap()).unwrap();
    let _peer = peer(Protocol::Core);
    for guessed in [0, 1, 4096] {
        assert!(claw_os_sdk::gui::request_file_read(guessed).is_err(),
            "guessing nodes must not discover a parent or another part of Home");
    }
    if !subroot {
        assert!(claw_os_sdk::gui::request_file_read(std::fs::metadata(&visible).unwrap().ino()).is_err(),
            "this metadata-only surface has no declared content-read authority");
    }
    let group = unsafe { libc::getpgrp() };
    let (mut ready, mut report) = UnixStream::pair().unwrap();
    ready.set_read_timeout(Some(DEADLINE)).unwrap();
    let child = unsafe { libc::fork() };
    assert!(child >= 0);
    if child == 0 {
        drop(ready);
        own_process_group(group);
        assert!(directory.metadata().unwrap().is_dir());
        report.write_all(b"ready").unwrap();
        drop(report);
        loop { std::thread::sleep(Duration::from_secs(1)); }
    }
    drop(report);
    let mut confirmed = [0; 5];
    ready.read_exact(&mut confirmed).unwrap();
    assert_eq!(&confirmed, b"ready");
    drop(ready);
    std::fs::write(private_marker(&format!("{prefix}-ready")), b"metadata-only authenticated window").unwrap();
    let _selected = if subroot {
        let ungranted: Vec<usize> = serde_json::from_str(
            &std::env::var("COS_APP_OPTIONAL_NEEDS_UNGRANTED_JSON").unwrap()).unwrap();
        assert_eq!(ungranted, [0, 1, 2, 3], "optional declarations must not become startup grants");
        let node = std::fs::metadata(&visible).unwrap().ino();
        let pending = claw_os_sdk::gui::request_file_read(node).unwrap();
        assert_eq!(pending.state, "pending");
        let status: serde_json::Value = serde_json::to_value(&pending).unwrap();
        assert_eq!(status["target_display"], visible.to_str().unwrap());
        std::fs::write(private_marker("subroot-metadata-pending.json"), serde_json::to_vec(&pending).unwrap()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            let status = claw_os_sdk::gui::file_read_status(&pending.selection_id, node).unwrap();
            if status.state == "selected" { break; }
            assert_eq!(status.state, "pending");
            assert!(File::open(&visible).is_err());
            assert!(std::time::Instant::now() < deadline, "subroot selected-file decision deadline");
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut selected = File::open(&visible).unwrap();
        let mut bytes = Vec::new();
        selected.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"metadata is not content\0\xff");
        assert!(File::open(home.join("metadata-visible")).is_err(),
            "a subroot leaf must never select the same-named owner-Home leaf");
        assert!(std::fs::write(&visible, b"read never grants write").is_err());
        let live_read = claw_os_sdk::gui::file_read_status(&pending.selection_id, node).unwrap();
        assert_eq!(live_read.state, "selected");
        let read_remaining_ms = live_read.expires_in_ms.unwrap();
        std::fs::write(private_marker("subroot-metadata-read-ready"),
            serde_json::to_vec(&serde_json::json!({
                "directory_fd":true, "directory_iterator":true, "saved_entries":saved_entry.is_some(),
                "file_fd":true, "read_remaining_ms":read_remaining_ms,
            })).unwrap()).unwrap();
        Some(selected)
    } else {
        None
    };
    loop {
        std::hint::black_box((&directory, &retained_entries, &saved_entry, &names, &_selected));
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn selected_read() {
    use std::fs::{File, OpenOptions};
    use std::io::{Seek, SeekFrom};
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::{FileExt, MetadataExt};

    const SIZE: usize = 2 * 1024 * 1024 + 29;
    fn byte(index: usize) -> u8 { index.wrapping_mul(43).wrapping_add(17) as u8 }
    let started = std::time::Instant::now();
    let mut trace_file = OpenOptions::new().write(true).create(true).truncate(true)
        .open(private_marker("selected-read-timing.jsonl")).ok();
    let mut stage = |source_step: &'static str| {
        let timing = serde_json::json!({
            "sourceStep": source_step,
            "elapsedMs": started.elapsed().as_millis() as u64,
        });
        eprintln!("private selected-read timing {timing}");
        // Host stderr is buffered until retirement; retain only safe phases.
        if let Some(file) = trace_file.as_mut() {
            let _ = writeln!(file, "{timing}");
        }
    };
    let home = PathBuf::from(std::env::var_os("COS_OWNER_HOME").unwrap());
    let ungranted = std::env::var("COS_APP_OPTIONAL_NEEDS_UNGRANTED_JSON").unwrap();
    let _: Vec<usize> = serde_json::from_str(&ungranted).unwrap();
    let directory = File::open(&home).unwrap();
    let mut filesystem: libc::statfs = unsafe { std::mem::zeroed() };
    assert_eq!(unsafe { libc::fstatfs(directory.as_raw_fd(), &mut filesystem) }, 0);
    assert_eq!(filesystem.f_type as u64, 0x6573_5546);
    let _peer = peer(Protocol::Core);
    let selected = home.join("read-selected").join(std::ffi::OsString::from_vec(b"binary-\xff.bin".to_vec()));
    let sibling = home.join("read-selected/sibling");
    let inherited = home.join("read-delegated/not-selected");
    let unselected = home.join("read-unselected");
    for path in [&selected, &sibling, &inherited, &unselected] {
        assert!(File::open(path).is_err(), "metadata or inherited read authority selected {path:?}");
    }
    let scalar = serde_json::json!({"action":"read","path":inherited}).to_string();
    assert!(claw_os_sdk::cos_call_json_with_stdin("__filesystem", "read",
        ["--request-stdin"], scalar.as_bytes()).is_err(),
        "scalar filesystem providers must not bypass native selection");
    let node = std::fs::metadata(&selected).unwrap().ino();
    let sibling_node = std::fs::metadata(&sibling).unwrap().ino();
    let directory_node = std::fs::metadata(home.join("read-selected")).unwrap().ino();
    assert!(claw_os_sdk::gui::request_file_read(directory_node).is_err());
    let broker = std::env::var_os("COS_BROKER_SOCK");
    unsafe { std::env::set_var("COS_BROKER_SOCK", private_marker("not-a-root-broker")); }
    let pending = claw_os_sdk::gui::request_file_read(node);
    unsafe {
        if let Some(broker) = broker { std::env::set_var("COS_BROKER_SOCK", broker); }
        else { std::env::remove_var("COS_BROKER_SOCK"); }
    }
    let pending = pending.expect("Root selected-file proposal through the authenticated private GUI hop");
    assert_eq!(pending.state, "pending");
    stage("request_pending");
    assert!(File::open(&selected).is_err(), "request success is not a read selection");
    assert!(claw_os_sdk::gui::file_read_status(&pending.selection_id, sibling_node).is_err());
    assert!(claw_os_sdk::gui::file_read_status(&format!("sr-{}", "f".repeat(32)), node).is_err());
    std::fs::write(private_marker("selected-read-ungranted.json"), ungranted).unwrap();
    std::fs::write(private_marker("selected-read-pending.json"), serde_json::to_vec(&pending).unwrap()).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        let status = claw_os_sdk::gui::file_read_status(&pending.selection_id, node).unwrap();
        if status.state == "selected" {
            let remaining = status.expires_in_ms.filter(|remaining| *remaining > 0).unwrap();
            stage("status_selected");
            let again = claw_os_sdk::gui::file_read_status(&pending.selection_id, node).unwrap();
            assert_eq!(again.state, "selected");
            assert!(again.expires_in_ms.is_some_and(|next| next <= remaining),
                "status polling cannot extend the approved read deadline");
            break;
        }
        assert_eq!(status.state, "pending");
        assert!(File::open(&selected).is_err());
        assert!(std::time::Instant::now() < deadline, "protected selected-file decision deadline");
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut file = File::open(&selected).unwrap();
    stage("native_file_opened");
    let mut data = Vec::new();
    file.read_to_end(&mut data).unwrap();
    assert_eq!(data.len(), SIZE);
    assert!(data.iter().enumerate().all(|(index, actual)| *actual == byte(index)));
    stage("native_read_complete");
    file.seek(SeekFrom::Start(19321)).unwrap();
    let mut slice = [0; 257];
    file.read_exact(&mut slice).unwrap();
    assert!(slice.iter().enumerate().all(|(index, actual)| *actual == byte(19321 + index)));
    let mapped = unsafe { libc::mmap(std::ptr::null_mut(), SIZE, libc::PROT_READ, libc::MAP_PRIVATE, file.as_raw_fd(), 0) };
    assert_ne!(mapped, libc::MAP_FAILED);
    assert_eq!(unsafe { std::ptr::read_volatile(mapped.cast::<u8>().add(SIZE - 1)) }, byte(SIZE - 1));
    let executable = unsafe { libc::mmap(std::ptr::null_mut(), SIZE, libc::PROT_READ | libc::PROT_EXEC,
        libc::MAP_PRIVATE, file.as_raw_fd(), 0) };
    assert_eq!(executable, libc::MAP_FAILED, "a read selection is not resource execution authority");
    stage("seek_mmap_complete");
    assert!(OpenOptions::new().read(true).write(true).open(&selected).is_err());
    assert!(std::fs::write(&selected, b"read is not write").is_err());
    assert!(File::open(&sibling).is_err(), "selected directory scope must not grant sibling content");
    let scalar_sibling = serde_json::json!({"action":"read","path":sibling}).to_string();
    assert!(claw_os_sdk::cos_call_json_with_stdin("__filesystem", "read",
        ["--request-stdin"], scalar_sibling.as_bytes()).is_err(),
        "selected status must not reopen an alternative scalar content route");
    assert!(File::open(&inherited).is_err());
    assert!(File::open(&unselected).is_err());
    assert!(std::fs::metadata(home.join(".ssh/secret")).is_err());
    stage("scalar_denial_done");
    let pending_sibling = claw_os_sdk::gui::request_file_read(sibling_node).unwrap();
    let pending_unused = claw_os_sdk::gui::request_file_read(
        std::fs::metadata(&unselected).unwrap().ino()).unwrap();
    assert_eq!(pending_sibling.state, "pending");
    assert_eq!(pending_unused.state, "pending");
    std::fs::write(private_marker("selected-read-unfinished.json"),
        serde_json::to_vec(&[pending_sibling, pending_unused]).unwrap()).unwrap();
    stage("pending_sibling_cleanup_ready");
    let group = unsafe { libc::getpgrp() };
    let (mut ready, mut report) = UnixStream::pair().unwrap();
    ready.set_read_timeout(Some(DEADLINE)).unwrap();
    let child = unsafe { libc::fork() };
    assert!(child >= 0);
    if child == 0 {
        drop(ready);
        own_process_group(group);
        report.write_all(b"ready").unwrap();
        drop(report);
    } else {
        drop(report);
        let mut confirmed = [0; 5];
        ready.read_exact(&mut confirmed).unwrap();
        assert_eq!(&confirmed, b"ready");
        drop(ready);
        std::fs::write(private_marker("selected-read-ready"), b"Root-selected binary content, retained FD and mmap").unwrap();
    }
    loop {
        assert!(File::open(&sibling).is_err());
        assert!(File::open(&unselected).is_err(),
            "an unused owner approval is not selected content");
        let mut retained = [0; 1];
        assert_eq!(file.read_at(&mut retained, 7).unwrap(), 1);
        assert_eq!(retained[0], byte(7));
        assert_eq!(unsafe { std::ptr::read_volatile(mapped.cast::<u8>().add(SIZE - 1)) }, byte(SIZE - 1));
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn selected_write() {
    use std::fs::{File, OpenOptions};
    use std::io::{Seek, SeekFrom};
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::{FileExt, MetadataExt};
    use std::time::Instant;

    let started = Instant::now();
    let mut trace_file = OpenOptions::new().write(true).create(true).truncate(true)
        .open(private_marker("selected-write-timing.jsonl")).ok();
    let mut stage = |source_step: &'static str| {
        let timing = serde_json::json!({
            "sourceStep": source_step,
            "elapsedMs": started.elapsed().as_millis() as u64,
        });
        eprintln!("private selected-write timing {timing}");
        if let Some(file) = trace_file.as_mut() {
            let _ = writeln!(file, "{timing}");
        }
    };

    fn wait_write(pending: &claw_os_sdk::generated::GuiFileWrite) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let current = claw_os_sdk::gui::file_write_status(&pending.selection_id, pending.node).unwrap();
            if current.state == "selected" { return; }
            assert_eq!(current.state, "pending");
            assert!(Instant::now() < deadline, "protected write decision deadline");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    let home = PathBuf::from(std::env::var_os("COS_OWNER_HOME").unwrap());
    let _peer = peer(Protocol::Core);
    let shared = home.join("write-shared");
    let write_only = home.join("write-only");
    let sibling = home.join("write-sibling");
    let non_utf = home.join(std::ffi::OsString::from_vec(b"write-\xff.bin".to_vec()));
    assert!(std::fs::write(&shared, b"metadata is not write").is_err());
    assert!(File::open(&shared).is_err());
    let node = std::fs::metadata(&shared).unwrap().ino();
    let pending_read = claw_os_sdk::gui::request_file_read(node).unwrap();
    assert_eq!(pending_read.state, "pending");
    std::fs::write(private_marker("write-read-pending.json"), serde_json::to_vec(&pending_read).unwrap()).unwrap();
    stage("write_read_request_pending");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let status = claw_os_sdk::gui::file_read_status(&pending_read.selection_id, node).unwrap();
        if status.state == "selected" { break; }
        assert_eq!(status.state, "pending");
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    stage("write_read_selected");
    assert!(OpenOptions::new().write(true).open(&shared).is_err(), "read selection granted write");
    assert!(claw_os_sdk::gui::request_file_write(std::fs::metadata(&non_utf).unwrap().ino()).is_err(),
        "non-UTF8 inverse identity must refuse, never convert lossily");
    let pending_only = claw_os_sdk::gui::request_file_write(std::fs::metadata(&write_only).unwrap().ino()).unwrap();
    let pending_write = claw_os_sdk::gui::request_file_write(node).unwrap();
    assert_eq!(pending_only.state, "pending");
    assert_eq!(pending_write.state, "pending");
    assert!(std::fs::write(&write_only, b"pending is not write").is_err());
    std::fs::write(private_marker("write-pending.json"), serde_json::to_vec(&serde_json::json!({
        "write_only":pending_only, "shared":pending_write,
    })).unwrap()).unwrap();
    stage("write_requests_pending");
    wait_write(&pending_only);
    wait_write(&pending_write);
    stage("write_selected");
    let only_bytes: Vec<u8> = (0_usize..8199).map(|index| index.wrapping_mul(31).wrapping_add(3) as u8).collect();
    let mut only = OpenOptions::new().write(true).truncate(true).open(&write_only).unwrap();
    only.write_all(&only_bytes).unwrap();
    only.sync_all().unwrap();
    assert!(File::open(&write_only).is_err(), "write-only selection exposed owner content");
    assert!(OpenOptions::new().read(true).write(true).open(&write_only).is_err());
    stage("write_only_complete");

    let mut old_read = File::open(&shared).unwrap();
    let mut version_one = Vec::new();
    old_read.read_to_end(&mut version_one).unwrap();
    stage("original_read_complete");
    let size = version_one.len();
    let mapped = unsafe { libc::mmap(std::ptr::null_mut(), size, libc::PROT_READ, libc::MAP_PRIVATE, old_read.as_raw_fd(), 0) };
    assert_ne!(mapped, libc::MAP_FAILED);
    let writer = OpenOptions::new().read(true).write(true).open(&shared).unwrap();
    writer.write_all_at(b"owned\0\xff", 7).unwrap();
    writer.set_len((size + 19) as u64).unwrap();
    writer.sync_all().unwrap();
    stage("direct_write_synced");
    version_one[7..14].copy_from_slice(b"owned\0\xff");
    version_one.resize(size + 19, 0);
    let deadline = Instant::now() + DEADLINE;
    while unsafe { std::ptr::read_volatile(mapped.cast::<u8>().add(7)) } != b'o' {
        assert!(Instant::now() < deadline, "own canonical write did not refresh the mapped read");
        std::thread::sleep(Duration::from_millis(10));
    }
    old_read.seek(SeekFrom::Start(0)).unwrap();
    let mut current = Vec::new();
    old_read.read_to_end(&mut current).unwrap();
    assert_eq!(current, version_one);
    stage("direct_read_revalidated");

    let reservation = claw_os_sdk::gui::reserve_file_save(&pending_write.selection_id, node).unwrap();
    stage("save_reserved");
    assert_eq!(reservation.parent_node, std::fs::metadata(&home).unwrap().ino());
    let scratch = home.join(&reservation.name);
    assert!(std::fs::write(home.join("unreserved-save-sibling"), b"no directory grant").is_err());
    let mut temporary = OpenOptions::new().read(true).write(true).create_new(true).open(&scratch).unwrap();
    let version_two: Vec<u8> = (0_usize..12317).map(|index| index.wrapping_mul(53).wrapping_add(9) as u8).collect();
    temporary.write_all(&version_two).unwrap();
    temporary.sync_all().unwrap();
    assert!(std::fs::rename(&scratch, &sibling).is_err(), "reservation replaced another file");
    assert!(std::fs::remove_file(&sibling).is_err(), "fs.write became fs.delete");
    assert!(std::fs::create_dir(home.join("arbitrary-write-directory")).is_err());
    std::fs::rename(&scratch, &shared).unwrap();
    File::open(&home).unwrap().sync_all().unwrap();
    assert!(!scratch.exists());
    assert_eq!(std::fs::read(&shared).unwrap(), version_two, "own replacement lost the existing read selection");
    stage("save_installed");
    old_read.seek(SeekFrom::Start(0)).unwrap();
    current.clear();
    old_read.read_to_end(&mut current).unwrap();
    assert_eq!(current, version_one, "retained old FD followed the replacement");
    assert_eq!(unsafe { std::ptr::read_volatile(mapped.cast::<u8>().add(7)) }, b'o');
    assert!(writer.write_all_at(b"stale", 0).is_err(), "old writable FD changed a detached inode");
    temporary.seek(SeekFrom::Start(0)).unwrap();
    current.clear();
    temporary.read_to_end(&mut current).unwrap();
    assert_eq!(current, version_two, "retained private scratch lost its authored bytes");
    stage("retained_versions_verified");
    let newest = OpenOptions::new().write(true).open(&shared).unwrap();
    newest.write_all_at(b"Z", 0).unwrap();
    newest.sync_all().unwrap();
    assert_eq!(std::fs::read(&shared).unwrap()[0], b'Z');
    temporary.seek(SeekFrom::Start(0)).unwrap();
    let mut first = [0];
    temporary.read_exact(&mut first).unwrap();
    assert_eq!(first[0], version_two[0], "scratch read exposed later target content");
    stage("final_write_verified");
    assert_eq!(claw_os_sdk::gui::file_read_status(&pending_read.selection_id, node).unwrap().state, "selected");
    assert_eq!(claw_os_sdk::gui::file_write_status(&pending_write.selection_id, node).unwrap().state, "selected");
    stage("write_statuses_verified");
    let unused = claw_os_sdk::gui::request_file_write(std::fs::metadata(&sibling).unwrap().ino()).unwrap();
    let pending = claw_os_sdk::gui::request_file_write(std::fs::metadata(home.join("write-pending-sibling")).unwrap().ino()).unwrap();
    std::fs::write(private_marker("write-cleanup-reviews.json"), serde_json::to_vec(&serde_json::json!({
        "unused":unused,"pending":pending,
    })).unwrap()).unwrap();
    stage("write_cleanup_pending");
    let group = unsafe { libc::getpgrp() };
    let (mut ready, mut report) = UnixStream::pair().unwrap();
    ready.set_read_timeout(Some(DEADLINE)).unwrap();
    let child = unsafe { libc::fork() };
    assert!(child >= 0);
    if child == 0 {
        drop(ready);
        own_process_group(group);
        report.write_all(b"ready").unwrap();
    } else {
        drop(report);
        let mut confirmed = [0; 5];
        ready.read_exact(&mut confirmed).unwrap();
        assert_eq!(&confirmed, b"ready");
        std::fs::write(private_marker("selected-write-ready"), b"canonical write/save and retained read revisions").unwrap();
        stage("write_ready");
    }
    loop {
        assert_eq!(unsafe { std::ptr::read_volatile(mapped.cast::<u8>().add(7)) }, b'o');
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn selected_create() {
    use std::fs::{File, OpenOptions};
    use std::os::unix::fs::MetadataExt;
    use std::time::Instant;

    fn wait_create(pending: &claw_os_sdk::generated::GuiFileCreate) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let status = claw_os_sdk::gui::file_create_status(&pending.selection_id, pending.parent_node).unwrap();
            if status.state == "selected" { return; }
            assert_eq!(status.state, "pending");
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    fn wait_go(marker: &str) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !private_marker(marker).exists() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    let home = PathBuf::from(std::env::var_os("COS_OWNER_HOME").unwrap());
    let parent = home.join("Projects");
    let _peer = peer(Protocol::Core);
    let parent_node = std::fs::metadata(&parent).unwrap().ino();
    assert_eq!(parent_node, 1, "declared metadata subroot must be a valid create parent");
    let leaf = "new ; '$draft'.bin";
    let target = parent.join(leaf);
    assert!(!target.exists());
    assert!(std::fs::write(&target, b"metadata must create nothing").is_err());
    let pending = claw_os_sdk::gui::request_file_create(parent_node, leaf).unwrap();
    assert_eq!(pending.state, "pending");
    assert!(!target.exists());
    std::fs::write(private_marker("create-pending.json"), serde_json::to_vec(&pending).unwrap()).unwrap();
    wait_create(&pending);
    let slot = claw_os_sdk::gui::reserve_file_create(&pending.selection_id, parent_node).unwrap();
    let again = claw_os_sdk::gui::reserve_file_create(&pending.selection_id, parent_node).unwrap();
    assert_eq!(again.reservation_id, slot.reservation_id);
    assert!(again.expires_in_ms <= slot.expires_in_ms);
    assert_eq!(slot.target_name, leaf);
    assert!(!target.exists());
    std::fs::write(private_marker("create-slot.json"), serde_json::to_vec(&slot).unwrap()).unwrap();
    wait_go("create-go");
    let bytes: Vec<u8> = (0_usize..16387).map(|index| index.wrapping_mul(47).wrapping_add(5) as u8).collect();
    let scratch = parent.join(&slot.name);
    let mut temporary = OpenOptions::new().read(true).write(true).create_new(true).open(&scratch).unwrap();
    temporary.write_all(&bytes).unwrap();
    temporary.sync_all().unwrap();
    assert!(std::fs::rename(&scratch, parent.join("unselected-child")).is_err());
    std::fs::rename(&scratch, &target).unwrap();
    File::open(&parent).unwrap().sync_all().unwrap();
    assert_eq!(claw_os_sdk::gui::file_create_status(&pending.selection_id, parent_node).unwrap().state, "created");
    assert_eq!(std::fs::metadata(&target).unwrap().len(), bytes.len() as u64);
    assert!(File::open(&target).is_err(), "creation must not mint a read grant");
    assert!(OpenOptions::new().write(true).open(&target).is_err(), "creation became an overwrite lease");
    assert!(claw_os_sdk::gui::reserve_file_create(&pending.selection_id, parent_node).is_err());
    assert!(std::fs::rename(&scratch, &target).is_err(), "committed creation was replayed");

    let race = claw_os_sdk::gui::request_file_create(parent_node, "collision").unwrap();
    std::fs::write(private_marker("create-race-pending.json"), serde_json::to_vec(&race).unwrap()).unwrap();
    wait_create(&race);
    let race_slot = claw_os_sdk::gui::reserve_file_create(&race.selection_id, parent_node).unwrap();
    let race_path = parent.join(&race_slot.name);
    let mut race_scratch = OpenOptions::new().write(true).create_new(true).open(&race_path).unwrap();
    race_scratch.write_all(b"must not replace foreign bytes\0\xff").unwrap();
    race_scratch.sync_all().unwrap();
    std::fs::write(private_marker("create-race-slot.json"), serde_json::to_vec(&race_slot).unwrap()).unwrap();
    wait_go("create-race-go");
    let error = std::fs::rename(&race_path, parent.join("collision")).unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::EEXIST));
    assert_eq!(claw_os_sdk::gui::file_create_status(&race.selection_id, parent_node).unwrap().state, "failed");
    assert!(File::open(parent.join("collision")).is_err());
    assert!(claw_os_sdk::gui::reserve_file_create(&race.selection_id, parent_node).is_err());
    let pending_cleanup = claw_os_sdk::gui::request_file_create(parent_node, "pending-cleanup").unwrap();
    let unused_cleanup = claw_os_sdk::gui::request_file_create(parent_node, "unused-cleanup").unwrap();
    assert_eq!(pending_cleanup.state, "pending");
    assert_eq!(unused_cleanup.state, "pending");
    std::fs::write(private_marker("create-cleanup-reviews.json"), serde_json::to_vec(
        &serde_json::json!({"pending":pending_cleanup,"unused":unused_cleanup}),
    ).unwrap()).unwrap();
    let group = unsafe { libc::getpgrp() };
    let (mut ready, mut report) = UnixStream::pair().unwrap();
    ready.set_read_timeout(Some(DEADLINE)).unwrap();
    let child = unsafe { libc::fork() };
    assert!(child >= 0);
    if child == 0 {
        drop(ready);
        own_process_group(group);
        report.write_all(b"ready").unwrap();
    } else {
        drop(report);
        let mut confirmed = [0; 5];
        ready.read_exact(&mut confirmed).unwrap();
        assert_eq!(&confirmed, b"ready");
        std::fs::write(private_marker("selected-create-ready"), b"one-shot native create and no-replace denial").unwrap();
    }
    loop {
        assert!(File::open(&target).is_err());
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn native_fs_kernel(view: &str) {
    use std::fs::{File, OpenOptions};
    use std::io::{Seek, SeekFrom};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::os::unix::fs::{FileExt, PermissionsExt};
    use std::path::Path;

    const SIZE: usize = 2 * 1024 * 1024 + 29;
    fn byte(index: usize) -> u8 { index.wrapping_mul(37).wrapping_add(11) as u8 }
    fn tag(value: &[u8; 4]) {
        std::io::stdout().write_all(value).unwrap();
        std::io::stdout().flush().unwrap();
    }
    fn next(expected: u8) {
        let mut token = [0];
        std::io::stdin().read_exact(&mut token).unwrap();
        assert_eq!(token[0], expected);
    }
    fn denied(path: &Path) {
        assert!(File::open(path).is_err(), "unselected/protected content opened: {path:?}");
    }
    fn events(file: &mut File, label: &str) -> Vec<(u32, Vec<u8>)> {
        let mut poll = libc::pollfd { fd: file.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        assert!(unsafe { libc::poll(&mut poll, 1, 5000) } > 0, "native inotify {label} event deadline");
        let mut buffer = [0_u8; 8192];
        let count = file.read(&mut buffer).unwrap();
        let mut at = 0;
        let mut result = Vec::new();
        while at + std::mem::size_of::<libc::inotify_event>() <= count {
            let event = unsafe { std::ptr::read_unaligned(buffer.as_ptr().add(at).cast::<libc::inotify_event>()) };
            at += std::mem::size_of::<libc::inotify_event>();
            let end = at + event.len as usize;
            assert!(end <= count);
            let name = &buffer[at..end];
            result.push((event.mask, name[..name.iter().position(|byte| *byte == 0).unwrap_or(name.len())].to_vec()));
            at = end;
        }
        assert_eq!(at, count);
        result
    }
    fn watcher(path: &Path, mask: u32) -> File {
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        assert!(fd >= 0);
        let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert!(unsafe { libc::inotify_add_watch(fd, path.as_ptr(), mask) } >= 0);
        unsafe { File::from_raw_fd(fd) }
    }
    fn priority(file: &File) {
        let mut poll = libc::pollfd { fd: file.as_raw_fd(), events: libc::POLLPRI, revents: 0 };
        assert!(unsafe { libc::poll(&mut poll, 1, 5000) } > 0, "native backing poll deadline");
        assert_ne!(poll.revents & libc::POLLPRI, 0);
    }
    fn selected_writes(view: &Path) -> File {
        let mut names = Vec::new();
        for _ in 0..2 {
            let mut size = [0; 2];
            std::io::stdin().read_exact(&mut size).unwrap();
            let size = u16::from_le_bytes(size) as usize;
            assert!((1..=512).contains(&size));
            let mut name = vec![0; size];
            std::io::stdin().read_exact(&mut name).unwrap();
            let name = PathBuf::from(std::ffi::OsString::from_vec(name));
            assert!(!name.is_absolute() && name.components().count() == 1);
            names.push(view.join(name));
        }
        let mut directory_watch = watcher(view, libc::IN_CREATE | libc::IN_MOVED_TO | libc::IN_DELETE);
        let mut output = OpenOptions::new().write(true).truncate(true).open(view.join("write-only")).unwrap();
        denied(&view.join("write-only"));
        assert!(OpenOptions::new().read(true).write(true).open(view.join("write-only")).is_err());
        assert!(OpenOptions::new().write(true).open(view.join("read-only")).is_err());
        assert_eq!(std::fs::read(view.join("read-only")).unwrap(), b"selected read-only content\0");
        let mut data: Vec<u8> = (0_usize..2 * 1024 * 1024 + 29)
            .map(|index| index.wrapping_mul(13).wrapping_add(29) as u8).collect();
        output.write_all(&data).unwrap();
        output.seek(SeekFrom::Start(12345)).unwrap();
        output.write_all(b"\0\xffpatch").unwrap();
        data[12345..12352].copy_from_slice(b"\0\xffpatch");
        output.set_len(data.len() as u64).unwrap();
        output.sync_all().unwrap();
        assert!(output.set_len(8 * 1024 * 1024 + 1).is_err());
        assert_eq!(std::fs::metadata(view.join("write-only")).unwrap().len(), data.len() as u64);
        let mut handles = Vec::new();
        loop {
            match File::open(view.join("read-only")) {
                Ok(file) => handles.push(file),
                Err(error) => {
                    assert_eq!(error.raw_os_error(), Some(libc::EMFILE));
                    break;
                }
            }
            assert!(handles.len() < 128);
        }
        let exhausted = OpenOptions::new().write(true).truncate(true)
            .open(view.join("write-only")).unwrap_err();
        assert_eq!(exhausted.raw_os_error(), Some(libc::EMFILE));
        assert_eq!(output.metadata().unwrap().len(), data.len() as u64);
        drop(handles);
        assert!(std::fs::set_permissions(view.join("write-only"),
            std::fs::Permissions::from_mode(0o700)).is_err());
        assert!(std::fs::write(view.join("unselected"), b"must not replace sibling").is_err());
        assert!(std::fs::write(view.join("arbitrary-sibling"), b"no new sibling authority").is_err());
        assert!(std::fs::create_dir(view.join("arbitrary-directory")).is_err());
        assert!(std::fs::remove_file(view.join("write-only")).is_err());
        assert!(std::fs::remove_dir(view.join("empty-dir")).is_err());
        assert!(std::fs::rename(view.join("write-only"), view.join("unselected")).is_err());
        let error = std::process::Command::new(view.join("exec-target")).status().unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::EACCES));
        let mut old = File::open(view.join("atomic")).unwrap();
        let writable = OpenOptions::new().read(true).write(true).open(view.join("atomic")).unwrap();
        let mapping = unsafe { libc::mmap(std::ptr::null_mut(), 4096,
            libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, writable.as_raw_fd(), 0) };
        assert_eq!(mapping, libc::MAP_FAILED, "writable mmap must not bypass canonical mutation callbacks");
        drop(writable);
        let mut temp = OpenOptions::new().read(true).write(true).create_new(true).open(&names[0]).unwrap();
        let mut saved: Vec<u8> = (0_usize..262167).map(|index| index.wrapping_mul(19) as u8).collect();
        temp.write_all(&saved).unwrap();
        temp.sync_all().unwrap();
        assert!(std::fs::rename(&names[0], view.join("unselected")).is_err());
        assert!(std::fs::rename(&names[0], view.join("selected-dir/child")).is_err());
        std::fs::rename(&names[0], view.join("atomic")).unwrap();
        let mut before = Vec::new();
        old.read_to_end(&mut before).unwrap();
        assert_eq!(before, b"atomic-before\0\xfe");
        assert_eq!(std::fs::read(view.join("atomic")).unwrap(), saved);
        temp.seek(SeekFrom::Start(0)).unwrap();
        let mut own = Vec::new();
        temp.read_to_end(&mut own).unwrap();
        assert_eq!(own, saved);
        temp.write_at(b"\0\xfd", 777).unwrap();
        saved[777..779].copy_from_slice(b"\0\xfd");
        temp.sync_all().unwrap();
        assert_eq!(std::fs::read(view.join("atomic")).unwrap(), saved);
        let discarded = OpenOptions::new().write(true).create_new(true).open(&names[1]).unwrap();
        drop(discarded);
        std::fs::remove_file(&names[1]).unwrap();
        File::open(view).unwrap().sync_all().unwrap();
        let seen = events(&mut directory_watch, "atomic rename");
        assert!(seen.iter().any(|(mask, name)| mask & libc::IN_MOVED_TO != 0 && name == b"atomic"));
        assert!(!names[0].exists() && !names[1].exists());
        tag(b"WROK");

        let watched = File::open(view.join("watched")).unwrap();
        assert_eq!(std::fs::read(view.join("watched")).unwrap(), b"watch-before\0");
        let mut backing_watch = watcher(&view.join("watched"), libc::IN_DELETE_SELF | libc::IN_ATTRIB);
        tag(b"WATC");
        priority(&watched);
        next(b'C');
        let mut changed = [0_u8; 4096];
        let count = watched.read_at(&mut changed, 0).unwrap();
        assert_eq!(&changed[..count], b"watch-after\0\xffexternal");
        assert!(std::fs::read_dir(view).unwrap().any(|entry| entry.unwrap().file_name() == "external-entry"));
        denied(&view.join("external-entry"));
        tag(b"CHNG");
        priority(&watched);
        drop(watched);
        let seen = events(&mut backing_watch, "closed selected file deletion");
        assert!(seen.iter().any(|(mask, _)| mask & libc::IN_DELETE_SELF != 0));
        assert!(std::fs::metadata(view.join("watched")).is_err());
        tag(b"GONE");
        output
    }

    assert_ne!(unsafe { libc::getuid() }, 0);
    assert_eq!(unsafe { libc::getuid() }, unsafe { libc::geteuid() });
    assert_eq!(std::env::var("HOME").unwrap(), "/data");
    assert_eq!(std::env::var("XDG_CONFIG_HOME").unwrap(), "/data/config");
    assert_eq!(std::env::var("XDG_DATA_HOME").unwrap(), "/data/share");
    assert_eq!(std::env::var("XDG_CACHE_HOME").unwrap(), "/data/cache");
    assert!(!Path::new("/dev/fuse").exists());
    assert!(!Path::new("/run/display-home").exists());
    check_inherited_descriptors();
    std::fs::create_dir_all("/data/config").unwrap();
    std::fs::write("/data/config/private", b"private App configuration").unwrap();
    assert_eq!(std::fs::read("/data/config/private").unwrap(), b"private App configuration");

    let view = Path::new(view);
    let binary = view.join("binary");
    let other = view.join("unselected");
    let raw = std::ffi::OsString::from_vec(b"binary-\xff".to_vec());
    let names: Vec<_> = std::fs::read_dir(view).unwrap().map(|entry| entry.unwrap().file_name()).collect();
    assert!(names.iter().any(|name| name.as_bytes() == raw.as_bytes()));
    assert!(!names.iter().any(|name| matches!(name.as_bytes(), b".ssh" | b"symlink-alias" | b"hardlink-alias")));
    assert_eq!(std::fs::metadata(&binary).unwrap().len(), SIZE as u64);
    assert!(std::fs::metadata(view.join("selected-dir")).unwrap().is_dir());
    for path in [&binary, &other, &view.join(&raw), &view.join("selected-dir/child")] { denied(path); }
    for name in [".ssh/secret", "symlink-alias", "hardlink-alias", "owner-denied"] {
        denied(&view.join(name));
    }
    tag(b"META");
    next(b'F');

    let mut file = File::open(&binary).unwrap();
    let mapped = unsafe { libc::mmap(std::ptr::null_mut(), SIZE, libc::PROT_READ, libc::MAP_PRIVATE, file.as_raw_fd(), 0) };
    assert_ne!(mapped, libc::MAP_FAILED);
    // Fault through the real view before a read() can fill its page cache.
    let bytes = unsafe { std::slice::from_raw_parts(mapped.cast::<u8>(), SIZE) };
    assert!(bytes.iter().enumerate().all(|(index, actual)| *actual == byte(index)));
    let mut all = Vec::new();
    file.read_to_end(&mut all).unwrap();
    assert_eq!(all, bytes);
    file.seek(SeekFrom::Start(19321)).unwrap();
    let mut slice = [0; 4097];
    file.read_exact(&mut slice).unwrap();
    assert!(slice.iter().enumerate().all(|(index, actual)| *actual == byte(19321 + index)));
    assert_eq!(std::fs::read(view.join(&raw)).unwrap(), b"\0\xffbinary-name\n");
    denied(&other);
    denied(&view.join("selected-dir/child"));
    assert!(OpenOptions::new().write(true).open(&binary).is_err());
    assert!(std::fs::write(view.join("must-not-exist"), b"no write authority").is_err());
    tag(b"FILE");
    next(b'D');

    assert_eq!(std::fs::read(view.join("selected-dir/child")).unwrap(), b"selected directory content\0");
    assert_eq!(std::fs::read(view.join("selected-dir/deeper/child")).unwrap(), b"selected nested content\xff");
    denied(&other);
    denied(&view.join("owner-denied"));
    tag(b"DIRS");
    let mut mode = [0];
    std::io::stdin().read_exact(&mut mode).unwrap();
    assert!(matches!(mode[0], b'H' | b'W'));
    let retained_write = if mode[0] == b'W' { Some(selected_writes(view)) } else { None };
    let mut ready = [-1; 2];
    assert_eq!(unsafe { libc::pipe2(ready.as_mut_ptr(), libc::O_CLOEXEC) }, 0);
    let descendant = unsafe { libc::fork() };
    assert!(descendant >= 0);
    if descendant == 0 {
        unsafe { libc::close(ready[0]); }
        assert!(unsafe { libc::setsid() } >= 0);
        assert_eq!(unsafe { libc::write(ready[1], b"1".as_ptr().cast(), 1) }, 1);
        unsafe { libc::close(ready[1]); }
        loop {
            assert_eq!(unsafe { std::ptr::read_volatile(mapped.cast::<u8>()) }, byte(0));
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    unsafe { libc::close(ready[1]); }
    let mut acknowledged = [0_u8; 1];
    assert_eq!(unsafe { libc::read(ready[0], acknowledged.as_mut_ptr().cast(), 1) }, 1);
    assert_eq!(&acknowledged, b"1");
    unsafe { libc::close(ready[0]); }
    tag(b"HELD");
    if let Some(output) = retained_write.as_ref() {
        next(b'F');
        assert!(output.write_at(b"refused", 0).is_err());
        tag(b"DENY");
        next(b'U');
        tag(b"UNKN");
        assert!(output.write_at(b"\xab", 3).is_err(), "an unrecorded completion must not return write success");
    }
    loop {
        let mut byte_read = [0; 1];
        assert_eq!(file.read_at(&mut byte_read, 7).unwrap(), 1);
        assert_eq!(byte_read[0], byte(7));
        assert_eq!(unsafe { std::ptr::read_volatile(mapped.cast::<u8>().add(SIZE - 1)) }, byte(SIZE - 1));
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|selector| selector == "native-fs-kernel") {
        assert_eq!(args.len(), 2);
        native_fs_kernel(&args[1]);
        return;
    }
    if args
        .first()
        .is_some_and(|selector| selector == "file-manager-surface")
    {
        file_manager(&args[1..]);
        return;
    }
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
        "named" => {
            assert_eq!(std::env::current_exe().unwrap().file_name().unwrap(), "gui-widget");
            assert_eq!(std::env::var("COS_APP_ID").as_deref(), Ok("gui-boundary-test"));
            assert!(private_marker("marker").ends_with("apps/gui-boundary-test/marker"));
            zero();
        }
        "read" => read_only(),
        "write" => write_only(),
        "both" => both(),
        "panel" => {
            let _panel = panel();
            zero();
        }
        "panel-read" => {
            let _panel = panel();
            read_only();
        }
        "panel-write" => {
            let _panel = panel();
            write_only();
        }
        "window" => zero(),
        "panel-revoke" => {
            let _panel = panel();
            revoke();
        }
        "revoke" => revoke(),
        "aliases" => aliases(),
        "metadata" => home_metadata(false),
        "metadata-subroot" => { let _panel = panel(); home_metadata(true); }
        "selected-read" => selected_read(),
        "selected-write" => selected_write(),
        "selected-create" => selected_create(),
        "package" => package_probe::run(),
        _ => panic!("unknown private GUI probe mode"),
    }
    println!(
        "{}",
        serde_json::json!({"private_gui_probe": args[1], "passed": true, "argv": args})
    );
}
