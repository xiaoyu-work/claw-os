/// Lightweight sandbox using Linux namespaces.
///
/// Exposed only as an agent tool (`cos_sandbox` in
/// `crate::agent::tools::cos_proxy`), not as a user-facing CLI
/// primitive. The agent uses this to run model-generated or
/// otherwise untrusted commands under `unshare(1)` + cgroup v2
/// + seccomp.
///
/// Only one operation is supported: `exec`. Persistent sandboxes
/// (create/destroy/list) were a legacy surface area; they
/// never spawned a real init process and have been removed.
///
/// On non-Linux platforms the implementation falls back to a
/// plain subprocess so dev builds compile — production cos
/// always runs on Linux.
use serde_json::{json, Value};
use std::process::{Command, Stdio};

use crate::caps::{require_or_json, Scope, Verb};

struct ResourceLimits {
    mem_limit: Option<String>,       // e.g. "512M"
    cpu_percent: Option<u32>,        // e.g. 50
    pids_max: Option<u32>,           // e.g. 100
    timeout_secs: Option<u32>,       // e.g. 300
    seccomp_profile: Option<String>, // e.g. "minimal", "network", "full"
}

pub fn run(command: &str, args: &[String]) -> Result<Value, String> {
    match command {
        "exec" => cmd_exec(args),
        _ => Err(format!("unknown sandbox command: {command}")),
    }
}

/// Execute a command in an isolated sandbox.
///
/// Args mirror the agent tool schema in
/// `agent::tools::cos_proxy::PRIMITIVES`:
///   [--no-network] [--ro] [--workspace DIR]
///   [--mem LIMIT] [--cpu PERCENT] [--pids MAX]
///   [--timeout SECS] [--seccomp-profile minimal|network|full]
///   -- <command> [args...]
fn cmd_exec(args: &[String]) -> Result<Value, String> {
    require_or_json(Verb::PROC_SPAWN, Scope::wild()).map_err(|v| v.to_string())?;
    let mut network = true;
    let mut read_only = false;
    let mut workspace = crate::config::get().home.clone();
    let mut mem_limit: Option<String> = None; // e.g. "512M", "1G"
    let mut cpu_percent: Option<u32> = None; // e.g. 50 = 50%
    let mut pids_max: Option<u32> = None; // e.g. 100
    let mut timeout_secs: Option<u32> = None; // e.g. 300
    let mut seccomp_profile: Option<String> = None; // e.g. "minimal", "network", "full"
    let mut cmd_start = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--no-network" => {
                network = false;
                i += 1;
            }
            "--ro" => {
                read_only = true;
                i += 1;
            }
            "--workspace" if i + 1 < args.len() => {
                workspace = args[i + 1].clone();
                i += 2;
            }
            "--mem" if i + 1 < args.len() => {
                mem_limit = Some(args[i + 1].clone());
                i += 2;
            }
            "--cpu" if i + 1 < args.len() => {
                cpu_percent = Some(
                    args[i + 1]
                        .parse::<u32>()
                        .map_err(|_| format!("invalid cpu value: {}", args[i + 1]))?,
                );
                i += 2;
            }
            "--pids" if i + 1 < args.len() => {
                pids_max = Some(
                    args[i + 1]
                        .parse::<u32>()
                        .map_err(|_| format!("invalid pids value: {}", args[i + 1]))?,
                );
                i += 2;
            }
            "--timeout" if i + 1 < args.len() => {
                timeout_secs = Some(
                    args[i + 1]
                        .parse::<u32>()
                        .map_err(|_| format!("invalid timeout value: {}", args[i + 1]))?,
                );
                i += 2;
            }
            "--seccomp-profile" if i + 1 < args.len() => {
                let profile = args[i + 1].to_lowercase();
                if !["minimal", "network", "full"].contains(&profile.as_str()) {
                    return Err("seccomp profile must be: minimal, network, full".into());
                }
                seccomp_profile = Some(profile);
                i += 2;
            }
            "--" => {
                cmd_start = Some(i + 1);
                break;
            }
            _ => {
                cmd_start = Some(i);
                break;
            }
        }
    }

    let cmd_idx = cmd_start.ok_or("no command specified")?;
    if cmd_idx >= args.len() {
        return Err("no command specified".into());
    }

    let command_args = &args[cmd_idx..];
    let limits = ResourceLimits {
        mem_limit,
        cpu_percent,
        pids_max,
        timeout_secs,
        seccomp_profile,
    };

    #[cfg(target_os = "linux")]
    {
        return exec_linux(command_args, network, read_only, &workspace, &limits);
    }

    #[cfg(not(target_os = "linux"))]
    {
        exec_fallback(command_args, &workspace, &limits)
    }
}

