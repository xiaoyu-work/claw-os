//! Typed launch contract for the desktop "Ask Claw" overlay.
//!
//! Bundled desktop apps own small, app-specific context structs and implement
//! [`Context`] for them. This module owns the stable envelope, JSON encoding,
//! context bound, private context-file handoff, Agent UI discovery, activation
//! arguments, and supervised process launch.

use std::fmt::{self, Display, Formatter};
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
#[cfg(not(unix))]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(not(unix))]
use std::time::UNIX_EPOCH;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::{exec, BridgeError};

/// Maximum serialized context accepted by the Agent UI.
pub const MAX_CONTEXT_BYTES: usize = 32 * 1024;

/// Maximum age of an abandoned context file before a later launch removes it.
pub const STALE_CONTEXT_AGE: Duration = Duration::from_secs(10 * 60);

const AGENT_UI_ENV: &str = "COS_AGENT_UI_BIN";
const DEFAULT_AGENT_UI: &str = "cos-agent-ui";
const CONTEXT_DIRECTORY: &str = "claw-os-ask-claw";
const CONTEXT_PREFIX: &str = ".context-";
const CONTEXT_SUFFIX: &str = ".json";
const CREATE_ATTEMPTS: usize = 16;
const OVERLAY_FLAG: &str = "--overlay";
const VOICE_FLAG: &str = "--voice";
const QUERY_FLAG: &str = "--query";
const CONTEXT_FLAG: &str = "--context";
const CONTEXT_FILE_FLAG: &str = "--context-file";

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

/// Errors produced by the private context-file handoff.
#[derive(Debug, thiserror::Error)]
pub enum ContextFileError {
    #[error("XDG_RUNTIME_DIR is required for Ask Claw context handoff")]
    MissingRuntimeDirectory,

    #[error("XDG_RUNTIME_DIR must be absolute")]
    RelativeRuntimeDirectory,

    #[error("Ask Claw runtime path is not a private real directory: {0}")]
    InsecureDirectory(PathBuf),

    #[error("Ask Claw context path is outside the private runtime directory: {0}")]
    InvalidPath(PathBuf),

    #[error("Ask Claw context file is not a private owned regular file: {0}")]
    InsecureFile(PathBuf),

    #[error("Ask Claw activation cannot contain both inline and file context")]
    ConflictingContext,

    #[error("Ask Claw context file is {actual} bytes; the limit is {limit} bytes")]
    TooLarge { actual: u64, limit: usize },

