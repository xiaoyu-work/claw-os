//! Capture ordinary App return values without claiming verified effects.

use crate::activities::{ReceiptOutcome, ReceiptReport, ResultKind, ResultSummary};
use crate::agent::safety::redact::Redactor;

const MAX_RESULT_BYTES: usize = 16 * 1024 * 1024;
const MAX_PREVIEW_BYTES: usize = 2048;

pub fn capture(
    id: String,
    app_id: String,
    operation: String,
    package_digest: String,
    result: Result<Option<String>, String>,
) -> ReceiptReport {
    let mut report = ReceiptReport {
        id,
        app_id,
        operation,
        package_digest,
        outcome: ReceiptOutcome::Indeterminate,
        result: None,
        error: None,
    };
    match result {
        Err(error) => report.error = Some(nonempty_error(&error)),
        Ok(output) => {
            let output = output.unwrap_or_default();
            if output.len() > MAX_RESULT_BYTES {
                report.error = Some(
                    "App output exceeded the receipt size bound; effects remain unverified".into(),
                );
                return report;
            }
            let parsed = serde_json::from_str::<serde_json::Value>(&output).ok();
            let kind = if output.is_empty() {
                ResultKind::Empty
            } else if parsed.is_some() {
                ResultKind::Json
            } else {
                ResultKind::Text
            };
            let redacted = display_text(&Redactor::default_set().redact(&output));
            let (preview, truncated) = clip(&redacted, MAX_PREVIEW_BYTES);
            report.result = Some(ResultSummary {
                kind,
                sha256: format!("sha256:{}", crate::crypto::sha256_hex(output.as_bytes())),
                bytes: output.len() as u64,
                preview,
                preview_truncated: truncated,
            });
            report.outcome = ReceiptOutcome::Returned;
            if let Some(error) = parsed
                .as_ref()
                .and_then(|value| value.get("error"))
                .filter(|error| !error.is_null())
            {
                report.outcome = ReceiptOutcome::ReportedError;
                report.error = Some(nonempty_error(
                    &error
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| error.to_string()),
                ));
            }
        }
    }
    report
}

pub fn sanitize(report: &mut ReceiptReport) {
    let redactor = Redactor::default_set();
    if let Some(result) = &mut report.result {
        let redacted = display_text(&redactor.redact(&result.preview));
        let (preview, truncated) = clip(&redacted, MAX_PREVIEW_BYTES);
        result.preview = preview;
        result.preview_truncated |= truncated;
    }
    if let Some(error) = &mut report.error {
        *error = nonempty_error(error);
    }
}

pub fn diagnostic(value: &str) -> String {
    clip(
        &display_text(&Redactor::default_set().redact(value)),
        MAX_PREVIEW_BYTES,
    )
    .0
}

fn display_text(value: &str) -> String {
    let mut text = String::with_capacity(value.len());
    for character in value.chars() {
        if character.is_control() && !matches!(character, '\n' | '\r' | '\t') {
            text.extend(character.escape_default());
        } else {
            text.push(character);
        }
    }
    text
}

fn nonempty_error(value: &str) -> String {
    if value.trim().is_empty() {
        "App returned an error without diagnostic text".into()
    } else {
        diagnostic(value)
    }
}

fn clip(value: &str, limit: usize) -> (String, bool) {
    if value.len() <= limit {
        return (value.to_string(), false);
    }
    let mut end = limit;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_string(), true)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/operations/receipts.rs"
    ));
}
