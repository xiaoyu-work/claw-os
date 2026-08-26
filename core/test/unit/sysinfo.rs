use super::*;

#[test]
fn info_distinguishes_ubuntu_host_from_claw_agent() {
    let info = info_from_os_release(
        Some(
            "NAME=\"Ubuntu\"\n\
             PRETTY_NAME=\"Ubuntu 26.04 LTS\"\n\
             ID=ubuntu\n\
             ID_LIKE=debian\n\
             VERSION_ID=\"26.04\"\n",
        ),
        "0.1.0",
    );

    assert_eq!(info["name"], "ubuntu");
    assert_eq!(info["version"], "26.04");
    assert_eq!(info["distribution"]["pretty_name"], "Ubuntu 26.04 LTS");
    assert_eq!(info["agent"]["name"], "claw-os-agent");
    assert_eq!(info["claw_os"], false);
    assert_eq!(info["environment"], "claw-agent-on-host");
}

#[test]
fn info_recognizes_full_claw_os_from_os_release() {
    let info = info_from_os_release(
        Some(
            "NAME=\"ClawOS\"\n\
             PRETTY_NAME=\"ClawOS 0.1.0\"\n\
             ID=clawos\n\
             ID_LIKE=debian\n\
             VERSION_ID=\"0.1.0\"\n",
        ),
        "0.1.0",
    );

    assert_eq!(info["name"], "clawos");
    assert_eq!(info["claw_os"], true);
    assert_eq!(info["environment"], "claw-os");
}

#[cfg(target_os = "linux")]
#[test]
fn read_arg_long_form() {
    let args = vec!["--top".to_string(), "25".to_string()];
    assert_eq!(read_arg(&args, "--top"), Some("25"));
}

#[cfg(target_os = "linux")]
#[test]
fn read_arg_equals_form() {
    let args = vec!["--lines=50".to_string()];
    assert_eq!(read_arg(&args, "--lines"), Some("50"));
}

#[cfg(target_os = "linux")]
#[test]
fn read_arg_missing() {
    let args = vec!["--other".to_string(), "v".to_string()];
    assert_eq!(read_arg(&args, "--top"), None);
}

#[cfg(target_os = "linux")]
#[test]
fn has_flag_works() {
    let args = vec!["--failed-only".to_string()];
    assert!(has_flag(&args, "--failed-only"));
    assert!(!has_flag(&args, "--failed"));
}

#[cfg(target_os = "linux")]
#[test]
fn state_name_maps_common_codes() {
    assert_eq!(state_name("R"), "running");
    assert_eq!(state_name("S"), "sleeping");
    assert_eq!(state_name("Z"), "zombie");
    assert_eq!(state_name("?"), "unknown");
}

#[test]
fn desktop_command_returns_object() {
    let v = cmd_desktop().expect("desktop should always succeed");
    assert!(v.is_object());
    // env vars may be unset; we just verify the keys exist as JSON keys.
    for key in [
        "desktop",
        "session_type",
        "wayland_display",
        "x_display",
        "seat",
        "vt",
        "user",
        "runtime_dir",
    ] {
        assert!(v.get(key).is_some(), "missing key {key}");
    }
}

