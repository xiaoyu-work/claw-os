//! Opaque stdio hosting through the ordinary verified App operation contract.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::path::{Path, PathBuf};
use std::process::{Child, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use super::{AppIdentitySession, AppLaunch, LaunchBindingRef, LaunchRequest};
use crate::caps::manifest::Runtime;

static CANCELLED: AtomicBool = AtomicBool::new(false);
static SIGNAL_OWNER: Mutex<()> = Mutex::new(());

pub(super) fn cancelled() -> bool {
    CANCELLED.load(Ordering::SeqCst)
}

extern "C" fn cancel_from_signal(_signal: libc::c_int) {
    CANCELLED.store(true, Ordering::SeqCst);
}

struct StdioSignals {
    previous: Vec<(libc::c_int, libc::sigaction)>,
    _owner: MutexGuard<'static, ()>,
}

impl StdioSignals {
    fn install() -> Result<Self, String> {
        let owner = SIGNAL_OWNER
            .try_lock()
            .map_err(|error| format!("App stdio signal handling is unavailable: {error}"))?;
        CANCELLED.store(false, Ordering::SeqCst);
        let mut guard = Self {
            previous: Vec::new(),
            _owner: owner,
        };
        for signal in [libc::SIGINT, libc::SIGTERM] {
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            action.sa_sigaction = cancel_from_signal as *const () as libc::sighandler_t;
            let mut previous = std::mem::MaybeUninit::uninit();
            if unsafe { libc::sigaction(signal, &action, previous.as_mut_ptr()) } != 0 {
                return Err(format!(
                    "install App stdio signal handler: {}",
                    io::Error::last_os_error()
                ));
            }
            guard
                .previous
                .push((signal, unsafe { previous.assume_init() }));
        }
        Ok(guard)
    }
}

impl Drop for StdioSignals {
    fn drop(&mut self) {
        for (signal, previous) in self.previous.iter().rev() {
            if unsafe { libc::sigaction(*signal, previous, std::ptr::null_mut()) } != 0 {
                tracing::error!(
                    error = %io::Error::last_os_error(),
                    "failed to restore App stdio signal handler"
                );
            }
        }
        CANCELLED.store(false, Ordering::SeqCst);
    }
}

fn require_not_cancelled() -> Result<(), String> {
    if cancelled() {
        Err("App stdio host was cancelled".to_string())
    } else {
        Ok(())
    }
}

/// Run a manifest operation as an opaque, long-lived stdio service.
///
/// The operation must explicitly declare `stdin: true`. Its normal needs and
/// effective arguments determine authorization; no permissions are inferred
/// from other operations or MCP tools. The App owns all framing and payload
/// interpretation. Only the ordinary launch broker enters its sandbox.
pub fn run_app_stdio(
    launch: &AppLaunch,
    operation: &str,
    args: &[String],
    data_dir: &str,
    apps_dir: &str,
) -> Result<(), String> {
    let entry = stdio_entry(launch, operation)?;
    let _signals = StdioSignals::install()?;
    validate_runtime_overrides()?;
    let (program, mut argv) = super::session_program(launch.manifest().runtime, &entry)?;
    if !matches!(launch.manifest().runtime, Runtime::Binary) {
        program_is_trusted(&program)?;
    }
    if matches!(launch.manifest().runtime, Runtime::Python) {
        argv.insert(0, "-I".to_string());
    }
    let runner = Path::new("/usr/local/bin/claw-app-runner")
        .canonicalize()
        .map_err(|error| format!("resolve App launch runner: {error}"))?;
    program_is_trusted(&runner)?;
    program_is_trusted(
        &Path::new("/usr/local/bin/cos")
            .canonicalize()
            .map_err(|error| format!("resolve App runtime CLI: {error}"))?,
    )?;
    let relative = entry
        .strip_prefix(launch.dir())
        .map_err(|error| format!("App entry escaped its package: {error}"))?
        .to_str()
        .ok_or_else(|| "App entry is not UTF-8".to_string())?
        .to_string();
    let ((mut session, effective_args), binding) = launch
        .bind_for_session(std::slice::from_ref(&relative), || {
            stdio_session(launch, operation, args)
        })?;
    let token = uuid::Uuid::new_v4().simple().to_string();
    let prepared = prepare_stdio_worker(
        launch,
        &session,
        operation,
        &effective_args,
        program,
        argv,
        runner,
        &token,
        data_dir,
        apps_dir,
        &binding,
    )?;
    execute_stdio(
        launch,
        &mut session,
        prepared,
        &token,
        std::io::stdin().as_fd(),
        &relative,
    )
}

fn stdio_entry(launch: &AppLaunch, operation: &str) -> Result<PathBuf, String> {
    if !launch.ceiling().allows_mcp_attach() {
        return Err(format!(
            "App `{}` is developer-trusted and may not attach a long-lived stdio host",
            launch.app_id()
        ));
    }
    let declared = launch
        .manifest()
        .operations
        .get(operation)
        .ok_or_else(|| format!("App `{}` has no operation `{operation}`", launch.app_id()))?;
    if !declared.stdin {
        return Err(format!(
            "App operation `{operation}` does not declare stdin input"
        ));
    }
    let manifest = launch.manifest();
    let entry = manifest
        .entry
        .as_deref()
        .unwrap_or_else(|| manifest.runtime.default_entry());
    crate::worker::entry::verified_entrypoint(
        launch.app_id(),
        launch.package(),
        entry,
        manifest.runtime,
    )
}

fn stdio_session(
    launch: &AppLaunch,
    operation: &str,
    args: &[String],
) -> Result<(AppIdentitySession, Vec<String>), String> {
    stdio_entry(launch, operation)?;
    if !super::use_clawd_app_session_backend()? {
        return AppIdentitySession::for_operation(launch, launch.app_id(), operation, args);
    }
    let bound = super::bind_operation_args(&launch.manifest().operations[operation], args)?;
    // Unregistered launchers use the daemon's peer-derived policy and approval
    // flow. Do not manufacture an Admin parent or present an App name as proof.
    let params = super::app_registration_params(
        launch.app_id(),
        &LaunchRequest::Operation {
            operation,
            args: &bound.argv,
        },
        None,
        &launch.package_ref(),
    )?;
    let result =
        super::consent::register_with_wait(launch.app_id(), params).map_err(String::from)?;
    let session = AppIdentitySession::from_clawd_registration(
        launch.app_id(),
        result,
        None,
        Some(&launch.ceiling()),
        &launch.package_ref(),
    )?;
    Ok((session, bound.argv))
}

fn program_is_trusted(program: &Path) -> Result<(), String> {
    let metadata = crate::provenance::fsec::require_secure_location(program, &[0])
        .map_err(|error| format!("App runtime must be a protected system executable: {error}"))?;
    if !metadata.is_file || metadata.mode & 0o111 == 0 {
        return Err(format!(
            "App runtime is not an executable regular file: {}",
            program.display()
        ));
    }
    Ok(())
}

fn validate_runtime_overrides() -> Result<(), String> {
    for (name, expected) in [
        ("COS_BIN", "/usr/local/bin/cos"),
        ("COS_SDK_PYTHON_DIR", "/usr/lib/cos/python"),
    ] {
        if std::env::var_os(name).is_some_and(|value| value != std::ffi::OsStr::new(expected)) {
            return Err(format!(
                "App stdio hosting requires the installed runtime: {name} must be unset or `{expected}`"
            ));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn prepare_stdio_worker(
    launch: &AppLaunch,
    session: &AppIdentitySession,
    operation: &str,
    args: &[String],
    program: PathBuf,
    argv: Vec<String>,
    runner: PathBuf,
    token: &str,
    data_dir: &str,
    apps_dir: &str,
    binding: &LaunchBindingRef,
) -> Result<crate::worker::PreparedLaunch, String> {
    let program = program
        .to_str()
        .ok_or_else(|| "App stdio program is not UTF-8".to_string())?;
    let mut gated_argv = vec![
        "--launch-gate".to_string(),
        token.to_string(),
        "--".to_string(),
        program.to_string(),
    ];
    gated_argv.extend(argv);
    let extra_env = BTreeMap::from([
        ("COS_COMMAND".to_string(), operation.to_string()),
        (
            "COS_ARGS_JSON".to_string(),
            serde_json::to_string(args)
                .map_err(|error| format!("serialize App arguments: {error}"))?,
        ),
        (
            "COS_APP_MANIFEST".to_string(),
            launch
                .dir()
                .join(launch.package().manifest_path())
                .to_string_lossy()
                .into_owned(),
        ),
        (
            "COS_SDK_PYTHON_DIR".to_string(),
            "/usr/lib/cos/python".to_string(),
        ),
        ("CLAW_COS_BIN".to_string(), "/usr/local/bin/cos".to_string()),
    ]);
    super::prepare_app_worker(
        session,
        launch.app_id(),
        launch.dir(),
        operation,
        runner,
        gated_argv,
        data_dir,
        apps_dir,
        extra_env,
        crate::worker::StdioPlan::Streamed,
        false,
        binding,
    )
}

fn execute_stdio(
    launch: &AppLaunch,
    session: &mut AppIdentitySession,
    prepared: crate::worker::PreparedLaunch,
    token: &str,
    input: BorrowedFd<'_>,
    entry: &str,
) -> Result<(), String> {
    let policy = prepared.facts["policy"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let crate::worker::PreparedLaunch {
        mut command,
        resources,
        ..
    } = prepared;
    let mut child = command
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|error| format!("spawn sandboxed App stdio host: {error}"))?;
    let pid = child.id();
    let owner = crate::provenance::runtime::current_owner();
    let close_wait = crate::worker::Limits::operation().runtime;
    let result = (|| {
        session.bind_process(pid)?;
        if session.uses_local_backend() {
            crate::provenance::runtime::register(owner, session.id(), launch.package());
            crate::provenance::runtime::bind_process(owner, session.id(), pid);
        }
        let _current = launch.bind(&[entry.to_string()])?;
        relay_stdin(&mut child, input, token, close_wait, || {
            resources.kill_all(Some(pid));
        })
    })();
    resources.kill_all(Some(pid));
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    if session.uses_local_backend() {
        crate::provenance::runtime::deregister(owner, session.id());
    }
    crate::worker::audit::outcome(
        &policy,
        &format!("app:{}/stdio", launch.app_id()),
        serde_json::json!({
            "exit_code": result.as_ref().ok().and_then(|(status, _)| status.code()),
            "failed": !result.as_ref().is_ok_and(|(status, timeout)| status.success() && !timeout),
            "timed_out": result.as_ref().is_ok_and(|(_, timeout)| *timeout),
            "cancelled": cancelled(),
        }),
    );
    let (status, timed_out) = result?;
    if timed_out {
        return Err(format!(
            "App stdio host did not exit within {}s after its input closed",
            close_wait.as_secs()
        ));
    }
    if !status.success() {
        return Err(format!(
            "App stdio host exited with code {}",
            status.code().unwrap_or(-1)
        ));
    }
    Ok(())
}

/// Bound memory and cancellation without a thread stranded in a blocking read
/// of the caller's stdin. Output remains inherited and is never interpreted.
fn relay_stdin(
    child: &mut Child,
    input: BorrowedFd<'_>,
    token: &str,
    close_wait: Duration,
    stop: impl Fn(),
) -> Result<(ExitStatus, bool), String> {
    require_not_cancelled()?;
    let mut writer = child
        .stdin
        .take()
        .ok_or_else(|| "App stdio input is unavailable".to_string())?;
    writer
        .write_all(token.as_bytes())
        .map_err(|error| format!("release App launch gate: {error}"))?;
    let fd = writer.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(format!(
            "configure App stdio input: {}",
            io::Error::last_os_error()
        ));
    }
    let mut buffer = [0_u8; 16 * 1024];
    let mut start = 0;
    let mut end = 0;
    loop {
        require_not_cancelled()?;
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("wait for App stdio host: {error}"))?
        {
            return Ok((status, false));
        }
        let mut polls = [
            libc::pollfd {
                fd: if start == end { input.as_raw_fd() } else { -1 },
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: if start < end { writer.as_raw_fd() } else { -1 },
                events: libc::POLLOUT,
                revents: 0,
            },
        ];
        if unsafe { libc::poll(polls.as_mut_ptr(), polls.len() as libc::nfds_t, 100) } < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(format!("poll App stdio: {error}"));
        }
        if polls.iter().any(|poll| poll.revents & libc::POLLNVAL != 0) {
            return Err("App stdio descriptor became invalid".to_string());
        }
        if polls[0].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
            let read =
                unsafe { libc::read(input.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) };
            if read < 0 {
                let error = io::Error::last_os_error();
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) {
                    continue;
                }
                return Err(format!("read App stdio input: {error}"));
            }
            start = 0;
            end = read as usize;
            if end == 0 {
                drop(writer);
                return wait_after_eof(child, close_wait, stop);
            }
        }
        if polls[1].revents & (libc::POLLOUT | libc::POLLHUP | libc::POLLERR) != 0 {
            match writer.write(&buffer[start..end]) {
                Ok(0) => return Err("App stdio input stopped accepting bytes".to_string()),
                Ok(written) => start += written,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                    ) => {}
                Err(error) => return Err(format!("write App stdio input: {error}")),
            }
        }
    }
}

fn wait_after_eof(
    child: &mut Child,
    close_wait: Duration,
    stop: impl Fn(),
) -> Result<(ExitStatus, bool), String> {
    let deadline = Instant::now() + close_wait;
    loop {
        require_not_cancelled()?;
        match child.try_wait() {
            Ok(Some(status)) => return Ok((status, false)),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Ok(None) => {
                stop();
                let _ = child.kill();
                let status = child
                    .wait()
                    .map_err(|error| format!("reap timed-out App stdio host: {error}"))?;
                return Ok((status, true));
            }
            Err(error) => return Err(format!("wait for App stdio host: {error}")),
        }
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/bridge/stdio.rs"
    ));
}

#[cfg(all(test, target_os = "linux"))]
mod cli_tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/bridge/stdio_cli.rs"
    ));
}
