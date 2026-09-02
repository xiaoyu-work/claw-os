//! Usage aggregator over the `ai.jsonl` run-log stream.
//!
//! The audit trail [`crate::agent::llm::run_log`] writes one JSON
//! line per AI call (chat, embed, image, audio, vision — plus every
//! gate denial). This module reads that stream and produces summary
//! breakdowns: totals, per-provider, per-model, per-session.
//!
//! ## Why a separate module
//!
//! The run log is the *write* side of the per-call audit trail. The
//! aggregator is the *read* side. Separating them keeps the writer
//! tiny (zero-state, zero-deps beyond serde/chrono) and lets the
//! reader evolve independently — adding new groupings or filters
//! never has to touch the writer hot path.
//!
//! ## Robustness to log mutation
//!
//! The log is owned by the agent runtime; nothing else writes it.
//! But it can grow unbounded, be log-rotated, or be partially
//! truncated by an unclean shutdown. The reader handles these:
//!
//!   * Malformed lines are skipped (counted in `parse_errors`).
//!   * Empty lines are skipped silently.
//!   * Files that don't exist yet (no calls have been made) are
//!     treated as empty — `aggregate_default` returns a zero summary.
//!   * Partial last lines (no trailing newline) are still parsed.
//!
//! ## Filtering
//!
//! [`UsageQuery`] accepts optional filters: time range, provider,
//! model, session id. All filters are AND-combined. The full
//! aggregator runs when the query is left at default.
//!
//! Library + CLI wrapper. The CLI surface is `cos agent usage` in
//! [`crate::agent::mod`], which calls into [`aggregate_path_filtered`]
//! after parsing scope + flags.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::run_log::{run_log_path, LlmRunRecord};

pub const MAX_QUERY_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_QUERY_RECORDS: usize = 200_000;
const MAX_RECORD_BYTES: u64 = 1024 * 1024;
const MAX_BREAKDOWN_BUCKETS: usize = 1024;
const MAX_BREAKDOWN_KEY_BYTES: usize = 128;

/// Optional filters for [`aggregate_filtered`].
///
/// All filters AND together. `None`/empty fields don't constrain the
/// result.
#[derive(Debug, Clone, Default)]
pub struct UsageQuery {
    /// Inclusive lower bound on `timestamp`.
    pub since: Option<DateTime<Utc>>,
    /// Exclusive upper bound on `timestamp`.
    pub until: Option<DateTime<Utc>>,
    /// Provider name match (exact). `None` = any.
    pub provider: Option<String>,
    /// Model id match (exact). `None` = any.
    pub model: Option<String>,
    /// Session id match (exact). `None` = any.
    pub session_id: Option<String>,
    /// App id match (exact). `None` = any. App-less records (system
    /// calls) never match a `Some(_)` filter.
    pub app_id: Option<String>,
    /// Derived modality verb match (exact). `None` = any. Records
    /// without a verb (e.g. older log entries) never match a
    /// `Some(_)` filter.
    pub verb: Option<String>,
    /// `Some(true)` = only successful calls, `Some(false)` = only
    /// errored calls, `None` = both.
    pub status_ok: Option<bool>,
}

impl UsageQuery {
    /// True if `rec` matches every active filter.
    pub fn matches(&self, rec: &LlmRunRecord) -> bool {
        if let Some(p) = &self.provider {
            if &rec.provider != p {
                return false;
            }
        }
        if let Some(m) = &self.model {
            if &rec.model != m {
                return false;
            }
        }
        if let Some(s) = &self.session_id {
            if rec.session_id.as_deref() != Some(s.as_str()) {
                return false;
            }
        }
        if let Some(a) = &self.app_id {
            if rec.app_id.as_deref() != Some(a.as_str()) {
                return false;
            }
        }
        if let Some(v) = &self.verb {
            if rec.verb.as_deref() != Some(v.as_str()) {
                return false;
            }
        }
        if let Some(ok) = self.status_ok {
            let is_ok = rec.status == "ok";
            if is_ok != ok {
                return false;
            }
        }
        if self.since.is_some() || self.until.is_some() {
            let Some(ts) = parse_ts(&rec.timestamp) else {
                // Records with unparseable timestamps fail any
                // time filter.
                return false;
            };
            if let Some(s) = self.since {
                if ts < s {
                    return false;
                }
            }
            if let Some(u) = self.until {
                if ts >= u {
                    return false;
                }
            }
        }
        true
    }
}

