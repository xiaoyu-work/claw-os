use super::*;

#[test]
fn host_environment_is_an_allowlist_without_broker_or_credentials() {
    for key in INHERITED_ENV_KEYS {
        assert!(!key.starts_with("CLAWD_"), "{key}");
        let lowered = key.to_ascii_lowercase();
        for secret in ["token", "secret", "password", "credential", "api_key"] {
            assert!(!lowered.contains(secret), "{key}");
        }
    }
    assert!(!INHERITED_ENV_KEYS.contains(&"HOME"));
    assert!(!INHERITED_ENV_KEYS.contains(&"PATH"));
}

#[test]
fn host_resource_limits_are_finite() {
    assert!(HOST_NOFILE_LIMIT <= 1024);
    assert!(HOST_NPROC_LIMIT <= 1024);
    assert!(HOST_ADDRESS_SPACE_LIMIT <= 2 * 1024 * 1024 * 1024);
    assert!(HOST_FILE_SIZE_LIMIT <= 256 * 1024 * 1024);
}

#[test]
fn escaped_descendants_are_killed_outside_the_process_group() {
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg("setsid sh -c 'sleep 60' & echo $!; wait")
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn escape tree");
    let root = child.id();
    let mut line = String::new();
    use std::io::BufRead;
    std::io::BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let escaped = line.trim().parse::<u32>().expect("escaped pid");
    assert!(crate::proc::is_pid_alive(escaped));
    unsafe {
        terminate_host_tree(root, None);
    }
    let _ = child.wait();
    for _ in 0..50 {
        if !crate::proc::is_pid_alive(escaped) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("escaped descendant {escaped} survived host cleanup");
}
