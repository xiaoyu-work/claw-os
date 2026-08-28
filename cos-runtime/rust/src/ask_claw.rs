//! Typed launch contract for the desktop "Ask Claw" overlay.
//!
//! Bundled desktop apps own small, app-specific context structs and implement
//! [`Context`] for them. This module owns the stable envelope, JSON encoding,
//! bounds, anonymous stdin handoff, Agent UI discovery, activation arguments,
//! and bounded transient process launch.

use std::fmt::{self, Display, Formatter};
#[cfg(target_os = "linux")]
use std::io::Write;
use std::io::{self, Read};
#[cfg(target_os = "linux")]
use std::os::fd::{AsRawFd, FromRawFd};
#[cfg(target_os = "linux")]
use std::os::unix::net::UnixStream;
#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::process::{Child, Command, Stdio};
use std::str::FromStr;
#[cfg(target_os = "linux")]
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
#[cfg(target_os = "linux")]
use std::time::Instant;

use serde::{Deserialize, Serialize};

/// Maximum serialized host context accepted by the Agent UI.
pub const MAX_CONTEXT_BYTES: usize = 32 * 1024;

/// Maximum serialized activation accepted from the Agent UI's stdin.
pub const MAX_ACTIVATION_BYTES: usize = MAX_CONTEXT_BYTES * 2 + 1024;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const READY_FD: i32 = 3;
const READY_MESSAGE: &[u8] = b"READY\n";
const PACKAGED_AGENT_UI: &str = "/usr/local/bin/cos-agent-ui";
const OVERLAY_FLAG: &str = "--overlay";
const VOICE_FLAG: &str = "--voice";
const QUERY_FLAG: &str = "--query";
const CONTEXT_FLAG: &str = "--context";
const CONTEXT_STDIN_FLAG: &str = "--context-stdin";
const READY_FD_FLAG: &str = "--ready-fd";

/// A host-owned, typed description of the context for an Ask Claw request.
///
/// Implementations must serialize as a JSON object. The runtime inserts the
/// stable `app` field, so implementations must not define that field.
pub trait Context: Serialize {
    /// Stable application identity presented to the Agent.
    const APP_ID: &'static str;
}

