//! Typed launch contract for the desktop "Ask Claw" overlay.
//!
//! Bundled desktop apps own small, app-specific context structs and implement
//! [`Context`] for them. This module owns the stable envelope, JSON encoding,
//! context bound, Agent UI discovery, activation arguments, and supervised
//! process launch.

use std::fmt::{self, Display, Formatter};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::{exec, BridgeError};

/// Maximum serialized context accepted on the Agent UI command line.
pub const MAX_CONTEXT_BYTES: usize = 32 * 1024;

const AGENT_UI_ENV: &str = "COS_AGENT_UI_BIN";
const DEFAULT_AGENT_UI: &str = "cos-agent-ui";
const OVERLAY_FLAG: &str = "--overlay";
const VOICE_FLAG: &str = "--voice";
const QUERY_FLAG: &str = "--query";
const CONTEXT_FLAG: &str = "--context";

/// A host-owned, typed description of the context for an Ask Claw request.
///
/// Implementations must serialize as a JSON object. The runtime inserts the
/// stable `app` field, so implementations must not define that field.
pub trait Context: Serialize {
    /// Stable application identity presented to the Agent.
    const APP_ID: &'static str;
}

/// Errors produced before the overlay process is launched.
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
}

/// Errors from context preparation or the supervised process launch.
#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    #[error(transparent)]
    Context(#[from] ContextError),

    #[error("failed to launch Ask Claw: {0}")]
    Process(#[source] BridgeError),
}

/// Single-instance activation sent to the Agent UI.
///
/// The same type is used for initial CLI activation and libcosmic's
/// subsequent D-Bus activation payload.
#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
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

    fn arguments(&self) -> Vec<String> {
        let mut args = vec![OVERLAY_FLAG.to_string()];
        if self.voice {
            args.push(VOICE_FLAG.to_string());
        }
        if let Some(query) = &self.query {
            args.push(QUERY_FLAG.to_string());
            args.push(query.clone());
        }
        if let Some(context) = &self.context {
            args.push(CONTEXT_FLAG.to_string());
            args.push(context.clone());
        }
        args
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
    pub help: bool,
    pub unknown: Vec<String>,
}

impl UiArguments {
    pub fn activation(&self) -> Option<Activation> {
        self.overlay.then(|| Activation {
            voice: self.voice,
            query: self.query.clone(),
            context: self.context.clone(),
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
            "-h" | "--help" => parsed.help = true,
            _ => parsed.unknown.push(argument),
        }
    }
    parsed
}

/// Usage text for the shared Agent UI activation contract.
pub const UI_USAGE: &str = "cos-agent-ui [--overlay] [--voice] [--query TEXT] [--context TEXT]";

/// Serialize a typed app context into the stable, bounded wire representation.
pub fn serialize_context<C: Context>(context: &C) -> Result<String, ContextError> {
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

    let serialized = serde_json::to_string(&fields)?;
    let actual = serialized.len();
    if actual > MAX_CONTEXT_BYTES {
        return Err(ContextError::TooLarge {
            actual,
            limit: MAX_CONTEXT_BYTES,
        });
    }
    Ok(serialized)
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

/// Launch Ask Claw through the runtime's supervised process boundary.
pub fn launch<C: Context>(context: &C) -> Result<exec::StartResult, LaunchError> {
    let context = serialize_context(context)?;
    let activation = Activation::overlay_with_context(context);
    let argv = launch_argv(&activation);
    let argv = argv.iter().map(String::as_str).collect::<Vec<_>>();
    exec::start(&argv).map_err(LaunchError::Process)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/ask_claw.rs"
    ));
}
