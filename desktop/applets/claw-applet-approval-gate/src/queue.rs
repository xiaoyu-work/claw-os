// SPDX-License-Identifier: GPL-3.0-only

//! Transport only. Projection, supported choices and decisions belong to the OS.

use std::{process::Stdio, time::Duration};

use clawd_client::{
    Client, Command, MAX_RESPONSE_BYTES,
    system_review::{PendingReviews, ReviewAction, ReviewDecision, SystemReview},
};
use serde_json::json;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

const HELPER_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_ERROR_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone)]
pub struct LoadError(pub String);

fn parse_pending(value: serde_json::Value) -> Result<PendingReviews, LoadError> {
    let pending: PendingReviews = serde_json::from_value(value)
        .map_err(|error| LoadError(format!("parse OS review queue: {error}")))?;
    pending
        .validate()
        .map_err(|error| LoadError(error.to_string()))?;
    Ok(pending)
}

fn parse_review(value: serde_json::Value) -> Result<SystemReview, LoadError> {
    let review: SystemReview = serde_json::from_value(value)
        .map_err(|error| LoadError(format!("parse OS review: {error}")))?;
    review
        .validate()
        .map_err(|error| LoadError(error.to_string()))?;
    Ok(review)
}

pub async fn load_pending() -> Result<PendingReviews, LoadError> {
    let value = Client::from_env()
        .map_err(|error| LoadError(error.to_string()))?
        .call(Command::SystemReviewPending, json!({"limit": 100}))
        .await
        .map_err(|error| LoadError(error.to_string()))?;
    parse_pending(value)
}

pub async fn show(id: &str) -> Result<SystemReview, LoadError> {
    let value = Client::from_env()
        .map_err(|error| LoadError(error.to_string()))?
        .call(Command::SystemReviewShow, json!({"id": id}))
        .await
        .map_err(|error| LoadError(error.to_string()))?;
    let review = parse_review(value)?;
    if review.id != id {
        return Err(LoadError("OS returned a different review id".into()));
    }
    Ok(review)
}

fn helper_command() -> tokio::process::Command {
    let mut command = tokio::process::Command::new("/usr/bin/pkexec");
    command
        .arg("/usr/local/bin/claw-approval-helper")
        .arg("--system-review-json");
    command
}

fn owner_cancel_request(decision: &ReviewDecision) -> Option<(Command, serde_json::Value)> {
    (decision.action == ReviewAction::Cancel)
        .then(|| (Command::SystemReviewCancel, json!({"review": decision})))
}

pub async fn decide(
    review: &SystemReview,
    decision: &ReviewDecision,
) -> Result<SystemReview, LoadError> {
    let input = decision
        .encode_for(review)
        .map_err(|error| LoadError(error.to_string()))?;
    let result = if let Some((command, params)) = owner_cancel_request(decision) {
        let value = Client::from_env()
            .map_err(|error| LoadError(error.to_string()))?
            .call(command, params)
            .await
            .map_err(|error| LoadError(format!("cancel OS review: {error}")))?;
        parse_review(value)?
    } else {
        run_helper(helper_command(), &input, HELPER_TIMEOUT).await?
    };
    if result.id != review.id || result.revision < review.revision {
        return Err(LoadError(
            "OS decision returned a different or older review; refresh required".into(),
        ));
    }
    Ok(result)
}

async fn read_bounded(reader: impl AsyncRead + Unpin, limit: usize) -> Result<Vec<u8>, LoadError> {
    let mut bytes = Vec::new();
    reader
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| LoadError(format!("read review helper: {error}")))?;
    if bytes.len() > limit {
        return Err(LoadError(
            "review helper output exceeds its size limit".into(),
        ));
    }
    Ok(bytes)
}

async fn run_helper(
    mut command: tokio::process::Command,
    input: &[u8],
    timeout: Duration,
) -> Result<SystemReview, LoadError> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| LoadError(format!("launch trusted review helper: {error}")))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| LoadError("helper stdin was not piped".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| LoadError("helper stdout was not piped".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| LoadError("helper stderr was not piped".into()))?;
    let (_, output, errors, status) = tokio::time::timeout(timeout, async {
        tokio::try_join!(
            async {
                stdin
                    .write_all(input)
                    .await
                    .map_err(|error| LoadError(format!("write review decision: {error}")))?;
                stdin
                    .shutdown()
                    .await
                    .map_err(|error| LoadError(format!("close review decision input: {error}")))?;
                drop(stdin);
                Ok::<(), LoadError>(())
            },
            read_bounded(stdout, MAX_RESPONSE_BYTES),
            read_bounded(stderr, MAX_ERROR_BYTES),
            async {
                child
                    .wait()
                    .await
                    .map_err(|error| LoadError(format!("wait for review helper: {error}")))
            },
        )
    })
    .await
    .map_err(|_| {
        LoadError("review helper timed out; the OS state must be refreshed before retrying".into())
    })??;
    if !status.success() {
        let errors = String::from_utf8(errors)
            .map_err(|error| LoadError(format!("helper error is not UTF-8: {error}")))?;
        return Err(LoadError(format!(
            "OS decision cancelled or failed ({status}): {}",
            errors.trim()
        )));
    }
    let value = serde_json::from_slice(&output)
        .map_err(|error| LoadError(format!("invalid review helper JSON: {error}")))?;
    parse_review(value)
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/queue.rs"));
}
