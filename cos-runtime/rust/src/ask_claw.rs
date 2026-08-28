//! Typed launch contract for the desktop "Ask Claw" overlay.
//!
//! Bundled desktop apps own small, app-specific context structs and implement
//! [`Context`] for them. This module owns the stable envelope, JSON encoding,
//! bounds, anonymous socket handoff, Agent UI discovery, activation arguments,
//! and bounded transient process launch.

use std::fmt::{self, Display, Formatter};
#[cfg(target_os = "linux")]
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
#[cfg(target_os = "linux")]
use std::os::fd::{AsRawFd, FromRawFd};
#[cfg(target_os = "linux")]
use std::os::unix::net::{UnixListener, UnixStream};
#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;
#[cfg(target_os = "linux")]
use std::path::Path;
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::process::{Child, Command, Stdio};
use std::str::FromStr;
#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(target_os = "linux")]
use std::thread;
use std::time::Duration;
#[cfg(target_os = "linux")]
use std::time::Instant;

use serde::{Deserialize, Serialize};

/// Maximum serialized host context accepted by the Agent UI.
pub const MAX_CONTEXT_BYTES: usize = 32 * 1024;

/// Maximum serialized activation accepted from the Agent UI's private socket.
pub const MAX_ACTIVATION_BYTES: usize = MAX_CONTEXT_BYTES * 2 + 1024;
pub const SDK_LAUNCHER_PROTOCOL: u32 = 1;
pub const PACKAGED_SDK_LAUNCHER: &str = "/usr/local/bin/cos-ask-claw-launcher";

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const ACTIVATION_FD: i32 = 3;
const EXECUTABLE_FD: i32 = 10;
const READY_MESSAGE: &[u8] = b"READY\n";
const SDK_READY_MESSAGE: &[u8] = b"READY 1\n";
const SDK_ACCEPTED_MESSAGE: &[u8] = b"ACCEPTED 1\n";
const PACKAGED_AGENT_UI: &str = "/usr/local/bin/cos-agent-ui";
const OVERLAY_FLAG: &str = "--overlay";
const VOICE_FLAG: &str = "--voice";
const QUERY_FLAG: &str = "--query";
const CONTEXT_FLAG: &str = "--context";
const CONTEXT_SOCKET_FLAG: &str = "--context-socket";
const ACTIVATION_FD_FLAG: &str = "--activation-fd";
#[cfg(target_os = "linux")]
static SDK_ENDPOINT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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

/// Errors while reading and validating an explicit socket activation.
#[derive(Debug, thiserror::Error)]
pub enum ActivationInputError {
    #[error("Ask Claw activation cannot contain both inline and socket context")]
    ConflictingContext,

    #[error("Ask Claw activation is {actual} bytes; the limit is {limit} bytes")]
    TooLarge { actual: usize, limit: usize },

