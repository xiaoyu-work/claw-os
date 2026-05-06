//! Usage aggregator over the `llm.jsonl` run-log stream.
//!
//! The audit trail [`crate::agent::llm::run_log`] writes one JSON
//! line per LLM call. This module reads that stream and produces
//! summary breakdowns: totals, per-provider, per-model, per-session.
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
//! Library-only this commit. No CLI subcommand exposed yet — when
//! we later add `cos agent usage`, it will wrap this.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::run_log::{run_log_path, LlmRunRecord};

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
    /// Number of malformed log lines encountered.
    pub parse_errors: usize,
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
        s.total.add(rec);
        s.by_provider
            .entry(rec.provider.clone())
            .or_default()
            .add(rec);
        s.by_model.entry(rec.model.clone()).or_default().add(rec);
        if let Some(sid) = &rec.session_id {
            s.by_session.entry(sid.clone()).or_default().add(rec);
        }
    }
    s
}

/// Read + aggregate the default `llm.jsonl`. Convenience wrapper.
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
    use super::*;
    use crate::agent::llm::run_log::record_to_path;
    use crate::agent::llm::{EngineInfo, FinishReason, Usage};

    fn write(path: &Path, recs: &[LlmRunRecord]) {
        for r in recs {
            record_to_path(r, path).unwrap();
        }
    }

    fn rec(
        provider: &str,
        model: &str,
        input: u32,
        output: u32,
        session_id: Option<&str>,
    ) -> LlmRunRecord {
        let mut r = LlmRunRecord::from_success(
            provider,
            model,
            None,
            FinishReason::Stop,
            &Usage {
                input_tokens: input,
                output_tokens: output,
                ..Default::default()
            },
            10,
            session_id,
        );
        // pin timestamp so tests are deterministic
        r.timestamp = "2026-01-01T00:00:00.000Z".into();
        r
    }

    #[test]
    fn empty_path_returns_zero_summary() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("never-existed.jsonl");
        let s = aggregate_path(&p);
        assert_eq!(s.total.calls, 0);
        assert!(s.by_provider.is_empty());
        assert_eq!(s.parse_errors, 0);
    }

    #[test]
    fn aggregates_totals() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("llm.jsonl");
        write(
            &p,
            &[
                rec("anthropic", "claude-sonnet-4.6", 100, 50, Some("s1")),
                rec("anthropic", "claude-sonnet-4.6", 200, 80, Some("s1")),
                rec("openai_compat", "gpt-5", 300, 70, Some("s2")),
            ],
        );
        let s = aggregate_path(&p);
        assert_eq!(s.total.calls, 3);
        assert_eq!(s.total.input_tokens, 600);
        assert_eq!(s.total.output_tokens, 200);
        assert_eq!(s.total.success, 3);
        assert_eq!(s.total.error, 0);
    }

    #[test]
    fn breaks_down_by_provider_model_session() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("llm.jsonl");
        write(
            &p,
            &[
                rec("anthropic", "sonnet", 100, 50, Some("s1")),
                rec("anthropic", "haiku", 50, 25, Some("s1")),
                rec("openai_compat", "gpt-5", 300, 70, Some("s2")),
            ],
        );
        let s = aggregate_path(&p);
        // 2 providers, 3 models, 2 sessions.
        assert_eq!(s.by_provider.len(), 2);
        assert_eq!(s.by_model.len(), 3);
        assert_eq!(s.by_session.len(), 2);

        let anth = &s.by_provider["anthropic"];
        assert_eq!(anth.calls, 2);
        assert_eq!(anth.input_tokens, 150);
        assert_eq!(anth.output_tokens, 75);

        let s1 = &s.by_session["s1"];
        assert_eq!(s1.calls, 2);
    }

    #[test]
    fn separates_success_and_error_counts() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("llm.jsonl");
        let mut err = LlmRunRecord::from_error(
            "anthropic",
            "sonnet",
            None,
            "529 overloaded",
            5,
            Some("s1"),
        );
        err.timestamp = "2026-01-01T00:00:00.000Z".into();
        write(&p, &[rec("anthropic", "sonnet", 10, 10, Some("s1")), err]);
        let s = aggregate_path(&p);
        assert_eq!(s.total.calls, 2);
        assert_eq!(s.total.success, 1);
        assert_eq!(s.total.error, 1);
    }

    #[test]
    fn skips_malformed_lines_and_counts_them() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("llm.jsonl");
        let r = rec("anthropic", "sonnet", 1, 1, None);
        record_to_path(&r, &p).unwrap();
        // Append a couple of bad lines.
        let mut body = std::fs::read_to_string(&p).unwrap();
        body.push_str("not valid json\n{\"truncated\":\n");
        std::fs::write(&p, body).unwrap();
        let s = aggregate_path(&p);
        assert_eq!(s.total.calls, 1);
        assert_eq!(s.parse_errors, 2);
    }

    #[test]
    fn skips_blank_lines() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("llm.jsonl");
        let r = rec("anthropic", "sonnet", 1, 1, None);
        record_to_path(&r, &p).unwrap();
        let mut body = std::fs::read_to_string(&p).unwrap();
        body.push_str("\n\n   \n");
        std::fs::write(&p, body).unwrap();
        let s = aggregate_path(&p);
        assert_eq!(s.total.calls, 1);
        assert_eq!(s.parse_errors, 0);
    }

    #[test]
    fn filter_by_provider() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("llm.jsonl");
        write(
            &p,
            &[
                rec("anthropic", "sonnet", 10, 10, None),
                rec("openai_compat", "gpt-5", 20, 20, None),
            ],
        );
        let q = UsageQuery {
            provider: Some("anthropic".into()),
            ..Default::default()
        };
        let s = aggregate_path_filtered(&p, &q);
        assert_eq!(s.total.calls, 1);
        assert!(s.by_provider.contains_key("anthropic"));
        assert!(!s.by_provider.contains_key("openai_compat"));
    }

    #[test]
    fn filter_by_session() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("llm.jsonl");
        write(
            &p,
            &[
                rec("anthropic", "sonnet", 10, 10, Some("s1")),
                rec("anthropic", "sonnet", 20, 20, Some("s2")),
            ],
        );
        let q = UsageQuery {
            session_id: Some("s2".into()),
            ..Default::default()
        };
        let s = aggregate_path_filtered(&p, &q);
        assert_eq!(s.total.calls, 1);
        assert_eq!(s.total.input_tokens, 20);
    }

    #[test]
    fn filter_by_status() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("llm.jsonl");
        let mut err = LlmRunRecord::from_error("a", "b", None, "x", 5, None);
        err.timestamp = "2026-01-01T00:00:00.000Z".into();
        write(&p, &[rec("a", "b", 10, 10, None), err]);
        let only_ok = UsageQuery {
            status_ok: Some(true),
            ..Default::default()
        };
        assert_eq!(aggregate_path_filtered(&p, &only_ok).total.calls, 1);
        let only_err = UsageQuery {
            status_ok: Some(false),
            ..Default::default()
        };
        assert_eq!(aggregate_path_filtered(&p, &only_err).total.calls, 1);
    }

    #[test]
    fn filter_by_time_range_inclusive_lower_exclusive_upper() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("llm.jsonl");
        let mut a = rec("x", "y", 1, 1, None);
        a.timestamp = "2026-01-01T00:00:00.000Z".into();
        let mut b = rec("x", "y", 2, 2, None);
        b.timestamp = "2026-01-02T00:00:00.000Z".into();
        let mut c = rec("x", "y", 3, 3, None);
        c.timestamp = "2026-01-03T00:00:00.000Z".into();
        write(&p, &[a, b, c]);
        let q = UsageQuery {
            since: Some("2026-01-02T00:00:00Z".parse().unwrap()),
            until: Some("2026-01-03T00:00:00Z".parse().unwrap()),
            ..Default::default()
        };
        let s = aggregate_path_filtered(&p, &q);
        // Only the b record falls in [02, 03).
        assert_eq!(s.total.calls, 1);
        assert_eq!(s.total.input_tokens, 2);
    }

    #[test]
    fn cache_tokens_aggregated() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("llm.jsonl");
        let mut r = LlmRunRecord::from_success(
            "anthropic",
            "sonnet",
            None,
            FinishReason::Stop,
            &Usage {
                input_tokens: 100,
                output_tokens: 50,
                cache_read_tokens: 200,
                cache_write_tokens: 80,
            },
            42,
            Some("s1"),
        );
        r.timestamp = "2026-01-01T00:00:00.000Z".into();
        write(&p, &[r]);
        let s = aggregate_path(&p);
        assert_eq!(s.total.cache_read_tokens, 200);
        assert_eq!(s.total.cache_write_tokens, 80);
        assert_eq!(s.by_provider["anthropic"].cache_read_tokens, 200);
    }

    #[test]
    fn old_log_lines_without_cache_fields_default_to_zero() {
        // Simulate a log line written before p4-usage added cache fields.
        // The old shape: no cache_read_tokens / cache_write_tokens fields.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("llm.jsonl");
        let line = serde_json::json!({
            "timestamp": "2026-01-01T00:00:00.000Z",
            "provider": "anthropic",
            "model": "sonnet",
            "duration_ms": 5,
            "input_tokens": 10,
            "output_tokens": 20,
            "finish_reason": "stop",
            "status": "ok",
        });
        std::fs::write(&p, format!("{line}\n")).unwrap();
        let s = aggregate_path(&p);
        assert_eq!(s.total.calls, 1);
        assert_eq!(s.total.cache_read_tokens, 0);
        assert_eq!(s.total.cache_write_tokens, 0);
    }

    #[test]
    fn duration_ms_summed() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("llm.jsonl");
        let mut a = rec("x", "y", 1, 1, None);
        a.duration_ms = 100;
        let mut b = rec("x", "y", 1, 1, None);
        b.duration_ms = 250;
        write(&p, &[a, b]);
        let s = aggregate_path(&p);
        assert_eq!(s.total.total_duration_ms, 350);
    }

    #[test]
    fn aggregate_default_does_not_panic() {
        // Real default path may or may not exist on the host. Just
        // verify the wrapper doesn't blow up.
        let _ = aggregate_default();
    }

    #[test]
    fn query_matches_combines_filters_and() {
        let r = rec("anthropic", "sonnet", 1, 1, Some("s1"));
        let q = UsageQuery {
            provider: Some("anthropic".into()),
            model: Some("sonnet".into()),
            session_id: Some("s1".into()),
            status_ok: Some(true),
            ..Default::default()
        };
        assert!(q.matches(&r));
        let q_bad = UsageQuery {
            provider: Some("openai".into()),
            ..q
        };
        assert!(!q_bad.matches(&r));
    }

    #[test]
    fn unparseable_timestamp_excluded_from_time_range_query() {
        let mut r = rec("x", "y", 1, 1, None);
        r.timestamp = "not a timestamp".into();
        let q = UsageQuery {
            since: Some("2026-01-01T00:00:00Z".parse().unwrap()),
            ..Default::default()
        };
        assert!(!q.matches(&r));
    }

    #[test]
    fn unparseable_timestamp_passes_when_no_time_filter() {
        let mut r = rec("x", "y", 1, 1, None);
        r.timestamp = "garbage".into();
        let q = UsageQuery::default();
        assert!(q.matches(&r));
    }
}
