//! LLM usage insights — aggregate the JSONL run-record stream into
//! human-readable summaries for `cos agent status`, periodic
//! reports, and budget enforcement decisions.
//!
//! Reads from `paths::ai_run_log_path()` (or any path the caller
//! supplies). All aggregation is in-memory and dependency-free
//! beyond `serde_json`. The log file is opened read-only; this
//! module never mutates it.
//!
//! Three views:
//!
//!   * [`InsightsReport::all`]  — overall + per-provider + per-model
//!   * [`InsightsReport::by_session`] — per-session totals
//!   * [`InsightsReport::recent`] — last N records, untouched
//!
//! Each summary carries call counts, token totals, finish-reason
//! histogram, and average / median duration.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::agent::llm::run_log::LlmRunRecord;

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct UsageBucket {
    pub calls: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub total_duration_ms: u64,
    pub finish_reasons: BTreeMap<String, u64>,
    pub errors: u64,
}

impl UsageBucket {
    pub fn average_duration_ms(&self) -> Option<u64> {
        self.total_duration_ms.checked_div(self.calls)
    }

    pub fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens + self.cache_read_tokens + self.cache_write_tokens
    }

    fn fold(&mut self, rec: &LlmRunRecord) {
        self.calls += 1;
        self.input_tokens = self.input_tokens.saturating_add(rec.input_tokens as u64);
        self.output_tokens = self.output_tokens.saturating_add(rec.output_tokens as u64);
        self.cache_read_tokens = self
            .cache_read_tokens
            .saturating_add(rec.cache_read_tokens as u64);
        self.cache_write_tokens = self
            .cache_write_tokens
            .saturating_add(rec.cache_write_tokens as u64);
        self.total_duration_ms = self.total_duration_ms.saturating_add(rec.duration_ms);
        *self
            .finish_reasons
            .entry(rec.finish_reason.clone())
            .or_default() += 1;
        if rec.error.is_some() {
            self.errors += 1;
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct InsightsReport {
    pub overall: UsageBucket,
    pub per_provider: BTreeMap<String, UsageBucket>,
    pub per_model: BTreeMap<String, UsageBucket>,
}

impl InsightsReport {
    /// Build aggregate report from a path. Missing files yield an
    /// empty report; malformed lines are silently skipped.
    pub fn from_path(path: &Path) -> Self {
        Self::from_path_filtered(path, &InsightsFilter::default())
    }

    pub fn from_path_filtered(path: &Path, filter: &InsightsFilter) -> Self {
        // Stream the JSONL log line-by-line so a multi-GB run log
        // doesn't blow the heap during `cos agent status` or budget
        // enforcement. A missing file yields an empty report.
        let Ok(file) = File::open(path) else {
            return Self::default();
        };
        let reader = BufReader::new(file);
        let mut report = Self::default();
        for line in reader.lines() {
            let Ok(line) = line else { continue };
            Self::fold_one(&mut report, &line, filter);
        }
        report
    }

    pub fn from_lines<'a, I: IntoIterator<Item = &'a str>>(lines: I) -> Self {
        Self::from_lines_filtered(lines, &InsightsFilter::default())
    }

    pub fn from_lines_filtered<'a, I: IntoIterator<Item = &'a str>>(
        lines: I,
        filter: &InsightsFilter,
    ) -> Self {
        let mut report = Self::default();
        for line in lines {
            Self::fold_one(&mut report, line, filter);
        }
        report
    }

    fn fold_one(report: &mut Self, line: &str, filter: &InsightsFilter) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        let Ok(rec) = serde_json::from_str::<LlmRunRecord>(line) else {
            return;
        };
        if !filter.matches(&rec) {
            return;
        }
        report.overall.fold(&rec);
        report
            .per_provider
            .entry(rec.provider.clone())
            .or_default()
            .fold(&rec);
        report
            .per_model
            .entry(rec.model.clone())
            .or_default()
            .fold(&rec);
    }

    /// Convenience: read the default cos run-log path.
    pub fn from_default() -> Self {
        Self::from_path(&crate::paths::ai_run_log_path())
    }

    /// Per-session aggregation (records with `session_id == None`
    /// are grouped under the empty-string key).
    pub fn by_session(path: &Path) -> BTreeMap<String, UsageBucket> {
        Self::by_session_filtered(path, &InsightsFilter::default())
    }

    pub fn by_session_filtered(
        path: &Path,
        filter: &InsightsFilter,
    ) -> BTreeMap<String, UsageBucket> {
        let Ok(file) = File::open(path) else {
            return BTreeMap::new();
        };
        let reader = BufReader::new(file);
        let mut out: BTreeMap<String, UsageBucket> = BTreeMap::new();
        for line in reader.lines() {
            let Ok(line) = line else { continue };
            let Ok(rec) = serde_json::from_str::<LlmRunRecord>(&line) else {
                continue;
            };
            if !filter.matches(&rec) {
                continue;
            }
            let key = rec.session_id.clone().unwrap_or_default();
            out.entry(key).or_default().fold(&rec);
        }
        out
    }

    /// Return the most recent `n` records as parsed structs (newest
    /// last). Useful for `cos agent status --tail 5`-style readouts.
    pub fn recent(path: &Path, n: usize) -> Vec<LlmRunRecord> {
        Self::recent_filtered(path, n, &InsightsFilter::default())
    }

    pub fn recent_filtered(path: &Path, n: usize, filter: &InsightsFilter) -> Vec<LlmRunRecord> {
        let Ok(file) = File::open(path) else {
            return Vec::new();
        };
        let reader = BufReader::new(file);
        // Bounded ring-buffer: keep at most `n` filtered records.
        // Avoids buffering an entire multi-GB run log just to drop
        // all but the trailing window.
        let mut ring: std::collections::VecDeque<LlmRunRecord> =
            std::collections::VecDeque::with_capacity(n.min(1024));
        for line in reader.lines() {
            let Ok(line) = line else { continue };
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(rec) = serde_json::from_str::<LlmRunRecord>(line) else {
                continue;
            };
            if !filter.matches(&rec) {
                continue;
            }
            if n == 0 {
                continue;
            }
            if ring.len() == n {
                ring.pop_front();
            }
            ring.push_back(rec);
        }
        ring.into_iter().collect()
    }
}

/// Predicate over [`LlmRunRecord`] used by every aggregator. All
/// fields default to "no constraint"; the empty filter is a no-op
/// equivalent to the old, unfiltered API.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InsightsFilter {
    /// Inclusive lower bound on `LlmRunRecord.timestamp`. Records
    /// with unparseable RFC3339 timestamps are EXCLUDED when this
    /// bound is set (a malformed timestamp can't be ordered).
    pub since: Option<DateTime<Utc>>,
    /// Inclusive upper bound, same parsing rules as `since`.
    pub until: Option<DateTime<Utc>>,
    /// `Some(true)` keeps only successful records (status == "ok").
    /// `Some(false)` keeps only error records (any other status).
    /// `None` keeps everything.
    pub status_ok: Option<bool>,
    /// Optional exact-match provider filter.
    pub provider: Option<String>,
    /// Optional exact-match model filter.
    pub model: Option<String>,
}

