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
use std::fs;
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
        if self.calls == 0 {
            None
        } else {
            Some(self.total_duration_ms / self.calls)
        }
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
        let Ok(text) = fs::read_to_string(path) else {
            return Self::default();
        };
        Self::from_lines_filtered(text.lines(), filter)
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
        report
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
        let Ok(text) = fs::read_to_string(path) else {
            return BTreeMap::new();
        };
        let mut out: BTreeMap<String, UsageBucket> = BTreeMap::new();
        for line in text.lines() {
            let Ok(rec) = serde_json::from_str::<LlmRunRecord>(line) else {
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
        let Ok(text) = fs::read_to_string(path) else {
            return Vec::new();
        };
        let mut all: Vec<LlmRunRecord> = text
            .lines()
            .filter_map(|line| serde_json::from_str::<LlmRunRecord>(line.trim()).ok())
            .filter(|rec| filter.matches(rec))
            .collect();
        let take_from = all.len().saturating_sub(n);
        all.drain(0..take_from);
        all
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
    use super::*;

    fn rec_json(
        provider: &str,
        model: &str,
        session: Option<&str>,
        in_tok: u32,
        out_tok: u32,
        dur_ms: u64,
        finish: &str,
        error: Option<&str>,
    ) -> String {
        let session_field = match session {
            Some(s) => format!(",\"session_id\":\"{s}\""),
            None => String::new(),
        };
        let (status, error_field) = match error {
            Some(e) => ("error", format!(",\"error\":\"{e}\"")),
            None => ("ok", String::new()),
        };
        format!(
            "{{\"timestamp\":\"2025-01-01T00:00:00Z\"\
             {session_field},\
             \"provider\":\"{provider}\",\
             \"model\":\"{model}\",\
             \"duration_ms\":{dur_ms},\
             \"input_tokens\":{in_tok},\
             \"output_tokens\":{out_tok},\
             \"finish_reason\":\"{finish}\",\
             \"status\":\"{status}\"\
             {error_field}}}"
        )
    }

    #[test]
    fn empty_lines_yield_empty_report() {
        let r = InsightsReport::from_lines(std::iter::empty::<&str>());
        assert_eq!(r.overall.calls, 0);
        assert!(r.per_provider.is_empty());
        assert!(r.per_model.is_empty());
    }

    #[test]
    fn malformed_lines_are_skipped() {
        let json = rec_json("openai", "gpt-5", None, 10, 20, 100, "stop", None);
        let lines = vec!["garbage", json.as_str(), "", "{not_json"];
        let r = InsightsReport::from_lines(lines.into_iter());
        assert_eq!(r.overall.calls, 1);
    }

    #[test]
    fn aggregates_overall_and_per_provider_per_model() {
        let lines = vec![
            rec_json("openai", "gpt-5", None, 10, 20, 100, "stop", None),
            rec_json("openai", "gpt-5", None, 5, 15, 50, "stop", None),
            rec_json("anthropic", "claude-x", None, 7, 13, 80, "tool_use", None),
            rec_json(
                "openai",
                "gpt-5-mini",
                None,
                1,
                2,
                30,
                "length",
                Some("rate"),
            ),
        ];
        let r = InsightsReport::from_lines(lines.iter().map(|s| s.as_str()));
        assert_eq!(r.overall.calls, 4);
        assert_eq!(r.overall.input_tokens, 23);
        assert_eq!(r.overall.output_tokens, 50);
        assert_eq!(r.overall.errors, 1);
        assert_eq!(r.overall.finish_reasons["stop"], 2);
        assert_eq!(r.overall.finish_reasons["tool_use"], 1);
        assert_eq!(r.overall.finish_reasons["length"], 1);

        let openai = &r.per_provider["openai"];
        assert_eq!(openai.calls, 3);
        assert_eq!(openai.errors, 1);

        let anth = &r.per_provider["anthropic"];
        assert_eq!(anth.calls, 1);
        assert_eq!(anth.input_tokens, 7);

        assert_eq!(r.per_model["gpt-5"].calls, 2);
        assert_eq!(r.per_model["gpt-5-mini"].calls, 1);
        assert_eq!(r.per_model["claude-x"].calls, 1);
    }

    #[test]
    fn average_duration_some_when_calls_present() {
        let mut b = UsageBucket::default();
        b.fold(&LlmRunRecord {
            timestamp: "t".to_string(),
            trace_id: None,
            span_id: None,
            session_id: None,
            provider: "p".to_string(),
            model: "m".to_string(),
            engine_name: None,
            engine_version: None,
            duration_ms: 200,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            finish_reason: "stop".to_string(),
            status: "ok".to_string(),
            error: None,
            decision: "allowed".to_string(),
            denial_reason: None,
            app_id: None,
        });
        assert_eq!(b.average_duration_ms(), Some(200));
        let empty = UsageBucket::default();
        assert_eq!(empty.average_duration_ms(), None);
    }

    #[test]
    fn from_path_missing_file_returns_empty() {
        let nonexistent = std::env::temp_dir().join("cos-insights-no-such.jsonl");
        let r = InsightsReport::from_path(&nonexistent);
        assert_eq!(r, InsightsReport::default());
    }

    #[test]
    fn by_session_groups_correctly() {
        use std::path::PathBuf;
        use uuid::Uuid;
        let path: PathBuf = std::env::temp_dir().join(format!(
            "cos-insights-by-session-{}.jsonl",
            Uuid::new_v4().simple()
        ));
        std::fs::write(
            &path,
            format!(
                "{}\n{}\n{}\n",
                rec_json("p", "m", Some("s1"), 1, 2, 10, "stop", None),
                rec_json("p", "m", Some("s1"), 3, 4, 20, "stop", None),
                rec_json("p", "m", Some("s2"), 5, 6, 30, "stop", None),
            ),
        )
        .unwrap();
        let by = InsightsReport::by_session(&path);
        assert_eq!(by.len(), 2);
        assert_eq!(by["s1"].calls, 2);
        assert_eq!(by["s2"].calls, 1);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn recent_returns_last_n() {
        use std::path::PathBuf;
        use uuid::Uuid;
        let path: PathBuf = std::env::temp_dir().join(format!(
            "cos-insights-recent-{}.jsonl",
            Uuid::new_v4().simple()
        ));
        std::fs::write(
            &path,
            format!(
                "{}\n{}\n{}\n{}\n",
                rec_json("p1", "m", None, 1, 1, 10, "stop", None),
                rec_json("p2", "m", None, 1, 1, 10, "stop", None),
                rec_json("p3", "m", None, 1, 1, 10, "stop", None),
                rec_json("p4", "m", None, 1, 1, 10, "stop", None),
            ),
        )
        .unwrap();
        let last2 = InsightsReport::recent(&path, 2);
        assert_eq!(last2.len(), 2);
        assert_eq!(last2[0].provider, "p3");
        assert_eq!(last2[1].provider, "p4");
        let last10 = InsightsReport::recent(&path, 10);
        assert_eq!(last10.len(), 4);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn summary_line_renders() {
        let mut b = UsageBucket::default();
        b.calls = 3;
        b.input_tokens = 100;
        b.output_tokens = 200;
        b.total_duration_ms = 600;
        b.errors = 1;
        let s = summary_line("openai", &b);
        assert!(s.contains("3 calls"));
        assert!(s.contains("300 tokens"));
        assert!(s.contains("100+200"));
        assert!(s.contains("1 errors"));
        assert!(s.contains("avg 200ms"));
    }

    fn rec_json_at(ts: &str, provider: &str, model: &str, error: Option<&str>) -> String {
        let (status, error_field) = match error {
            Some(e) => ("error", format!(",\"error\":\"{e}\"")),
            None => ("ok", String::new()),
        };
        format!(
            "{{\"timestamp\":\"{ts}\",\
             \"provider\":\"{provider}\",\
             \"model\":\"{model}\",\
             \"duration_ms\":10,\
             \"input_tokens\":1,\
             \"output_tokens\":1,\
             \"finish_reason\":\"stop\",\
             \"status\":\"{status}\"{error_field}}}"
        )
    }

    #[test]
    fn empty_filter_matches_everything() {
        let json = rec_json("openai", "gpt-5", None, 1, 1, 10, "stop", None);
        let rec: LlmRunRecord = serde_json::from_str(&json).unwrap();
        let filter = InsightsFilter::default();
        assert!(filter.is_empty());
        assert!(filter.matches(&rec));
    }

    #[test]
    fn filter_by_status_ok_excludes_errors() {
        let ok_json = rec_json("p", "m", None, 1, 1, 10, "stop", None);
        let err_json = rec_json("p", "m", None, 1, 1, 10, "stop", Some("boom"));
        let ok: LlmRunRecord = serde_json::from_str(&ok_json).unwrap();
        let err: LlmRunRecord = serde_json::from_str(&err_json).unwrap();
        let mut f = InsightsFilter::default();
        f.status_ok = Some(true);
        assert!(f.matches(&ok));
        assert!(!f.matches(&err));
    }

    #[test]
    fn filter_by_status_error_excludes_ok() {
        let ok_json = rec_json("p", "m", None, 1, 1, 10, "stop", None);
        let err_json = rec_json("p", "m", None, 1, 1, 10, "stop", Some("boom"));
        let ok: LlmRunRecord = serde_json::from_str(&ok_json).unwrap();
        let err: LlmRunRecord = serde_json::from_str(&err_json).unwrap();
        let mut f = InsightsFilter::default();
        f.status_ok = Some(false);
        assert!(!f.matches(&ok));
        assert!(f.matches(&err));
    }

    #[test]
    fn filter_by_provider_and_model_exact_match() {
        let r1: LlmRunRecord =
            serde_json::from_str(&rec_json("openai", "gpt-5", None, 1, 1, 10, "stop", None))
                .unwrap();
        let r2: LlmRunRecord = serde_json::from_str(&rec_json(
            "anthropic",
            "claude",
            None,
            1,
            1,
            10,
            "stop",
            None,
        ))
        .unwrap();
        let mut f = InsightsFilter::default();
        f.provider = Some("openai".into());
        assert!(f.matches(&r1));
        assert!(!f.matches(&r2));
        let mut g = InsightsFilter::default();
        g.model = Some("claude".into());
        assert!(!g.matches(&r1));
        assert!(g.matches(&r2));
    }

    #[test]
    fn filter_since_excludes_earlier_records() {
        let r: LlmRunRecord =
            serde_json::from_str(&rec_json_at("2025-01-01T00:00:00Z", "p", "m", None)).unwrap();
        let mut f = InsightsFilter::default();
        f.since = Some(
            DateTime::parse_from_rfc3339("2025-06-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        );
        assert!(!f.matches(&r));
        f.since = Some(
            DateTime::parse_from_rfc3339("2024-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        );
        assert!(f.matches(&r));
    }

    #[test]
    fn filter_until_excludes_later_records() {
        let r: LlmRunRecord =
            serde_json::from_str(&rec_json_at("2025-06-01T00:00:00Z", "p", "m", None)).unwrap();
        let mut f = InsightsFilter::default();
        f.until = Some(
            DateTime::parse_from_rfc3339("2025-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        );
        assert!(!f.matches(&r));
        f.until = Some(
            DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        );
        assert!(f.matches(&r));
    }

    #[test]
    fn filter_excludes_records_with_unparseable_timestamp_when_bound_set() {
        let r: LlmRunRecord =
            serde_json::from_str(&rec_json_at("not-a-timestamp", "p", "m", None)).unwrap();
        // Without bounds: matches.
        let f0 = InsightsFilter::default();
        assert!(f0.matches(&r));
        // With since: excluded (can't be ordered).
        let mut f = InsightsFilter::default();
        f.since = Some(
            DateTime::parse_from_rfc3339("2024-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        );
        assert!(!f.matches(&r));
    }

    #[test]
    fn from_lines_filtered_aggregates_subset() {
        let lines = vec![
            rec_json_at("2025-01-01T00:00:00Z", "openai", "gpt-5", None),
            rec_json_at("2025-06-01T00:00:00Z", "openai", "gpt-5", None),
            rec_json_at("2025-06-01T00:00:00Z", "anthropic", "claude", Some("boom")),
        ];
        let mut f = InsightsFilter::default();
        f.since = Some(
            DateTime::parse_from_rfc3339("2025-03-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        );
        let r = InsightsReport::from_lines_filtered(lines.iter().map(|s| s.as_str()), &f);
        assert_eq!(r.overall.calls, 2);
        f.status_ok = Some(true);
        let r = InsightsReport::from_lines_filtered(lines.iter().map(|s| s.as_str()), &f);
        assert_eq!(r.overall.calls, 1);
        assert!(r.per_provider.contains_key("openai"));
        assert!(!r.per_provider.contains_key("anthropic"));
    }
}
