use super::*;
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};

pub(super) struct Proof {
    shell: NativeProcess,
    child: NativeProcess,
    marker: std::path::PathBuf,
    marker_inode: (u64, u64),
}

impl Proof {
    pub(super) fn verify_after_retirement(&self) {
        for process in [&self.shell, &self.child] {
            assert!(claw_display_control::identity::pidfd_exited(process.pidfd.as_fd()).unwrap());
        }
        let marker = std::fs::symlink_metadata(&self.marker).unwrap();
        assert!(marker.is_file() && marker.uid() == OWNER && marker.nlink() == 1);
        assert_eq!((marker.dev(), marker.ino()), self.marker_inode);
        assert_eq!(marker.len(), 0);
        assert_eq!(marker.mode() & 0o077, 0);
    }
}

fn find_child(
    retained: &RetainedInstance,
    parent: &NativeProcess,
    executable: &str,
) -> Option<NativeProcess> {
    let group =
        std::fs::read_link(format!("/proc/self/fd/{}", retained.group.as_raw_fd())).unwrap();
    let expected = std::fs::metadata(executable).unwrap();
    let mut matched = Vec::new();
    for pid in std::fs::read_to_string(group.join("cgroup.procs"))
        .unwrap()
        .lines()
    {
        let pid: i32 = pid.parse().unwrap();
        if pid == parent.identity.pid()
            || !crate::proc::process_descends_from(pid as u32, parent.identity.pid() as u32)
        {
            continue;
        }
        let Ok(running) = std::fs::metadata(format!("/proc/{pid}/exe")) else {
            continue;
        };
        if (running.dev(), running.ino()) != (expected.dev(), expected.ino()) {
            continue;
        }
        let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0_u32) };
        assert!(raw >= 0);
        let pidfd = unsafe { OwnedFd::from_raw_fd(raw as i32) };
        let identity = ProcessIdentity::from_pidfd(pidfd.as_fd()).unwrap();
        identity.assert_current().unwrap();
        assert_eq!((identity.uid(), identity.gid()), (OWNER, OWNER));
        let current = std::fs::metadata(format!("/proc/{pid}/exe")).unwrap();
        assert_eq!(
            (current.dev(), current.ino()),
            (expected.dev(), expected.ino())
        );
        assert!(crate::proc::process_descends_from(
            pid as u32,
            parent.identity.pid() as u32
        ));
        matched.push(NativeProcess {
            identity,
            pidfd,
            inode: (current.dev(), current.ino()),
        });
    }
    assert!(
        matched.len() <= 1,
        "ambiguous real Terminal child executable"
    );
    matched.pop()
}

