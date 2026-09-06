use super::*;

#[test]
fn wire_version_must_be_an_explicit_leading_v1_flag() {
    let (args, enabled) =
        extract_wire_version(vec!["--wire=1".into(), "ai".into(), "tools".into()]).unwrap();
    assert!(enabled);
    assert_eq!(args, ["ai", "tools"]);

    let (args, enabled) =
        extract_wire_version(vec!["ai".into(), "tools".into(), "--wire=1".into()]).unwrap();
    assert!(!enabled);
    assert_eq!(args, ["ai", "tools", "--wire=1"]);

    assert!(extract_wire_version(vec!["--wire=2".into(), "ai".into()]).is_err());
}

#[test]
fn extract_format_strips_plain_flag() {
    let (args, fmt) = extract_format(vec!["agent".into(), "--plain".into(), "status".into()]);
    assert_eq!(args, vec!["agent".to_string(), "status".to_string()]);
    assert!(matches!(fmt, OutputFormat::Compact));
}

#[test]
fn extract_format_recognises_pretty_alias() {
    let (_, fmt) = extract_format(vec!["agent".into(), "--pretty".into()]);
    assert!(matches!(fmt, OutputFormat::Pretty));
}

#[test]
fn extract_format_recognises_compact_aliases() {
    for alias in ["--plain", "--compact", "--json"] {
        let (_, fmt) = extract_format(vec!["agent".into(), alias.into()]);
        assert!(matches!(fmt, OutputFormat::Compact), "alias {alias}");
    }
}

#[test]
fn extract_format_preserves_delimiter_and_app_data_flags() {
    let original = vec![
        "app".to_string(),
        "exec".to_string(),
        "run".to_string(),
        "--".to_string(),
        "--json".to_string(),
        "--plain".to_string(),
        "--compact".to_string(),
        "--pretty".to_string(),
    ];
    let (args, _) = extract_format(original.clone());
    assert_eq!(args, original);
}

#[test]
fn extract_format_leaves_internal_bridge_argv_alone() {
    // `cos __memory remember --json <payload>` is a private wire
    // format between the SDK and the kernel: stripping `--json` here
    // would silently deliver a payload-less call to the bridge.
    let original = vec![
        "__memory".to_string(),
        "remember".to_string(),
        "--json".to_string(),
        "{\"source\":\"expense-tracker\",\"text\":\"x\"}".to_string(),
    ];
    let (args, fmt) = extract_format(original.clone());
    assert_eq!(args, original);
    assert!(matches!(fmt, OutputFormat::Compact));
}

#[test]
fn stdin_request_is_explicit_and_respects_end_of_options() {
    let (args, requested) = extract_stdin_request(
        vec![
            "app".into(),
            "doc".into(),
            "rewrite".into(),
            "--stdin".into(),
        ],
        true,
    );
    assert!(requested);
    assert_eq!(args, ["app", "doc", "rewrite"]);

    for command in ["redact", "tokens", "summarise"] {
        let original = vec![
            "agent".to_string(),
            command.to_string(),
            "--stdin".to_string(),
        ];
        let (args, requested) = extract_stdin_request(original.clone(), false);
        assert!(!requested, "{command}");
        assert_eq!(args, original, "{command}");
    }

    let (args, requested) = extract_stdin_request(
        vec![
            "app".into(),
            "doc".into(),
            "rewrite".into(),
            "--".into(),
            "--stdin".into(),
        ],
        true,
    );
    assert!(!requested);
    assert_eq!(args, ["app", "doc", "rewrite", "--", "--stdin"]);
}

#[test]
fn stdin_request_is_not_consumed_for_app_management_or_non_stdin_operations() {
    for original in [
        vec![
            "app".into(),
            "install".into(),
            "source".into(),
            "--stdin".into(),
        ],
        vec![
            "app".into(),
            "create".into(),
            "demo".into(),
            "--stdin".into(),
        ],
        vec!["app".into(), "tool".into(), "list".into(), "--stdin".into()],
        vec![
            "app".into(),
            "doc".into(),
            "rewrite".into(),
            "--schema".into(),
            "--stdin".into(),
        ],
        vec!["app".into(), "fs".into(), "read".into(), "--stdin".into()],
    ] {
        let (args, requested) = extract_stdin_request(original.clone(), false);
        assert!(!requested);
        assert_eq!(args, original);
    }
}

#[test]
fn requested_stdin_is_streamed_with_a_hard_limit() {
    assert_eq!(
        read_requested_stdin(std::io::Cursor::new(b"1234"), 4).unwrap(),
        b"1234"
    );
    let error = read_requested_stdin(std::io::Cursor::new(b"12345"), 4).unwrap_err();
    assert!(error.contains("4-byte limit"), "unexpected: {error}");
}

#[test]
fn json_stdin_marker_is_preserved_and_input_is_bounded() {
    let original = vec![
        "app".into(),
        "fs".into(),
        "write".into(),
        "--args-stdin".into(),
    ];
    let (args, requested) = extract_stdin_request(original.clone(), true);
    assert!(requested);
    assert_eq!(args, original);
    assert!(!extract_stdin_request(original, false).1);
    let limit = cos::clawd::wire::bounded::APP_ARGS_STDIN_MAX_BYTES;
    assert_eq!(
        read_requested_stdin(std::io::repeat(b' '), limit).unwrap_err(),
        format!("App stdin exceeds configured {limit}-byte limit")
    );
    assert_eq!(
        read_requested_stdin(&vec![b'x'; limit][..], limit)
            .unwrap()
            .len(),
        limit
    );
}

#[test]
fn render_pretty_indents_json() {
    let out = render("{\"a\":1,\"b\":[2,3]}", OutputFormat::Pretty);
    assert!(out.contains("\n"));
    assert!(out.contains("  \"a\""));
}

#[test]
fn render_compact_strips_whitespace() {
    let out = render("{\n  \"a\": 1\n}", OutputFormat::Compact);
    assert_eq!(out, "{\"a\":1}");
}

#[test]
fn render_passes_non_json_through_unchanged() {
    let raw = "plain text output";
    assert_eq!(render(raw, OutputFormat::Pretty), raw);
    assert_eq!(render(raw, OutputFormat::Compact), raw);
}

#[test]
fn wire_success_requires_an_object_and_wraps_it_once() {
    assert_eq!(
        wire_success(r#"{"text":"hello"}"#).unwrap(),
        serde_json::json!({
            "ok": true,
            "wire_version": 1,
            "data": {"text": "hello"},
        })
    );
    assert!(wire_success("null").is_err());
    assert!(wire_success("plain text").is_err());
}

#[test]
fn wire_failure_promotes_protocol_fields_and_keeps_detail() {
    assert_eq!(
        wire_failure(
            r#"{"error":"denied","code":"PERMISSION_DENIED","audit_id":"01J","scope":"fs"}"#
        ),
        serde_json::json!({
            "ok": false,
            "wire_version": 1,
            "error": "denied",
            "code": "PERMISSION_DENIED",
            "audit_id": "01J",
            "detail": {"scope": "fs"},
        })
    );
    assert_eq!(
        wire_failure("boom"),
        serde_json::json!({
            "ok": false,
            "wire_version": 1,
            "error": "boom",
            "code": "INTERNAL_ERROR",
        })
    );
}