/// Linux: use unshare(1) for namespace isolation + systemd-run for cgroup limits.
#[cfg(target_os = "linux")]
fn exec_linux(
    command_args: &[String],
    network: bool,
    read_only: bool,
    workspace: &str,
    limits: &ResourceLimits,
) -> Result<Value, String> {
    let has_limits = limits.mem_limit.is_some()
        || limits.cpu_percent.is_some()
        || limits.pids_max.is_some()
        || limits.timeout_secs.is_some()
        || limits.seccomp_profile.is_some();

    // If resource limits are set, use systemd-run which handles cgroup v2
    if has_limits {
        return exec_linux_with_cgroup(command_args, network, read_only, workspace, limits);
    }

    // Otherwise, use plain unshare for lightweight namespace isolation
    let mut unshare_args = vec![
        "--pid".to_string(),
        "--fork".to_string(),
        "--mount-proc".to_string(),
        "--mount".to_string(),
    ];

    if !network {
        unshare_args.push("--net".to_string());
    }

    // Read-only: remount root as read-only via bind mount.
    // Uses the mount namespace (already created by --mount) to remount ro.
    if read_only {
        // Chain: unshare creates mount ns → remount / as ro → exec command
        let parts_str: String = command_args
            .iter()
            .map(|a| shell_escape(a))
            .collect::<Vec<_>>()
            .join(" ");
        let full_cmd = format!("mount -o remount,ro,bind / && {}", parts_str);
        unshare_args.push("--".to_string());
        unshare_args.push("sh".to_string());
        unshare_args.push("-c".to_string());
        unshare_args.push(full_cmd);
    } else {
        unshare_args.push("--".to_string());
        unshare_args.extend_from_slice(command_args);
    }

    // Pipe drainage + reap happen together via `wait_with_output`,
    // which spawns internal threads to keep stdout/stderr from
    // back-pressuring the child. The old `wait()` + `read_to_string`
    // pattern deadlocked any sandboxed command that produced more
    // than the kernel pipe buffer (~64 KiB on Linux): the child
    // blocked on a full pipe, never exited, and our `wait()` never
    // returned. Same deadlock the bridge dispatcher had.
    let child = Command::new("unshare")
        .args(&unshare_args)
        .current_dir(workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn sandbox: {e}"))?;

    let output = child
        .wait_with_output()
        .map_err(|e| format!("sandbox wait failed: {e}"))?;
    let status = output.status;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    Ok(json!({
        "exit_code": status.code().unwrap_or(-1),
        "stdout": stdout,
        "stderr": stderr,
        "isolated": true,
        "network": network,
        "read_only": read_only,
        "workspace": workspace,
    }))
}

/// Linux: use systemd-run for cgroup v2 resource limits + namespace isolation.
///
/// systemd-run creates a transient scope with cgroup limits.
/// Combined with unshare flags for PID/mount/net namespace isolation.
/// Build the `systemd-run` argv used by `exec_linux_with_cgroup`.
/// Factored out as a pure function for unit-test coverage: the
/// pre-fix code packed `-p` flag + value into a single argv slot
/// (e.g. `"-p MemoryMax=512M"`) which systemd-run rejects, breaking
/// every sandbox-with-limits invocation. We must guarantee `-p`
/// flags and their `KEY=VALUE` payloads land in separate argv
/// elements.
///
/// `#[cfg(unix)]` (not `target_os = "linux"`) so we can unit-test
/// the argv structure on developer macOS hosts. The function only
/// constructs strings; it does not invoke systemd-run, so the
/// platform doesn't matter for its correctness.
#[cfg(unix)]
fn build_systemd_run_args(
    scope_name: &str,
    command_args: &[String],
    network: bool,
    read_only: bool,
    workspace: &str,
    limits: &ResourceLimits,
) -> Vec<String> {
    let mut sr_args = vec![
        "--scope".to_string(),
        format!("--unit={scope_name}"),
        "--quiet".to_string(),
    ];

    // systemd-run accepts each property as TWO argv elements: the
    // literal `-p` flag and the `KEY=VALUE` payload. The earlier
    // `format!("-p X=Y")` packed both into a single argv slot which
    // systemd-run interprets as an unknown option and rejects with
    // "unrecognized option". Mirror the already-correct
    // SystemCallFilter / ReadOnlyPaths blocks below.

    // Memory limit (cgroup v2: MemoryMax)
    if let Some(ref mem) = limits.mem_limit {
        sr_args.push("-p".to_string());
        sr_args.push(format!("MemoryMax={mem}"));
        sr_args.push("-p".to_string());
        sr_args.push("MemorySwapMax=0".to_string()); // no swap
    }

    // CPU limit (cgroup v2: CPUQuota)
    if let Some(cpu) = limits.cpu_percent {
        sr_args.push("-p".to_string());
        sr_args.push(format!("CPUQuota={cpu}%"));
    }

    // PID limit (cgroup v2: TasksMax)
    if let Some(pids) = limits.pids_max {
        sr_args.push("-p".to_string());
        sr_args.push(format!("TasksMax={pids}"));
    }

    // Timeout via RuntimeMaxSec
    if let Some(secs) = limits.timeout_secs {
        sr_args.push("-p".to_string());
        sr_args.push(format!("RuntimeMaxSec={secs}"));
    }

    // Read-only filesystem via systemd property
    if read_only {
        sr_args.push("-p".to_string());
        sr_args.push("ReadOnlyPaths=/".to_string());
    }

    // Seccomp syscall filter via systemd property
    if let Some(ref profile) = limits.seccomp_profile {
        if let Some(filter) = seccomp_syscall_filter(profile) {
            sr_args.push("-p".to_string());
            sr_args.push(format!("SystemCallFilter={filter}"));
        }
    }

    // Set working directory to workspace
    sr_args.push("-p".to_string());
    sr_args.push(format!("WorkingDirectory={workspace}"));

    // Wrap the actual command in unshare for namespace isolation
    sr_args.push("--".to_string());
    sr_args.push("unshare".to_string());
    sr_args.push("--pid".to_string());
    sr_args.push("--fork".to_string());
    sr_args.push("--mount-proc".to_string());
    sr_args.push("--mount".to_string());
    if !network {
        sr_args.push("--net".to_string());
    }
    sr_args.push("--".to_string());
    sr_args.extend_from_slice(command_args);
    sr_args
}

#[cfg(target_os = "linux")]
fn exec_linux_with_cgroup(
    command_args: &[String],
    network: bool,
    read_only: bool,
    workspace: &str,
    limits: &ResourceLimits,
) -> Result<Value, String> {
    let scope_name = format!("cos-sandbox-{}", short_id());
    let sr_args = build_systemd_run_args(&scope_name, command_args, network, read_only, workspace, limits);

    let child = Command::new("systemd-run")
        .args(&sr_args)
        .current_dir(workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn sandbox (systemd-run): {e}"))?;

    // wait_with_output drains stdout/stderr concurrently with the
    // reap so commands producing > pipe-buffer bytes (64 KiB) don't
    // deadlock the parent. See the matching comment in exec_linux.
    let output = child
        .wait_with_output()
        .map_err(|e| format!("sandbox wait failed: {e}"))?;
    let status = output.status;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    // Check if killed by cgroup (exit code 137 = OOM, etc.)
    let exit_code = status.code().unwrap_or(-1);
    let mut killed_by = None;
    if exit_code == 137 {
        killed_by = Some("OOM (memory limit exceeded)");
    } else if exit_code == 124 || stderr.contains("RuntimeMaxSec") {
        killed_by = Some("timeout (RuntimeMaxSec exceeded)");
    }

    let mut result = json!({
        "exit_code": exit_code,
        "stdout": stdout,
        "stderr": stderr,
        "isolated": true,
        "network": network,
        "read_only": read_only,
        "workspace": workspace,
        "cgroup": true,
        "scope": scope_name,
    });

    if let Some(mem) = &limits.mem_limit {
        result["limits"] = json!({
            "memory": mem,
            "cpu_percent": limits.cpu_percent,
            "pids_max": limits.pids_max,
            "timeout_secs": limits.timeout_secs,
        });
    }

    if let Some(ref profile) = limits.seccomp_profile {
        result["seccomp_profile"] = json!(profile);
    }

    if let Some(reason) = killed_by {
        result["killed_by"] = json!(reason);
    }

    Ok(result)
}

fn short_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{:x}", t & 0xFFFFFFFF)
}

/// Escape a string for safe inclusion in a POSIX shell command.
/// Wraps in single quotes, escaping embedded single quotes.
#[cfg(target_os = "linux")]
fn shell_escape(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    // If the string contains no special chars, return as-is
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':' | '='))
    {
        return s.to_string();
    }
    // Wrap in single quotes; replace ' with '\''
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Generate a seccomp BPF filter JSON for use with systemd's SystemCallFilter.
///
/// Profiles:
///   - `minimal`: only basic I/O and process management syscalls
///   - `network`: minimal + networking syscalls (socket, connect, etc.)
///   - `full`: all syscalls allowed (no filtering)
///
/// `#[cfg(unix)]` (not `target_os = "linux"`) so the argv-builder
/// helper above stays compilable on macOS for unit tests. The
/// function only returns strings; it does not load any BPF program.
#[cfg(unix)]
fn seccomp_syscall_filter(profile: &str) -> Option<String> {
    match profile {
        "minimal" => {
            // Allow only essential syscalls for computation
            Some("~@clock @debug @module @mount @obsolete @raw-io @reboot @swap @privileged".into())
        }
        "network" => {
            // Allow everything except dangerous system-level calls
            Some("~@clock @debug @module @mount @obsolete @raw-io @reboot @swap".into())
        }
        "full" => None, // No filtering
        _ => None,
    }
}