    #[error("failed to read Ask Claw activation socket: {0}")]
    Io(#[from] io::Error),

    #[error("Ask Claw activation is malformed: {0}")]
    Malformed(#[from] serde_json::Error),

    #[error("Ask Claw activation context is invalid: {0}")]
    InvalidContext(&'static str),

    #[error("Ask Claw does not accept payload-bearing {0} arguments")]
    ProhibitedArgument(&'static str),

    #[error("Ask Claw socket activation requires a valid inherited descriptor")]
    MissingReadyChannel,

    #[error("failed to use Ask Claw activation socket: {0}")]
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
/// anonymous socket. Context-free launches may also serialize it through
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
    pub context_socket: bool,
    pub activation_fd: Option<i32>,
    pub help: bool,
    pub unknown: Vec<String>,
}

impl UiArguments {
    /// Resolve this invocation into the activation sent to the UI instance.
    ///
    /// The reader is consumed only for an explicit private activation,
    /// so ordinary UI launches cannot block waiting for input.
    pub fn activation<R: Read>(
        &self,
        input: R,
    ) -> Result<Option<Activation>, ActivationInputError> {
        if self.context.is_some() {
            if self.context_socket {
                return Err(ActivationInputError::ConflictingContext);
            }
            return Err(ActivationInputError::ProhibitedArgument("--context"));
        }
        if self.query.is_some() {
            return Err(ActivationInputError::ProhibitedArgument("--query"));
        }
        if self.context_socket {
            let activation = read_activation(input)?;
            return Ok(self.overlay.then_some(activation));
        }
        Ok(self.overlay.then(|| Activation {
            voice: self.voice,
            query: self.query.clone(),
            context: self.context.clone(),
        }))
    }

    /// Resolve activation from the inherited Unix socket and close it.
    pub fn activation_from_process_socket(
        &self,
    ) -> Result<Option<Activation>, ActivationInputError> {
        if !self.context_socket {
            if self.activation_fd.is_some() {
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
        let activation_fd = self
            .activation_fd
            .ok_or(ActivationInputError::MissingReadyChannel)?;
        require_process_handoff_isolation()?;
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::FromRawFd;
            use std::os::unix::net::UnixStream;

            // SAFETY: the direct parent transfers this descriptor once during
            // process startup; UnixStream owns and closes it on every path.
            let socket = unsafe { UnixStream::from_raw_fd(activation_fd) };
            self.activation_from_socket(socket)
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(ActivationInputError::Isolation(
                IsolationError::UnsupportedPlatform,
            ))
        }
    }

    #[cfg(target_os = "linux")]
    fn activation_from_socket(
        &self,
        mut socket: UnixStream,
    ) -> Result<Option<Activation>, ActivationInputError> {
        socket
            .write_all(READY_MESSAGE)
            .and_then(|_| socket.flush())
            .map_err(ActivationInputError::ReadyIo)?;
        let payload =
            read_frame(&mut socket, MAX_ACTIVATION_BYTES).map_err(|error| match error {
                FrameError::Io(error) => ActivationInputError::Io(error),
                FrameError::TooLarge(actual) => ActivationInputError::TooLarge {
                    actual,
                    limit: MAX_ACTIVATION_BYTES,
                },
            })?;
        let activation = read_activation(payload.as_slice())?;
        Ok(self.overlay.then_some(activation))
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
            CONTEXT_SOCKET_FLAG => parsed.context_socket = true,
            ACTIVATION_FD_FLAG => {
                parsed.activation_fd = arguments
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
pub const UI_USAGE: &str =
    "cos-agent-ui [--overlay] [--voice] [--context-socket --activation-fd FD]";

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

#[derive(Debug)]
enum FrameError {
    Io(io::Error),
    TooLarge(usize),
}

fn read_frame<R: Read>(reader: &mut R, limit: usize) -> Result<Vec<u8>, FrameError> {
    let mut header = [0_u8; 4];
    reader.read_exact(&mut header).map_err(FrameError::Io)?;
    let length = u32::from_be_bytes(header) as usize;
    if length > limit {
        return Err(FrameError::TooLarge(length));
    }
    let mut payload = vec![0_u8; length];
    reader.read_exact(&mut payload).map_err(FrameError::Io)?;
    Ok(payload)
}

fn write_frame<W: Write>(writer: &mut W, payload: &[u8]) -> io::Result<()> {
    let length = u32::try_from(payload.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "frame is too large"))?;
    writer.write_all(&length.to_be_bytes())?;
    writer.write_all(payload)?;
    writer.flush()
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
struct TrustedExecutable {
    file: File,
}

#[cfg(not(target_os = "linux"))]
struct TrustedExecutable;

#[cfg(target_os = "linux")]
fn packaged_agent_ui() -> Result<TrustedExecutable, LaunchError> {
    let path = PathBuf::from(PACKAGED_AGENT_UI);
    open_executable(&path, true)
}

#[cfg(not(target_os = "linux"))]
fn packaged_agent_ui() -> Result<TrustedExecutable, LaunchError> {
    Err(LaunchError::Isolation(IsolationError::UnsupportedPlatform))
}

#[cfg(target_os = "linux")]
fn open_executable(
    path: &Path,
    require_packaged_owner: bool,
) -> Result<TrustedExecutable, LaunchError> {
    if !path.is_absolute() {
        return Err(LaunchError::UntrustedExecutable(path.to_path_buf()));
    }
    if require_packaged_owner {
        for parent in [
            Path::new("/usr"),
            Path::new("/usr/local"),
            Path::new("/usr/local/bin"),
        ] {
            let metadata =
                std::fs::symlink_metadata(parent).map_err(LaunchError::ExecutableUnavailable)?;
            use std::os::unix::fs::MetadataExt;
            if metadata.file_type().is_symlink()
                || !metadata.is_dir()
                || metadata.uid() != 0
                || metadata.mode() & 0o022 != 0
            {
                return Err(LaunchError::UntrustedExecutable(parent.to_path_buf()));
            }
        }
    }
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(LaunchError::ExecutableUnavailable)?;
    let metadata = file
        .metadata()
        .map_err(LaunchError::ExecutableUnavailable)?;
    let mode = metadata.mode();
    if !metadata.is_file()
        || mode & 0o111 == 0
        || (require_packaged_owner && (metadata.uid() != 0 || mode & 0o022 != 0))
    {
        return Err(LaunchError::UntrustedExecutable(path.to_path_buf()));
    }
    Ok(TrustedExecutable { file })
}

fn launch_argv() -> Vec<String> {
    vec![
        format!("/proc/self/fd/{EXECUTABLE_FD}"),
        OVERLAY_FLAG.to_string(),
        CONTEXT_SOCKET_FLAG.to_string(),
        ACTIVATION_FD_FLAG.to_string(),
        ACTIVATION_FD.to_string(),
    ]
}

fn prepare_launch<C: Context>(context: &C) -> Result<Vec<u8>, LaunchError> {
    let context = serialize_context(context)?;
    serialize_activation(&Activation::overlay_with_context(context))
}

fn serialize_activation(activation: &Activation) -> Result<Vec<u8>, LaunchError> {
    let payload = serde_json::to_vec(activation).map_err(LaunchError::ActivationSerialization)?;
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
    program: TrustedExecutable,
) -> Result<(), LaunchError> {
    let argv = launch_argv();
    let (mut activation_parent, activation_child) =
        UnixStream::pair().map_err(LaunchError::ReadyChannel)?;
    let activation_child_fd = activation_child.as_raw_fd();
    let executable_fd = program.file.as_raw_fd();
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: pre_exec performs only async-signal-safe libc calls.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) != 0 {
                return Err(io::Error::last_os_error());
            }
            let executable_tmp = libc::fcntl(executable_fd, libc::F_DUPFD_CLOEXEC, 20);
            if executable_tmp < 0 {
                return Err(io::Error::last_os_error());
            }
            let activation_tmp = libc::fcntl(activation_child_fd, libc::F_DUPFD_CLOEXEC, 20);
            if activation_tmp < 0 {
                libc::close(executable_tmp);
                return Err(io::Error::last_os_error());
            }
            if libc::dup2(executable_tmp, EXECUTABLE_FD) < 0
                || libc::dup2(activation_tmp, ACTIVATION_FD) < 0
            {
                libc::close(executable_tmp);
                libc::close(activation_tmp);
                return Err(io::Error::last_os_error());
            }
            libc::close(executable_tmp);
            libc::close(activation_tmp);
            Ok(())
        });
    }

    let started = Instant::now();
    let mut child = command.spawn().map_err(LaunchError::Spawn)?;
    drop(program);
    drop(activation_child);
    let remaining = timeout
        .checked_sub(started.elapsed())
        .ok_or(LaunchError::Timeout(timeout));
    let remaining = match remaining {
        Ok(remaining) => remaining,
        Err(error) => return terminate_child(child, error),
    };
    if let Err(error) = activation_parent.set_read_timeout(Some(remaining)) {
        return terminate_child(child, LaunchError::Ready(error));
    }
    let mut ready = [0_u8; READY_MESSAGE.len()];
    if let Err(error) = activation_parent.read_exact(&mut ready) {
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

    let remaining = timeout
        .checked_sub(started.elapsed())
        .ok_or(LaunchError::Timeout(timeout));
    let remaining = match remaining {
        Ok(remaining) => remaining,
        Err(error) => return terminate_child(child, error),
    };
    if let Err(error) = activation_parent.set_write_timeout(Some(remaining)) {
        return terminate_child(child, LaunchError::Write(error));
    }
    if let Err(error) = write_frame(&mut activation_parent, payload) {
        let failure = if matches!(
            error.kind(),
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
        ) {
            LaunchError::Timeout(timeout)
        } else {
            LaunchError::Write(error)
        };
        return terminate_child(child, failure);
    }
    let _ = activation_parent.shutdown(std::net::Shutdown::Write);
    drop(activation_parent);

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

#[derive(Deserialize, Serialize)]
struct SdkLaunchRequest {
    protocol: u32,
    app: String,
    hint: Option<String>,
}

#[derive(Serialize)]
struct SdkContext<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    hint: Option<&'a str>,
}

impl Context for SdkContext<'_> {
    const APP_ID: &'static str = "sdk-app";
}

#[doc(hidden)]
pub fn run_sdk_launcher() -> Result<(), LaunchError> {
    #[cfg(target_os = "linux")]
    let expected_parent = unsafe { libc::getppid() };
    #[cfg(target_os = "linux")]
    let expected_uid = unsafe { libc::geteuid() };
    require_process_handoff_isolation()?;
    #[cfg(not(target_os = "linux"))]
    {
        return Err(LaunchError::Isolation(IsolationError::UnsupportedPlatform));
    }
    #[cfg(target_os = "linux")]
    {
        let arguments = std::env::args().skip(1).collect::<Vec<_>>();
        let protocol = arguments
            .windows(2)
            .find(|pair| pair[0] == "--protocol")
            .and_then(|pair| pair[1].parse::<u32>().ok())
            .ok_or_else(|| {
                LaunchError::Ready(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "missing SDK launcher protocol",
                ))
            })?;
        if protocol != SDK_LAUNCHER_PROTOCOL {
            return Err(LaunchError::Ready(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsupported SDK launcher protocol",
            )));
        }
        if expected_parent <= 1 {
            return Err(LaunchError::Ready(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "SDK launcher has no live direct parent",
            )));
        }
        let (listener, endpoint) = bind_sdk_listener().map_err(LaunchError::Ready)?;
        io::stdout()
            .write_all(format!("SOCKET {SDK_LAUNCHER_PROTOCOL} @{endpoint}\n").as_bytes())
            .and_then(|_| io::stdout().flush())
            .map_err(LaunchError::Ready)?;
        let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
        let mut client = accept_sdk_parent(&listener, expected_parent, expected_uid, deadline)?;
        drop(listener);
        client
            .set_write_timeout(Some(sdk_deadline_remaining(deadline)?))
            .map_err(LaunchError::Ready)?;
        client
            .write_all(SDK_READY_MESSAGE)
            .and_then(|_| client.flush())
            .map_err(LaunchError::Ready)?;
        let input = read_frame(&mut client, MAX_CONTEXT_BYTES).map_err(|error| match error {
            FrameError::Io(error) => LaunchError::Write(error),
            FrameError::TooLarge(actual) => LaunchError::ActivationTooLarge {
                actual,
                limit: MAX_CONTEXT_BYTES,
            },
        })?;
        if unsafe { libc::getppid() } != expected_parent {
            return Err(LaunchError::Ready(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "SDK launcher parent exited during handoff",
            )));
        }
        let request: SdkLaunchRequest =
            serde_json::from_slice(&input).map_err(LaunchError::ActivationSerialization)?;
        if request.protocol != SDK_LAUNCHER_PROTOCOL || request.app.trim().is_empty() {
            return Err(LaunchError::Ready(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid SDK launcher request",
            )));
        }
        let mut value = serde_json::to_value(SdkContext {
            hint: request.hint.as_deref(),
        })
        .map_err(ContextError::Serialization)?;
        value
            .as_object_mut()
            .unwrap()
            .insert("app".to_string(), serde_json::Value::String(request.app));
        let context = serde_json::to_string(&value).map_err(ContextError::Serialization)?;
        if context.len() > MAX_CONTEXT_BYTES {
            return Err(ContextError::TooLarge {
                actual: context.len(),
                limit: MAX_CONTEXT_BYTES,
            }
            .into());
        }
        let activation = Activation::overlay_with_context(context);
        let payload = serialize_activation(&activation)?;
        let program = packaged_agent_ui()?;
        client
            .set_write_timeout(Some(sdk_deadline_remaining(deadline)?))
            .map_err(LaunchError::Ready)?;
        client
            .write_all(SDK_ACCEPTED_MESSAGE)
            .and_then(|_| client.flush())
            .map_err(LaunchError::Ready)?;
        drop(client);
        launch_prepared_with_program(&payload, HANDSHAKE_TIMEOUT, program)
    }
}

