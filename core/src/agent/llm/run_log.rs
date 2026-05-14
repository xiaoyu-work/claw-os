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

    /// Attach an explicit `app_id` to this record. Used by the
    /// app-gated paths (`cos agent chat --app <id>`) so allowed calls
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
    crate::filelock::append_locked(path, &line)
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
    use super::*;

    fn sample_engine() -> EngineInfo {
        EngineInfo {
            name: "llama-cpp".into(),
            version: "b4001".into(),
        }
    }

    fn sample_usage() -> Usage {
        Usage {
            input_tokens: 12,
            output_tokens: 34,
            ..Default::default()
        }
    }

    #[test]
    fn from_success_captures_engine_info() {
        let r = LlmRunRecord::from_success(
            "llama_local",
            "/tmp/m.gguf",
            Some(sample_engine()),
            FinishReason::Stop,
            &sample_usage(),
            42,
            Some("sess-123"),
        );
        assert_eq!(r.provider, "llama_local");
        assert_eq!(r.model, "/tmp/m.gguf");
        assert_eq!(r.engine_name.as_deref(), Some("llama-cpp"));
        assert_eq!(r.engine_version.as_deref(), Some("b4001"));
        assert_eq!(r.duration_ms, 42);
        assert_eq!(r.input_tokens, 12);
        assert_eq!(r.output_tokens, 34);
        assert_eq!(r.finish_reason, "stop");
        assert_eq!(r.status, "ok");
        assert!(r.error.is_none());
        assert_eq!(r.session_id.as_deref(), Some("sess-123"));
        assert_eq!(r.decision, "allowed");
        assert!(r.denial_reason.is_none());
        assert!(r.app_id.is_none());
    }

    #[test]
    fn from_success_omits_engine_for_cloud() {
        let r = LlmRunRecord::from_success(
            "openai_compat",
            "gpt-5",
            None,
            FinishReason::ToolUse,
            &sample_usage(),
            123,
            None,
        );
        assert!(r.engine_name.is_none());
        assert!(r.engine_version.is_none());
        assert_eq!(r.finish_reason, "tool_use");
        assert!(r.session_id.is_none());
    }

    #[test]
    fn from_success_treats_blank_engine_strings_as_absent() {
        let r = LlmRunRecord::from_success(
            "x",
            "y",
            Some(EngineInfo {
                name: String::new(),
                version: String::new(),
            }),
            FinishReason::Other,
            &Usage::default(),
            1,
            None,
        );
        assert!(r.engine_name.is_none());
        assert!(r.engine_version.is_none());
    }

    #[test]
    fn from_error_captures_message() {
        let r = LlmRunRecord::from_error(
            "llama_local",
            "/tmp/m.gguf",
            Some(sample_engine()),
            "library load failed",
            7,
            Some("s-1"),
        );
        assert_eq!(r.status, "error");
        assert_eq!(r.error.as_deref(), Some("library load failed"));
        assert_eq!(r.engine_version.as_deref(), Some("b4001"));
        assert_eq!(r.input_tokens, 0);
        assert_eq!(r.output_tokens, 0);
        assert_eq!(r.decision, "allowed");
        assert!(r.denial_reason.is_none());
    }

    /// Denials must surface as a distinct status + decision, carry the
    /// stable reason token, and attribute to the calling app.
    #[test]
    fn from_denial_sets_decision_and_reason() {
        let r = LlmRunRecord::from_denial(
            "summarize",
            "claude-sonnet-4",
            "budget_exceeded",
            "monthly unit cap reached (1000000/1000000)",
            3,
            Some("s-1"),
        );
        assert_eq!(r.provider, "gate");
        assert_eq!(r.status, "denied");
        assert_eq!(r.decision, "denied");
        assert_eq!(r.finish_reason, "denied");
        assert_eq!(r.denial_reason.as_deref(), Some("budget_exceeded"));
        assert_eq!(r.app_id.as_deref(), Some("summarize"));
        assert!(r.error.as_deref().unwrap().contains("monthly unit cap"));
        assert!(r.engine_name.is_none());
    }

    /// `with_app` attaches an app id to an otherwise app-agnostic
    /// success record. Used by the app-gated chat path.
    #[test]
    fn with_app_attaches_id() {
        let r = LlmRunRecord::from_success(
            "openai_compat",
            "gpt-4o",
            None,
            FinishReason::Stop,
            &sample_usage(),
            10,
            None,
        )
        .with_app("summarize");
        assert_eq!(r.app_id.as_deref(), Some("summarize"));
        assert_eq!(r.decision, "allowed");
    }

    /// A log line missing the new `decision` / `denial_reason` /
    /// `app_id` fields (pre-Phase-8 format) must still deserialise as
    /// an "allowed" record.
    #[test]
    fn legacy_jsonl_lines_default_to_allowed() {
        let legacy = r#"{
            "timestamp": "2026-04-01T00:00:00.000Z",
            "provider": "mock",
            "model": "mock-model",
            "duration_ms": 5,
            "finish_reason": "stop",
            "status": "ok"
        }"#;
        let r: LlmRunRecord = serde_json::from_str(legacy).expect("valid legacy line");
        assert_eq!(r.decision, "allowed");
        assert!(r.denial_reason.is_none());
        assert!(r.app_id.is_none());
    }

    /// `record_to_path` is what runs in tests because the public `record()`
    /// is a no-op under `cfg(test)`. Round-trip the JSON to make sure the
    /// schema actually reaches disk in the expected shape.
    #[test]
    fn record_to_path_round_trips_through_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ai.jsonl");
        let r = LlmRunRecord::from_success(
            "mock",
            "mock-model",
            None,
            FinishReason::Stop,
            &sample_usage(),
            5,
            None,
        );
        record_to_path(&r, &p).expect("write should succeed");
        record_to_path(&r, &p).expect("second write should append, not fail");

        let body = std::fs::read_to_string(&p).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2, "second write should append a line");
        let parsed: LlmRunRecord = serde_json::from_str(lines[0]).expect("valid jsonl");
        assert_eq!(parsed.provider, "mock");
        assert_eq!(parsed.model, "mock-model");
        assert_eq!(parsed.input_tokens, 12);
        assert_eq!(parsed.decision, "allowed");
    }

    /// Public `record()` MUST be a no-op in test builds — verifying by
    /// confirming it doesn't panic and the host's run log path doesn't
    /// get touched. We can't reliably stat the host log path without
    /// racing other tests, so just check that the call returns.
    #[test]
    fn record_is_a_noop_in_tests() {
        let r = LlmRunRecord::from_success(
            "mock",
            "mock-model",
            None,
            FinishReason::Stop,
            &Usage::default(),
            0,
            None,
        );
        // Doesn't panic, doesn't touch disk.
        record(&r);
    }

    #[test]
    fn finish_reason_str_covers_all_variants() {
        assert_eq!(finish_reason_str(FinishReason::Stop), "stop");
        assert_eq!(finish_reason_str(FinishReason::Length), "length");
        assert_eq!(finish_reason_str(FinishReason::ToolUse), "tool_use");
        assert_eq!(finish_reason_str(FinishReason::Refusal), "refusal");
        assert_eq!(
            finish_reason_str(FinishReason::ContentFilter),
            "content_filter"
        );
        assert_eq!(finish_reason_str(FinishReason::Other), "other");
    }
}
