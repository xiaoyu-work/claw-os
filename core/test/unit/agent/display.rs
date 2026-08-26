use super::*;

#[test]
fn role_label_and_glyph() {
    assert_eq!(Role::User.label(), "user");
    assert_eq!(Role::Assistant.glyph(), '<');
    assert_eq!(Role::Tool.glyph(), '*');
    assert_eq!(Role::System.label(), "system");
}

#[test]
fn render_message_no_wrap_no_truncate() {
    let cfg = DisplayConfig {
        wrap_at: 0,
        truncate_at: None,
        ..DisplayConfig::default()
    };
    let s = render_message(Role::Assistant, "hello world", &cfg);
    assert_eq!(s, "< [assistant] hello world");
}

#[test]
fn render_message_truncates() {
    let cfg = DisplayConfig {
        wrap_at: 0,
        truncate_at: Some(5),
        continuation_indent: 0,
    };
    let s = render_message(Role::User, "abcdefghij", &cfg);
    assert!(s.contains("abcde"));
    assert!(s.contains("5 chars omitted"));
}

#[test]
fn render_message_wraps_long_lines() {
    let cfg = DisplayConfig {
        wrap_at: 30,
        truncate_at: None,
        continuation_indent: 2,
    };
    let s = render_message(Role::User, "one two three four five six seven", &cfg);
    let lines: Vec<&str> = s.split('\n').collect();
    assert!(lines.len() > 1);
    // First line carries the role prefix.
    assert!(lines[0].starts_with("> [user] "));
    // Continuation lines have the indent.
    for line in &lines[1..] {
        assert!(line.starts_with("  "));
    }
}

#[test]
fn render_message_preserves_paragraph_breaks() {
    let cfg = DisplayConfig {
        wrap_at: 80,
        truncate_at: None,
        continuation_indent: 0,
    };
    let s = render_message(Role::Assistant, "first\nsecond", &cfg);
    let lines: Vec<&str> = s.split('\n').collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].ends_with("first"));
    assert_eq!(lines[1], "second");
}

#[test]
fn render_tool_call_and_result() {
    let cfg = DisplayConfig {
        wrap_at: 0,
        truncate_at: None,
        ..DisplayConfig::default()
    };
    let c = render_tool_call("cos_fs", "{path: '/a'}", &cfg);
    assert!(c.contains("call cos_fs"));
    let r = render_tool_result("cos_fs", true, "wrote 12 bytes", &cfg);
    assert!(r.contains("ok"));
    assert!(r.contains("wrote 12 bytes"));
    let e = render_tool_result("cos_fs", false, "EACCES", &cfg);
    assert!(e.contains("err"));
}

#[test]
fn format_duration_buckets() {
    assert_eq!(format_duration(Duration::from_millis(450)), "450ms");
    assert_eq!(format_duration(Duration::from_secs_f64(12.345)), "12.3s");
    let s = format_duration(Duration::from_secs_f64(83.4));
    assert_eq!(s, "1m 23.4s");
}

#[test]
fn format_bytes_buckets() {
    assert_eq!(format_bytes(0), "0 B");
    assert_eq!(format_bytes(1023), "1023 B");
    assert_eq!(format_bytes(1024), "1.0 KB");
    assert_eq!(format_bytes(1024 * 1024), "1.0 MB");
    assert_eq!(format_bytes(1024 * 1024 * 1024), "1.0 GB");
}

#[test]
fn format_usage_row_columns_align() {
    let s = format_usage_row("openai", "gpt-5", 100);
    assert!(s.contains("openai"));
    assert!(s.contains("gpt-5"));
    assert!(s.contains("100"));
}

#[test]
fn wrap_line_breaks_at_spaces_when_possible() {
    let lines = wrap_line("abc def ghi", 6);
    assert_eq!(lines, vec!["abc", "def", "ghi"]);
}

#[test]
fn wrap_line_falls_back_to_hard_break_for_long_words() {
    let lines = wrap_line("abcdefghijkl", 4);
    assert!(lines.iter().all(|l| l.chars().count() <= 4));
    let joined: String = lines.join("");
    assert_eq!(joined, "abcdefghijkl");
}

#[test]
fn wrap_line_empty_returns_single_empty() {
    assert_eq!(wrap_line("", 10), vec![String::new()]);
}
