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
        verb: None,
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
