//! Tool-execution progress events for streaming UIs.
//!
//! The provider-level [`crate::agent::llm::accumulate::StreamSink`]
//! only surfaces events that come out of the LLM stream itself
//! (`TextDelta`, `ToolUseStart`, `ToolInputDelta`, `ToolUse`,
//! `Message`, `Done`, `Warning`). Tool *execution* happens in the
//! agent-runtime layer ([`crate::agent::runtime::turn::run_turn`] and
//! its streaming twin) **after** the provider stream finishes — the
//! result blob is appended to the next request's message history but
//! is never echoed back through `StreamSink`.
//!
//! This module fills that gap. A [`ProgressSink`] is a separate sink
//! invoked by the turn-dispatch loop at two well-defined points:
//!
//! * [`ProgressSink::on_tool_start`] — right before the tool runs,
//!   so the UI can show a "running …" indicator with heartbeat dots
//!   while a slow tool (e.g. `cos_sysinfo largest_files /`) crunches
//!   the filesystem.
//! * [`ProgressSink::on_tool_result`] — right after the tool returns,
//!   so diagnostic sinks can record latency and a bounded output preview.
//!   User-facing sinks are wrapped by `runtime::presentation`, which keeps
//!   only tool identity and success/failure status.
//!
//! The trait is intentionally separate from `StreamSink`:
//!
//! * `StreamSink` lives in the provider crate's vocabulary — events
//!   come from `Provider::chat_stream`. Adding tool-execution events
//!   there would force every provider impl (anthropic, openai_compat,
//!   bedrock, gemini, copilot, ollama, mock) to ignore variants they
//!   never produce.
//! * `ProgressSink` lives in the runtime crate — its events come from
//!   the dispatch loop. A single sink implementor (e.g. `ChatSink` in
//!   `agent/mod.rs`) can implement both traits and write to the same
//!   terminal stream, so a diagnostic client can see a unified log:
//!
//!   ```text
//!   [tool_use id=toolu_X name=cos_sysinfo] {"command":"largest_files", ...}
//!   [tool_result id=toolu_X name=cos_sysinfo ok ms=8421 bytes=1240]
//!   {"search_root":"/", "files":[...], ...}
//!   ```
//!
//! Complete inputs and results remain in the runtime message trajectory and
//! session/audit stores; presentation sinks never need to expose them.

use std::collections::HashMap;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tokio::sync::oneshot;

/// Sink for tool-execution progress events emitted by the
/// agent-runtime turn loop. Implementations must be `Send + Sync`
/// because the dispatch loop runs inside a tokio task; they should
/// be cheap (each callback runs synchronously before the next tool
/// dispatch begins, so a slow sink back-pressures the turn).
///
/// Both methods default to no-ops so implementors can opt into only
/// the events they care about. The runtime calls them in pairs:
/// every `on_tool_start(id, …)` is followed by exactly one
/// `on_tool_result(id, …)` with the same id (modulo a panic in
/// `dispatch_tool`, which is fatal anyway).
pub trait ProgressSink: Send + Sync {
    /// Tool dispatch is about to begin. Useful for showing a spinner
    /// or "running…" indicator. The `input` is the parsed JSON the
    /// LLM produced; sinks should treat it as opaque.
    fn on_tool_start(&self, id: &str, name: &str, input: &Value) {
        let _ = (id, name, input);
    }

    /// Tool dispatch finished. `ok` is `!result.is_error`,
    /// `latency_ms` is wall-clock from `on_tool_start` to result,
    /// `bytes_returned` is the byte length of the full result body
    /// (sinks decide whether to display all of it), and
    /// `content_preview` is a renderer-truncated rendering of the
    /// result. Full content is preserved separately in the LLM
    /// message history and the session FTS5 store; the preview is
    /// purely for terminal display.
    fn on_tool_result(
        &self,
        id: &str,
        name: &str,
        ok: bool,
        latency_ms: u64,
        bytes_returned: usize,
        content_preview: &str,
    ) {
        let _ = (id, name, ok, latency_ms, bytes_returned, content_preview);
    }
}