#[cfg(target_os = "linux")]
fn bind_sdk_listener() -> io::Result<(UnixListener, String)> {
    let sequence = SDK_ENDPOINT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let endpoint = format!(
        "claw-ask-v{SDK_LAUNCHER_PROTOCOL}-{}-{sequence}",
        std::process::id()
    );
    if endpoint.len() + 1 > 108 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Ask Claw socket endpoint is too long",
        ));
    }
    // SAFETY: every syscall receives initialized values and checked lengths;
    // ownership of the successfully-bound descriptor transfers to UnixListener.
    unsafe {
        let fd = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0);
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut address: libc::sockaddr_un = std::mem::zeroed();
        address.sun_family = libc::AF_UNIX as libc::sa_family_t;
        for (target, source) in address.sun_path[1..].iter_mut().zip(endpoint.as_bytes()) {
            *target = *source as libc::c_char;
        }
        let length = std::mem::offset_of!(libc::sockaddr_un, sun_path) + 1 + endpoint.len();
        if libc::bind(
            fd,
            std::ptr::addr_of!(address).cast::<libc::sockaddr>(),
            length as libc::socklen_t,
        ) != 0
            || libc::listen(fd, 8) != 0
        {
            let error = io::Error::last_os_error();
            libc::close(fd);
            return Err(error);
        }
        Ok((UnixListener::from_raw_fd(fd), endpoint))
    }
}

