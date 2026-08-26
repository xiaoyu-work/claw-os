use super::*;

fn mk_limits(mem: Option<&str>, cpu: Option<u32>, pids: Option<u32>, secs: Option<u32>) -> ResourceLimits {
    ResourceLimits {
        mem_limit: mem.map(|s| s.to_string()),
        cpu_percent: cpu,
        pids_max: pids,
        timeout_secs: secs,
        seccomp_profile: None,
    }
}

/// Helper: assert the windowed pair (left, right) appears
/// consecutively somewhere in `args`. Models the exact systemd-run
/// argv contract: `-p` and `KEY=VAL` MUST be separate elements.
fn contains_window(args: &[String], left: &str, right: &str) -> bool {
    args.windows(2).any(|w| w[0] == left && w[1] == right)
}

/// Anti-test for the original bug: no single argv element may
/// contain both `-p ` and `=` packed together (e.g.
/// "-p MemoryMax=512M"). systemd-run rejects that form.
fn no_packed_p_flag(args: &[String]) {
    for a in args {
        assert!(
            !(a.starts_with("-p ") && a.contains('=')),
            "argv contains packed -p flag: {a:?}"
        );
        // Also catch the related "-p X" with embedded space form.
        assert!(
            !(a.starts_with("-p ") || a == "-p MemorySwapMax=0"),
            "argv contains space-packed -p flag: {a:?}"
        );
    }
}

#[test]
fn systemd_run_args_split_memory_limit() {
    let args = build_systemd_run_args(
        "scope-x",
        &["echo".to_string(), "hi".to_string()],
        false,
        false,
        "/tmp",
        &mk_limits(Some("512M"), None, None, None),
    );
    assert!(contains_window(&args, "-p", "MemoryMax=512M"), "missing MemoryMax pair: {args:?}");
    assert!(contains_window(&args, "-p", "MemorySwapMax=0"), "missing MemorySwapMax pair: {args:?}");
    no_packed_p_flag(&args);
}

#[test]
fn systemd_run_args_split_cpu_pids_timeout() {
    let args = build_systemd_run_args(
        "scope-y",
        &["true".to_string()],
        false,
        false,
        "/var/lib/cos/ws",
        &mk_limits(None, Some(50), Some(100), Some(300)),
    );
    assert!(contains_window(&args, "-p", "CPUQuota=50%"), "missing CPUQuota pair: {args:?}");
    assert!(contains_window(&args, "-p", "TasksMax=100"), "missing TasksMax pair: {args:?}");
    assert!(contains_window(&args, "-p", "RuntimeMaxSec=300"), "missing RuntimeMaxSec pair: {args:?}");
    no_packed_p_flag(&args);
}

#[test]
fn systemd_run_args_split_working_directory_and_readonly() {
    let args = build_systemd_run_args(
        "scope-z",
        &["true".to_string()],
        false,
        true,
        "/sandbox/ws-1",
        &mk_limits(None, None, None, None),
    );
    assert!(
        args.windows(3).any(|window| {
            window[0] == "--ro-bind"
                && window[1] == "/sandbox/ws-1"
                && window[2] == "/workspace"
        }),
        "missing read-only workspace bind: {args:?}"
    );
    no_packed_p_flag(&args);
}

/// With no optional limits the trailing argv still dispatches through
/// bubblewrap and keeps the root filesystem minimal.
#[test]
fn systemd_run_args_no_limits_dispatches_bwrap_command() {
    let args = build_systemd_run_args(
        "scope-empty",
        &["true".to_string()],
        true,
        false,
        "/tmp",
        &mk_limits(None, None, None, None),
    );
    no_packed_p_flag(&args);
    let pos_bwrap = args.iter().position(|s| s == "bwrap").expect("bwrap in argv");
    let dash_dash_count = args.iter().filter(|s| s.as_str() == "--").count();
    assert!(dash_dash_count >= 2, "expected two `--` separators, got {dash_dash_count}: {args:?}");
    assert!(args.iter().any(|s| s == "true"), "missing trailing command: {args:?}");
    assert!(args.iter().any(|s| s == "--share-net"));
    let _ = pos_bwrap;
}

#[test]
fn systemd_run_args_network_off_keeps_private_net_namespace() {
    let args = build_systemd_run_args(
        "scope-net-off",
        &["true".to_string()],
        false,
        false,
        "/tmp",
        &mk_limits(None, None, None, None),
    );
    assert!(!args.iter().any(|s| s == "--share-net"));
    assert!(args.iter().any(|s| s == "--unshare-all"));
}

#[cfg(not(target_os = "linux"))]
#[test]
fn fallback_refuses_plain_subprocess_execution() {
    let result = exec_fallback(&["true".to_string()], "/tmp", &mk_limits(None, None, None, None));
    assert!(result.is_err());
}