fn pty_identity(process: &NativeProcess, shell: &NativeProcess) -> serde_json::Value {
    let root = std::path::PathBuf::from(format!("/proc/{}", shell.identity.pid()));
    let terminal = std::path::PathBuf::from(format!("/proc/{}", process.identity.pid()));
    let slave_path = std::fs::read_link(root.join("fd/0")).unwrap();
    let index = slave_path
        .strip_prefix("/dev/pts")
        .unwrap()
        .to_str()
        .unwrap();
    let index: u32 = index.parse().expect("shell stdin must be a real PTY slave");
    let slave = std::fs::metadata(root.join("fd/0")).unwrap();
    assert!(slave.file_type().is_char_device());
    for descriptor in [1, 2] {
        let other = std::fs::metadata(root.join(format!("fd/{descriptor}"))).unwrap();
        assert!(other.file_type().is_char_device());
        assert_eq!(
            (other.dev(), other.ino(), other.rdev()),
            (slave.dev(), slave.ino(), slave.rdev()),
        );
    }
    let mounted_slave = std::fs::metadata(
        terminal
            .join("root")
            .join(slave_path.strip_prefix("/").unwrap()),
    )
    .unwrap();
    assert_eq!(
        (
            mounted_slave.dev(),
            mounted_slave.ino(),
            mounted_slave.rdev()
        ),
        (slave.dev(), slave.ino(), slave.rdev()),
    );
    let private_pts = std::fs::metadata(terminal.join("root/dev/pts")).unwrap();
    assert_eq!(slave.dev(), private_pts.dev());
    assert_ne!(
        private_pts.dev(),
        std::fs::metadata("/dev/pts").unwrap().dev(),
        "Terminal must not use the host devpts instance"
    );
    let native_mounts = std::fs::metadata(terminal.join("ns/mnt")).unwrap();
    let shell_mounts = std::fs::metadata(root.join("ns/mnt")).unwrap();
    assert_eq!(
        (native_mounts.dev(), native_mounts.ino()),
        (shell_mounts.dev(), shell_mounts.ino()),
    );
    let stat =
        claw_display_control::identity::read_bounded(root.join("stat").to_str().unwrap(), 8192)
            .unwrap();
    let fields = stat
        .rsplit_once(')')
        .unwrap()
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    assert!(fields.len() >= 6);
    assert_eq!(fields[1].parse::<i32>().unwrap(), process.identity.pid());
    assert_eq!(fields[2].parse::<i32>().unwrap(), shell.identity.pid());
    assert_eq!(fields[3].parse::<i32>().unwrap(), shell.identity.pid());
    assert_ne!(fields[4].parse::<i64>().unwrap(), 0);
    let master = std::fs::read_dir(terminal.join("fd"))
        .unwrap()
        .any(|entry| {
            let entry = entry.unwrap();
            let Ok(metadata) = std::fs::metadata(entry.path()) else {
                return false;
            };
            if !metadata.file_type().is_char_device()
                || libc::major(metadata.rdev()) != 5
                || libc::minor(metadata.rdev()) != 2
            {
                return false;
            }
            let info = terminal.join("fdinfo").join(entry.file_name());
            claw_display_control::identity::read_bounded(info.to_str().unwrap(), 4096).is_ok_and(
                |info| {
                    info.lines().any(|line| {
                        line.strip_prefix("tty-index:")
                            .and_then(|value| value.trim().parse::<u32>().ok())
                            == Some(index)
                    })
                },
            )
        });
    assert!(master, "real Terminal must retain the matching PTY master");
    serde_json::json!({
        "native_pid": process.identity.pid(), "shell_pid": shell.identity.pid(),
        "shell_start_ticks": shell.identity.start_ticks(), "pty_index": index,
        "slave_device": slave.dev(), "slave_inode": slave.ino(), "slave_rdev": slave.rdev(),
        "separate_shell_session": true, "private_devpts": true, "matching_master": true,
    })
}

