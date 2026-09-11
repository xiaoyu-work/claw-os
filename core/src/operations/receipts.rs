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
            let (display, upstream_truncated) = match parsed.as_ref() {
                Some(value)
                    if value.get("kind").and_then(serde_json::Value::as_str)
                        == Some("file_change_plan")
                        && value.get("schema").is_some() =>
                {
                    match file_plan_preview(value, &report.app_id) {
                        Ok(preview) => preview,
                        Err(error) => {
                            report.error = Some(error);
                            return report;
                        }
                    }
                }
                _ => (output.clone(), false),
            };
            let redacted = display_text(&Redactor::default_set().redact(&display));
            let (preview, truncated) = clip(&redacted, MAX_PREVIEW_BYTES);
            report.result = Some(ResultSummary {
                kind,
                sha256: format!("sha256:{}", crate::crypto::sha256_hex(output.as_bytes())),
                bytes: output.len() as u64,
                preview,
                preview_truncated: truncated || upstream_truncated,
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

pub(crate) fn capture_session(
    app_id: &str,
    tool: &str,
    package_digest: &str,
    result: Result<(String, bool), String>,
) -> ReceiptReport {
    let (result, reported_error) = match result {
        Ok((output, is_error)) => {
            let error = is_error.then(|| nonempty_error(&output));
            (Ok(Some(output)), error)
        }
        Err(error) => (Err(error), None),
    };
    let mut report = capture(
        uuid::Uuid::new_v4().to_string(),
        app_id.to_string(),
        ReceiptReport::session_operation(tool),
        package_digest.to_string(),
        result,
    );
    if report.result.is_some() {
        if let Some(error) = reported_error {
            report.outcome = ReceiptOutcome::ReportedError;
            report.error = Some(error);
        }
    }
    report
}

fn file_plan_preview(value: &serde_json::Value, app_id: &str) -> Result<(String, bool), String> {
    claw_os_sdk::generated::validate_file_change_plan(value).map_err(|_| {
        "App returned an invalid file-change-plan report; no plan was trusted".to_string()
    })?;
    let text = |field: &str| {
        value
            .get(field)
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "App file-change-plan report omitted a text field".to_string())
    };
    let path = text("path")?;
    let diff = text("diff")?;
    let plan_id = text("plan_id")?;
    let review = text("review")?;
    if path.len() > 1024
        || !std::path::Path::new(path).is_absolute()
        || diff.len() > 65536
        || path.chars().any(char::is_control)
    {
        return Err("App file-change-plan report exceeded its text bounds".into());
    }
    let id = uuid::Uuid::parse_str(plan_id)
        .map_err(|_| "App file-change-plan report has an invalid plan ID".to_string())?;
    if id.to_string() != plan_id || !digest(review) || !digest(text("after_sha256")?) {
        return Err("App file-change-plan report has an invalid identity or digest".into());
    }
    let reference = crate::objects::parse_reference(text("reference")?)
        .map_err(|_| "App file-change-plan report has an invalid object reference".to_string())?;
    if reference.app_id != app_id
        || reference.object_id != path
        || reference.revision.as_deref() != Some(plan_id)
    {
        return Err("App file-change-plan reference does not match the reported proposal".into());
    }
    let exists = value["before_exists"]
        .as_bool()
        .ok_or_else(|| "App file-change-plan report omitted baseline state".to_string())?;
    let before_hash = value["before_sha256"].as_str();
    if (exists && !before_hash.is_some_and(digest))
        || (!exists && (!value["before_sha256"].is_null() || value["before_bytes"] != 0))
    {
        return Err("App file-change-plan baseline is inconsistent".into());
    }
    let would_change = !exists
        || before_hash != Some(text("after_sha256")?)
        || value["before_bytes"] != value["after_bytes"];
    if value["would_change"].as_bool() != Some(would_change) {
        return Err("App file-change-plan change summary is inconsistent".into());
    }
    let created = chrono::DateTime::parse_from_rfc3339(text("created_at")?)
        .map_err(|_| "App file-change-plan creation time is invalid".to_string())?;
    let expires = chrono::DateTime::parse_from_rfc3339(text("expires_at")?)
        .map_err(|_| "App file-change-plan expiry is invalid".to_string())?;
    if expires <= created {
        return Err("App file-change-plan lifetime is invalid".into());
    }
    let warnings = value["warnings"]
        .as_array()
        .ok_or_else(|| "App file-change-plan warnings are invalid".to_string())?;
    if warnings.len() > 16
        || warnings
            .iter()
            .any(|warning| warning.as_str().is_none_or(|text| text.len() > 1024))
    {
        return Err("App file-change-plan warnings exceed their bounds".into());
    }
    let mut preview = format!(
        "App-reported file change plan; not authorization or OS-confirmed effects.\nPlan: {}\nTarget: {path}\nState: {}\nReview: {}\n\n{diff}",
        plan_id, text("state")?, review,
    );
    let truncated = value
        .get("diff_truncated")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| "App file-change-plan report omitted its truncation state".to_string())?;
    if truncated {
        preview.push_str(
            "\n[App plan diff was truncated; inspect the full proposal before applying.]",
        );
    }
    Ok((preview, truncated))
}

fn digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
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
