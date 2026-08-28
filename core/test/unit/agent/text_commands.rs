use super::*;
use crate::agent::run;

#[test]
fn prompt_show_returns_prompt_string() {
    let v = prompt_cmd(&[]).expect("prompt show ok");
    let p = v
        .get("prompt")
        .and_then(|x| x.as_str())
        .expect("prompt str");
    assert!(!p.is_empty());
    let chars = v.get("chars").and_then(|x| x.as_u64()).expect("chars");
    assert!(chars > 0);
}

#[test]
fn prompt_show_default_includes_size_breakdown() {
    let v = prompt_cmd(&["show".into()]).expect("show ok");
    assert!(v.get("scaffold_chars").is_some());
    assert!(v.get("approx_tokens").is_some());
    assert!(v.get("extra_path").is_some()); // null when not provided
    assert_eq!(v["scope"], "new-session-candidate");
    assert_eq!(
        v["prompt_version"],
        crate::agent::prompt::CANONICAL_PROMPT_VERSION
    );
    assert!(v.get("turn_context_sources").is_some());
}

#[test]
fn prompt_raw_omits_size_breakdown() {
    let v = prompt_cmd(&["show".into(), "--raw".into()]).expect("raw ok");
    assert!(v.get("prompt").is_some());
    assert!(v.get("scaffold_chars").is_none());
    assert!(v.get("extra_path").is_none());
    assert!(v.get("turn_context").is_some());
}

#[test]
fn prompt_extra_appends_file_content() {
    let dir = tempfile::tempdir().expect("tmp");
    let extra = dir.path().join("preface.md");
    std::fs::write(&extra, "ZZZUNIQUEMARKERZZZ_extra_preface_text").expect("write");
    let baseline = prompt_cmd(&["show".into()]).expect("baseline");
    let with_extra = prompt_cmd(&[
        "show".into(),
        "--extra".into(),
        extra.to_string_lossy().to_string(),
    ])
    .expect("with extra");
    let baseline_chars = baseline.get("chars").and_then(|x| x.as_u64()).unwrap();
    let extra_chars = with_extra.get("chars").and_then(|x| x.as_u64()).unwrap();
    assert!(extra_chars > baseline_chars, "extra should grow prompt");
    let p = with_extra.get("prompt").and_then(|x| x.as_str()).unwrap();
    assert!(
        p.contains("ZZZUNIQUEMARKERZZZ_extra_preface_text"),
        "extra content must be in prompt"
    );
    assert_eq!(
        with_extra.get("extra_path").and_then(|x| x.as_str()),
        Some(extra.to_string_lossy().as_ref())
    );
}

#[test]
fn prompt_build_alias_works() {
    let v = prompt_cmd(&["build".into()]).expect("build ok");
    assert!(v.get("prompt").is_some());
}

#[test]
fn prompt_extra_nonexistent_file_does_not_panic() {
    // build_system_prompt silently swallows file IO errors and
    // falls back to scaffold-only — preserve that here.
    let v = prompt_cmd(&[
        "show".into(),
        "--extra".into(),
        "Z:\\definitely\\not\\a\\real\\path".into(),
    ])
    .expect("ok");
    assert!(v.get("prompt").and_then(|x| x.as_str()).is_some());
}

#[test]
fn think_scrub_strips_think_block() {
    let v = think_scrub_cmd(&["before <think>secret reasoning</think> after".into()]).expect("ok");
    let out = v.get("scrubbed").and_then(|x| x.as_str()).unwrap();
    assert!(!out.contains("secret reasoning"), "got {out}");
    assert!(out.contains("before"));
    assert!(out.contains("after"));
    assert_eq!(v.get("changed").and_then(|x| x.as_bool()), Some(true));
}

#[test]
fn think_scrub_unchanged_for_clean_input() {
    let v = think_scrub_cmd(&["just plain text".into()]).expect("ok");
    assert_eq!(v.get("changed").and_then(|x| x.as_bool()), Some(false));
}

#[test]
fn think_scrub_check_returns_detection_only() {
    let v = think_scrub_cmd(&[
        "--check".into(),
        "<thinking>internal</thinking> answer".into(),
    ])
    .expect("ok");
    assert_eq!(v.get("has_thinking").and_then(|x| x.as_bool()), Some(true));
    assert!(v.get("scrubbed").is_none());
}

#[test]
fn think_scrub_check_negative() {
    let v = think_scrub_cmd(&["--check".into(), "no tags here".into()]).expect("ok");
    assert_eq!(v.get("has_thinking").and_then(|x| x.as_bool()), Some(false));
}

#[test]
fn think_scrub_handles_multiline_block() {
    let v = think_scrub_cmd(&["<thinking>\nline one\nline two\n</thinking>\nfinal".into()])
        .expect("ok");
    let out = v.get("scrubbed").and_then(|x| x.as_str()).unwrap();
    assert!(!out.contains("line one"), "got {out}");
    assert!(out.contains("final"));
}