/// Drop-everything implementation, used by non-interactive paths
/// (clawd jobs, JSON output mode) where progress events would just
/// pollute structured output. Hand this out as the default when a
/// caller doesn't care about progress.
pub struct NullProgressSink;

impl ProgressSink for NullProgressSink {}

/// Convenience constructor — `null_progress()` is shorter and reads
/// nicer at call sites than `Arc::new(NullProgressSink)`.
pub fn null_progress() -> Arc<dyn ProgressSink> {
    Arc::new(NullProgressSink)
}

// ---------------------------------------------------------------------------
// Preview / truncation helpers
// ---------------------------------------------------------------------------

/// Maximum bytes of a successful tool result to render to the
/// terminal. Errors are never truncated (they're small and the user
/// needs them all to debug). Tunable per-call via
/// [`preview_with_limit`].
pub const DEFAULT_PREVIEW_BYTES: usize = 2 * 1024;

/// Render a tool result for terminal display. Detects JSON and
/// pretty-prints it; otherwise returns the raw text. Successful
/// results are truncated at `DEFAULT_PREVIEW_BYTES` on a UTF-8 char
/// boundary; failures are returned in full.
///
/// This is the helper [`ProgressSink`] implementors should reach for
/// when they need a sensible default. Callers that want different
/// behaviour (e.g. an SSE client that always wants pretty JSON, or
/// a log scraper that wants the raw bytes) can format directly off
/// the full `result.content`.
pub fn render_preview(content: &str, ok: bool) -> String {
    if !ok {
        // Errors: full content, no pretty-print (error messages are
        // usually one line and we don't want to mangle stack
        // traces). Caller renders them with a visible marker.
        return content.to_string();
    }
    preview_with_limit(content, DEFAULT_PREVIEW_BYTES)
}

/// Render up to `max_bytes` of `content` for terminal display. Tries
/// JSON pretty-print first; falls back to raw text. Truncates on a
/// UTF-8 char boundary so the output is always valid UTF-8.
pub fn preview_with_limit(content: &str, max_bytes: usize) -> String {
    let body = match serde_json::from_str::<Value>(content) {
        Ok(v) => serde_json::to_string_pretty(&v).unwrap_or_else(|_| content.to_string()),
        Err(_) => content.to_string(),
    };
    truncate_utf8(&body, max_bytes)
}

/// Truncate `s` so that the returned string contains at most
/// `max_bytes` UTF-8 bytes, never splitting a code-point. If
/// truncation happens, appends `\n[… truncated]` so callers see
/// the marker; the marker is *not* counted against `max_bytes` (the
/// goal of the cap is to bound the body, not the formatted line).
fn truncate_utf8(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    // Walk char boundaries to find the largest prefix that fits.
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = String::with_capacity(end + 32);
    out.push_str(&s[..end]);
    out.push_str("\n[… truncated]");
    out
}

// ---------------------------------------------------------------------------
// Heartbeat helper — shared by terminal [`ProgressSink`] implementations.
// ---------------------------------------------------------------------------

/// Interval between heartbeat dots for an in-flight tool. Tuned to
/// "long enough that quick reads don't dot, short enough that a
/// stalled filesystem walk feels alive". Two seconds is the sweet
/// spot in practice.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(2);

/// Manages background heartbeat tasks for in-flight tool calls.
/// Designed to be embedded in [`ProgressSink`] implementations that
/// render to a terminal:
///
/// * `start(id)` spawns a tokio task that writes a `.` to stderr
///   every `HEARTBEAT_INTERVAL`, starting *after* the first interval
///   tick (so tools that finish in under 2s don't print anything).
/// * `stop(id)` cancels the task associated with `id` and reaps it.
///   Idempotent — extra `stop` calls (or `stop` for an unknown id)
///   are silent no-ops.
///
/// All methods are sync and cheap (Mutex lock + HashMap insert/take).
/// The actual writes happen on the spawned task, so they don't
/// serialise with the dispatch loop.
///
/// Must be created inside a tokio runtime — the spawn happens on
/// whichever runtime is current at `start()` time.
#[derive(Default)]
pub struct Heartbeat {
    inflight: Mutex<HashMap<String, oneshot::Sender<()>>>,
}

