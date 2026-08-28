//! Typed launch contract for the desktop "Ask Claw" overlay.
//!
//! Bundled desktop apps own small, app-specific context structs and implement
//! [`Context`] for them. This module owns the stable envelope, JSON encoding,
//! bounds, anonymous stdin handoff, Agent UI discovery, activation arguments,
//! and supervised process launch.

use std::fmt::{self, Display, Formatter};
use std::io::{self, Read};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::exec;

/// Maximum serialized host context accepted by the Agent UI.
pub const MAX_CONTEXT_BYTES: usize = 32 * 1024;

/// Maximum serialized activation accepted from the Agent UI's stdin.
pub const MAX_ACTIVATION_BYTES: usize = MAX_CONTEXT_BYTES * 2 + 1024;

const AGENT_UI_ENV: &str = "COS_AGENT_UI_BIN";
const DEFAULT_AGENT_UI: &str = "cos-agent-ui";
const OVERLAY_FLAG: &str = "--overlay";
const VOICE_FLAG: &str = "--voice";
const QUERY_FLAG: &str = "--query";
const CONTEXT_FLAG: &str = "--context";
const CONTEXT_STDIN_FLAG: &str = "--context-stdin";

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

    #[error("failed to launch Ask Claw: {0}")]
    Process(#[from] exec::StartError),
}

/// Single-instance activation sent to the Agent UI.
///
/// Shared launchers serialize this value to the child process's anonymous
/// stdin. The new UI process reads it before libcosmic forwards the same value
/// through its existing single-instance D-Bus activation mechanism.
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
        if self.context_stdin {
            if self.context.is_some() {
                return Err(ActivationInputError::ConflictingContext);
            }
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
            return self.activation(io::empty());
        }
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
            "-h" | "--help" => parsed.help = true,
            _ => parsed.unknown.push(argument),
        }
    }
    parsed
}

/// Usage text for the shared Agent UI activation contract.
pub const UI_USAGE: &str =
    "cos-agent-ui [--overlay] [--voice] [--query TEXT] [--context-stdin] [--context TEXT]";

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

fn agent_ui_executable() -> String {
    std::env::var(AGENT_UI_ENV).unwrap_or_else(|_| DEFAULT_AGENT_UI.to_string())
}

fn launch_argv() -> Vec<String> {
    vec![
        agent_ui_executable(),
        OVERLAY_FLAG.to_string(),
        CONTEXT_STDIN_FLAG.to_string(),
    ]
}

/// Launch Ask Claw through the runtime's supervised process boundary.
pub fn launch<C: Context>(context: &C) -> Result<exec::LaunchHandle, LaunchError> {
    let context = serialize_context(context)?;
    let activation = Activation::overlay_with_context(context);
    let payload = serde_json::to_vec(&activation).map_err(LaunchError::ActivationSerialization)?;
    if payload.len() > MAX_ACTIVATION_BYTES {
        return Err(LaunchError::ActivationTooLarge {
            actual: payload.len(),
            limit: MAX_ACTIVATION_BYTES,
        });
    }
    let argv = launch_argv();
    let argv = argv.iter().map(String::as_str).collect::<Vec<_>>();
    exec::start_with_stdin(&argv, &payload).map_err(LaunchError::Process)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/ask_claw.rs"
    ));
}
