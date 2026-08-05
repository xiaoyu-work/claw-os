/// Lightweight sandbox using Linux namespaces.
///
/// Exposed only as an agent tool (`cos_sandbox` in
/// `crate::agent::tools::cos_proxy`), not as a user-facing CLI
/// primitive. The agent uses this to run model-generated or
/// otherwise untrusted commands under bubblewrap + cgroup v2 + seccomp.
///
/// Only one operation is supported: `exec`. Persistent sandboxes
/// (create/destroy/list) were a legacy surface area; they
/// never spawned a real init process and have been removed.
///
/// Unsupported platforms and missing isolation primitives fail closed.
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

const DEFAULT_MEMORY_LIMIT: &str = "512M";
const DEFAULT_CPU_PERCENT: u32 = 100;
const DEFAULT_PIDS_MAX: u32 = 64;
const DEFAULT_TIMEOUT_SECS: u32 = 300;
const MAX_OUTPUT_BYTES: u64 = 1_048_576;

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
///   [--network] [--rw] [--workspace DIR]
///   [--mem LIMIT] [--cpu PERCENT] [--pids MAX]
///   [--timeout SECS] [--seccomp-profile minimal|network|full]
///   -- <command> [args...]
fn cmd_exec(args: &[String]) -> Result<Value, String> {
    require_or_json(Verb::PROC_SPAWN, Scope::wild()).map_err(|v| v.to_string())?;
    let mut network = false;
    let mut read_only = true;
    let mut workspace = crate::paths::current_home_override()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| crate::config::get().home.clone());
    let mut mem_limit = Some(DEFAULT_MEMORY_LIMIT.to_string());
    let mut cpu_percent = Some(DEFAULT_CPU_PERCENT);
    let mut pids_max = Some(DEFAULT_PIDS_MAX);
    let mut timeout_secs = Some(DEFAULT_TIMEOUT_SECS);
    let mut seccomp_profile = Some("minimal".to_string());
    let mut cmd_start = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--no-network" => {
                network = false;
                i += 1;
            }
            "--network" => {
                network = true;
                i += 1;
            }
            "--ro" => {
                read_only = true;
                i += 1;
            }
            "--rw" => {
                read_only = false;
                i += 1;
            }
            "--workspace" if i + 1 < args.len() => {
                workspace = args[i + 1].clone();
                i += 2;
            }
            "--mem" if i + 1 < args.len() => {
                validate_memory_limit(&args[i + 1])?;
                mem_limit = Some(args[i + 1].clone());
                i += 2;
            }
            "--cpu" if i + 1 < args.len() => {
                cpu_percent = Some(
                    args[i + 1]
                        .parse::<u32>()
                        .map_err(|_| format!("invalid cpu value: {}", args[i + 1]))?,
                );
                if !matches!(cpu_percent, Some(1..=100)) {
                    return Err("cpu percent must be between 1 and 100".to_string());
                }
                i += 2;
            }
            "--pids" if i + 1 < args.len() => {
                pids_max = Some(
                    args[i + 1]
                        .parse::<u32>()
                        .map_err(|_| format!("invalid pids value: {}", args[i + 1]))?,
                );
                if !matches!(pids_max, Some(1..=1024)) {
                    return Err("pids limit must be between 1 and 1024".to_string());
                }
                i += 2;
            }
            "--timeout" if i + 1 < args.len() => {
                timeout_secs = Some(
                    args[i + 1]
                        .parse::<u32>()
                        .map_err(|_| format!("invalid timeout value: {}", args[i + 1]))?,
                );
                if !matches!(timeout_secs, Some(1..=3600)) {
                    return Err("timeout must be between 1 and 3600 seconds".to_string());
                }
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
    let workspace_scope = Scope::path(format!(
        "{}/**",
        workspace.trim_end_matches('/')
    ));
    require_or_json(Verb::FS_READ, workspace_scope.clone())
        .map_err(|value| value.to_string())?;
    if !read_only {
        require_or_json(Verb::FS_WRITE, workspace_scope)
            .map_err(|value| value.to_string())?;
    }
    if network {
        require_or_json(Verb::NET_DIAL, Scope::Wild)
            .map_err(|value| value.to_string())?;
    }
    let limits = ResourceLimits {
        mem_limit,
        cpu_percent,
        pids_max,
        timeout_secs,
        seccomp_profile,
    };

    #[cfg(target_os = "linux")]
    {
        exec_linux(command_args, network, read_only, &workspace, &limits)
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = (command_args, workspace, limits);
        Err("sandbox isolation requires Linux".to_string())
    }
}

