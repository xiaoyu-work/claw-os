//! Terminal output formatting for the agent.
//!
//! Consumed by the (future) `clawos/cli/` crate to render
//! conversation, tool calls, and runtime events to a human user.
//! Lives in `core` so the same formatting can be reused by
//! gateway adapters, log dumps, and the TUI without duplicating
//! the layout rules.
//!
//! Library-only — no terminal control code emission. Each function
//! returns a `String` the caller can print, write to a log, or
//! pipe through a colorizer.
//!
//! Pure-functional formatting; ANSI emission lives in the cli
//! crate so headless tools (gateway / mcp-server) don't pull in a
//! tty dependency.

use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
    Tool,
    System,
}

impl Role {
    pub fn label(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
            Self::System => "system",
        }
    }

    /// Single-character glyph for compact transcripts. Avoids
    /// dependence on Unicode-aware terminals.
    pub fn glyph(self) -> char {
        match self {
            Self::User => '>',
            Self::Assistant => '<',
            Self::Tool => '*',
            Self::System => '#',
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayConfig {
    /// Soft wrap text at this column. 0 disables wrapping.
    pub wrap_at: usize,
    /// Indent for continuation lines after the role prefix.
    pub continuation_indent: usize,
    /// If set, truncate long content to this many chars, replacing
    /// the tail with "… (<n> chars omitted)".
    pub truncate_at: Option<usize>,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            wrap_at: 100,
            continuation_indent: 2,
            truncate_at: Some(8000),
        }
    }
}

/// Render one transcript line: `<glyph> [role] <content>`.
pub fn render_message(role: Role, content: &str, cfg: &DisplayConfig) -> String {
    let trimmed = content.trim_end();
    let truncated = match cfg.truncate_at {
        Some(cap) if trimmed.chars().count() > cap => {
            let head: String = trimmed.chars().take(cap).collect();
            let omitted = trimmed.chars().count() - cap;
            format!("{head}… ({omitted} chars omitted)")
        }
        _ => trimmed.to_string(),
    };
    let prefix = format!("{} [{}] ", role.glyph(), role.label());
    if cfg.wrap_at == 0 {
        return format!("{prefix}{truncated}");
    }
    // `wrap_at` is a character budget, so the prefix subtraction must
    // also be in characters — `.len()` is bytes and silently
    // under-budgets continuation lines as soon as a non-ASCII glyph
    // appears in the role prefix (a localised label, a translated
    // glyph, etc.).
    let avail = cfg
        .wrap_at
        .saturating_sub(prefix.chars().count())
        .max(20);
    let mut out = String::new();
    let mut first = true;
    for paragraph in truncated.split('\n') {
        for line in wrap_line(paragraph, avail) {
            if first {
                out.push_str(&prefix);
                out.push_str(&line);
                first = false;
            } else {
                out.push('\n');
                for _ in 0..cfg.continuation_indent {
                    out.push(' ');
                }
                out.push_str(&line);
            }
        }
    }
    out
}

/// Render a tool-call summary line. Mirrors the role glyph
/// approach but adds the tool name.
pub fn render_tool_call(name: &str, args_summary: &str, cfg: &DisplayConfig) -> String {
    render_message(Role::Tool, &format!("call {name}({args_summary})"), cfg)
}

/// Render a tool-result summary line.
pub fn render_tool_result(name: &str, ok: bool, summary: &str, cfg: &DisplayConfig) -> String {
    let status = if ok { "ok" } else { "err" };
    render_message(
        Role::Tool,
        &format!("result {name} [{status}] {summary}"),
        cfg,
    )
}

/// Format a Duration for transcript rows: `12.3s` / `450ms` /
/// `1m 23.4s`. Always 1 decimal place for sub-minute values.
pub fn format_duration(d: Duration) -> String {
    let secs = d.as_secs_f64();
    if secs < 1.0 {
        format!("{}ms", d.as_millis())
    } else if secs < 60.0 {
        format!("{secs:.1}s")
    } else {
        let mins = (secs / 60.0).floor() as u64;
        let rem = secs - (mins as f64 * 60.0);
        format!("{mins}m {rem:.1}s")
    }
}

/// Format byte counts for human display: `1.2 KB`, `45.6 MB`.
/// Uses powers of 1024 (binary) since the agent mostly deals with
/// in-memory buffers.
pub fn format_bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut value = n as f64;
    let mut idx = 0usize;
    while value >= 1024.0 && idx + 1 < UNITS.len() {
        value /= 1024.0;
        idx += 1;
    }
    format!("{value:.1} {}", UNITS[idx])
}

/// Format a usage row for `cos agent status`: aligns provider /
/// model / count columns into a single string. Token-only — the
/// kernel never measures usage in dollars.
pub fn format_usage_row(provider: &str, model: &str, count: u64) -> String {
    format!("{provider:<12} {model:<32} {count:>6}")
}

fn wrap_line(line: &str, width: usize) -> Vec<String> {
    if line.is_empty() {
        return vec![String::new()];
    }
    let chars: Vec<char> = line.chars().collect();
    let mut out = Vec::new();
    let mut start = 0usize;
    while start < chars.len() {
        let end = (start + width).min(chars.len());
        if end >= chars.len() {
            out.push(chars[start..end].iter().collect());
            break;
        }
        // Try to break at the last space within the window.
        let mut split = end;
        if let Some(pos) = chars[start..end].iter().rposition(|c| *c == ' ') {
            if pos > 0 {
                split = start + pos;
            }
        }
        out.push(chars[start..split].iter().collect::<String>());
        // Skip the space we broke on (if any).
        start = if split < chars.len() && chars[split] == ' ' {
            split + 1
        } else {
            split
        };
    }
    out
}

#[cfg(test)]
mod tests {
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
}
