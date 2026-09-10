//! Terminal presentation of OS-owned review requests. No local approval store.

use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;
use std::process::Stdio;

use clawd_client::system_review::{
    display_text, format_terminal, PendingReviews, PermissionSelection, ReviewAction,
    ReviewDecision, ReviewKind, ReviewStatus, SystemReview,
};
use serde_json::{json, Value};

use crate::apps::permission_review::PermissionReview;
use crate::clawd::routes::Command;
use crate::provenance::runtime::PackageRef;

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

fn parse_review(value: Value) -> Result<SystemReview, String> {
    let review: SystemReview = serde_json::from_value(value)
        .map_err(|error| format!("invalid system review response: {error}"))?;
    review.validate().map_err(|error| error.to_string())?;
    let valid_id = [("rv-", 32), ("ap-", 12)].iter().any(|(prefix, length)| {
        review.id.strip_prefix(prefix).is_some_and(|suffix| {
            suffix.len() == *length
                && suffix
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
    });
    if !valid_id || review.revision == 0 {
        return Err("system review response has an invalid request identity or revision".into());
    }
    Ok(review)
}

fn present(review: &SystemReview) -> Result<(), String> {
    let text = format_terminal(review).map_err(|error| error.to_string())?;
    let mut stderr = std::io::stderr().lock();
    writeln!(stderr, "{text}").map_err(|error| format!("display system review: {error}"))?;
    stderr
        .flush()
        .map_err(|error| format!("flush system review: {error}"))
}

fn interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

fn confirm(action: ReviewAction) -> Result<bool, String> {
    let mut stderr = std::io::stderr().lock();
    write!(stderr, "{}? [y/N] ", action.text().english())
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

fn permission_choices(
    review: &SystemReview,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<Vec<PermissionSelection>, String> {
    let mut choices = Vec::new();
    for permission in &review.permissions {
        if permission.supported_choices.is_empty() {
            continue;
        }
        writeln!(
            output,
            "Choose for {} ({}) [empty input cancels]:",
            display_text(&permission.label),
            display_text(&permission.scope.description)
        )
        .map_err(|error| format!("display permission choices: {error}"))?;
        for (index, choice) in permission.supported_choices.iter().enumerate() {
            writeln!(output, "  {}. {}", index + 1, choice.text().english())
                .map_err(|error| format!("display permission choice: {error}"))?;
        }
        output
            .flush()
            .map_err(|error| format!("flush permission choices: {error}"))?;
        let mut answer = String::new();
        input
            .read_line(&mut answer)
            .map_err(|error| format!("read permission choice: {error}"))?;
        let choice = answer
            .trim()
            .parse::<usize>()
            .ok()
            .and_then(|index| index.checked_sub(1))
            .and_then(|index| permission.supported_choices.get(index))
            .copied()
            .ok_or("no valid permission choice selected; the review remains pending")?;
        choices.push(PermissionSelection {
            permission_id: permission.id.clone(),
            choice,
        });
    }
    Ok(choices)
}

fn approval_action(review: &SystemReview) -> Result<ReviewAction, String> {
    review
        .actions
        .iter()
        .copied()
        .find(|action| *action != ReviewAction::Cancel)
        .ok_or_else(|| "the review has no available confirmation or permission action".into())
}

fn helper_decision(
    decision: &ReviewDecision,
    review: &SystemReview,
) -> Result<SystemReview, String> {
    let bytes = decision
        .encode_for(review)
        .map_err(|error| error.to_string())?;
    let mut child = std::process::Command::new("/usr/bin/pkexec")
        .arg("/usr/local/bin/claw-approval-helper")
        .arg("--system-review-json")
        .stdin(Stdio::piped())
        .stderr(Stdio::inherit())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|error| format!("launch trusted system review helper: {error}"))?;
    // Polkit authenticates on the controlling terminal; stdin carries only the displayed decision.
    let sent = child
        .stdin
        .take()
        .ok_or("system review helper stdin is unavailable".to_string())
        .and_then(|mut stdin| {
            stdin
                .write_all(&bytes)
                .map_err(|error| format!("send displayed review decision: {error}"))
        });
    let output = child
        .wait_with_output()
        .map_err(|error| format!("wait for system review helper: {error}"))?;
    sent?;
    if !output.status.success() {
        return Err(format!(
            "system review authentication was cancelled or failed; inspect request {} before retrying",
            review.id,
        ));
    }
    parse_review(
        serde_json::from_slice(&output.stdout)
            .map_err(|error| format!("invalid system review helper response: {error}"))?,
    )
}

fn decide(review: &SystemReview, decision: &ReviewDecision) -> Result<SystemReview, String> {
    decision
        .validate_for(review)
        .map_err(|error| error.to_string())?;
    let outcome = if decision.action == ReviewAction::Cancel {
        request(Command::SystemReviewCancel, json!({"review": decision})).and_then(parse_review)
    } else {
        if !interactive() {
            return Err("system review decisions require an interactive OS presentation".into());
        }
        helper_decision(decision, review)
    };
    let refreshed =
        request(Command::SystemReviewShow, json!({"id": review.id})).and_then(parse_review);
    match (outcome, refreshed) {
        (Ok(response), Ok(latest)) => {
            if response.id != review.id || latest.id != review.id {
                return Err("OS decision response does not match the presented review".into());
            }
            Ok(latest)
        }
        (Err(error), Ok(latest)) => {
            present(&latest)?;
            Err(error)
        }
        (result, Err(refresh)) => Err(match result {
            Ok(_) => {
                format!("decision submitted but its current OS state is unavailable: {refresh}")
            }
            Err(error) => format!("{error}; refresh also failed: {refresh}"),
        }),
    }
}

fn matches_install(review: &SystemReview, expected: &PackageRef, local: &PermissionReview) -> bool {
    matches!(review.kind, ReviewKind::Install | ReviewKind::Update)
        && review.subject.app_id.as_deref() == Some(expected.id.as_str())
        && review.subject.version.as_deref() == Some(local.app_version.as_str())
        && review.subject.publisher == expected.publisher_key_id
        && review.contract_digest.as_deref() == Some(local.contract_digest.as_str())
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
    let mut review = parse_review(request(
        Command::SystemReviewPrepare,
        json!({"source": source, "expected_package": expected}),
    )?)?;
    let local = PermissionReview::from_manifest(&app.manifest)?;
    if !matches_install(&review, &expected, &local) {
        return Err("system review does not match the staged App package".into());
    }
    present(&review)?;
    if review.status == ReviewStatus::Pending {
        if auto_yes || !interactive() {
            return Err(crate::errors::error_with_details(
                "approval_required",
                "App installation is waiting for OS permission review; --yes is not a user decision",
                json!({"review_id":review.id, "status":"pending", "hint":"Use the OS review interface or cos review show <id>."}),
            ).to_string());
        }
        let action = approval_action(&review)?;
        if !confirm(action)? {
            decide(
                &review,
                &ReviewDecision {
                    id: review.id.clone(),
                    revision: review.revision,
                    action: ReviewAction::Cancel,
                    choices: Vec::new(),
                },
            )?;
            return Err(
                "App installation was declined; the existing version was not replaced".into(),
            );
        }
        review = decide(
            &review,
            &ReviewDecision {
                id: review.id.clone(),
                revision: review.revision,
                action,
                choices: Vec::new(),
            },
        )?;
    }
    if review.status != ReviewStatus::Confirmed || !matches_install(&review, &expected, &local) {
        return Err(format!(
            "system review {} is not confirmed for this package; refresh its state",
            review.id
        ));
    }
    let consumed = parse_review(request(
        Command::SystemReviewConsume,
        json!({"id": review.id, "source": source, "expected_package": expected}),
    )?)?;
    if consumed.id != review.id
        || consumed.status != ReviewStatus::Consumed
        || !matches_install(&consumed, &expected, &local)
    {
        return Err("system review confirmation was not consumed for the staged package".into());
    }
    Ok((local, consumed.id))
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
        let value = request(Command::SystemReviewPending, json!({"limit":100}))?;
        let pending: PendingReviews = serde_json::from_value(value.clone())
            .map_err(|error| format!("invalid pending reviews: {error}"))?;
        pending.validate().map_err(|error| error.to_string())?;
        for review in pending.reviews {
            present(&review)?;
        }
        return Ok(Some(value.to_string()));
    }
    if args.len() != 2 || !matches!(command, "show" | "approve" | "deny") {
        return Err("usage: cos review pending | show <id> | approve <id> | deny <id>".into());
    }
    let value = request(Command::SystemReviewShow, json!({"id":args[1]}))?;
    let review = parse_review(value.clone())?;
    present(&review)?;
    if command == "show" {
        return Ok(Some(value.to_string()));
    }
    let action = if command == "deny" {
        ReviewAction::Cancel
    } else {
        if !interactive() {
            return Err("system review decisions require an interactive OS presentation".into());
        }
        approval_action(&review)?
    };
    let choices = if action == ReviewAction::ApplyChoices {
        permission_choices(
            &review,
            &mut std::io::stdin().lock(),
            &mut std::io::stderr().lock(),
        )?
    } else {
        Vec::new()
    };
    if action != ReviewAction::Cancel && !confirm(action)? {
        return Err("system review confirmation was cancelled".into());
    }
    let decided = decide(
        &review,
        &ReviewDecision {
            id: review.id.clone(),
            revision: review.revision,
            action,
            choices,
        },
    )?;
    Ok(Some(serde_json::to_string(&decided).map_err(|error| {
        format!("encode decided review: {error}")
    })?))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/router/system_review.rs"
    ));
}
