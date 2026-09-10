//! Terminal presentation of OS-owned review requests. No local approval store.

use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;
use std::process::Stdio;

use serde::Deserialize;
use serde_json::{json, Value};

use crate::approvals::system_review::{ReviewKind, ReviewState};
use crate::apps::permission_review::PermissionReview;
use crate::clawd::routes::Command;
use crate::provenance::runtime::PackageRef;

#[derive(Deserialize)]
struct ReviewReply {
    id: String,
    kind: ReviewKind,
    state: ReviewState,
    permission_review: PermissionReview,
    package: PackageRef,
}

fn request(command: Command, params: Value) -> Result<Value, String> {
    let response = crate::clawd::client::request_blocking(
        crate::clawd::config::socket_path(),
        crate::clawd::protocol::Request::build(command, params),
    )
    .map_err(|error| format!("system review service is unavailable: {error}"))?;
    if !response.ok {
        let error = response
            .error
            .ok_or("system review response has no error detail")?;
        return Err(crate::errors::error_with_details(
            &error.code,
            &error.message,
            error.data.unwrap_or(Value::Null),
        )
        .to_string());
    }
    response
        .result
        .ok_or_else(|| "system review response has no result".into())
}

fn parse_review(value: Value) -> Result<ReviewReply, String> {
    let reply: ReviewReply = serde_json::from_value(value)
        .map_err(|error| format!("invalid system review response: {error}"))?;
    let valid_id = reply.id.strip_prefix("rv-").is_some_and(|suffix| {
        suffix.len() == 32
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    });
    if !valid_id
        || reply.permission_review.schema_version != 1
        || reply.permission_review.permissions_granted
        || reply.package.id != reply.permission_review.app_id
    {
        return Err("system review response violates the disclosure contract".into());
    }
    Ok(reply)
}

fn present(reply: &ReviewReply) -> Result<(), String> {
    let mut stderr = std::io::stderr().lock();
    writeln!(
        stderr,
        "Claw OS - System review {}\n{}",
        reply.id,
        reply.permission_review.format_for_review()?,
    )
    .map_err(|error| format!("display system review: {error}"))?;
    stderr
        .flush()
        .map_err(|error| format!("flush system review: {error}"))
}

fn interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

fn confirm(reply: &ReviewReply) -> Result<bool, String> {
    let mut stderr = std::io::stderr().lock();
    let action = match reply.kind {
        ReviewKind::AppInstall => "installation",
        ReviewKind::AppActivation => "activation",
    };
    write!(
        stderr,
        "Confirm this {action} review (not capability grants)? [y/N] "
    )
    .map_err(|error| format!("display system review confirmation: {error}"))?;
    stderr
        .flush()
        .map_err(|error| format!("flush system review confirmation: {error}"))?;
    let mut answer = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut answer)
        .map_err(|error| format!("read system review confirmation: {error}"))?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn decide(id: &str, approve: bool) -> Result<Value, String> {
    if !approve {
        return request(Command::SystemReviewCancel, json!({"id": id}));
    }
    if !interactive() {
        return Err("system review decisions require an interactive OS presentation".into());
    }
    let output = std::process::Command::new("/usr/bin/pkexec")
        .arg("/usr/local/bin/claw-approval-helper")
        .arg("--system-review")
        .arg("--id")
        .arg(id)
        .arg("--decision")
        .arg(if approve { "approve" } else { "deny" })
        .stdin(Stdio::inherit())
        .stderr(Stdio::inherit())
        .stdout(Stdio::piped())
        .output()
        .map_err(|error| format!("launch trusted system review helper: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "system review authentication was cancelled or failed; inspect request {id} before retrying"
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("invalid system review helper response: {error}"))
}

pub(super) fn confirm_install(
    app: &crate::apps::App,
    source: &Path,
    auto_yes: bool,
) -> Result<(PermissionReview, String), String> {
    let expected = PackageRef::of(app.require_verified()?);
    let source = source
        .canonicalize()
        .map_err(|error| format!("resolve system review source: {error}"))?;
    let prepared = request(
        Command::SystemReviewPrepare,
        json!({
            "source": source,
            "expected_package": expected,
        }),
    )?;
    let mut reply = parse_review(prepared)?;
    let local = PermissionReview::from_manifest(&app.manifest)?;
    if reply.package != expected || reply.permission_review.contract_digest != local.contract_digest
    {
        return Err("system review does not match the staged App package".into());
    }
    present(&reply)?;
    if reply.state == ReviewState::Pending {
        if auto_yes || !interactive() {
            return Err(crate::errors::error_with_details(
                "approval_required",
                "App installation is waiting for OS permission review; --yes is not a user decision",
                json!({"review_id":reply.id, "status":"pending", "hint":"Use the OS review interface or cos review show <id>."}),
            ).to_string());
        }
        if !confirm(&reply)? {
            decide(&reply.id, false)?;
            return Err(
                "App installation was declined; the existing version was not replaced".into(),
            );
        }
        let decided = parse_review(decide(&reply.id, true)?)?;
        if decided.id != reply.id
            || decided.package != expected
            || decided.permission_review.contract_digest != local.contract_digest
        {
            return Err("OS decision response does not match the presented review".into());
        }
        reply = decided;
    }
    if reply.state != ReviewState::Approved {
        return Err(format!(
            "system review {} is not approved; refresh its state",
            reply.id
        ));
    }
    let consumed = parse_review(request(
        Command::SystemReviewConsume,
        json!({
            "id": reply.id,
            "source": source,
            "expected_package": expected,
        }),
    )?)?;
    if consumed.id != reply.id
        || consumed.state != ReviewState::Consumed
        || consumed.package != expected
    {
        return Err("system review confirmation was not consumed for the staged package".into());
    }
    Ok((consumed.permission_review, consumed.id))
}

pub(super) fn dispatch(args: &[String]) -> Result<Option<String>, String> {
    if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "--help" | "-h"))
    {
        return crate::cli_help::show_help_for("review");
    }
    if args.iter().any(|arg| arg == "--schema") {
        return match args.first().filter(|arg| !arg.starts_with("--")) {
            Some(command) => crate::cli_help::show_command_schema("review", command),
            None => crate::cli_help::show_builtin_schema("review"),
        };
    }
    let command = args.first().map(String::as_str).unwrap_or("pending");
    if command == "pending" && args.len() <= 1 {
        return Ok(Some(
            request(Command::SystemReviewPending, json!({"limit":100}))?.to_string(),
        ));
    }
    if args.len() != 2 || !matches!(command, "show" | "approve" | "deny") {
        return Err("usage: cos review pending | show <id> | approve <id> | deny <id>".into());
    }
    let value = request(Command::SystemReviewShow, json!({"id":args[1]}))?;
    let reply = parse_review(value.clone())?;
    present(&reply)?;
    if command == "show" {
        return Ok(Some(value.to_string()));
    }
    if command == "approve" && !interactive() {
        return Err("system review decisions require an interactive OS presentation".into());
    }
    if command == "approve" && !confirm(&reply)? {
        return Err("system review confirmation was cancelled".into());
    }
    let decision = decide(&reply.id, command == "approve")?;
    let decided = parse_review(decision.clone())?;
    if decided.id != reply.id || decided.package != reply.package {
        return Err("OS decision response does not match the presented review".into());
    }
    Ok(Some(decision.to_string()))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/router/system_review.rs"
    ));
}
