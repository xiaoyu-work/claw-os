use super::*;
use crate::agent::llm::run_log::record_to_path;
use crate::agent::llm::{FinishReason, Usage};

fn write(path: &Path, recs: &[LlmRunRecord]) {
    for r in recs {
        record_to_path(r, path).unwrap();
    }
}

#[test]
fn streaming_snapshot_preserves_log_and_insight_metrics() {
    let first = serde_json::to_string(&rec("anthropic", "sonnet", 10, 2, Some("s1"))).unwrap();
    let second = serde_json::to_string(&rec("anthropic", "sonnet", 5, 1, Some("s1"))).unwrap();
    let body = format!("{first}\nnot-json\n{second}\n");
    let summary = aggregate_reader_filtered(
        std::io::Cursor::new(body.as_bytes()),
        &UsageQuery::default(),
        MAX_QUERY_BYTES,
    )
    .unwrap();

    assert_eq!(summary.log_lines, 3);
    assert_eq!(summary.log_bytes, body.len() as u64);
    assert_eq!(summary.parse_errors, 1);
    assert_eq!(summary.total.finish_reasons["stop"], 2);
    assert_eq!(summary.total.errors, 0);
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
fn bounded_reader_refuses_more_than_the_declared_limit() {
    let error = read_records_bounded(std::io::Cursor::new(b"123456789"), 8).unwrap_err();
    assert!(error.contains("8 byte query limit"));
}

#[test]
fn streaming_aggregation_caps_record_count() {
    let data = "{}\n".repeat(MAX_QUERY_RECORDS + 1);
    let error = aggregate_reader_filtered(
        std::io::Cursor::new(data),
        &UsageQuery::default(),
        MAX_QUERY_BYTES,
    )
    .unwrap_err();
    assert!(error.contains("record query limit"));
}

#[test]
fn aggregates_totals() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("ai.jsonl");
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
    let p = dir.path().join("ai.jsonl");
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
    let p = dir.path().join("ai.jsonl");
    let mut err =
        LlmRunRecord::from_error("anthropic", "sonnet", None, "529 overloaded", 5, Some("s1"));
    err.timestamp = "2026-01-01T00:00:00.000Z".into();
    write(&p, &[rec("anthropic", "sonnet", 10, 10, Some("s1")), err]);
    let s = aggregate_path(&p);
    assert_eq!(s.total.calls, 2);
    assert_eq!(s.total.success, 1);
    assert_eq!(s.total.error, 1);
    assert_eq!(s.total.errors, 1);
    assert_eq!(s.total.finish_reasons["stop"], 1);
    assert_eq!(s.total.finish_reasons["error"], 1);
}

#[test]
fn skips_malformed_lines_and_counts_them() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("ai.jsonl");
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
    let p = dir.path().join("ai.jsonl");
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
    let p = dir.path().join("ai.jsonl");
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
    let p = dir.path().join("ai.jsonl");
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
    let p = dir.path().join("ai.jsonl");
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
    let p = dir.path().join("ai.jsonl");
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
    let p = dir.path().join("ai.jsonl");
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
    let p = dir.path().join("ai.jsonl");
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
    let p = dir.path().join("ai.jsonl");
    let mut a = rec("x", "y", 1, 1, None);
    a.duration_ms = 100;
    let mut b = rec("x", "y", 1, 1, None);
    b.duration_ms = 250;
    write(&p, &[a, b]);
    let s = aggregate_path(&p);
    assert_eq!(s.total.total_duration_ms, 350);
}

#[test]
fn breakdowns_are_capped_without_losing_total_usage() {
    let records: Vec<LlmRunRecord> = (0..1500)
        .map(|index| {
            rec(
                "anthropic",
                "sonnet",
                1,
                1,
                Some(&format!("session-{index:04}")),
            )
        })
        .collect();
    let summary = aggregate(&records);

    assert_eq!(summary.total.calls, 1500);
    assert_eq!(summary.by_session.len(), MAX_BREAKDOWN_BUCKETS);
    assert!(summary.breakdown_truncated);
    assert!(
        serde_json::to_vec(&summary).unwrap().len()
            < crate::clawd::wire::MAX_RESPONSE_BYTES
    );
}