    #[error("Ask Claw context file is not UTF-8")]
    InvalidUtf8(#[from] std::string::FromUtf8Error),

    #[error("Ask Claw context file is not valid context JSON: {0}")]
    InvalidJson(#[from] serde_json::Error),

    #[error("Ask Claw context file has no string `app` field")]
    MissingApp,

    #[error("Ask Claw context file operation failed: {0}")]
    Io(#[from] io::Error),
}

/// Errors from context preparation or the supervised process launch.
#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    #[error(transparent)]
    Context(#[from] ContextError),

    #[error(transparent)]
    ContextFile(#[from] ContextFileError),

    #[error("failed to launch Ask Claw: {0}")]
    Process(#[source] BridgeError),
}

/// Single-instance activation sent to the Agent UI.
///
/// The same type is used for initial CLI activation and libcosmic's
/// subsequent D-Bus activation payload. Shared launchers populate
/// `context_file`; `context` remains only for legacy external callers and is
/// never populated by [`launch`].
#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Activation {
    pub voice: bool,
    pub query: Option<String>,
    pub context: Option<String>,
    pub context_file: Option<PathBuf>,
}

impl Activation {
    fn overlay_with_context_file(context_file: PathBuf) -> Self {
        Self {
            context_file: Some(context_file),
            ..Self::default()
        }
    }

    fn arguments(&self) -> Vec<String> {
        let mut args = vec![OVERLAY_FLAG.to_string()];
        if self.voice {
            args.push(VOICE_FLAG.to_string());
        }
        if let Some(query) = &self.query {
            args.push(QUERY_FLAG.to_string());
            args.push(query.clone());
        }
        if let Some(context_file) = &self.context_file {
            args.push(CONTEXT_FILE_FLAG.to_string());
            args.push(context_file.to_string_lossy().into_owned());
        } else if let Some(context) = &self.context {
            args.push(CONTEXT_FLAG.to_string());
            args.push(context.clone());
        }
        args
    }

    /// Read, validate, and unlink a private context file exactly once.
    ///
    /// Legacy inline context needs no resolution. A conflicting activation is
    /// rejected rather than allowing one source to shadow the other.
    pub fn resolve_context_file(&mut self) -> Result<(), ContextFileError> {
        let Some(path) = self.context_file.take() else {
            return Ok(());
        };
        if self.context.is_some() {
            remove_if_expected_context_path(&path);
            return Err(ContextFileError::ConflictingContext);
        }
        self.context = Some(read_context_file_once(&path)?);
        Ok(())
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
    pub context_file: Option<PathBuf>,
    pub help: bool,
    pub unknown: Vec<String>,
}

impl UiArguments {
    pub fn activation(&self) -> Option<Activation> {
        self.overlay.then(|| Activation {
            voice: self.voice,
            query: self.query.clone(),
            context: self.context.clone(),
            context_file: self.context_file.clone(),
        })
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
            CONTEXT_FILE_FLAG => parsed.context_file = arguments.next().map(PathBuf::from),
            "-h" | "--help" => parsed.help = true,
            _ => parsed.unknown.push(argument),
        }
    }
    parsed
}

/// Usage text for the shared Agent UI activation contract.
pub const UI_USAGE: &str =
    "cos-agent-ui [--overlay] [--voice] [--query TEXT] [--context-file PATH] [--context TEXT]";

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

/// Return whether a typed context fits the encoded context-file bound.
pub fn context_fits<C: Context>(context: &C) -> Result<bool, ContextError> {
    Ok(encode_context(context)?.len() <= MAX_CONTEXT_BYTES)
}

/// Serialize a typed app context into the stable, bounded wire representation.
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

fn agent_ui_executable() -> String {
    std::env::var(AGENT_UI_ENV).unwrap_or_else(|_| DEFAULT_AGENT_UI.to_string())
}

fn launch_argv(activation: &Activation) -> Vec<String> {
    let mut argv = Vec::with_capacity(1 + activation.arguments().len());
    argv.push(agent_ui_executable());
    argv.extend(activation.arguments());
    argv
}

struct StagedContextFile {
    path: PathBuf,
    cleanup: bool,
}

impl StagedContextFile {
    fn persist(mut self) {
        self.cleanup = false;
    }
}

impl Drop for StagedContextFile {
    fn drop(&mut self) {
        if self.cleanup {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn stage_context_file(contents: &str) -> Result<StagedContextFile, ContextFileError> {
    let directory = context_directory(true)?;
    let (path, mut file) = create_unique_context_file(&directory)?;
    let staged = StagedContextFile {
        path,
        cleanup: true,
    };
    file.write_all(contents.as_bytes())?;
    file.sync_all()?;
    drop(file);
    Ok(staged)
}

fn context_directory(create: bool) -> Result<PathBuf, ContextFileError> {
    let runtime_root =
        std::env::var_os("XDG_RUNTIME_DIR").ok_or(ContextFileError::MissingRuntimeDirectory)?;
    let runtime_root = PathBuf::from(runtime_root);
    if !runtime_root.is_absolute() {
        return Err(ContextFileError::RelativeRuntimeDirectory);
    }
    validate_private_directory(&runtime_root)?;

    let directory = runtime_root.join(CONTEXT_DIRECTORY);
    if create {
        let mut builder = DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&directory) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(ContextFileError::Io(error)),
        }
    }
    validate_private_directory(&directory)?;
    if create {
        cleanup_stale_context_files(&directory, SystemTime::now(), STALE_CONTEXT_AGE)?;
    }
    Ok(directory)
}

fn validate_private_directory(path: &Path) -> Result<(), ContextFileError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ContextFileError::InsecureDirectory(path.to_path_buf()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != current_uid()
            || (!skip_test_mode_validation() && metadata.mode() & 0o077 != 0)
        {
            return Err(ContextFileError::InsecureDirectory(path.to_path_buf()));
        }
    }
    Ok(())
}

fn create_unique_context_file(directory: &Path) -> Result<(PathBuf, File), ContextFileError> {
    for _ in 0..CREATE_ATTEMPTS {
        let token = context_token()?;
        let path = directory.join(format!("{CONTEXT_PREFIX}{token}{CONTEXT_SUFFIX}"));
        match open_private_new_file(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(ContextFileError::Io(error)),
        }
    }
    Err(ContextFileError::Io(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique Ask Claw context file",
    )))
}

#[cfg(unix)]
fn context_token() -> io::Result<String> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut random = [0_u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut random)?;
    let mut token = String::with_capacity(random.len() * 2);
    for byte in random {
        token.push(HEX[(byte >> 4) as usize] as char);
        token.push(HEX[(byte & 0x0f) as usize] as char);
    }
    Ok(token)
}

#[cfg(not(unix))]
fn context_token() -> io::Result<String> {
    static NEXT_FILE: AtomicU64 = AtomicU64::new(0);
    let sequence = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    Ok(format!("{}-{nonce}-{sequence}", std::process::id()))
}

fn open_private_new_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

fn open_private_context_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

fn validate_context_path(path: &Path) -> Result<(), ContextFileError> {
    let directory = context_directory(false)?;
    if path.parent() != Some(directory.as_path()) || !is_context_file_name(path) {
        return Err(ContextFileError::InvalidPath(path.to_path_buf()));
    }
    Ok(())
}

fn is_context_file_name(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let Some(token) = name
        .strip_prefix(CONTEXT_PREFIX)
        .and_then(|name| name.strip_suffix(CONTEXT_SUFFIX))
    else {
        return false;
    };
    !token.is_empty()
        && token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn read_context_file_once(path: &Path) -> Result<String, ContextFileError> {
    validate_context_path(path)?;
    let claimed_path = claim_context_file(path)?;
    let mut file = match open_private_context_file(&claimed_path) {
        Ok(file) => file,
        Err(error) => {
            let _ = fs::remove_file(&claimed_path);
            return Err(ContextFileError::Io(error));
        }
    };
    let metadata = match file.metadata() {
        Ok(metadata) => metadata,
        Err(error) => {
            let _ = fs::remove_file(&claimed_path);
            return Err(ContextFileError::Io(error));
        }
    };
    let validation = validate_private_context_file(&claimed_path, &metadata);
    let unlink = fs::remove_file(&claimed_path);
    validation?;
    unlink?;

    let mut bytes = Vec::with_capacity(metadata.len().min(MAX_CONTEXT_BYTES as u64) as usize);
    Read::by_ref(&mut file)
        .take(MAX_CONTEXT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_CONTEXT_BYTES {
        return Err(ContextFileError::TooLarge {
            actual: bytes.len() as u64,
            limit: MAX_CONTEXT_BYTES,
        });
    }
    let context = String::from_utf8(bytes)?;
    let value: serde_json::Value = serde_json::from_str(&context)?;
    if !value
        .as_object()
        .and_then(|object| object.get("app"))
        .is_some_and(serde_json::Value::is_string)
    {
        return Err(ContextFileError::MissingApp);
    }
    Ok(context)
}

fn claim_context_file(path: &Path) -> Result<PathBuf, ContextFileError> {
    let directory = path
        .parent()
        .ok_or_else(|| ContextFileError::InvalidPath(path.to_path_buf()))?;
    for _ in 0..CREATE_ATTEMPTS {
        let claimed_path = directory.join(format!(
            "{CONTEXT_PREFIX}claim-{}{CONTEXT_SUFFIX}",
            context_token()?
        ));
        if claimed_path.exists() {
            continue;
        }
        match fs::rename(path, &claimed_path) {
            Ok(()) => return Ok(claimed_path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(ContextFileError::Io(error)),
        }
    }
    Err(ContextFileError::Io(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not atomically claim Ask Claw context file",
    )))
}

fn validate_private_context_file(
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<(), ContextFileError> {
    if !metadata.is_file() || metadata.len() > MAX_CONTEXT_BYTES as u64 {
        return if metadata.len() > MAX_CONTEXT_BYTES as u64 {
            Err(ContextFileError::TooLarge {
                actual: metadata.len(),
                limit: MAX_CONTEXT_BYTES,
            })
        } else {
            Err(ContextFileError::InsecureFile(path.to_path_buf()))
        };
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != current_uid()
            || (!skip_test_mode_validation() && metadata.mode() & 0o777 != 0o600)
        {
            return Err(ContextFileError::InsecureFile(path.to_path_buf()));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn current_uid() -> u32 {
    // SAFETY: geteuid has no preconditions and does not dereference pointers.
    unsafe { libc::geteuid() }
}

#[cfg(all(unix, test))]
fn skip_test_mode_validation() -> bool {
    std::env::var_os("COS_ASK_CLAW_TEST_PERMISSIVE_FS").is_some()
}

#[cfg(all(unix, not(test)))]
fn skip_test_mode_validation() -> bool {
    false
}

fn remove_if_expected_context_path(path: &Path) {
    if validate_context_path(path).is_ok() {
        let _ = fs::remove_file(path);
    }
}

fn cleanup_stale_context_files(
    directory: &Path,
    now: SystemTime,
    max_age: Duration,
) -> Result<(), ContextFileError> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if !is_context_file_name(&path) {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)?;
        let stale = metadata
            .modified()
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age >= max_age);
        if stale && (metadata.is_file() || metadata.file_type().is_symlink()) {
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(ContextFileError::Io(error)),
            }
        }
    }
    Ok(())
}

/// Launch Ask Claw through the runtime's supervised process boundary.
pub fn launch<C: Context>(context: &C) -> Result<exec::LaunchHandle, LaunchError> {
    let context = serialize_context(context)?;
    let staged = stage_context_file(&context)?;
    let activation = Activation::overlay_with_context_file(staged.path.clone());
    let argv = launch_argv(&activation);
    let argv = argv.iter().map(String::as_str).collect::<Vec<_>>();
    let handle = exec::start(&argv).map_err(LaunchError::Process)?;
    staged.persist();
    Ok(handle)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/ask_claw.rs"
    ));
}