#[test]
fn unknown_command_errors() {
    let r = run("definitely-not-a-real-command", &[]);
    assert!(r.is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn loadavg_smoke() {
    // /proc/loadavg should always exist on Linux test runners.
    let v = cmd_loadavg().expect("loadavg");
    assert!(v["load_1min"].as_f64().is_some());
    assert!(v["cores"].as_u64().unwrap_or(0) >= 1);
}

#[cfg(target_os = "linux")]
#[test]
fn sample_proc_stats_returns_self() {
    let pid = std::process::id();
    let map = sample_proc_stats().expect("sample");
    assert!(map.contains_key(&pid), "current process should be visible");
}

#[cfg(target_os = "linux")]
#[test]
fn clk_tck_is_sensible() {
    let v = clk_tck();
    assert!(v >= 50 && v <= 10_000, "clk_tck out of range: {v}");
}

#[cfg(target_os = "linux")]
#[test]
fn num_cores_is_at_least_one() {
    assert!(num_cores() >= 1);
}

// -----------------------------------------------------------------
// Capability gate
// -----------------------------------------------------------------

/// Regression: the top-level `run()` gate is read-only system
/// observation, NOT kernel-module loading. Before this fix every
/// subcommand asked for [`Verb::SYS_KERNEL`] ("Load kernel modules
/// … reserved for trusted system tools", Risk::Critical), which
/// the clawd-routed agent doesn't have by default. The correct
/// verb per `caps::catalog` is [`Verb::SYS_OBSERVE`] ("Inspect
/// system state … without changing them", Risk::Low) — already
/// granted by [`crate::clawd::system_caps::system_agent_caps`].
/// This test fails closed if anyone re-classifies the gate as a
/// privileged verb again.
#[test]
fn run_clears_gate_with_sys_observe_only() {
    use crate::caps::{Cap, CapSet, Role};
    use crate::proc::{deregister_session, register_session, SessionInfo};

    let _lock = crate::caps::test_env_lock::env_lock();

    // Redirect COS_DATA_DIR so the registry write lands in a
    // tempdir, isolated from any concurrent test and from the
    // real per-user proc/registry.json.
    let tmp = tempfile::tempdir().expect("tempdir");
    let prev_data = env::var_os("COS_DATA_DIR");
    env::set_var("COS_DATA_DIR", tmp.path());

    let prev_sess = env::var_os("COS_SESSION");
    let prev_perms = env::var_os("COS_PERMS_MODE");
    env::remove_var("COS_PERMS_MODE");

    // Build a session that holds SYS_OBSERVE only — mirrors what
    // `clawd::system_caps::system_agent_caps` hands out. PID is
    // our own so the ancestry check in caps::enforcement passes
    // without a real fork.
    let session_id = format!("sysinfo-cap-test-{}", std::process::id());
    let mut caps = CapSet::new();
    caps.insert(Cap::new(Verb::SYS_OBSERVE, Scope::Wild));
    let info = SessionInfo {
        session_id: session_id.clone(),
        pid: std::process::id(),
        command: vec!["sysinfo-cap-test".into()],
        started_at: chrono::Utc::now().to_rfc3339(),
        stdout_path: String::new(),
        stderr_path: String::new(),
        group: None,
        parent: None,
        workdir: None,
        exit_code: None,
        ended_at: None,
        tier: None,
        scope: None,
        priority: None,
        caps: Some(caps),
        transient_caps: None,
        role: Some(Role::Observer.name().to_string()),
        app_id: None,
        pending_bind: false,
        start_time_ticks: None,
    };
    register_session(info).expect("register session");
    env::set_var("COS_SESSION", &session_id);

    // Bogus command name so we hit the dispatch's "unknown
    // command" arm immediately after the cap gate clears. If the
    // gate is still on SYS_KERNEL, this errors with
    // "permission denied" / "verb-not-granted" instead.
    let result = run("__definitely-not-a-real-command__", &[]);

    // Restore env BEFORE asserting so a panic doesn't leak state
    // into other tests that share the lock.
    deregister_session(&session_id);
    match prev_sess {
        Some(v) => env::set_var("COS_SESSION", v),
        None => env::remove_var("COS_SESSION"),
    }
    match prev_perms {
        Some(v) => env::set_var("COS_PERMS_MODE", v),
        None => env::remove_var("COS_PERMS_MODE"),
    }
    match prev_data {
        Some(v) => env::set_var("COS_DATA_DIR", v),
        None => env::remove_var("COS_DATA_DIR"),
    }

    let err = result.expect_err("dispatch should error on bogus command");
    let lower = err.to_lowercase();
    assert!(
        !lower.contains("permission denied") && !lower.contains("not granted"),
        "SYS_OBSERVE should be sufficient to clear the run() cap gate, but got: {err}"
    );
    assert!(
        lower.contains("unknown command"),
        "expected to reach command dispatch (unknown command arm), got: {err}"
    );
}
