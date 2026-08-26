use super::*;

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
