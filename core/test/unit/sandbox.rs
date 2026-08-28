use super::*;

fn exec_args(args: &[&str]) -> Vec<String> {
    args.iter().map(|value| (*value).to_string()).collect()
}

#[test]
fn defaults_deny_egress_and_keep_the_workspace_read_only() {
    let request = parse_exec(&exec_args(&["--", "echo", "hi"])).expect("parse");
    assert!(request.read_only);
    assert!(request.endpoints.is_empty());
    assert_eq!(request.command, vec!["echo".to_string(), "hi".to_string()]);
    assert_eq!(request.limits.memory_bytes, DEFAULT_MEMORY_BYTES);
    assert_eq!(request.limits.pids_max, DEFAULT_PIDS_MAX);
    assert_eq!(request.limits.output_bytes, MAX_OUTPUT_BYTES);
    assert_eq!(
        request.limits.runtime,
        std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS)
    );
}

#[test]
fn unrestricted_network_is_refused_rather_than_granted() {
    let error = parse_exec(&exec_args(&["--network", "--", "curl", "https://x"])).unwrap_err();
    assert!(error.contains("--allow-host"), "{error}");
}

#[test]
fn egress_is_only_ever_a_list_of_exact_endpoints() {
    let request = parse_exec(&exec_args(&[
        "--allow-host",
        "api.example.com:443",
        "--",
        "curl",
        "https://api.example.com",
    ]))
    .expect("parse");
    assert_eq!(
        request.endpoints,
        vec![Endpoint::new("api.example.com", 443)]
    );

    for bad in ["*.example.com:443", "api.example.com", "api.example.com:0"] {
        assert!(
            parse_exec(&exec_args(&["--allow-host", bad, "--", "true"])).is_err(),
            "{bad} accepted"
        );
    }
}

#[test]
fn no_network_clears_previously_granted_endpoints() {
    let request = parse_exec(&exec_args(&[
        "--allow-host",
        "api.example.com:443",
        "--no-network",
        "--",
        "true",
    ]))
    .expect("parse");
    assert!(request.endpoints.is_empty());
}

#[test]
fn resource_limits_are_bounded() {
    assert!(parse_exec(&exec_args(&["--cpu", "0", "--", "true"])).is_err());
    assert!(parse_exec(&exec_args(&["--cpu", "101", "--", "true"])).is_err());
    assert!(parse_exec(&exec_args(&["--pids", "0", "--", "true"])).is_err());
    assert!(parse_exec(&exec_args(&["--pids", "4096", "--", "true"])).is_err());
    assert!(parse_exec(&exec_args(&["--timeout", "0", "--", "true"])).is_err());
    assert!(parse_exec(&exec_args(&["--timeout", "7200", "--", "true"])).is_err());

    let request = parse_exec(&exec_args(&[
        "--cpu",
        "50",
        "--pids",
        "32",
        "--timeout",
        "60",
        "--mem",
        "256M",
        "--",
        "true",
    ]))
    .expect("parse");
    assert_eq!(request.limits.cpu_percent, 50);
    assert_eq!(request.limits.pids_max, 32);
    assert_eq!(request.limits.runtime, std::time::Duration::from_secs(60));
    assert_eq!(request.limits.memory_bytes, 256 * 1024 * 1024);
}

#[test]
fn memory_limits_are_parsed_and_range_checked() {
    assert_eq!(parse_memory_limit("512M").expect("512M"), 512 * 1024 * 1024);
    assert_eq!(parse_memory_limit("1G").expect("1G"), 1024 * 1024 * 1024);
    assert!(parse_memory_limit("8M").is_err(), "below the floor");
    assert!(parse_memory_limit("8G").is_err(), "above the ceiling");
    assert!(parse_memory_limit("").is_err());
    assert!(parse_memory_limit("lots").is_err());
}

#[test]
fn a_missing_command_is_refused() {
    assert!(parse_exec(&exec_args(&[])).is_err());
    assert!(parse_exec(&exec_args(&["--"])).is_err());
    assert!(parse_exec(&exec_args(&["--rw"])).is_err());
}

#[test]
fn an_unknown_command_is_not_dispatched() {
    let error = run("create", &[]).unwrap_err();
    assert!(error.contains("unknown sandbox command"), "{error}");
}

#[test]
fn the_workspace_flag_selects_the_only_bound_host_path() {
    let request = parse_exec(&exec_args(&[
        "--workspace",
        "/srv/project",
        "--rw",
        "--",
        "make",
    ]))
    .expect("parse");
    assert_eq!(request.workspace, "/srv/project");
    assert!(!request.read_only);
}