impl InsightsFilter {
    /// True when the record passes every active constraint.
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
        if let Some(want_ok) = self.status_ok {
            let is_ok = rec.status == "ok";
            if want_ok != is_ok {
                return false;
            }
        }
        if self.since.is_some() || self.until.is_some() {
            let parsed = DateTime::parse_from_rfc3339(&rec.timestamp)
                .map(|d| d.with_timezone(&Utc))
                .ok();
            let Some(ts) = parsed else { return false };
            if let Some(s) = self.since {
                if ts < s {
                    return false;
                }
            }
            if let Some(u) = self.until {
                if ts > u {
                    return false;
                }
            }
        }
        true
    }

    pub fn is_empty(&self) -> bool {
        self.since.is_none()
            && self.until.is_none()
            && self.status_ok.is_none()
            && self.provider.is_none()
            && self.model.is_none()
    }
}

/// Summarise a UsageBucket as a single human-readable line.
pub fn summary_line(label: &str, b: &UsageBucket) -> String {
    let avg = b.average_duration_ms().unwrap_or(0);
    format!(
        "{label}: {} calls, {} tokens ({}+{} I/O, +{} cache_r, +{} cache_w), {} errors, avg {avg}ms",
        b.calls,
        b.total_tokens(),
        b.input_tokens,
        b.output_tokens,
        b.cache_read_tokens,
        b.cache_write_tokens,
        b.errors,
    )
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/insights.rs"
    ));
}
