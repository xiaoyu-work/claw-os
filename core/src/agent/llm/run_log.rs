//! Per-AI-call run record — Phase 2.4 audit trail, generalised in
//! Phase 8 to cover all modalities (chat, embed, image, audio,
//! vision).
//!
//! Every successful, errored, **or denied** AI call appends a JSON
//! line to `<log_dir>/ai.jsonl` capturing:
//!
//!   - timestamp / trace_id / session_id
//!   - provider name + model id
//!   - engine_name + engine_version (for local engines; cloud → null)
//!   - latency, token usage, finish_reason, error
//!   - decision (`"allowed"` | `"denied"`) + denial_reason
//!
//! ## Why a separate stream from `audit.rs`?
//!
//! `audit.rs` logs ONE record per `cos <app> <cmd>` invocation. A
//! single `cos agent ask` invocation can produce many AI calls (one
//! per turn). Operators need granular per-call records to diagnose
//! "this answer was bad" issues — knowing the exact engine version
//! that produced any given answer is essential for reproducibility.
//! The gate also emits a record for every **denied** call so abuse
//! attempts are visible, not just successful invocations.
//!
//! ## Test isolation
//!
//! In `#[cfg(test)]` builds, [`record`] is a no-op. Existing
//! `agent::runtime::*` tests use mock providers that exercise
//! `run_turn`; without this guard they'd start writing to the host's
//! `<log_dir>/ai.jsonl`. To unit-test the actual write path, use
//! [`record_to_path`] which takes an explicit destination.
//!
//! ## Best-effort writes
//!
//! Recording failures (read-only filesystem, disk full, etc.) MUST NOT
//! propagate into the chat response. Errors are swallowed with a
//! `tracing::warn!`.

use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use super::{EngineInfo, FinishReason, Usage};

/// Schema for one line of `<log_dir>/ai.jsonl`.
///
/// Fields use `Option` for "not applicable" (e.g. cloud providers
/// don't emit `engine_name`/`engine_version`). `serde(default)` keeps
/// log files forward-compatible if we add fields later.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LlmRunRecord {
    /// ISO 8601 UTC, e.g. `"2026-05-05T17:30:21.123Z"`.
    pub timestamp: String,

    /// `COS_TRACE_ID` env at record time, or `None`.
    #[serde(default)]
    pub trace_id: Option<String>,

    /// `COS_SPAN_ID` env at record time, or `None`.
    #[serde(default)]
    pub span_id: Option<String>,

    /// Agent session identifier (memory-DB session_id), or `None` if
    /// memory was not enabled.
    #[serde(default)]
    pub session_id: Option<String>,

    /// Provider stable name (`"llama_local"`, `"openai_compat"`,
    /// `"mock"`, ...).
    pub provider: String,

    /// Model identifier as the provider sees it.
    pub model: String,

    /// Local engine package name (`"llama-cpp"`). `None` for cloud.
    #[serde(default)]
    pub engine_name: Option<String>,

    /// Local engine package version (`"b4001"`). `None` for cloud.
    #[serde(default)]
    pub engine_version: Option<String>,

    pub duration_ms: u64,

    /// Tokens consumed by the prompt.
    #[serde(default)]
    pub input_tokens: u32,

    /// Tokens produced.
    #[serde(default)]
    pub output_tokens: u32,

    /// Cached prompt tokens hit (Anthropic prompt cache, etc.).
    /// Always 0 for providers without cache support. New in p4-usage;
    /// older log lines that lacked this field deserialise as 0.
    #[serde(default)]
    pub cache_read_tokens: u32,

    /// Tokens written to a prompt cache. Charged at a premium rate
    /// (Anthropic: 125 % of input). New in p4-usage.
    #[serde(default)]
    pub cache_write_tokens: u32,

    /// `"stop" | "length" | "tool_use" | "refusal" | "content_filter" | "other"`
    /// for allowed calls. `"denied"` when the gate refused.
    pub finish_reason: String,

    /// `"ok" | "error" | "denied"`. `"ok"` is the only fully
    /// successful state; `"error"` covers post-gate provider errors;
    /// `"denied"` matches `decision = "denied"`.
    pub status: String,

    /// Error message when `status == "error"`.
    #[serde(default)]
    pub error: Option<String>,

    /// `"allowed"` if the App–AI gate let the request through (regardless
    /// of whether the provider then succeeded); `"denied"` if the gate
    /// rejected it before the provider was contacted. Defaults to
    /// `"allowed"` so log lines written before this field existed
    /// continue to parse as successful gate-pass calls.
    #[serde(default = "default_decision")]
    pub decision: String,

    /// Short stable token describing **why** the gate denied
    /// (`"no_ai_policy" | "unknown_verb" | "bad_origin" |
    /// "origin_not_allowed" | "untrusted_verb_required" |
    /// "model_not_allowed" | "no_default_model" | "caps_denied" |
    /// "budget_exceeded" | "safety_block" | "unknown_app" |
    /// "internal"`). `None` when `decision == "allowed"`.
    #[serde(default)]
    pub denial_reason: Option<String>,

    /// App id this call was attributed to. `None` for the system
    /// agent (which uses `system.agent`) and for legacy log lines
    /// that didn't carry this field.
    #[serde(default)]
    pub app_id: Option<String>,

    /// AI verb the gate derived for this call (`"ai.chat"`,
    /// `"ai.image.generate"`, etc). Phase-8 multimodal addition;
    /// older log lines that lacked this field deserialise as `None`.
    /// System-agent records also leave this `None` because the agent
    /// always rides `ai.chat`.
    #[serde(default)]
    pub verb: Option<String>,
}