/// Parsed JSONL plus a count of malformed lines we skipped.
#[derive(Debug)]
pub struct ReadResult {
    pub records: Vec<LlmRunRecord>,
    pub parse_errors: usize,
}

/// Read every record from `path`. Missing file → empty result.
/// Malformed lines → skipped (counted).
pub fn read_records(path: &Path) -> ReadResult {
    if !path.exists() {
        return ReadResult {
            records: Vec::new(),
            parse_errors: 0,
        };
    }
    let body = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => {
            return ReadResult {
                records: Vec::new(),
                parse_errors: 0,
            };
        }
    };
    parse_records(&body)
}

pub fn read_records_bounded(
    reader: impl Read,
    max_bytes: u64,
) -> Result<ReadResult, String> {
    let mut bytes = Vec::new();
    reader
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read AI usage log: {error}"))?;
    if bytes.len() as u64 > max_bytes {
        return Err(format!(
            "AI usage log exceeds the {} byte query limit",
            max_bytes
        ));
    }
    let body =
        String::from_utf8(bytes).map_err(|_| "AI usage log is not valid UTF-8".to_string())?;
    Ok(parse_records(&body))
}

fn parse_records(body: &str) -> ReadResult {
    let mut records = Vec::new();
    let mut parse_errors = 0usize;
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<LlmRunRecord>(line) {
            Ok(r) => records.push(r),
            Err(_) => parse_errors += 1,
        }
    }
    ReadResult {
        records,
        parse_errors,
    }
}

/// Per-bucket totals (used inside the breakdown maps).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Totals {
    pub calls: u64,
    pub success: u64,
    pub error: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub total_duration_ms: u64,
    pub finish_reasons: BTreeMap<String, u64>,
    pub errors: u64,
}

impl Totals {
    fn add(&mut self, rec: &LlmRunRecord) {
        self.calls += 1;
        if rec.status == "ok" {
            self.success += 1;
        } else {
            self.error += 1;
        }
        self.input_tokens += rec.input_tokens as u64;
        self.output_tokens += rec.output_tokens as u64;
        self.cache_read_tokens += rec.cache_read_tokens as u64;
        self.cache_write_tokens += rec.cache_write_tokens as u64;
        self.total_duration_ms += rec.duration_ms;
        if rec.error.is_some() {
            self.errors += 1;
        }
        let finish_reason = Self::canonical_finish_reason(&rec.finish_reason);
        *self
            .finish_reasons
            .entry(finish_reason.to_string())
            .or_default() += 1;
    }

    fn canonical_finish_reason(reason: &str) -> &'static str {
        match reason {
            "stop" => "stop",
            "length" => "length",
            "tool_use" => "tool_use",
            "refusal" => "refusal",
            "content_filter" => "content_filter",
            "error" => "error",
            "denied" => "denied",
            _ => "other",
        }
    }
}

/// Top-level summary produced by [`aggregate`] / [`aggregate_filtered`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageSummary {
    pub total: Totals,
    pub by_provider: BTreeMap<String, Totals>,
    pub by_model: BTreeMap<String, Totals>,
    /// Only includes records that had a `session_id` set.
    pub by_session: BTreeMap<String, Totals>,
    /// Only includes records that had an `app_id` set (i.e. app-gated
    /// calls). System calls and untagged records are excluded.
    pub by_app: BTreeMap<String, Totals>,
    /// Only includes records that had a derived `verb` set.
    pub by_verb: BTreeMap<String, Totals>,
    /// Number of malformed log lines encountered.
    pub parse_errors: usize,
    /// Physical lines scanned from the selected log.
    pub log_lines: u64,
    /// Physical bytes scanned from the selected log.
    pub log_bytes: u64,
    /// At least one distinct or oversized breakdown key was omitted.
    pub breakdown_truncated: bool,
}

/// Aggregate every record. No filtering.
pub fn aggregate(records: &[LlmRunRecord]) -> UsageSummary {
    aggregate_filtered(records, &UsageQuery::default())
}

/// Aggregate records that satisfy `query`.
pub fn aggregate_filtered(records: &[LlmRunRecord], query: &UsageQuery) -> UsageSummary {
    let mut s = UsageSummary::default();
    for rec in records {
        if !query.matches(rec) {
            continue;
        }
        add_record(&mut s, rec);
    }
    s
}