#[cfg(target_os = "linux")]
fn accept_sdk_parent(
    listener: &UnixListener,
    expected_parent: libc::pid_t,
    expected_uid: libc::uid_t,
    deadline: Instant,
) -> Result<UnixStream, LaunchError> {
    accept_sdk_peer(
        listener,
        expected_parent,
        expected_uid,
        expected_parent,
        deadline,
    )
}

#[cfg(target_os = "linux")]
fn accept_sdk_peer(
    listener: &UnixListener,
    expected_peer: libc::pid_t,
    expected_uid: libc::uid_t,
    parent_to_monitor: libc::pid_t,
    deadline: Instant,
) -> Result<UnixStream, LaunchError> {
    listener.set_nonblocking(true).map_err(LaunchError::Ready)?;
    loop {
        if unsafe { libc::getppid() } != parent_to_monitor {
            return Err(LaunchError::Ready(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "SDK launcher parent exited before handoff",
            )));
        }
        match listener.accept() {
            Ok((stream, _)) => {
                if sdk_peer_is_expected(
                    sdk_peer_credentials(&stream).map_err(LaunchError::Ready)?,
                    expected_peer,
                    expected_uid,
                ) {
                    let remaining = sdk_deadline_remaining(deadline)?;
                    stream
                        .set_read_timeout(Some(remaining))
                        .map_err(LaunchError::Ready)?;
                    stream
                        .set_write_timeout(Some(remaining))
                        .map_err(LaunchError::Ready)?;
                    return Ok(stream);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err(LaunchError::Timeout(HANDSHAKE_TIMEOUT));
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(LaunchError::Ready(error)),
        }
    }
}

#[cfg(target_os = "linux")]
fn sdk_deadline_remaining(deadline: Instant) -> Result<Duration, LaunchError> {
    deadline
        .checked_duration_since(Instant::now())
        .ok_or(LaunchError::Timeout(HANDSHAKE_TIMEOUT))
}

#[cfg(target_os = "linux")]
fn sdk_peer_is_expected(
    actual: (libc::pid_t, libc::uid_t),
    expected_parent: libc::pid_t,
    expected_uid: libc::uid_t,
) -> bool {
    actual == (expected_parent, expected_uid)
}

#[cfg(target_os = "linux")]
fn sdk_peer_credentials(stream: &UnixStream) -> io::Result<(libc::pid_t, libc::uid_t)> {
    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: credentials and its exact initialized length are writable output
    // storage for SO_PEERCRED on this connected AF_UNIX socket.
    if unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            std::ptr::addr_of_mut!(credentials).cast(),
            &mut length,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    if length as usize != std::mem::size_of::<libc::ucred>() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid SDK peer credentials",
        ));
    }
    Ok((credentials.pid, credentials.uid))
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