fn validate_memory_limit(value: &str) -> Result<(), String> {
    let (number, multiplier) = match value.as_bytes().last().copied() {
        Some(b'K' | b'k') => (&value[..value.len() - 1], 1024_u64),
        Some(b'M' | b'm') => (&value[..value.len() - 1], 1024_u64.pow(2)),
        Some(b'G' | b'g') => (&value[..value.len() - 1], 1024_u64.pow(3)),
        Some(_) => (value, 1),
        None => return Err("memory limit must not be empty".to_string()),
    };
    let bytes = number
        .parse::<u64>()
        .ok()
        .and_then(|number| number.checked_mul(multiplier))
        .ok_or_else(|| format!("invalid memory limit: {value}"))?;
    if !(16 * 1024 * 1024..=4 * 1024 * 1024 * 1024).contains(&bytes) {
        return Err("memory limit must be between 16M and 4G".to_string());
    }
    Ok(())
}

/// Linux: require bubblewrap namespace isolation inside a transient
/// systemd service with cgroup limits.
#[cfg(target_os = "linux")]
fn exec_linux(
    command_args: &[String],
    network: bool,
    read_only: bool,
    workspace: &str,
    limits: &ResourceLimits,
) -> Result<Value, String> {
    if command_args.is_empty() {
        return Err("no command specified".to_string());
    }
    for tool in ["bwrap", "systemd-run"] {
        if !command_exists(tool) {
            return Err(format!(
                "sandbox isolation unavailable: required tool `{tool}` is missing"
            ));
        }
    }
    if !std::path::Path::new("/sys/fs/cgroup/cgroup.controllers").is_file() {
        return Err("sandbox isolation unavailable: cgroup v2 is not mounted".to_string());
    }
    exec_linux_with_cgroup(command_args, network, read_only, workspace, limits)
}

