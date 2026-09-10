//! Read-only waits for OS launch review and capability decisions.

use std::collections::BTreeSet;
use std::io::Write;
use std::time::{Duration, Instant};

use clawd_client::system_review::{ReviewKind, ReviewStatus, SystemReview};
use serde_json::{json, Value};

use super::{ClawdCallError, ClawdCommand};

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum PendingConsent {
    SystemReview(String),
    Capabilities(Vec<String>),
}

pub(super) fn register_with_wait(app_id: &str, params: Value) -> Result<Value, ClawdCallError> {
    // Private Hosts cannot reach around their route-filtered broker to inspect
    // an owner's reviews. Their controller receives the original refusal.
    let can_poll_review =
        std::env::var_os(crate::extension_host::protocol::BROKER_SOCKET_ENV).is_none();
    register_with(
        app_id,
        params,
        can_poll_review,
        Instant::now() + super::APPROVAL_WAIT,
        super::clawd_request,
        |pending, deadline| match pending {
            PendingConsent::SystemReview(id) => wait_for_review(app_id, id, deadline),
            PendingConsent::Capabilities(ids) => super::wait_for_approvals_until(ids, deadline),
        },
    )
}

fn register_with(
    app_id: &str,
    params: Value,
    can_poll_review: bool,
    deadline: Instant,
    mut request: impl FnMut(ClawdCommand, Value) -> Result<Value, ClawdCallError>,
    mut wait: impl FnMut(&PendingConsent, Instant) -> Result<(), String>,
) -> Result<Value, ClawdCallError> {
    let mut completed = BTreeSet::new();
    loop {
        let denial = match request(ClawdCommand::AppSessionRegister, params.clone()) {
            Ok(result) => return Ok(result),
            Err(error) => error,
        };
        let pending = match pending_consent(&denial, app_id) {
            Ok(Some(pending)) => pending,
            Ok(None) => return Err(denial),
            Err(error) => return Err(denial.with_context(&error)),
        };
        if matches!(pending, PendingConsent::SystemReview(_)) && !can_poll_review {
            return Err(denial);
        }
        if !completed.insert(pending.clone()) {
            return Err(denial.with_context(
                "the broker still refuses registration after the decision; refresh the OS review",
            ));
        }
        super::check_approval_wait(deadline)
            .and_then(|()| wait(&pending, deadline))
            .and_then(|()| super::check_approval_wait(deadline))
            .map_err(|error| denial.with_context(&error))?;
        // A displayed decision only permits another request. The broker must
        // consume its own confirmation and settle capabilities again.
    }
}

fn pending_consent(error: &ClawdCallError, app_id: &str) -> Result<Option<PendingConsent>, String> {
    if error.code.as_deref() != Some("not_authorized") {
        return Ok(None);
    }
    let Some(data) = error.data.as_ref() else {
        return Ok(None);
    };
    match data.get("status").and_then(Value::as_str) {
        Some("system_review_required") => {
            let id = data
                .get("system_review_id")
                .and_then(Value::as_str)
                .ok_or("OS review refusal omitted its request id")?;
            let valid_id = id.strip_prefix("rv-").is_some_and(|suffix| {
                suffix.len() == 32
                    && suffix
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            });
            if !valid_id || data.get("app_id").and_then(Value::as_str) != Some(app_id) {
                return Err("OS review refusal does not match this App activation".into());
            }
            Ok(Some(PendingConsent::SystemReview(id.to_string())))
        }
        Some("approval_required") => {
            let ids = super::approval_requests(error);
            let unique: BTreeSet<_> = ids.iter().collect();
            if ids.is_empty() || unique.len() != ids.len() {
                return Err("App capability refusal has invalid approval request ids".into());
            }
            Ok(Some(PendingConsent::Capabilities(ids)))
        }
        _ => Ok(None),
    }
}

fn review_complete(review: &SystemReview, app_id: &str, id: &str) -> Result<bool, String> {
    review.validate().map_err(|error| error.to_string())?;
    if review.id != id
        || review.revision == 0
        || review.kind != ReviewKind::Activation
        || review.subject.app_id.as_deref() != Some(app_id)
    {
        return Err("OS review status does not match the requested App activation".into());
    }
    match review.status {
        ReviewStatus::Pending => Ok(false),
        ReviewStatus::Confirmed | ReviewStatus::Consumed => Ok(true),
        status => Err(format!(
            "App activation review {id} is {status:?}; launch remains refused"
        )),
    }
}

fn wait_for_review(app_id: &str, id: &str, deadline: Instant) -> Result<(), String> {
    poll_review(
        app_id,
        id,
        deadline,
        super::APPROVAL_POLL,
        || {
            // The shared broker client checks UID 0 on this same connection
            // before writing any system.review request bytes.
            let value = super::clawd_request(ClawdCommand::SystemReviewShow, json!({"id": id}))
                .map_err(String::from)?;
            serde_json::from_value(value)
                .map_err(|error| format!("invalid OS App review response: {error}"))
        },
        |review| {
            let text = clawd_client::system_review::format_terminal(review)
                .map_err(|error| error.to_string())?;
            let mut stderr = std::io::stderr().lock();
            writeln!(stderr, "{text}")
                .and_then(|()| stderr.flush())
                .map_err(|error| format!("display OS App review: {error}"))
        },
    )
}

fn poll_review(
    app_id: &str,
    id: &str,
    deadline: Instant,
    interval: Duration,
    mut request: impl FnMut() -> Result<SystemReview, String>,
    mut present: impl FnMut(&SystemReview) -> Result<(), String>,
) -> Result<(), String> {
    let mut displayed: Option<SystemReview> = None;
    loop {
        super::check_approval_wait(deadline)?;
        let review = request()?;
        let complete = review_complete(&review, app_id, id)?;
        if displayed.as_ref().is_some_and(|previous| {
            review.revision < previous.revision
                || (review.revision == previous.revision && review != *previous)
        }) {
            return Err("OS review status changed without a newer presentation revision".into());
        }
        if displayed.as_ref().map(|review| review.revision) != Some(review.revision) {
            present(&review)?;
            displayed = Some(review);
        }
        if complete {
            return Ok(());
        }
        std::thread::sleep(interval.min(deadline.saturating_duration_since(Instant::now())));
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/bridge/consent.rs"
    ));
}