fn default_decision() -> String {
    "allowed".to_string()
}

impl LlmRunRecord {
    /// Build a record for a successful chat completion that passed
    /// the gate.
    pub fn from_success(
        provider: &str,
        model: &str,
        engine: Option<EngineInfo>,
        finish_reason: FinishReason,
        usage: &Usage,
        duration_ms: u64,
        session_id: Option<&str>,
    ) -> Self {
        Self {
            timestamp: now_iso8601(),
            trace_id: env_var_nonempty("COS_TRACE_ID"),
            span_id: env_var_nonempty("COS_SPAN_ID"),
            session_id: nonempty(session_id),
            provider: provider.to_string(),
            model: model.to_string(),
            engine_name: engine
                .as_ref()
                .map(|e| e.name.clone())
                .filter(|s| !s.is_empty()),
            engine_version: engine
                .as_ref()
                .map(|e| e.version.clone())
                .filter(|s| !s.is_empty()),
            duration_ms,
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_read_tokens: usage.cache_read_tokens,
            cache_write_tokens: usage.cache_write_tokens,
            finish_reason: finish_reason_str(finish_reason).to_string(),
            status: "ok".to_string(),
            error: None,
            decision: "allowed".to_string(),
            denial_reason: None,
            app_id: None,
            verb: None,
        }
    }

    /// Build a record for a chat call that passed the gate but failed
    /// at the provider (network error, 5xx, malformed response, ...).
    pub fn from_error(
        provider: &str,
        model: &str,
        engine: Option<EngineInfo>,
        error: &str,
        duration_ms: u64,
        session_id: Option<&str>,
    ) -> Self {
        Self {
            timestamp: now_iso8601(),
            trace_id: env_var_nonempty("COS_TRACE_ID"),
            span_id: env_var_nonempty("COS_SPAN_ID"),
            session_id: nonempty(session_id),
            provider: provider.to_string(),
            model: model.to_string(),
            engine_name: engine
                .as_ref()
                .map(|e| e.name.clone())
                .filter(|s| !s.is_empty()),
            engine_version: engine
                .as_ref()
                .map(|e| e.version.clone())
                .filter(|s| !s.is_empty()),
            duration_ms,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            finish_reason: "error".to_string(),
            status: "error".to_string(),
            error: Some(error.to_string()),
            decision: "allowed".to_string(),
            denial_reason: None,
            app_id: None,
            verb: None,
        }
    }

