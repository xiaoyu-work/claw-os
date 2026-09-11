//! Explicit App execution with an immutable, caller-reported Activity receipt.

use serde_json::{json, Value};

use crate::activities::{Activity, ActivityState, ReceiptOutcome};
use crate::clawd::routes::Command;
use crate::operations::{cli::request, receipts};

pub(super) fn run(command: &str, args: &[String]) -> Result<Value, String> {
    if command == "preview" {
        return crate::operations::cli::run(command, args);
    }
    if command != "execute" {
        return Err("expected operation preview or execute".into());
    }
    execute_with(args, request, |app, operation, argv| {
        super::app_commands::require_runnable(app)?;
        super::run_app_command(&app.manifest.id, operation, argv, app, None)
    })
}

fn execute_with(
    args: &[String],
    mut broker: impl FnMut(Command, Value) -> Result<Value, String>,
    execute: impl FnOnce(&crate::apps::App, &str, &[String]) -> Result<Option<String>, String>,
) -> Result<Value, String> {
    let (app_id, operation, activity_id, argv) = parse_execute(args)?;
    let detail = broker(Command::ActivityGet, json!({"id":activity_id,"limit":1}))?;
    let activity: Activity = serde_json::from_value(
        detail
            .get("activity")
            .cloned()
            .ok_or("broker omitted Activity metadata")?,
    )
    .map_err(|error| format!("invalid Activity metadata: {error}"))?;
    let canonical = uuid::Uuid::parse_str(&activity_id)
        .map_err(|error| error.to_string())?
        .to_string();
    if activity.id != canonical {
        return Err("broker returned a different Activity".into());
    }
    if activity.state != ActivityState::Active {
        return Err("Activity must be active before starting an operation".into());
    }
    let app = crate::apps::find_verified(&super::apps_dir(), &app_id)?;
    let preview = crate::operations::preview(&app, &operation, &argv)?;
    let report_id = uuid::Uuid::new_v4().to_string();
    let result = execute(&app, &operation, &argv);
    let mut report =
        receipts::capture(report_id, app_id, operation, preview.package_digest, result);
    receipts::sanitize(&mut report);
    report.validate().map_err(|error| error.to_string())?;
    let record_params = json!({"id":activity.id,"report":report});
    let receipt = match broker(Command::ActivityReceiptRecord, record_params) {
        Ok(receipt) => receipt,
        Err(error) => {
            return Err(json!({
                "error":"App attempt returned, but receipt recording failed; do not re-execute to retry recording",
                "recording_error":receipts::diagnostic(&error),
                "activity_id":activity.id,
                "report":report,
                "retry":"Pipe the report JSON to cos activity record-receipt ACTIVITY_ID --stdin",
            }).to_string());
        }
    };
    if report.outcome != ReceiptOutcome::Returned {
        return Err(json!({
            "error":"App reported an error or its result is indeterminate; effects are not verified",
            "receipt":receipt,
        }).to_string());
    }
    Ok(receipt)
}

fn parse_execute(args: &[String]) -> Result<(String, String, String, Vec<String>), String> {
    if args.len() < 5 || args[2] != "--activity" || args[4] != "--" {
        return Err("usage: cos operation execute APP OPERATION --activity ID -- [APP ARGS]; stdin is not forwarded".into());
    }
    crate::activities::validate_id(&args[3]).map_err(|error| error.to_string())?;
    let argv = args[5..].to_vec();
    crate::operations::validate_arguments(&argv)?;
    Ok((args[0].clone(), args[1].clone(), args[3].clone(), argv))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/router/operation_commands.rs"
    ));
}