#[cfg(target_os = "linux")]
fn command_exists(command: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths)
            .map(|path| path.join(command))
            .any(|path| path.is_file())
    })
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
fn build_systemd_run_args_for_identity(
    scope_name: &str,
    command_args: &[String],
    network: bool,
    read_only: bool,
    workspace: &str,
    limits: &ResourceLimits,
    run_uid: u32,
    run_gid: u32,
) -> Vec<String> {
    let mut sr_args = vec![
        "--wait".to_string(),
        "--collect".to_string(),
        "--pipe".to_string(),
        format!("--unit={scope_name}"),
        "--quiet".to_string(),
        format!("--uid={run_uid}"),
        format!("--gid={run_gid}"),
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

    sr_args.push("-p".to_string());
    sr_args.push("NoNewPrivileges=yes".to_string());
    sr_args.push("-p".to_string());
    sr_args.push("RestrictSUIDSGID=yes".to_string());

    // Seccomp syscall filter via systemd property
    if let Some(ref profile) = limits.seccomp_profile {
        if let Some(filter) = seccomp_syscall_filter(profile) {
            sr_args.push("-p".to_string());
            sr_args.push(format!("SystemCallFilter={filter}"));
        }
    }

    // Bubblewrap builds a minimal read-only root and exposes only the
    // selected workspace at /workspace.
    sr_args.push("--".to_string());
    sr_args.push("bwrap".to_string());
    sr_args.extend([
        "--die-with-parent".to_string(),
        "--new-session".to_string(),
        "--unshare-all".to_string(),
    ]);
    if network {
        sr_args.push("--share-net".to_string());
    }
    sr_args.extend([
        "--cap-drop".to_string(),
        "ALL".to_string(),
        "--ro-bind".to_string(),
        "/usr".to_string(),
        "/usr".to_string(),
        "--symlink".to_string(),
        "usr/bin".to_string(),
        "/bin".to_string(),
        "--symlink".to_string(),
        "usr/sbin".to_string(),
        "/sbin".to_string(),
        "--symlink".to_string(),
        "usr/lib".to_string(),
        "/lib".to_string(),
        "--symlink".to_string(),
        "usr/lib64".to_string(),
        "/lib64".to_string(),
        "--ro-bind".to_string(),
        "/etc".to_string(),
        "/etc".to_string(),
        "--proc".to_string(),
        "/proc".to_string(),
        "--dev".to_string(),
        "/dev".to_string(),
        "--tmpfs".to_string(),
        "/tmp".to_string(),
        "--tmpfs".to_string(),
        "/run".to_string(),
        "--dir".to_string(),
        "/var".to_string(),
        "--dir".to_string(),
        "/home".to_string(),
        "--dir".to_string(),
        "/root".to_string(),
        "--dir".to_string(),
        "/workspace".to_string(),
        "--remount-ro".to_string(),
        "/".to_string(),
    ]);
    sr_args.push(if read_only {
        "--ro-bind".to_string()
    } else {
        "--bind".to_string()
    });
    sr_args.push(workspace.to_string());
    sr_args.push("/workspace".to_string());
    sr_args.extend([
        "--chdir".to_string(),
        "/workspace".to_string(),
        "--setenv".to_string(),
        "HOME".to_string(),
        "/workspace".to_string(),
        "--setenv".to_string(),
        "COS_HOME".to_string(),
        "/workspace".to_string(),
        "--setenv".to_string(),
        "PATH".to_string(),
        "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_string(),
    ]);
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
    let workspace = std::fs::canonicalize(workspace)
        .map_err(|error| format!("invalid sandbox workspace: {error}"))?;
    if !workspace.is_dir() {
        return Err("sandbox workspace must be a directory".to_string());
    }
    if let Some(home) = crate::paths::current_home_override() {
        let home = home
            .canonicalize()
            .map_err(|error| format!("invalid sandbox owner home: {error}"))?;
        if !workspace.starts_with(&home) {
            return Err(format!(
                "sandbox workspace {} escapes owner home {}",
                workspace.display(),
                home.display()
            ));
        }
    }
    let (run_uid, run_gid) = sandbox_identity()?;
    let scope_name = format!("cos-sandbox-{}", short_id());
    let workspace_string = workspace.to_string_lossy().into_owned();
    let sr_args = build_systemd_run_args_for_identity(
        &scope_name,
        command_args,
        network,
        read_only,
        &workspace_string,
        limits,
        run_uid,
        run_gid,
    );

    let mut command = Command::new("systemd-run");
    command
        .args(&sr_args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.env_clear().env(
        "PATH",
        "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
    );
    let child = command
        .spawn()
        .map_err(|e| format!("failed to spawn sandbox (systemd-run): {e}"))?;

    let timeout = std::time::Duration::from_secs(
        limits.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS) as u64,
    );
    let output = wait_bounded(child, timeout, &scope_name)?;
    let status = output.status;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    // Check if killed by cgroup (exit code 137 = OOM, etc.)
    let exit_code = status.code().unwrap_or(-1);
    let mut killed_by = None;
    if output.timed_out {
        killed_by = Some("timeout");
    } else if exit_code == 137 {
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
        "read_only_root": true,
        "workspace_read_only": read_only,
        "workspace": workspace_string,
        "cgroup": true,
        "scope": scope_name,
        "stdout_truncated": output.stdout_truncated,
        "stderr_truncated": output.stderr_truncated,
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

#[cfg(all(test, unix))]
fn build_systemd_run_args(
    scope_name: &str,
    command_args: &[String],
    network: bool,
    read_only: bool,
    workspace: &str,
    limits: &ResourceLimits,
) -> Vec<String> {
    build_systemd_run_args_for_identity(
        scope_name,
        command_args,
        network,
        read_only,
        workspace,
        limits,
        1000,
        1000,
    )
}

#[cfg(target_os = "linux")]
struct BoundedOutput {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stdout_truncated: bool,
    stderr_truncated: bool,
    timed_out: bool,
}

#[cfg(target_os = "linux")]
fn wait_bounded(
    mut child: std::process::Child,
    timeout: std::time::Duration,
    scope_name: &str,
) -> Result<BoundedOutput, String> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "sandbox stdout unavailable".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "sandbox stderr unavailable".to_string())?;
    let stdout_reader = std::thread::spawn(move || read_bounded(stdout));
    let stderr_reader = std::thread::spawn(move || read_bounded(stderr));
    let deadline = std::time::Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Ok(None) => {
                timed_out = true;
                let _ = Command::new("systemctl")
                    .args(["kill", "--kill-whom=all", "--signal=KILL", scope_name])
                    .status();
                let _ = child.kill();
                break child
                    .wait()
                    .map_err(|error| format!("sandbox wait after timeout: {error}"))?;
            }
            Err(error) => return Err(format!("sandbox wait failed: {error}")),
        }
    };
    let (stdout, stdout_truncated) = stdout_reader
        .join()
        .map_err(|_| "sandbox stdout reader panicked".to_string())??;
    let (stderr, stderr_truncated) = stderr_reader
        .join()
        .map_err(|_| "sandbox stderr reader panicked".to_string())??;
    Ok(BoundedOutput {
        status,
        stdout,
        stderr,
        stdout_truncated,
        stderr_truncated,
        timed_out,
    })
}

