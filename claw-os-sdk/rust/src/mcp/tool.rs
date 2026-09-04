use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde_json::{Map, Value};
use tokio::sync::{watch, Mutex as AsyncMutex};

use crate::generated::{McpCallContext, McpPrincipal};

use super::transport::TransportError;

/// A cooperative MCP call cancellation or authenticated deadline expiry.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
#[error("{reason}")]
pub struct CallCancelled {
    call_id: String,
    reason: String,
}

impl CallCancelled {
    pub fn call_id(&self) -> &str {
        &self.call_id
    }

    pub fn reason(&self) -> &str {
        &self.reason
    }
}

/// Errors produced while inspecting a call or reporting progress.
#[derive(Debug, thiserror::Error)]
pub enum CallContextError {
    #[error(transparent)]
    Cancelled(#[from] CallCancelled),
    #[error("{0} must be a finite non-negative number")]
    InvalidProgress(&'static str),
    #[error("progress output failed: {0}")]
    Progress(#[from] TransportError),
}

/// Optional fields on an outbound MCP progress notification.
#[derive(Debug, Clone, Default)]
pub struct Progress {
    pub total: Option<f64>,
    pub message: Option<String>,
}

impl Progress {
    pub fn total(mut self, total: f64) -> Self {
        self.total = Some(total);
        self
    }

    pub fn message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }
}

#[async_trait]
pub(crate) trait ProgressSink: Send + Sync {
    async fn emit_progress(
        &self,
        token: Value,
        progress: f64,
        update: Progress,
    ) -> Result<(), TransportError>;
}

pub(crate) struct Cancellation {
    cancelled: AtomicBool,
    reason: Mutex<Option<String>>,
    progress_gate: AsyncMutex<()>,
    signal: watch::Sender<bool>,
}

impl Cancellation {
    pub(crate) fn new() -> Self {
        let (signal, _) = watch::channel(false);
        Self {
            cancelled: AtomicBool::new(false),
            reason: Mutex::new(None),
            progress_gate: AsyncMutex::new(()),
            signal,
        }
    }

    pub(crate) async fn cancel(&self, reason: impl Into<String>) {
        let _gate = self.progress_gate.lock().await;
        if !self.cancelled.load(Ordering::Acquire) {
            *self.reason.lock().expect("cancellation reason poisoned") = Some(reason.into());
            self.cancelled.store(true, Ordering::Release);
            self.signal.send_replace(true);
        }
    }

    fn reason(&self) -> Option<String> {
        self.reason
            .lock()
            .expect("cancellation reason poisoned")
            .clone()
    }
}

/// Gateway-authenticated identity, lineage, deadline, cancellation, and progress
/// state for one manifest-declared tool call.
#[derive(Clone)]
pub struct CallContext {
    authenticated: Arc<McpCallContext>,
    cancellation: Arc<Cancellation>,
    progress_token: Option<Value>,
    progress_sink: Arc<dyn ProgressSink>,
}

impl std::fmt::Debug for CallContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CallContext")
            .field("authenticated", &self.authenticated)
            .field("deadline_unix_ms", &self.deadline_unix_ms())
            .field("progress_requested", &self.progress_requested())
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

impl CallContext {
    pub(crate) fn new(
        authenticated: McpCallContext,
        cancellation: Arc<Cancellation>,
        progress_token: Option<Value>,
        progress_sink: Arc<dyn ProgressSink>,
    ) -> Self {
        Self {
            authenticated: Arc::new(authenticated),
            cancellation,
            progress_token,
            progress_sink,
        }
    }

    /// The immutable generated wire snapshot validated by
    /// `validate_mcp_call_context`.
    pub fn authenticated(&self) -> &McpCallContext {
        &self.authenticated
    }

    pub fn caller(&self) -> &McpPrincipal {
        &self.authenticated.caller
    }

    pub fn call_id(&self) -> &str {
        &self.authenticated.call_id
    }

    pub fn trace_id(&self) -> &str {
        &self.authenticated.trace_id
    }

    pub fn parent_call_id(&self) -> Option<&str> {
        self.authenticated.parent_call_id.as_deref()
    }

    pub fn depth(&self) -> u8 {
        self.authenticated.depth
    }

    pub fn session_id(&self) -> Option<&str> {
        self.authenticated.session_id.as_deref()
    }

    pub fn task_id(&self) -> Option<&str> {
        self.authenticated.task_id.as_deref()
    }

    /// The authenticated Unix-millisecond deadline exactly as supplied on the
    /// wire.
    pub fn deadline_unix_ms(&self) -> Option<u64> {
        self.authenticated.deadline_unix_ms
    }

    pub fn progress_requested(&self) -> bool {
        self.progress_token.is_some()
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancellation.cancelled.load(Ordering::Acquire)
            || self.deadline_unix_ms().is_some_and(deadline_expired)
    }

    /// Wait until the caller cancels the request or its authenticated deadline
    /// expires.
    pub async fn cancelled(&self) {
        let mut signal = self.cancellation.signal.subscribe();
        loop {
            if self.is_cancelled() {
                return;
            }
            match self.deadline_unix_ms() {
                Some(deadline_unix_ms) => {
                    let Some(wait) = deadline_wait(deadline_unix_ms) else {
                        return;
                    };
                    tokio::select! {
                        changed = signal.changed() => {
                            if changed.is_err() {
                                return;
                            }
                        }
                        _ = tokio::time::sleep(wait) => {}
                    }
                }
                None => {
                    if signal.changed().await.is_err() {
                        return;
                    }
                }
            }
        }
    }

    /// Return a typed error when the call was cancelled or exceeded its
    /// authenticated deadline.
    pub fn check_cancelled(&self) -> Result<(), CallCancelled> {
        if !self.is_cancelled() {
            return Ok(());
        }
        let reason = self.cancellation.reason().unwrap_or_else(|| {
            if self.deadline_unix_ms().is_some_and(deadline_expired) {
                format!("call `{}` exceeded its deadline", self.call_id())
            } else {
                format!("call `{}` was cancelled", self.call_id())
            }
        });
        Err(CallCancelled {
            call_id: self.call_id().to_string(),
            reason,
        })
    }

    /// Emit `notifications/progress` when the request supplied
    /// `_meta.progressToken`. Calls without a token return successfully without
    /// writing a frame.
    pub async fn report_progress(
        &self,
        progress: f64,
        update: Progress,
    ) -> Result<(), CallContextError> {
        let Some(token) = self.progress_token.clone() else {
            self.check_cancelled()?;
            return Ok(());
        };
        if !progress.is_finite() || progress < 0.0 {
            return Err(CallContextError::InvalidProgress("progress"));
        }
        if update
            .total
            .is_some_and(|total| !total.is_finite() || total < 0.0)
        {
            return Err(CallContextError::InvalidProgress("total"));
        }
        let _gate = self.cancellation.progress_gate.lock().await;
        self.check_cancelled()?;
        self.progress_sink
            .emit_progress(token, progress, update)
            .await?;
        Ok(())
    }
}

const MAX_DEADLINE_WAIT: Duration = Duration::from_secs(24 * 60 * 60);

pub(crate) fn unix_time_ms() -> u64 {
    let milliseconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis();
    u64::try_from(milliseconds).unwrap_or(u64::MAX)
}

pub(crate) fn deadline_expired(deadline_unix_ms: u64) -> bool {
    unix_time_ms() >= deadline_unix_ms
}

pub(crate) fn deadline_wait(deadline_unix_ms: u64) -> Option<Duration> {
    let remaining = deadline_unix_ms.checked_sub(unix_time_ms())?;
    if remaining == 0 {
        return None;
    }
    Some(Duration::from_millis(remaining).min(MAX_DEADLINE_WAIT))
}

/// Explicit MCP tool result.
#[derive(Debug, Clone)]
pub struct ToolResult {
    pub(crate) text: String,
    pub(crate) is_error: bool,
    pub(crate) structured_content: Option<Map<String, Value>>,
}

impl ToolResult {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: false,
            structured_content: None,
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            text: message.into(),
            is_error: true,
            structured_content: None,
        }
    }

    pub fn structured(value: Value) -> Result<Self, ToolResultError> {
        let rendered = serde_json::to_string(&value)
            .map_err(|error| ToolResultError::Encode(error.to_string()))?;
        Self::structured_with_text(value, rendered)
    }

    pub fn structured_with_text(
        value: Value,
        text: impl Into<String>,
    ) -> Result<Self, ToolResultError> {
        let Value::Object(object) = value else {
            return Err(ToolResultError::NotObject);
        };
        Ok(Self {
            text: text.into(),
            is_error: false,
            structured_content: Some(object),
        })
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ToolResultError {
    #[error("structured MCP content must be an object")]
    NotObject,
    #[error("cannot render structured MCP content: {0}")]
    Encode(String),
}

/// Implementation for one tool declared in the authoritative App manifest.
#[async_trait]
pub trait Tool: Send + Sync + 'static {
    fn name(&self) -> &str;

    async fn handle(&self, args: Value, context: CallContext) -> ToolResult;
}