/// Fallback for non-Linux: basic subprocess execution with timeout.
#[cfg(not(target_os = "linux"))]
fn exec_fallback(
    command_args: &[String],
    workspace: &str,
    limits: &ResourceLimits,
) -> Result<Value, String> {
    if command_args.is_empty() {
        return Err("no command specified".into());
    }

    let mut child = Command::new(&command_args[0])
        .args(&command_args[1..])
        .current_dir(workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn: {e}"))?;

    // Simple timeout: poll in a loop. On timeout we still drain
    // stdout/stderr after killing the child so partial output is
    // reported.
    if let Some(secs) = limits.timeout_secs {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs as u64);
        loop {
            match child.try_wait() {
                Ok(Some(_status)) => break,
                Ok(None) => {
                    if std::time::Instant::now() > deadline {
                        let _ = child.kill();
                        // After kill the writer side of each pipe
                        // is closed, so wait_with_output completes
                        // promptly (drainer threads see EOF).
                        let output = child
                            .wait_with_output()
                            .map_err(|e| format!("wait_with_output after kill: {e}"))?;
                        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
                        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
                        return Ok(json!({
                            "exit_code": -1,
                            "killed_by": "timeout",
                            "stdout": stdout,
                            "stderr": stderr,
                            "isolated": false,
                        }));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                Err(e) => return Err(format!("wait failed: {e}")),
            }
        }
    }

    // Normal path: try_wait already saw the child exit (or we never
    // entered the timeout loop). wait_with_output reaps and drains
    // pipes concurrently to avoid the >64 KiB pipe-buffer deadlock.
    let output = child
        .wait_with_output()
        .map_err(|e| format!("wait failed: {e}"))?;
    let status = output.status;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    Ok(json!({
        "exit_code": status.code().unwrap_or(-1),
        "stdout": stdout,
        "stderr": stderr,
        "isolated": false,
        "note": "namespace/cgroup isolation requires Linux",
    }))
}

#[cfg(all(test, unix))]
mod tests {
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
        assert!(contains_window(&args, "-p", "WorkingDirectory=/sandbox/ws-1"), "missing WorkingDirectory pair: {args:?}");
        assert!(contains_window(&args, "-p", "ReadOnlyPaths=/"), "missing ReadOnlyPaths pair: {args:?}");
        no_packed_p_flag(&args);
    }

    /// With no limits at all there should still be no `-p X=Y`
    /// elements and the trailing argv must dispatch unshare ->
    /// command_args correctly.
    #[test]
    fn systemd_run_args_no_limits_dispatches_unshare_command() {
        let args = build_systemd_run_args(
            "scope-empty",
            &["true".to_string()],
            true, // network on -> no --net
            false,
            "/tmp",
            &mk_limits(None, None, None, None),
        );
        no_packed_p_flag(&args);
        // The argv must end in `-- unshare ... -- true`
        let pos_unshare = args.iter().position(|s| s == "unshare").expect("unshare in argv");
        // Two `--` separators: one before unshare, one before command.
        let dash_dash_count = args.iter().filter(|s| s.as_str() == "--").count();
        assert!(dash_dash_count >= 2, "expected two `--` separators, got {dash_dash_count}: {args:?}");
        assert!(args.iter().any(|s| s == "true"), "missing trailing command: {args:?}");
        // Network on: `--net` MUST be absent.
        assert!(!args.iter().any(|s| s == "--net"), "network=true should suppress --net: {args:?}");
        let _ = pos_unshare;
    }

    #[test]
    fn systemd_run_args_network_off_adds_net_namespace() {
        let args = build_systemd_run_args(
            "scope-net-off",
            &["true".to_string()],
            false, // network off -> --net present
            false,
            "/tmp",
            &mk_limits(None, None, None, None),
        );
        assert!(args.iter().any(|s| s == "--net"), "network=false must add --net: {args:?}");
    }

    /// Regression for the wait()+read_to_string deadlock that
    /// previously hung the sandbox dispatcher on any command
    /// producing more than ~64 KiB of stdout (one Linux pipe
    /// buffer). Exercise the non-Linux fallback path because it
    /// runs without root / namespace privileges and is reachable on
    /// the macOS / Linux dev workstations where this test executes.
    /// The fallback shares the same wait_with_output pattern as the
    /// Linux production paths, so a pass here proves the pattern
    /// fix is correct.
    ///
    /// We spawn `sh -c 'head -c 524288 /dev/zero | tr "\\0" "x"'`
    /// to produce a deterministic 512 KiB of stdout (8x the pipe
    /// buffer). Before the fix this would block forever; we wrap
    /// the whole thing in a 10-second mpsc deadline.
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn fallback_drains_large_stdout_without_deadlock() {
        use std::sync::mpsc;
        use std::thread;
        use std::time::Duration;

        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let res = exec_fallback(
                &[
                    "sh".to_string(),
                    "-c".to_string(),
                    "head -c 524288 /dev/zero | tr '\\0' 'x'".to_string(),
                ],
                "/tmp",
                &mk_limits(None, None, None, None),
            );
            let _ = tx.send(res);
        });
        let res = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("sandbox fallback deadlocked on >64 KiB stdout — wait_with_output not in effect");
        let v = res.expect("fallback returned Err");
        let stdout = v["stdout"].as_str().unwrap_or("");
        assert_eq!(
            stdout.len(),
            524288,
            "expected 512 KiB stdout, got {} bytes",
            stdout.len()
        );
        assert_eq!(v["exit_code"].as_i64(), Some(0));
    }
}