#[test]
fn think_scrub_from_file() {
    let dir = tempfile::tempdir().expect("tmp");
    let p = dir.path().join("trace.txt");
    std::fs::write(&p, "<reasoning>internal</reasoning>\nthe answer is 42").expect("write");
    let v = think_scrub_cmd(&["--file".into(), p.to_string_lossy().to_string()]).expect("ok");
    let out = v.get("scrubbed").and_then(|x| x.as_str()).unwrap();
    assert!(!out.contains("internal"), "got {out}");
    assert!(out.contains("the answer is 42"));
}

#[test]
fn tokens_basic_input() {
    // chars / 4 with a min of 1 — see estimate_text_tokens.
    let v = tokens_cmd(&["hello world this is some text".into()]).expect("ok");
    let chars = v.get("chars").and_then(|x| x.as_u64()).unwrap();
    let tokens = v.get("approx_tokens").and_then(|x| x.as_u64()).unwrap();
    assert_eq!(chars, "hello world this is some text".len() as u64);
    assert!(tokens >= 1);
    assert!(tokens <= chars, "tokens should be <= chars");
}

#[test]
fn tokens_from_file() {
    let dir = tempfile::tempdir().expect("tmp");
    let p = dir.path().join("body.txt");
    let content = "x".repeat(400);
    std::fs::write(&p, &content).expect("write");
    let v = tokens_cmd(&["--file".into(), p.to_string_lossy().to_string()]).expect("ok");
    assert_eq!(v.get("chars").and_then(|x| x.as_u64()), Some(400));
    // chars / 4 = 100
    assert_eq!(v.get("approx_tokens").and_then(|x| x.as_u64()), Some(100));
}

#[test]
fn tokens_includes_method_label() {
    let v = tokens_cmd(&["abc".into()]).expect("ok");
    let m = v.get("method").and_then(|x| x.as_str()).unwrap();
    assert!(m.contains("chars"), "got {m}");
}

#[test]
fn read_text_input_joins_positional_with_spaces() {
    let (s, _) = read_text_input(&["a".into(), "b".into(), "c".into()], "tokens").expect("ok");
    assert_eq!(s, "a b c");
}

#[test]
fn title_cmd_returns_first_line_clamped() {
    let v = title_cmd(&["hello world".into()]).expect("title ok");
    assert_eq!(v.get("title").and_then(|s| s.as_str()), Some("hello world"));
    assert_eq!(v.get("method").and_then(|s| s.as_str()), Some("heuristic"));
}

#[test]
fn title_cmd_strips_slash_command_verb() {
    let v = title_cmd(&["/ask hello there".into()]).expect("title ok");
    assert_eq!(v.get("title").and_then(|s| s.as_str()), Some("hello there"));
}

#[test]
fn title_cmd_takes_first_line_only() {
    let v = title_cmd(&["one\ntwo\nthree".into()]).expect("title ok");
    assert_eq!(v.get("title").and_then(|s| s.as_str()), Some("one"));
}

#[test]
fn title_cmd_empty_input_falls_back_to_untitled() {
    let v = title_cmd(&["   ".into()]).expect("title ok");
    assert_eq!(v.get("title").and_then(|s| s.as_str()), Some("untitled"));
}

#[test]
fn title_cmd_requires_some_input() {
    let err = title_cmd(&[]).unwrap_err();
    assert!(err.contains("title"));
}

#[test]
fn title_cmd_llm_without_aux_errs() {
    // No auxiliary config in test env → CLI should err clearly.
    let err = title_cmd(&["hello".into(), "--llm".into()]).unwrap_err();
    assert!(err.contains("auxiliary"));
}

#[test]
fn title_cmd_llm_flag_is_consumed_not_treated_as_input() {
    // Without --llm we still get heuristic from "hello"; confirms
    // flag isn't joined into the input.
    let v = title_cmd(&["hello".into()]).expect("title ok");
    assert_eq!(v.get("title").and_then(|s| s.as_str()), Some("hello"));
}

#[test]
fn title_cmd_with_aux_none_falls_back_to_heuristic() {
    let v = title_cmd_with_aux("/help me", None).expect("ok");
    assert_eq!(v.get("method").and_then(|s| s.as_str()), Some("heuristic"));
    assert_eq!(v.get("title").and_then(|s| s.as_str()), Some("me"));
}

#[test]
fn title_cmd_with_aux_uses_mock_response() {
    use crate::agent::llm::auxiliary::{AuxiliaryClient, AuxiliaryConfig};
    use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
    use crate::config::AgentConfig;
    let cfg = AgentConfig::default();
    let provider = MockProvider::new("title-mock", &cfg);
    provider.push_response(MockResponse::Text("Quick rust setup".into()));
    let aux = AuxiliaryClient::new(
        std::sync::Arc::new(provider),
        AuxiliaryConfig::new("mock", "title-mock"),
    );
    let v = title_cmd_with_aux("How do I install rust?", Some(&aux)).expect("ok");
    assert_eq!(v.get("method").and_then(|s| s.as_str()), Some("llm"));
    assert_eq!(
        v.get("title").and_then(|s| s.as_str()),
        Some("Quick rust setup")
    );
    assert_eq!(v.get("provider").and_then(|s| s.as_str()), Some("mock"));
    assert_eq!(v.get("model").and_then(|s| s.as_str()), Some("title-mock"));
}