#[test]
fn capped_summary_fits_broker_response_with_escape_heavy_keys() {
    fn key(prefix: &str, index: usize) -> String {
        format!("{prefix}-{index:04}-{}", "\u{0001}".repeat(110))
    }

    let records: Vec<LlmRunRecord> = (0..MAX_BREAKDOWN_BUCKETS)
        .map(|index| {
            let provider = key("p", index);
            let model = key("m", index);
            let session = key("s", index);
            let mut record = rec(&provider, &model, 1, 1, Some(&session));
            record.app_id = Some(key("a", index));
            record.verb = Some(key("v", index));
            record.finish_reason = "\\".repeat(64);
            record
        })
        .collect();
    let summary = aggregate(&records);
    let encoded = serde_json::to_vec(&summary).unwrap();

    assert_eq!(summary.by_session.len(), MAX_BREAKDOWN_BUCKETS);
    assert_eq!(summary.total.finish_reasons["other"], MAX_BREAKDOWN_BUCKETS as u64);
    assert!(
        encoded.len() < crate::clawd::wire::MAX_RESPONSE_BYTES,
        "bounded summary is {} bytes",
        encoded.len()
    );
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

#[test]
fn aggregates_by_app_and_verb() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("ai.jsonl");
    let mut a = rec("anthropic", "sonnet", 100, 50, Some("s1"));
    a = a.with_app("summarize").with_verb("ai.chat");
    let mut b = rec("anthropic", "sonnet", 200, 80, Some("s1"));
    b = b.with_app("summarize").with_verb("ai.chat");
    let mut c = rec("openai_compat", "gpt-image-1", 0, 0, Some("s2"));
    c = c.with_app("doc").with_verb("ai.image.generate");
    // System call: no app, no verb — should not appear in by_app
    // or by_verb but should still count in total.
    let d = rec("anthropic", "sonnet", 5, 5, None);
    write(&p, &[a, b, c, d]);
    let s = aggregate_path(&p);
    assert_eq!(s.total.calls, 4);
    assert_eq!(s.by_app.len(), 2);
    assert_eq!(s.by_app["summarize"].calls, 2);
    assert_eq!(s.by_app["summarize"].input_tokens, 300);
    assert_eq!(s.by_app["doc"].calls, 1);
    assert_eq!(s.by_verb.len(), 2);
    assert_eq!(s.by_verb["ai.chat"].calls, 2);
    assert_eq!(s.by_verb["ai.image.generate"].calls, 1);
}

#[test]
fn filter_by_app() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("ai.jsonl");
    let a = rec("anthropic", "sonnet", 10, 10, Some("s1")).with_app("summarize");
    let b = rec("anthropic", "sonnet", 20, 20, Some("s2")).with_app("doc");
    write(&p, &[a, b]);
    let q = UsageQuery {
        app_id: Some("summarize".into()),
        ..Default::default()
    };
    let s = aggregate_path_filtered(&p, &q);
    assert_eq!(s.total.calls, 1);
    assert_eq!(s.total.input_tokens, 10);
    assert!(s.by_app.contains_key("summarize"));
    assert!(!s.by_app.contains_key("doc"));
}

#[test]
fn filter_by_verb() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("ai.jsonl");
    let a = rec("anthropic", "sonnet", 10, 10, None).with_verb("ai.chat");
    let b = rec("anthropic", "sonnet", 20, 20, None).with_verb("ai.image.generate");
    write(&p, &[a, b]);
    let q = UsageQuery {
        verb: Some("ai.image.generate".into()),
        ..Default::default()
    };
    let s = aggregate_path_filtered(&p, &q);
    assert_eq!(s.total.calls, 1);
    assert_eq!(s.total.input_tokens, 20);
}

#[test]
fn app_filter_excludes_records_without_app() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("ai.jsonl");
    let a = rec("anthropic", "sonnet", 10, 10, None).with_app("summarize");
    // No app_id — should be excluded by `app_id: Some(_)`.
    let b = rec("anthropic", "sonnet", 99, 99, None);
    write(&p, &[a, b]);
    let q = UsageQuery {
        app_id: Some("summarize".into()),
        ..Default::default()
    };
    let s = aggregate_path_filtered(&p, &q);
    assert_eq!(s.total.calls, 1);
    assert_eq!(s.total.input_tokens, 10);
}
