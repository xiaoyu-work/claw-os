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

const DEFAULT_APP_STDIN_MAX_BYTES: usize = 16 * 1024 * 1024;
const WIRE_V1_FLAG: &str = "--wire=1";

fn extract_wire_version(mut argv: Vec<String>) -> Result<(Vec<String>, bool), String> {
    let Some(first) = argv.first() else {
        return Ok((argv, false));
    };
    if first == WIRE_V1_FLAG {
        argv.remove(0);
        return Ok((argv, true));
    }
    if first.starts_with("--wire=") {
        return Err(format!(
            "unsupported wire version in `{first}`; this kernel supports `--wire=1`"
        ));
    }
    Ok((argv, false))
}

/// Pull `--plain` / `--compact` / `--json` / `--pretty` out of argv
/// before the router sees them. Returns the kept args plus the
/// resolved format. When neither flag is present we auto-pretty on a
/// TTY and stay compact for pipes / redirects, matching what most
/// modern CLIs do.
///
/// Hidden internal bridges (`__memory`, `__policy`, …) are exempt:
/// their argv is a private wire format between the SDK and the kernel,
/// and `cos __memory remember --json <payload>` must reach the bridge
/// with its flag intact. They always answer in compact JSON anyway.
fn extract_format(argv: Vec<String>) -> (Vec<String>, OutputFormat) {
    if argv.first().is_some_and(|first| first.starts_with("__")) {
        return (argv, OutputFormat::Compact);
    }
    let mut kept = Vec::with_capacity(argv.len());
    let mut explicit: Option<OutputFormat> = None;
    let mut options = true;
    for a in argv {
        if options && a == "--" {
            options = false;
            kept.push(a);
            continue;
        }
        match (options, a.as_str()) {
            (true, "--plain" | "--compact" | "--json") => explicit = Some(OutputFormat::Compact),
            (true, "--pretty") => explicit = Some(OutputFormat::Pretty),
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

fn extract_stdin_request(argv: Vec<String>, operation_accepts_stdin: bool) -> (Vec<String>, bool) {
    if !operation_accepts_stdin {
        return (argv, false);
    }
    if argv.get(3..).is_some_and(|args| args == ["--args-stdin"]) {
        return (argv, true);
    }
    let mut kept = Vec::with_capacity(argv.len());
    let mut requested = false;
    let mut options = true;
    for (index, arg) in argv.into_iter().enumerate() {
        if index < 3 {
            kept.push(arg);
        } else if options && arg == "--" {
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

fn app_stdin_max_bytes() -> Result<usize, String> {
    match std::env::var("COS_APP_STDIN_MAX_BYTES") {
        Ok(raw) => raw
            .parse::<usize>()
            .ok()
            .filter(|value| *value > 0)
            .ok_or_else(|| "COS_APP_STDIN_MAX_BYTES must be a positive integer".to_string()),
        Err(std::env::VarError::NotPresent) => Ok(DEFAULT_APP_STDIN_MAX_BYTES),
        Err(error) => Err(format!("read COS_APP_STDIN_MAX_BYTES: {error}")),
    }
}

fn read_requested_stdin<R: Read>(reader: R, limit: usize) -> Result<Vec<u8>, String> {
    let mut data = Vec::new();
    reader
        .take(limit.saturating_add(1) as u64)
        .read_to_end(&mut data)
        .map_err(|error| format!("read requested stdin: {error}"))?;
    if data.len() > limit {
        return Err(format!("App stdin exceeds configured {limit}-byte limit"));
    }
    Ok(data)
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

fn wire_success(payload: &str) -> Result<serde_json::Value, String> {
    let data: serde_json::Value = serde_json::from_str(payload)
        .map_err(|error| format!("wire reply is not JSON: {error}"))?;
    if !data.is_object() {
        return Err("wire success payload must be a JSON object".to_string());
    }
    Ok(serde_json::json!({
        "ok": true,
        "wire_version": 1,
        "data": data,
    }))
}

fn wire_failure(error: &str) -> serde_json::Value {
    let mut source = serde_json::from_str::<serde_json::Value>(error)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    let message = source
        .remove("error")
        .and_then(|value| value.as_str().map(str::to_string))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            if error.is_empty() {
                "kernel request failed".to_string()
            } else {
                error.to_string()
            }
        });
    let code = source
        .remove("code")
        .and_then(|value| value.as_str().map(str::to_string))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "INTERNAL_ERROR".to_string());
    let audit_id = source
        .remove("audit_id")
        .and_then(|value| value.as_str().map(str::to_string));
    let mut detail = source
        .remove("detail")
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    detail.extend(source);

    let mut envelope = serde_json::json!({
        "ok": false,
        "wire_version": 1,
        "error": message,
        "code": code,
    });
    if let Some(audit_id) = audit_id {
        envelope["audit_id"] = serde_json::Value::String(audit_id);
    }
    if !detail.is_empty() {
        envelope["detail"] = serde_json::Value::Object(detail);
    }
    envelope
}

fn main() {
    let raw_args: Vec<String> = env::args().skip(1).collect();
    let (raw_args, wire_v1) = match extract_wire_version(raw_args) {
        Ok(parsed) => parsed,
        Err(error) => {
            println!(
                "{}",
                wire_failure(
                    &serde_json::json!({
                        "error": error,
                        "code": "INVALID_ARGS",
                    })
                    .to_string()
                )
            );
            process::exit(1);
        }
    };

    // After selecting the output protocol but before request argv is
    // interpreted or a session is bootstrapped, reject a `cos` binary
    // older than the release this system has already accepted. Decoding
    // the protocol first keeps this fail-closed refusal machine-readable.
    if let Err(refusal) =
        cos::update::runtime::enforce_startup(cos::update::runtime::Scope::CompiledEpoch)
    {
        if wire_v1 {
            println!(
                "{}",
                wire_failure(
                    &serde_json::json!({
                        "error": refusal.to_string(),
                        "code": "KERNEL_UNAVAILABLE",
                    })
                    .to_string()
                )
            );
        } else {
            eprintln!("cos: {refusal}");
        }
        process::exit(1);
    }
    let (raw_args, fmt) = extract_format(raw_args);
    let fmt = if wire_v1 { OutputFormat::Compact } else { fmt };
    let operation_accepts_stdin = router::app_operation_accepts_stdin(&raw_args);
    let (args, stdin_requested) = extract_stdin_request(raw_args, operation_accepts_stdin);

    // Bootstrap a CLI session if the caller didn't already gate us
    // through one (typical for `cos agent setup`, `cos agent chat`,
    // and other commands a human runs straight from a shell). The
    // guard cleans up its registry row on Drop.
    let _session_guard = caps::bootstrap_user_cli_session(&args);

    let result = if stdin_requested {
        match app_stdin_max_bytes().and_then(|limit| {
            let limit = if args.get(3..).is_some_and(|args| args == ["--args-stdin"]) {
                limit.min(cos::clawd::wire::bounded::APP_ARGS_STDIN_MAX_BYTES)
            } else {
                limit
            };
            read_requested_stdin(std::io::stdin(), limit)
        }) {
            Ok(stdin_data) => router::dispatch_with_stdin(&args, Some(stdin_data)),
            Err(error) => Err(error),
        }
    } else {
        router::dispatch(&args)
    };

    match result {
        Ok(Some(output)) => {
            if wire_v1 {
                match wire_success(&output) {
                    Ok(envelope) => println!("{envelope}"),
                    Err(error) => {
                        println!(
                            "{}",
                            wire_failure(
                                &serde_json::json!({
                                    "error": error,
                                    "code": "INTERNAL_ERROR",
                                })
                                .to_string()
                            )
                        );
                        process::exit(1);
                    }
                }
            } else {
                println!("{}", render(&output, fmt));
            }
        }
        Ok(None) if wire_v1 => {
            println!(
                "{}",
                wire_failure(
                    r#"{"error":"wire request completed without a response payload","code":"INTERNAL_ERROR"}"#
                )
            );
            process::exit(1);
        }
        Ok(None) => {}
        Err(e) => {
            if wire_v1 {
                println!("{}", wire_failure(&e));
                process::exit(1);
            }
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
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/main.rs"));
}