/// Schedule a transient Ask Claw overlay with a bounded auto-submit query.
pub fn launch_query(query: &str) -> Result<(), LaunchError> {
    require_process_handoff_isolation()?;
    if query.len() > MAX_CONTEXT_BYTES {
        return Err(LaunchError::ActivationTooLarge {
            actual: query.len(),
            limit: MAX_CONTEXT_BYTES,
        });
    }
    let program = packaged_agent_ui()?;
    let payload = serialize_activation(&Activation {
        query: Some(query.to_string()),
        ..Activation::default()
    })?;
    spawn_prepared(payload, program).map(|_| ())
}

#[cfg(target_os = "linux")]
fn spawn_prepared(
    payload: Vec<u8>,
    program: TrustedExecutable,
) -> Result<thread::JoinHandle<()>, LaunchError> {
    thread::Builder::new()
        .name("ask-claw-launch".to_string())
        .spawn(move || {
            if let Err(error) = launch_prepared_with_program(&payload, HANDSHAKE_TIMEOUT, program) {
                eprintln!("Ask Claw launch failed: {error}");
            }
        })
        .map_err(LaunchError::ThreadSpawn)
}

#[cfg(not(target_os = "linux"))]
fn spawn_prepared(
    _payload: Vec<u8>,
    _program: TrustedExecutable,
) -> Result<thread::JoinHandle<()>, LaunchError> {
    Err(LaunchError::Isolation(IsolationError::UnsupportedPlatform))
}

#[cfg(all(test, target_os = "linux"))]
fn launch_prepared_for_test(
    payload: &[u8],
    timeout: Duration,
    program: &Path,
) -> Result<(), LaunchError> {
    let program = open_executable(program, false)?;
    launch_prepared_with_program(payload, timeout, program)
}

#[cfg(all(test, target_os = "linux"))]
fn spawn_prepared_for_test(
    payload: Vec<u8>,
    program: PathBuf,
) -> Result<thread::JoinHandle<()>, LaunchError> {
    let program = open_executable(&program, false)?;
    spawn_prepared(payload, program)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/ask_claw.rs"
    ));
}