pub(super) async fn run(
    control: &OwnedFd,
    pam: &mut Child,
    retained: &RetainedInstance,
    process: &NativeProcess,
    session: &str,
    deadline: Instant,
    before: &serde_json::Value,
) -> Proof {
    use std::io::Write;

    let window = claw_display_control::Epoch(retained.instance.0).label();
    let state = Path::new("/run/display-home/.local/share/cos/apps/cosmic-term");
    let root = std::path::PathBuf::from(format!("/proc/{}/root", process.identity.pid()));
    let environment = std::fs::read(format!("/proc/{}/environ", process.identity.pid())).unwrap();
    let value = |key: &str| {
        environment
            .split(|byte| *byte == 0)
            .find_map(|entry| entry.strip_prefix(format!("{key}=").as_bytes()))
    };
    assert_eq!(
        value("COS_DATA_DIR"),
        Some(state.as_os_str().as_encoded_bytes())
    );
    assert_eq!(value("HOME"), value("COS_DATA_DIR"));
    let data = std::fs::symlink_metadata(state).unwrap();
    assert!(data.is_dir() && data.uid() == OWNER && data.mode() & 0o077 == 0);
    let mounted = std::fs::metadata(root.join(state.strip_prefix("/").unwrap())).unwrap();
    assert_eq!((mounted.dev(), mounted.ino()), (data.dev(), data.ino()));
    assert!(std::fs::metadata(root.join("run/display-home/native-editor-unselected.txt")).is_err());
    assert!(std::fs::metadata(root.join("run/user/62050/bus")).is_err());
    assert!(crate::approvals::list_pending_for_owner(Some(OWNER)).is_empty());
    let passwd = std::fs::metadata(root.join("etc/passwd"));
    eprintln!(
        "private actual Terminal default-shell inputs {}",
        serde_json::json!({
            "user_env_present": value("USER").is_some(), "shell_env_present": value("SHELL").is_some(),
            "passwd_present": passwd.is_ok(), "passwd_errno": passwd.err().and_then(|error| error.raw_os_error()),
            "no_identity_or_pty_resource_injected": true,
        })
    );

    let shell = loop {
        native_process(
            retained,
            process,
            "actual Terminal default PTY/shell startup",
        );
        if let Some(shell) = find_child(retained, process, "/bin/sh") {
            break shell;
        }
        if pam.try_wait().unwrap().is_some() || Instant::now() >= deadline {
            renderer_failure(control, pam, retained, process, session,
                "actual Terminal PTY/default-shell unavailable; no NSS, host devpts or authority fallback").await;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(
        std::fs::read_link(format!("/proc/{}/cwd", shell.identity.pid())).unwrap(),
        state,
    );
    eprintln!(
        "private actual Terminal PTY identity {}",
        pty_identity(process, &shell)
    );
    let marker = state.join(format!("native-pty-{window}"));
    assert_eq!(
        std::fs::symlink_metadata(&marker).unwrap_err().kind(),
        std::io::ErrorKind::NotFound,
    );
    let text = format!("/usr/bin/touch {}\n/bin/sleep 30\n", marker.display());
    assert!(text.len() <= 128);
    let directory = Path::new(OBSERVER).parent().unwrap();
    assert!(!directory.join("keyboard-command.json").exists());
    assert!(!directory.join("keyboard-ack.json").exists());
    let now = claw_display_control::wire::monotonic_ms().unwrap();
    let remaining = deadline
        .saturating_duration_since(Instant::now())
        .as_millis()
        .min(5000) as u64;
    assert!(
        remaining > 0,
        "original Terminal readiness budget was exhausted"
    );
    let request = serde_json::json!({
        "schema": "claw.private-keyboard/v1", "sequence": 1, "instance": window,
        "surface": "primary", "expires_ms": now + remaining, "kind": "text", "value": text,
    });
    let next = directory.join("keyboard-command.next");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_NOFOLLOW)
        .mode(0o600)
        .open(&next)
        .unwrap();
    file.write_all(&serde_json::to_vec(&request).unwrap())
        .unwrap();
    drop(file);
    std::fs::rename(next, directory.join("keyboard-command.json")).unwrap();
    loop {
        native_process(retained, process, "actual Terminal keyboard/PTY effect");
        native_process(retained, &shell, "real Terminal shell lifetime");
        let ack = directory.join("keyboard-ack.json");
        let delivered = match std::fs::symlink_metadata(&ack) {
            Ok(metadata) => {
                assert!(
                    metadata.is_file()
                        && metadata.uid() == OWNER
                        && metadata.mode() & 0o077 == 0
                        && metadata.len() <= 4096
                );
                let ack: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(ack).unwrap()).unwrap();
                ack["schema"] == "claw.private-keyboard/v1"
                    && ack["sequence"] == 1
                    && ack["instance"] == window
                    && ack["surface"] == "primary"
                    && ack["keyboard_events_sent"] == true
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => panic!("read protected keyboard acknowledgement: {error}"),
        };
        if delivered {
            if let (Ok(marker_info), Some(child)) = (
                std::fs::symlink_metadata(&marker),
                find_child(retained, &shell, "/bin/sleep"),
            ) {
                assert!(
                    marker_info.is_file()
                        && marker_info.uid() == OWNER
                        && marker_info.nlink() == 1
                        && marker_info.len() == 0
                        && marker_info.mode() & 0o077 == 0
                );
                if observe().is_some_and(|record| {
                    buffered_window(&record, &window)
                        && record["app_id"] == TERMINAL.desktop_id
                        && record["buffer_commits"].as_u64().unwrap()
                            > before["buffer_commits"].as_u64().unwrap()
                }) {
                    assert!(crate::approvals::list_pending_for_owner(Some(OWNER)).is_empty());
                    eprintln!(
                        "private actual Terminal typed PTY effect {}",
                        serde_json::json!({
                            "window": window, "shell_pid": shell.identity.pid(),
                            "child_pid": child.identity.pid(), "child_start_ticks": child.identity.start_ticks(),
                            "private_marker_device": marker_info.dev(), "private_marker_inode": marker_info.ino(),
                            "keyboard_ack_is_not_outcome": true,
                        })
                    );
                    return Proof {
                        shell,
                        child,
                        marker,
                        marker_inode: (marker_info.dev(), marker_info.ino()),
                    };
                }
            }
        }
        if pam.try_wait().unwrap().is_some() || Instant::now() >= deadline {
            renderer_failure(
                control,
                pam,
                retained,
                process,
                session,
                "actual Terminal keyboard delivery did not produce its private PTY command effect",
            )
            .await;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