/// Errors produced while encoding a typed host context.
#[derive(Debug, thiserror::Error)]
pub enum ContextError {
    #[error("Ask Claw context serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Ask Claw context for {app} must serialize as a JSON object")]
    NotAnObject { app: &'static str },

    #[error("Ask Claw context for {app} must not define the reserved `app` field")]
    ReservedAppField { app: &'static str },

    #[error("Ask Claw app id must not be empty")]
    EmptyAppId,

    #[error("Ask Claw context is {actual} bytes; the limit is {limit} bytes")]
    TooLarge { actual: usize, limit: usize },

    #[error("Ask Claw fixed context fields leave no room for bounded text")]
    NoRoomForText,
}

/// Errors while reading and validating an explicit stdin activation.
#[derive(Debug, thiserror::Error)]
pub enum ActivationInputError {
    #[error("Ask Claw activation cannot contain both inline and stdin context")]
    ConflictingContext,

    #[error("Ask Claw activation is {actual} bytes; the limit is {limit} bytes")]
    TooLarge { actual: usize, limit: usize },

    #[error("failed to read Ask Claw activation stdin: {0}")]
    Io(#[from] io::Error),

    #[error("Ask Claw activation is malformed: {0}")]
    Malformed(#[from] serde_json::Error),

    #[error("Ask Claw activation context is invalid: {0}")]
    InvalidContext(&'static str),

    #[error("Ask Claw does not accept payload-bearing {0} arguments")]
    ProhibitedArgument(&'static str),

    #[error("Ask Claw stdin activation requires a valid readiness descriptor")]
    MissingReadyChannel,

    #[error("failed to signal Ask Claw readiness: {0}")]
    ReadyIo(#[source] io::Error),

    #[error(transparent)]
    Isolation(#[from] IsolationError),
}

/// Errors when the OS cannot protect anonymous handoff data from peer processes.
#[derive(Debug, thiserror::Error)]
pub enum IsolationError {
    #[error("Ask Claw private context handoff requires Linux")]
    UnsupportedPlatform,

    #[error("failed to read kernel.yama.ptrace_scope: {0}")]
    PtraceScopeIo(#[source] io::Error),

    #[error("invalid kernel.yama.ptrace_scope value: {0}")]
    InvalidPtraceScope(String),

    #[error("Ask Claw private context requires kernel.yama.ptrace_scope=2 or stronger")]
    InsufficientPtraceScope,

    #[error("failed to mark the current process non-dumpable: {0}")]
    NonDumpable(#[source] io::Error),
}

/// Errors from context preparation or the supervised process launch.
#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    #[error(transparent)]
    Context(#[from] ContextError),

    #[error("Ask Claw activation serialization failed: {0}")]
    ActivationSerialization(#[source] serde_json::Error),

    #[error("Ask Claw activation is {actual} bytes; the limit is {limit} bytes")]
    ActivationTooLarge { actual: usize, limit: usize },

    #[error(transparent)]
    Isolation(#[from] IsolationError),

    #[error("failed to start Ask Claw launcher worker: {0}")]
    ThreadSpawn(#[source] io::Error),

    #[error("failed to create Ask Claw readiness channel: {0}")]
    ReadyChannel(#[source] io::Error),

    #[error("packaged Ask Claw executable is unavailable: {0}")]
    ExecutableUnavailable(#[source] io::Error),

    #[error("packaged Ask Claw executable failed trust validation: {0}")]
    UntrustedExecutable(PathBuf),

    #[error("failed to spawn Ask Claw: {0}")]
    Spawn(#[source] io::Error),

    #[error("Ask Claw exited before readiness with status {0:?}")]
    ChildExited(Option<i32>),

    #[error("Ask Claw readiness handshake failed: {0}")]
    Ready(#[source] io::Error),

    #[error("Ask Claw readiness or context write timed out after {0:?}")]
    Timeout(Duration),

    #[error("failed to write Ask Claw context: {0}")]
    Write(#[source] io::Error),

    #[error("failed while reaping Ask Claw: {0}")]
    Wait(#[source] io::Error),
}

/// Single-instance activation sent to the Agent UI.
///
/// Shared launchers serialize this value to a transient child process's
/// anonymous stdin. Context-free launches may also serialize it through
/// libcosmic's existing single-instance D-Bus activation mechanism.
#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Activation {
    pub voice: bool,
    pub query: Option<String>,
    pub context: Option<String>,
}

impl Activation {
    fn overlay_with_context(context: String) -> Self {
        Self {
            context: Some(context),
            ..Self::default()
        }
    }
}

impl Display for Activation {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        let json = serde_json::to_string(self).map_err(|_| fmt::Error)?;
        formatter.write_str(&json)
    }
}

impl FromStr for Activation {
    type Err = serde_json::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        serde_json::from_str(value)
    }
}

/// Parsed Agent UI command-line state.
#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct UiArguments {
    pub overlay: bool,
    pub voice: bool,
    pub query: Option<String>,
    pub context: Option<String>,
    pub context_stdin: bool,
    pub ready_fd: Option<i32>,
    pub help: bool,
    pub unknown: Vec<String>,
}

impl UiArguments {
    /// Resolve this invocation into the activation sent to the UI instance.
    ///
    /// Stdin is read only when the explicit `--context-stdin` flag is present,
    /// so ordinary `exec.start` callers cannot block waiting for input.
    pub fn activation<R: Read>(
        &self,
        stdin: R,
    ) -> Result<Option<Activation>, ActivationInputError> {
        if self.context.is_some() {
            if self.context_stdin {
                return Err(ActivationInputError::ConflictingContext);
            }
            return Err(ActivationInputError::ProhibitedArgument("--context"));
        }
        if self.query.is_some() {
            return Err(ActivationInputError::ProhibitedArgument("--query"));
        }
        if self.context_stdin {
            let activation = read_activation(stdin)?;
            return Ok(self.overlay.then_some(activation));
        }
        Ok(self.overlay.then(|| Activation {
            voice: self.voice,
            query: self.query.clone(),
            context: self.context.clone(),
        }))
    }

    /// Resolve activation from process stdin and close that descriptor.
    pub fn activation_from_process_stdin(
        &self,
    ) -> Result<Option<Activation>, ActivationInputError> {
        if !self.context_stdin {
            if self.ready_fd.is_some() {
                return Err(ActivationInputError::MissingReadyChannel);
            }
            return self.activation(io::empty());
        }
        if self.context.is_some() {
            return Err(ActivationInputError::ConflictingContext);
        }
        if self.query.is_some() {
            return Err(ActivationInputError::ProhibitedArgument("--query"));
        }
        let ready_fd = self
            .ready_fd
            .ok_or(ActivationInputError::MissingReadyChannel)?;
        require_process_handoff_isolation()?;
        signal_ready(ready_fd)?;
        #[cfg(unix)]
        {
            use std::fs::File;
            use std::os::fd::FromRawFd;

            // SAFETY: this is called once during process argument parsing,
            // before any other UI code borrows stdin. File takes ownership so
            // fd 0 is closed immediately after the bounded read (or conflict).
            let stdin = unsafe { File::from_raw_fd(0) };
            self.activation(stdin)
        }
        #[cfg(not(unix))]
        {
            self.activation(std::io::stdin().lock())
        }
    }
}

/// Parse the Agent UI command line using the same contract the launcher emits.
pub fn parse_ui_arguments<I, S>(arguments: I) -> UiArguments
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut parsed = UiArguments::default();
    let mut arguments = arguments.into_iter().map(Into::into);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            OVERLAY_FLAG => parsed.overlay = true,
            VOICE_FLAG => parsed.voice = true,
            QUERY_FLAG => parsed.query = arguments.next(),
            CONTEXT_FLAG => parsed.context = arguments.next(),
            CONTEXT_STDIN_FLAG => parsed.context_stdin = true,
            READY_FD_FLAG => {
                parsed.ready_fd = arguments
                    .next()
                    .and_then(|value| value.parse::<i32>().ok())
                    .filter(|fd| *fd >= 3);
            }
            "-h" | "--help" => parsed.help = true,
            _ => parsed.unknown.push(argument),
        }
    }
    parsed
}

/// Usage text for the shared Agent UI activation contract.
pub const UI_USAGE: &str = "cos-agent-ui [--overlay] [--voice] [--context-stdin --ready-fd FD]";

fn encode_context<C: Context>(context: &C) -> Result<String, ContextError> {
    if C::APP_ID.trim().is_empty() {
        return Err(ContextError::EmptyAppId);
    }

    let serde_json::Value::Object(mut fields) = serde_json::to_value(context)? else {
        return Err(ContextError::NotAnObject { app: C::APP_ID });
    };
    if fields.contains_key("app") {
        return Err(ContextError::ReservedAppField { app: C::APP_ID });
    }
    fields.insert(
        "app".to_string(),
        serde_json::Value::String(C::APP_ID.to_string()),
    );
    serde_json::to_string(&fields).map_err(ContextError::Serialization)
}

/// Return whether a typed context fits the encoded context bound.
pub fn context_fits<C: Context>(context: &C) -> Result<bool, ContextError> {
    let context = encode_context(context)?;
    if context.len() > MAX_CONTEXT_BYTES {
        return Ok(false);
    }
    let activation = Activation::overlay_with_context(context);
    Ok(serde_json::to_vec(&activation)?.len() <= MAX_ACTIVATION_BYTES)
}

/// Serialize a typed app context into the stable, bounded representation.
pub fn serialize_context<C: Context>(context: &C) -> Result<String, ContextError> {
    let serialized = encode_context(context)?;
    let actual = serialized.len();
    if actual > MAX_CONTEXT_BYTES {
        return Err(ContextError::TooLarge {
            actual,
            limit: MAX_CONTEXT_BYTES,
        });
    }
    Ok(serialized)
}

/// Return the newest line-aligned UTF-8 suffix accepted by `fits`.
///
/// Whole old lines are discarded first. If even the newest line is too wide,
/// the function finds the largest character-aligned suffix, so callers never
/// split a UTF-8 code point and never estimate JSON escaping overhead.
pub fn newest_fitting_text_suffix<F>(text: &str, mut fits: F) -> Result<Option<&str>, ContextError>
where
    F: FnMut(&str) -> Result<bool, ContextError>,
{
    if fits(text)? {
        return Ok(Some(text));
    }

    for (index, byte) in text.bytes().enumerate() {
        if byte == b'\n' && index + 1 < text.len() {
            let candidate = &text[index + 1..];
            if fits(candidate)? {
                return Ok(Some(candidate));
            }
        }
    }

    let mut boundaries = text
        .char_indices()
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    boundaries.push(text.len());
    let mut low = 0;
    let mut high = boundaries.len();
    while low < high {
        let middle = low + (high - low) / 2;
        if fits(&text[boundaries[middle]..])? {
            high = middle;
        } else {
            low = middle + 1;
        }
    }
    if low == boundaries.len() {
        return Ok(None);
    }
    Ok(Some(&text[boundaries[low]..]))
}

fn read_activation<R: Read>(reader: R) -> Result<Activation, ActivationInputError> {
    let mut payload = Vec::new();
    reader
        .take(MAX_ACTIVATION_BYTES as u64 + 1)
        .read_to_end(&mut payload)?;
    if payload.len() > MAX_ACTIVATION_BYTES {
        return Err(ActivationInputError::TooLarge {
            actual: payload.len(),
            limit: MAX_ACTIVATION_BYTES,
        });
    }
    let activation: Activation = serde_json::from_slice(&payload)?;
    if let Some(context) = &activation.context {
        validate_activation_context(context)?;
    }
    Ok(activation)
}

fn validate_activation_context(context: &str) -> Result<(), ActivationInputError> {
    if context.len() > MAX_CONTEXT_BYTES {
        return Err(ActivationInputError::TooLarge {
            actual: context.len(),
            limit: MAX_CONTEXT_BYTES,
        });
    }
    let value: serde_json::Value = serde_json::from_str(context)?;
    let app = value
        .as_object()
        .and_then(|object| object.get("app"))
        .and_then(serde_json::Value::as_str)
        .filter(|app| !app.trim().is_empty());
    if app.is_none() {
        return Err(ActivationInputError::InvalidContext(
            "expected an object with a non-empty string `app` field",
        ));
    }
    Ok(())
}

/// Fail closed unless the current process can protect anonymous handoff data
/// from hostile same-UID peers, then make the process non-dumpable.
pub fn require_process_handoff_isolation() -> Result<(), IsolationError> {
    #[cfg(target_os = "linux")]
    {
        let value = std::fs::read_to_string("/proc/sys/kernel/yama/ptrace_scope")
            .map_err(IsolationError::PtraceScopeIo)?;
        validate_ptrace_scope(&value)?;
        set_current_process_non_dumpable()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err(IsolationError::UnsupportedPlatform)
    }
}

fn validate_ptrace_scope(value: &str) -> Result<(), IsolationError> {
    let level = value
        .trim()
        .parse::<u32>()
        .map_err(|_| IsolationError::InvalidPtraceScope(value.trim().to_string()))?;
    if level < 2 {
        return Err(IsolationError::InsufficientPtraceScope);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn set_current_process_non_dumpable() -> Result<(), IsolationError> {
    // SAFETY: prctl with PR_SET_DUMPABLE has no pointer arguments.
    if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } != 0 {
        return Err(IsolationError::NonDumpable(io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn signal_ready(fd: i32) -> Result<(), ActivationInputError> {
    use std::fs::File;

    // SAFETY: the descriptor is supplied by the direct parent specifically
    // for this one-shot handshake and ownership is transferred to File.
    let mut ready = unsafe { File::from_raw_fd(fd) };
    ready
        .write_all(READY_MESSAGE)
        .and_then(|_| ready.flush())
        .map_err(ActivationInputError::ReadyIo)
}

#[cfg(not(target_os = "linux"))]
fn signal_ready(_fd: i32) -> Result<(), ActivationInputError> {
    Err(ActivationInputError::Isolation(
        IsolationError::UnsupportedPlatform,
    ))
}

fn packaged_agent_ui() -> Result<PathBuf, LaunchError> {
    let path = PathBuf::from(PACKAGED_AGENT_UI);
    validate_executable(&path, true)?;
    Ok(path)
}

fn validate_executable(path: &Path, require_packaged_owner: bool) -> Result<(), LaunchError> {
    if !path.is_absolute() {
        return Err(LaunchError::UntrustedExecutable(path.to_path_buf()));
    }
    let metadata = std::fs::symlink_metadata(path).map_err(LaunchError::ExecutableUnavailable)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(LaunchError::UntrustedExecutable(path.to_path_buf()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        let mode = metadata.mode();
        if mode & 0o111 == 0
            || (require_packaged_owner && (metadata.uid() != 0 || mode & 0o022 != 0))
        {
            return Err(LaunchError::UntrustedExecutable(path.to_path_buf()));
        }
    }
    Ok(())
}

fn launch_argv(program: &Path) -> Vec<String> {
    vec![
        program.to_string_lossy().into_owned(),
        OVERLAY_FLAG.to_string(),
        CONTEXT_STDIN_FLAG.to_string(),
        READY_FD_FLAG.to_string(),
        READY_FD.to_string(),
    ]
}

fn prepare_launch<C: Context>(context: &C) -> Result<Vec<u8>, LaunchError> {
    let context = serialize_context(context)?;
    let activation = Activation::overlay_with_context(context);
    let payload = serde_json::to_vec(&activation).map_err(LaunchError::ActivationSerialization)?;
    if payload.len() > MAX_ACTIVATION_BYTES {
        return Err(LaunchError::ActivationTooLarge {
            actual: payload.len(),
            limit: MAX_ACTIVATION_BYTES,
        });
    }
    Ok(payload)
}

#[cfg(target_os = "linux")]
fn launch_prepared_with_program(
    payload: &[u8],
    timeout: Duration,
    program: &Path,
) -> Result<(), LaunchError> {
    let argv = launch_argv(program);
    let (mut ready_parent, ready_child) = UnixStream::pair().map_err(LaunchError::ReadyChannel)?;
    let ready_child_fd = ready_child.as_raw_fd();
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: pre_exec performs only async-signal-safe libc calls.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) != 0 {
                return Err(io::Error::last_os_error());
            }
            if ready_child_fd == READY_FD {
                if libc::fcntl(READY_FD, libc::F_SETFD, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
            } else {
                if libc::dup2(ready_child_fd, READY_FD) < 0 {
                    return Err(io::Error::last_os_error());
                }
                libc::close(ready_child_fd);
            }
            Ok(())
        });
    }

    let started = Instant::now();
    let mut child = command.spawn().map_err(LaunchError::Spawn)?;
    drop(ready_child);
    let remaining = timeout
        .checked_sub(started.elapsed())
        .ok_or(LaunchError::Timeout(timeout));
    let remaining = match remaining {
        Ok(remaining) => remaining,
        Err(error) => return terminate_child(child, error),
    };
    if let Err(error) = ready_parent.set_read_timeout(Some(remaining)) {
        return terminate_child(child, LaunchError::Ready(error));
    }
    let mut ready = [0_u8; READY_MESSAGE.len()];
    if let Err(error) = ready_parent.read_exact(&mut ready) {
        let failure = if matches!(
            error.kind(),
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
        ) {
            LaunchError::Timeout(timeout)
        } else if error.kind() == io::ErrorKind::UnexpectedEof {
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => break LaunchError::ChildExited(status.code()),
                    Ok(None) if started.elapsed() < timeout => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Ok(None) => break LaunchError::Timeout(timeout),
                    Err(wait_error) => break LaunchError::Wait(wait_error),
                }
            }
        } else {
            LaunchError::Ready(error)
        };
        return terminate_child(child, failure);
    }
    if ready != READY_MESSAGE {
        return terminate_child(
            child,
            LaunchError::Ready(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid readiness response",
            )),
        );
    }

    let Some(mut stdin) = child.stdin.take() else {
        return terminate_child(
            child,
            LaunchError::Write(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "Ask Claw stdin is unavailable",
            )),
        );
    };
    let payload = payload.to_vec();
    let (sender, receiver) = mpsc::sync_channel(1);
    let writer = match thread::Builder::new()
        .name("ask-claw-stdin".to_string())
        .spawn(move || {
            let result = stdin.write_all(&payload).and_then(|_| stdin.flush());
            drop(stdin);
            let _ = sender.send(result);
        }) {
        Ok(writer) => writer,
        Err(error) => return terminate_child(child, LaunchError::ThreadSpawn(error)),
    };
    let remaining = timeout
        .checked_sub(started.elapsed())
        .ok_or(LaunchError::Timeout(timeout));
    let write_result = match remaining {
        Ok(remaining) => receiver.recv_timeout(remaining),
        Err(error) => {
            let result = terminate_child(child, error);
            let _ = writer.join();
            return result;
        }
    };
    match write_result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            let result = terminate_child(child, LaunchError::Write(error));
            let _ = writer.join();
            return result;
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            let result = terminate_child(child, LaunchError::Timeout(timeout));
            let _ = writer.join();
            return result;
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            let result = terminate_child(
                child,
                LaunchError::Write(io::Error::other("Ask Claw stdin writer disconnected")),
            );
            let _ = writer.join();
            return result;
        }
    }
    let _ = writer.join();

    child.wait().map_err(LaunchError::Wait)?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn terminate_child(mut child: Child, error: LaunchError) -> Result<(), LaunchError> {
    match child.kill() {
        Ok(()) => {}
        Err(kill_error) if kill_error.kind() == io::ErrorKind::InvalidInput => {}
        Err(kill_error) => return Err(LaunchError::Wait(kill_error)),
    }
    child.wait().map_err(LaunchError::Wait)?;
    Err(error)
}

/// Schedule Ask Claw without blocking the caller's UI thread.
///
/// Serialization and process-isolation checks complete before returning.
/// Process launch failures are logged by the worker with their typed error.
pub fn launch<C: Context>(context: &C) -> Result<(), LaunchError> {
    require_process_handoff_isolation()?;
    let program = packaged_agent_ui()?;
    let payload = prepare_launch(context)?;
    spawn_prepared(payload, program).map(|_| ())
}

#[cfg(target_os = "linux")]
fn spawn_prepared(
    payload: Vec<u8>,
    program: PathBuf,
) -> Result<thread::JoinHandle<()>, LaunchError> {
    thread::Builder::new()
        .name("ask-claw-launch".to_string())
        .spawn(move || {
            if let Err(error) = launch_prepared_with_program(&payload, HANDSHAKE_TIMEOUT, &program)
            {
                eprintln!("Ask Claw launch failed: {error}");
            }
        })
        .map_err(LaunchError::ThreadSpawn)
}

#[cfg(not(target_os = "linux"))]
fn spawn_prepared(
    _payload: Vec<u8>,
    _program: PathBuf,
) -> Result<thread::JoinHandle<()>, LaunchError> {
    Err(LaunchError::Isolation(IsolationError::UnsupportedPlatform))
}

#[cfg(all(test, target_os = "linux"))]
fn launch_prepared_for_test(
    payload: &[u8],
    timeout: Duration,
    program: &Path,
) -> Result<(), LaunchError> {
    validate_executable(program, false)?;
    launch_prepared_with_program(payload, timeout, program)
}

#[cfg(all(test, target_os = "linux"))]
fn spawn_prepared_for_test(
    payload: Vec<u8>,
    program: PathBuf,
) -> Result<thread::JoinHandle<()>, LaunchError> {
    validate_executable(&program, false)?;
    spawn_prepared(payload, program)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/ask_claw.rs"
    ));
}