    /// Build a record for a call the App–AI gate refused.
    ///
    /// `denial_reason` is the stable token (see the field doc above);
    /// `error` is the human-readable explanation. The provider name
    /// is `"gate"` to make it obvious in `cos agent run-log` reports
    /// that the line was emitted by the gate, not by a real upstream.
    /// `model` is the model the caller asked for (or `""` if the call
    /// was rejected before model resolution).
    pub fn from_denial(
        app_id: &str,
        model: &str,
        denial_reason: &str,
        error: &str,
        duration_ms: u64,
        session_id: Option<&str>,
    ) -> Self {
        Self {
            timestamp: now_iso8601(),
            trace_id: env_var_nonempty("COS_TRACE_ID"),
            span_id: env_var_nonempty("COS_SPAN_ID"),
            session_id: nonempty(session_id),
            provider: "gate".to_string(),
            model: model.to_string(),
            engine_name: None,
            engine_version: None,
            duration_ms,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            finish_reason: "denied".to_string(),
            status: "denied".to_string(),
            error: Some(error.to_string()),
            decision: "denied".to_string(),
            denial_reason: Some(denial_reason.to_string()),
            app_id: nonempty(Some(app_id)),
            verb: None,
        }
    }

    /// Build a record for a kernel-mediated **Tool** invocation
    /// (`cos ai tool <name>`). Tools have no model and no provider in
    /// the LLM sense, but they share the same audit stream so
    /// operators can grep one file for everything an App did under
    /// the AI surface.
    ///
    /// `provider` is hard-coded `"kernel"` and `model` is set to the
    /// catalog tool name (prefixed `tool:` to make accidental
    /// conflation with a real provider impossible). The caps verb
    /// the gate checked is recorded in `verb` via [`Self::with_verb`].
    /// `decision` is `"allowed"` on success or `"denied"` on capability
    /// rejection so existing audit dashboards split correctly.
    pub fn from_tool_call(
        tool_name: &str,
        app_id: &str,
        verb: &str,
        decision: &str,
        denial_reason: Option<&str>,
        error: Option<&str>,
        duration_ms: u64,
        session_id: Option<&str>,
    ) -> Self {
        let mut rec = Self {
            timestamp: now_iso8601(),
            trace_id: env_var_nonempty("COS_TRACE_ID"),
            span_id: env_var_nonempty("COS_SPAN_ID"),
            session_id: nonempty(session_id),
            provider: "kernel".to_string(),
            model: format!("tool:{tool_name}"),
            engine_name: None,
            engine_version: None,
            duration_ms,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            finish_reason: decision.to_string(),
            status: if decision == "allowed" { "ok" } else { "denied" }
                .to_string(),
            error: error.map(|e| e.to_string()),
            decision: decision.to_string(),
            denial_reason: denial_reason.map(|s| s.to_string()),
            app_id: nonempty(Some(app_id)),
            verb: None,
        };
        rec = rec.with_verb(verb);
        rec
    }

    /// Attach an explicit `app_id` to this record. Used by the
    /// App-gated path (`cos ai chat --app <id>`) so allowed calls
    /// are attributed to the requesting app, not just the provider.
    pub fn with_app(mut self, app_id: &str) -> Self {
        self.app_id = nonempty(Some(app_id));
        self
    }

    /// Attach the AI verb the gate derived for this call. The gate
    /// calls this on every record (success / error / denial) so the
    /// audit stream can be aggregated by modality.
    pub fn with_verb(mut self, verb: &str) -> Self {
        if !verb.is_empty() {
            self.verb = Some(verb.to_string());
        }
        self
    }
}

/// Append a record to `<log_dir>/ai.jsonl`. Best-effort:
/// io errors are logged via `tracing::warn!` and swallowed.
///
/// In `#[cfg(test)]` builds this is a no-op. Use [`record_to_path`]
/// to test the write path with an explicit destination.
pub fn record(rec: &LlmRunRecord) {
    if cfg!(test) {
        return;
    }
    let path = crate::paths::ai_run_log_path();
    if let Err(e) = record_to_path(rec, &path) {
        tracing::warn!("run_log: failed to record ai call: {e}");
    }
}