/// Read + aggregate the default `ai.jsonl`. Convenience wrapper.
pub fn aggregate_default() -> UsageSummary {
    let path = run_log_path();
    let read = read_records(&path);
    let mut s = aggregate(&read.records);
    s.parse_errors = read.parse_errors;
    s
}

/// Read + aggregate from a specific path. Used by tests; also useful
/// for callers that route logs to alternate destinations.
pub fn aggregate_path(path: &Path) -> UsageSummary {
    let read = read_records(path);
    let mut s = aggregate(&read.records);
    s.parse_errors = read.parse_errors;
    s
}

/// Read + aggregate from a specific path with filters applied.
pub fn aggregate_path_filtered(path: &Path, query: &UsageQuery) -> UsageSummary {
    let read = read_records(path);
    let mut s = aggregate_filtered(&read.records, query);
    s.parse_errors = read.parse_errors;
    s
}

pub fn aggregate_reader_filtered(
    reader: impl Read,
    query: &UsageQuery,
    max_bytes: u64,
) -> Result<UsageSummary, String> {
    use std::io::BufRead;

    let mut reader = std::io::BufReader::new(reader.take(max_bytes.saturating_add(1)));
    let mut summary = UsageSummary::default();
    let mut bytes_read = 0u64;
    let mut records_read = 0usize;
    let mut line = Vec::new();
    loop {
        line.clear();
        let count = {
            let mut bounded_line = std::io::Read::by_ref(&mut reader)
                .take(MAX_RECORD_BYTES.saturating_add(1));
            bounded_line
                .read_until(b'\n', &mut line)
                .map_err(|error| format!("read AI usage log: {error}"))?
        };
        if count == 0 {
            break;
        }
        summary.log_lines += 1;
        bytes_read = bytes_read.saturating_add(count as u64);
        summary.log_bytes = bytes_read;
        if bytes_read > max_bytes {
            return Err(format!(
                "AI usage log exceeds the {} byte query limit",
                max_bytes
            ));
        }
        if count as u64 > MAX_RECORD_BYTES {
            return Err(format!(
                "AI usage record exceeds the {} byte query limit",
                MAX_RECORD_BYTES
            ));
        }
        let line = std::str::from_utf8(&line)
            .map_err(|_| "AI usage log contains non-UTF-8 data".to_string())?
            .trim();
        if line.is_empty() {
            continue;
        }
        records_read += 1;
        if records_read > MAX_QUERY_RECORDS {
            return Err(format!(
                "AI usage log exceeds the {} record query limit",
                MAX_QUERY_RECORDS
            ));
        }
        match serde_json::from_str::<LlmRunRecord>(line) {
            Ok(record) if query.matches(&record) => add_record(&mut summary, &record),
            Ok(_) => {}
            Err(_) => summary.parse_errors += 1,
        }
    }
    Ok(summary)
}

fn add_record(summary: &mut UsageSummary, record: &LlmRunRecord) {
    summary.total.add(record);
    add_bucket(
        &mut summary.by_provider,
        &record.provider,
        record,
        &mut summary.breakdown_truncated,
    );
    add_bucket(
        &mut summary.by_model,
        &record.model,
        record,
        &mut summary.breakdown_truncated,
    );
    if let Some(session_id) = &record.session_id {
        add_bucket(
            &mut summary.by_session,
            session_id,
            record,
            &mut summary.breakdown_truncated,
        );
    }
    if let Some(app_id) = &record.app_id {
        add_bucket(
            &mut summary.by_app,
            app_id,
            record,
            &mut summary.breakdown_truncated,
        );
    }
    if let Some(verb) = &record.verb {
        add_bucket(
            &mut summary.by_verb,
            verb,
            record,
            &mut summary.breakdown_truncated,
        );
    }
}

fn add_bucket(
    buckets: &mut BTreeMap<String, Totals>,
    key: &str,
    record: &LlmRunRecord,
    truncated: &mut bool,
) {
    if key.len() > MAX_BREAKDOWN_KEY_BYTES {
        *truncated = true;
        return;
    }
    if let Some(bucket) = buckets.get_mut(key) {
        bucket.add(record);
        return;
    }
    if buckets.len() >= MAX_BREAKDOWN_BUCKETS {
        *truncated = true;
        return;
    }
    let mut bucket = Totals::default();
    bucket.add(record);
    buckets.insert(key.to_string(), bucket);
}

/// Re-export so callers don't need a separate import path for the
/// default log location.
pub fn default_log_path() -> PathBuf {
    run_log_path()
}

fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/llm/usage.rs"
    ));
}
