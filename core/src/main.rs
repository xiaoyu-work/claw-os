mod agent;
mod ai;
mod apps;
mod approvals;
mod audit;
mod bridge;
mod browser;
mod caps;
mod checkpoint;
mod config;
mod credential;
mod cron;
mod crypto;
mod engine_pkg;
pub mod errors;
mod filelock;
mod i18n;
mod ipc;
mod model;
mod netfilter;
mod paths;
mod perms;
mod policy;
mod proc;
mod router;
mod sandbox;
mod service;
mod sysinfo;
mod trace;
mod watch;

use std::env;
use std::io::IsTerminal;
use std::process;

/// Output formatting mode resolved from argv + tty detection.
#[derive(Clone, Copy, Debug)]
enum OutputFormat {
    /// 2-space-indent JSON, intended for humans reading in a terminal.
    Pretty,
    /// Single-line compact JSON, intended for piping to jq / scripts.
    Compact,
}

/// Pull `--plain` / `--compact` / `--json` / `--pretty` out of argv
/// before the router sees them. Returns the kept args plus the
/// resolved format. When neither flag is present we auto-pretty on a
/// TTY and stay compact for pipes / redirects, matching what most
/// modern CLIs do.
fn extract_format(argv: Vec<String>) -> (Vec<String>, OutputFormat) {
    let mut kept = Vec::with_capacity(argv.len());
    let mut explicit: Option<OutputFormat> = None;
    for a in argv {
        match a.as_str() {
            "--plain" | "--compact" | "--json" => explicit = Some(OutputFormat::Compact),
            "--pretty" => explicit = Some(OutputFormat::Pretty),
            _ => kept.push(a),
        }
    }
    let fmt = explicit.unwrap_or_else(|| {
        if std::io::stdout().is_terminal() {
            OutputFormat::Pretty
        } else {
            OutputFormat::Compact
        }
    });
    (kept, fmt)
}

/// Render a primitive's response string in the chosen format. If the
/// string parses as JSON we re-serialize it; if it doesn't (some
/// commands return plain text), we pass it through unchanged.
fn render(payload: &str, fmt: OutputFormat) -> String {
    match (fmt, serde_json::from_str::<serde_json::Value>(payload)) {
        (OutputFormat::Pretty, Ok(v)) => {
            serde_json::to_string_pretty(&v).unwrap_or_else(|_| payload.to_string())
        }
        (OutputFormat::Compact, Ok(v)) => {
            serde_json::to_string(&v).unwrap_or_else(|_| payload.to_string())
        }
        (_, Err(_)) => payload.to_string(),
    }
}

fn main() {
    let raw_args: Vec<String> = env::args().skip(1).collect();
    let (args, fmt) = extract_format(raw_args);

    // Bootstrap a CLI session if the caller didn't already gate us
    // through one (typical for `cos agent setup`, `cos agent chat`,
    // and other commands a human runs straight from a shell). The
    // guard cleans up its registry row on Drop.
    let _session_guard = caps::bootstrap_user_cli_session();

    let result = router::dispatch(&args);

    match result {
        Ok(Some(output)) => {
            println!("{}", render(&output, fmt));
        }
        Ok(None) => {}
        Err(e) => {
            // If a primitive returned a structured JSON error envelope as
            // its Err string (e.g. `{"error":"agent not configured",
            // "fix":"cos agent setup"}`), surface it as-is instead of
            // re-wrapping it in another `{"error":"..."}` layer. That
            // double-encoding made the structured fields invisible to
            // both humans and `jq` consumers.
            let err = match serde_json::from_str::<serde_json::Value>(&e) {
                Ok(v) if v.is_object() => v,
                _ => serde_json::json!({"error": e.to_string()}),
            };
            let rendered = match fmt {
                OutputFormat::Pretty => serde_json::to_string_pretty(&err)
                    .unwrap_or_else(|_| err.to_string()),
                OutputFormat::Compact => err.to_string(),
            };
            println!("{}", rendered);
            process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
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
}