impl Heartbeat {
    pub fn new() -> Self {
        Self {
            inflight: Mutex::new(HashMap::new()),
        }
    }

    /// Begin a heartbeat for `id`. Spawns a tokio task that prints
    /// `.` to stderr every `HEARTBEAT_INTERVAL`. The first tick is
    /// skipped so sub-interval tools (the common case) stay silent.
    ///
    /// `prefix` is written **before** the first dot, so callers can
    /// signal "we're waiting on $name" if they want a richer label.
    /// Pass an empty string for plain dots only.
    pub fn start(&self, id: &str, prefix: &str) {
        let (tx, mut rx) = oneshot::channel::<()>();
        {
            let mut g = self.inflight.lock().expect("heartbeat lock");
            // Replace any stale entry under the same id. The previous
            // sender drops → its task observes the channel close on
            // its next tick and exits. Should never happen in
            // practice (every `start` is paired with a `stop`) but
            // defending here keeps memory bounded if a tool panics.
            g.insert(id.to_string(), tx);
        }
        let prefix = prefix.to_string();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(HEARTBEAT_INTERVAL);
            // First tick fires immediately by default; consume it so
            // the first user-visible dot is at +HEARTBEAT_INTERVAL.
            ticker.tick().await;
            let mut printed_prefix = false;
            loop {
                tokio::select! {
                    _ = &mut rx => break,
                    _ = ticker.tick() => {
                        let stderr = std::io::stderr();
                        let mut e = stderr.lock();
                        if !printed_prefix && !prefix.is_empty() {
                            let _ = write!(e, "{prefix}");
                            printed_prefix = true;
                        }
                        let _ = write!(e, ".");
                        let _ = e.flush();
                    }
                }
            }
        });
    }

    /// Stop the heartbeat for `id`. Idempotent.
    pub fn stop(&self, id: &str) {
        let tx = {
            let mut g = self.inflight.lock().expect("heartbeat lock");
            g.remove(id)
        };
        if let Some(tx) = tx {
            // Receiver might already have exited (race during
            // shutdown). Either way the heartbeat task closes
            // promptly. We don't care about the result.
            let _ = tx.send(());
        }
    }
}

// ---------------------------------------------------------------------------
// Terminal sink rendering helper.
// ---------------------------------------------------------------------------

/// Format the one-line `[tool_result …]` header that interactive
/// sinks (`ChatSink`, `LiveSink`) print to stderr before the body
/// preview. Centralised here so all terminal sinks render the same
/// shape and so a future renderer can swap colouring/icons in one
/// place.
///
/// Example: `[tool_result id=toolu_AB1 name=cos_sysinfo ok ms=8421 bytes=1240]`
pub fn format_result_header(
    id: &str,
    name: &str,
    ok: bool,
    latency_ms: u64,
    bytes_returned: usize,
) -> String {
    let status = if ok { "ok" } else { "ERROR" };
    format!("[tool_result id={id} name={name} {status} ms={latency_ms} bytes={bytes_returned}]")
}

/// Write a fully-rendered tool-result block (header + body) to
/// `out` followed by a trailing newline. Used by terminal
/// [`ProgressSink`] implementations.
pub fn write_result_block(
    mut out: impl Write,
    id: &str,
    name: &str,
    ok: bool,
    latency_ms: u64,
    bytes_returned: usize,
    preview: &str,
) -> std::io::Result<()> {
    let header = format_result_header(id, name, ok, latency_ms, bytes_returned);
    writeln!(out)?;
    writeln!(out, "{header}")?;
    if !preview.is_empty() {
        writeln!(out, "{preview}")?;
    }
    out.flush()
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/runtime/progress.rs"
    ));
}
