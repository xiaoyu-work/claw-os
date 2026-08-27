use cos::{caps, router};
use std::env;
use std::io::{IsTerminal, Read};
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

fn extract_stdin_request(argv: Vec<String>) -> (Vec<String>, bool) {
    let mut kept = Vec::with_capacity(argv.len());
    let mut requested = false;
    let mut options = true;
    for arg in argv {
        if options && arg == "--" {
            options = false;
            kept.push(arg);
        } else if options && arg == "--stdin" {
            requested = true;
        } else {
            kept.push(arg);
        }
    }
    (kept, requested)
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
    let (raw_args, stdin_requested) = extract_stdin_request(raw_args);
    let (args, fmt) = extract_format(raw_args);

    // Bootstrap a CLI session if the caller didn't already gate us
    // through one (typical for `cos agent setup`, `cos agent chat`,
    // and other commands a human runs straight from a shell). The
    // guard cleans up its registry row on Drop.
    let _session_guard = caps::bootstrap_user_cli_session(&args);

    let mut stdin_data = Vec::new();
    let result = if stdin_requested {
        match std::io::stdin().read_to_end(&mut stdin_data) {
            Ok(_) => router::dispatch_with_stdin(&args, Some(&stdin_data)),
            Err(error) => Err(format!("read requested stdin: {error}")),
        }
    } else {
        router::dispatch(&args)
    };

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
                OutputFormat::Pretty => {
                    serde_json::to_string_pretty(&err).unwrap_or_else(|_| err.to_string())
                }
                OutputFormat::Compact => err.to_string(),
            };
            println!("{}", rendered);
            process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/main.rs"
    ));
}