#[test]
fn summarise_cmd_returns_first_sentence() {
    let v = summarise_cmd(&["First sentence. Second one.".into()]).expect("summarise ok");
    assert_eq!(
        v.get("summary").and_then(|s| s.as_str()),
        Some("First sentence.")
    );
    assert_eq!(v.get("clamped").and_then(|b| b.as_bool()), Some(false));
}

#[test]
fn summarise_cmd_clamps_to_max_with_ellipsis() {
    let v = summarise_cmd(&[
        "abcdefghij no terminator".into(),
        "--max".into(),
        "5".into(),
    ])
    .expect("summarise ok");
    let s = v.get("summary").and_then(|s| s.as_str()).unwrap_or("");
    assert_eq!(s.chars().count(), 5);
    assert!(s.ends_with('…'), "should end with ellipsis: {s:?}");
    assert_eq!(v.get("clamped").and_then(|b| b.as_bool()), Some(true));
}

#[test]
fn summarise_cmd_default_max_is_200() {
    let v = summarise_cmd(&["short input".into()]).expect("summarise ok");
    assert_eq!(v.get("max_chars").and_then(|n| n.as_u64()), Some(200));
}

#[test]
fn summarise_cmd_max_must_parse() {
    let err = summarise_cmd(&["--max".into(), "not-a-number".into(), "x".into()]).unwrap_err();
    assert!(err.contains("--max"));
}

#[test]
fn summarize_alias_dispatches_to_summarise() {
    // Confirm the US-spelling alias hits the same handler (now under `dev`).
    let v = run("dev", &["summarize".into(), "hello.".into()]).expect("summarize ok");
    assert_eq!(v.get("summary").and_then(|s| s.as_str()), Some("hello."));
}

#[test]
fn summarise_cmd_llm_without_aux_errs() {
    let err = summarise_cmd(&["hello there".into(), "--llm".into()]).unwrap_err();
    assert!(err.contains("auxiliary"));
}

#[test]
fn summarise_cmd_with_aux_none_falls_back_to_heuristic() {
    let v = summarise_cmd_with_aux("First sentence. Second one.", 200, None).expect("ok");
    assert_eq!(v.get("method").and_then(|s| s.as_str()), Some("heuristic"));
}

#[test]
fn summarise_cmd_with_aux_uses_mock_response_when_input_exceeds_max() {
    use crate::agent::llm::auxiliary::{AuxiliaryClient, AuxiliaryConfig};
    use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
    use crate::config::AgentConfig;
    let cfg = AgentConfig::default();
    let provider = MockProvider::new("sum-mock", &cfg);
    provider.push_response(MockResponse::Text("Compact summary".into()));
    let aux = AuxiliaryClient::new(
        std::sync::Arc::new(provider),
        AuxiliaryConfig::new("mock", "sum-mock"),
    );
    // Input must exceed max_chars to trigger the aux path (see summarise()).
    let big = "long ".repeat(60);
    let v = summarise_cmd_with_aux(&big, 50, Some(&aux)).expect("ok");
    assert_eq!(v.get("method").and_then(|s| s.as_str()), Some("llm"));
    assert_eq!(
        v.get("summary").and_then(|s| s.as_str()),
        Some("Compact summary")
    );
    assert_eq!(v.get("provider").and_then(|s| s.as_str()), Some("mock"));
}

#[test]
fn classify_cmd_matches_label_case_insensitively() {
    let v = classify_cmd(&[
        "POSITIVE".into(),
        "--labels".into(),
        "positive,negative,neutral".into(),
    ])
    .expect("classify ok");
    assert_eq!(v.get("matched").and_then(|m| m.as_str()), Some("positive"));
}

#[test]
fn classify_cmd_returns_null_on_no_match() {
    let v = classify_cmd(&[
        "definitely not a label".into(),
        "--labels".into(),
        "yes,no".into(),
    ])
    .expect("classify ok");
    assert_eq!(v.get("matched"), Some(&serde_json::Value::Null));
}

#[test]
fn classify_cmd_tolerates_trailing_punctuation() {
    let v =
        classify_cmd(&["yes.".into(), "--labels".into(), "yes,no".into()]).expect("classify ok");
    assert_eq!(v.get("matched").and_then(|m| m.as_str()), Some("yes"));
}

#[test]
fn classify_cmd_requires_labels_flag() {
    let err = classify_cmd(&["yes".into()]).unwrap_err();
    assert!(err.contains("--labels"));
}

#[test]
fn classify_cmd_empty_label_list_rejected() {
    let err = classify_cmd(&["yes".into(), "--labels".into(), ",, ,".into()]).unwrap_err();
    assert!(err.contains("--labels"));
}

#[test]
fn classify_cmd_returns_label_set_in_response() {
    let v = classify_cmd(&["yes".into(), "--labels".into(), "yes,no,maybe".into()])
        .expect("classify ok");
    let labels = v
        .get("labels")
        .and_then(|l| l.as_array())
        .expect("labels array");
    assert_eq!(labels.len(), 3);
}