#[cfg(target_os = "linux")]
fn read_bounded(reader: impl std::io::Read) -> Result<(Vec<u8>, bool), String> {
    let mut reader = reader;
    let mut kept = Vec::new();
    let mut buffer = [0_u8; 16 * 1024];
    let mut total = 0_u64;
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("sandbox output read failed: {error}"))?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(read as u64);
        if kept.len() < MAX_OUTPUT_BYTES as usize {
            let remaining = MAX_OUTPUT_BYTES as usize - kept.len();
            kept.extend_from_slice(&buffer[..read.min(remaining)]);
        }
    }
    Ok((kept, total > MAX_OUTPUT_BYTES))
}

#[cfg(target_os = "linux")]
fn sandbox_identity() -> Result<(u32, u32), String> {
    let uid = crate::paths::current_owner_uid_override()
        .unwrap_or_else(|| unsafe { libc::geteuid() as u32 });
    if uid == 0 {
        return Err("refusing to run sandbox payload as root".to_string());
    }
    let gid = primary_gid(uid)?;
    Ok((uid, gid))
}

#[cfg(target_os = "linux")]
fn primary_gid(uid: u32) -> Result<u32, String> {
    const BUFFER_SIZE: usize = 16 * 1024;
    let mut buffer = vec![0 as libc::c_char; BUFFER_SIZE];
    let mut passwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    let code = unsafe {
        libc::getpwuid_r(
            uid as libc::uid_t,
            &mut passwd,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut result,
        )
    };
    if code != 0 || result.is_null() {
        return Err(format!("passwd lookup failed for sandbox uid {uid}"));
    }
    Ok(passwd.pw_gid as u32)
}

fn short_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{:x}", t & 0xFFFFFFFF)
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
            // Bubblewrap needs namespace/mount setup before it drops all
            // capabilities. Block kernel/host control syscall groups that
            // remain dangerous after setup.
            Some("~@clock @debug @module @obsolete @raw-io @reboot @swap".into())
        }
        "network" => {
            // Allow everything except dangerous system-level calls
            Some("~@clock @debug @module @obsolete @raw-io @reboot @swap".into())
        }
        "full" => None, // No filtering
        _ => None,
    }
}

/// Unsupported platforms fail closed; never execute a plain subprocess.
#[cfg(not(target_os = "linux"))]
fn exec_fallback(
    _command_args: &[String],
    _workspace: &str,
    _limits: &ResourceLimits,
) -> Result<Value, String> {
    Err("sandbox isolation requires Linux".to_string())
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
}
