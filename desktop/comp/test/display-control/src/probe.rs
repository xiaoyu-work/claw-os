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
    let panel = std::env::args()
        .nth(2)
        .is_some_and(|mode| mode.starts_with("panel"));
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
    fn events(file: &mut File) -> Vec<(u32, Vec<u8>)> {
        let mut poll = libc::pollfd { fd: file.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        assert!(unsafe { libc::poll(&mut poll, 1, 5000) } > 0, "native inotify event deadline");
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
        let seen = events(&mut directory_watch);
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
        let seen = events(&mut backing_watch);
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
        _ => panic!("unknown private GUI probe mode"),
    }
    println!(
        "{}",
        serde_json::json!({"private_gui_probe": args[1], "passed": true, "argv": args})
    );
}