/// Append a record to a specific path. Used by:
///   - production [`record`] (which routes through `paths::ai_run_log_path`).
///   - unit tests that need to assert on the on-disk format without
///     polluting the host's log dir.
pub fn record_to_path(rec: &LlmRunRecord, path: &Path) -> Result<(), String> {
    let line = serde_json::to_string(rec).map_err(|e| format!("serialize: {e}"))?;
    // MEDIUM-8: previously we routed through `filelock::append_locked`,
    // which holds an `flock(LOCK_EX)` and writes the line — but never
    // fsyncs the file and never rotates. A crash within seconds of a
    // call could lose audit records and the file grows unbounded.
    // We now use a local helper that:
    //   1. Holds the same `flock(LOCK_EX)` so concurrent writes are
    //      still serialised across processes (kernel + sidecars).
    //   2. fsyncs the file before releasing the lock so the kernel
    //      page cache is flushed.
    //   3. Rotates the file once it grows past
    //      `RUN_LOG_ROTATE_BYTES` (50 MiB) by renaming the old file
    //      to `.1` (overwriting any prior `.1`). Single-generation
    //      rotation is plenty for an append-only audit trail.
    // TODO(kernel-core): consider promoting this into `filelock` so
    // every audit-style log (audit.jsonl, watch history, …) gets
    // the same fsync + rotation guarantees.
    append_locked_with_rotation(path, &line, RUN_LOG_ROTATE_BYTES)
}

/// Rotation threshold for the AI run log. Above this size the file is
/// renamed to `<path>.1` on the next write. 50 MiB ≈ ~50–100k entries
/// at our typical record size — enough to retain a day's worth of
/// audit on a busy host without consuming gigabytes.
pub const RUN_LOG_ROTATE_BYTES: u64 = 50 * 1024 * 1024;

/// Append `line` to `path` under an exclusive `flock`, fsync the file
/// before releasing the lock, and rotate the file to `<path>.1`
/// when it exceeds `rotate_bytes` (single-generation rotation).
fn append_locked_with_rotation(
    path: &Path,
    line: &str,
    rotate_bytes: u64,
) -> Result<(), String> {
    use std::fs::{self, OpenOptions};
    use std::io::Write;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("open {}: {e}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if ret != 0 {
            return Err(format!(
                "flock LOCK_EX {}: {}",
                path.display(),
                std::io::Error::last_os_error()
            ));
        }
    }

    writeln!(file, "{line}").map_err(|e| format!("write {}: {e}", path.display()))?;
    // Flush user-space buffers, then ask the kernel to commit to
    // disk. We accept the throughput cost — audit must be durable.
    file.flush()
        .map_err(|e| format!("flush {}: {e}", path.display()))?;
    file.sync_data()
        .map_err(|e| format!("fsync {}: {e}", path.display()))?;

    // Check size AFTER write under the same lock. If we overshot,
    // rotate to `<path>.1`. The next write will create a fresh
    // `<path>` from scratch.
    let size = file
        .metadata()
        .map(|m| m.len())
        .map_err(|e| format!("stat {}: {e}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        unsafe {
            libc::flock(file.as_raw_fd(), libc::LOCK_UN);
        }
    }
    drop(file);

    if size > rotate_bytes {
        let mut backup = path.as_os_str().to_owned();
        backup.push(".1");
        let backup = std::path::PathBuf::from(backup);
        // Best-effort: a rename failure shouldn't block the write
        // we just successfully fsynced.
        if let Err(e) = fs::rename(path, &backup) {
            tracing::warn!(
                "run_log: rotation of {} → {} failed: {e}",
                path.display(),
                backup.display()
            );
        }
    }
    Ok(())
}

/// `<log_dir>/ai.jsonl`. Re-exported for callers that want the path
/// without going through `paths::*`.
pub fn run_log_path() -> PathBuf {
    crate::paths::ai_run_log_path()
}

// ------------------------------------------------------------------
// helpers
// ------------------------------------------------------------------

fn now_iso8601() -> String {
    Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

fn env_var_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

fn nonempty(s: Option<&str>) -> Option<String> {
    s.filter(|x| !x.is_empty()).map(str::to_string)
}

fn finish_reason_str(reason: FinishReason) -> &'static str {
    match reason {
        FinishReason::Stop => "stop",
        FinishReason::Length => "length",
        FinishReason::ToolUse => "tool_use",
        FinishReason::Refusal => "refusal",
        FinishReason::ContentFilter => "content_filter",
        FinishReason::Other => "other",
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/llm/run_log.rs"
    ));
}
