//! Separate, one-use owner confirmation for an unbounded all-App stop.
//! Only the existing privileged OS decision channel may decide these records.

use std::path::{Path, PathBuf};

use clawd_client::system_review::{
    ReviewAction, ReviewDecision, ReviewDetail, ReviewKind, ReviewStatus, ReviewSubject,
    SystemReview,
};
use serde::{Deserialize, Serialize};

use super::impact::Impact;

const SCHEMA: u32 = 1;
const MAX_BYTES: u64 = 1024 * 1024;
const LIFETIME: u64 = 30 * 60;

#[derive(Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum State {
    Pending,
    Approved,
    Denied,
    Consumed,
    Stale,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Review {
    schema_version: u32,
    id: String,
    request_id: String,
    owner_uid: u32,
    plan_digest: String,
    impact_digest: String,
    operation: super::Operation,
    plan_summary: serde_json::Value,
    all_apps: Vec<String>,
    known_affected_apps: Vec<String>,
    uncertainties: Vec<String>,
    requested_at: u64,
    expires_at: u64,
    state: State,
    decided_by: Option<u32>,
    decided_at: Option<u64>,
}

fn now() -> Result<u64, String> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|time| time.as_secs())
        .map_err(|error| error.to_string())
}

fn directory() -> PathBuf {
    Path::new(super::ROOT).join("global-stop-reviews")
}

fn path(id: &str) -> Result<PathBuf, String> {
    if !id.strip_prefix("pg-").is_some_and(super::valid_id) {
        return Err("Invalid protected package impact review identity".into());
    }
    Ok(directory().join(format!("{id}.json")))
}

fn read(id: &str, owner: u32) -> Result<Review, String> {
    let review: Review = crate::approvals::presentation::read_owned_json(&path(id)?, MAX_BYTES)?
        .ok_or("Protected package impact review is unavailable")?;
    if review.schema_version != SCHEMA
        || review.id != id
        || review.owner_uid != owner
        || !super::valid_id(&review.request_id)
        || !crate::apps::store::valid_sha256(&review.plan_digest)
        || !crate::apps::store::valid_sha256(&review.impact_digest)
        || review.all_apps.is_empty()
        || review.all_apps.len() > 4096
        || review.uncertainties.is_empty()
    {
        return Err("Protected package impact review has an invalid owner/plan binding".into());
    }
    if matches!(
        review.state,
        State::Approved | State::Denied | State::Consumed
    ) && (review.decided_by != Some(owner) || review.decided_at.is_none())
    {
        return Err("Package impact review has no matching protected owner decision".into());
    }
    Ok(review)
}

fn save(review: &Review) -> Result<(), String> {
    let bytes = serde_json::to_vec(review).map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("Package impact review exceeds its bound".into());
    }
    crate::agent::util::atomic_write_with_fsync(&path(&review.id)?, &bytes)
        .map_err(|error| error.to_string())
}

fn bound(review: &Review) -> Result<bool, String> {
    let record = super::read(&review.request_id)?;
    Ok(record.owner_uid == review.owner_uid
        && record.operation == review.operation
        && (!matches!(review.state, State::Pending | State::Approved)
            || record.phase != super::Phase::NotApplied)
        && record.global_review_id.as_deref() == Some(review.id.as_str())
        && record
            .plan
            .as_ref()
            .is_some_and(|plan| plan.delivery.digest == review.plan_digest)
        && record
            .plan
            .as_ref()
            .is_some_and(|plan| plan.summary() == review.plan_summary)
        && record.impact.as_ref().is_some_and(|impact| {
            impact.digest == review.impact_digest && impact.needs_confirmation()
        }))
}

fn status(review: &Review) -> Result<ReviewStatus, String> {
    if !bound(review)? {
        return Ok(ReviewStatus::Stale);
    }
    if matches!(review.state, State::Pending | State::Approved) && now()? >= review.expires_at {
        return Ok(ReviewStatus::Expired);
    }
    Ok(match review.state {
        State::Pending => ReviewStatus::Pending,
        State::Approved => ReviewStatus::Confirmed,
        State::Denied => ReviewStatus::Cancelled,
        State::Consumed => ReviewStatus::Consumed,
        State::Stale => ReviewStatus::Stale,
    })
}

fn view(review: &Review) -> Result<SystemReview, String> {
    let status = status(review)?;
    let mut disclosures = vec![
        ReviewDetail { label: "Requested system package operation".into(), value: serde_json::to_string(&review.operation).map_err(|error| error.to_string())? },
        ReviewDetail { label: "Concrete plan: versions, architectures, hashes and removals".into(), value: serde_json::to_string(&review.plan_summary).map_err(|error| error.to_string())? },
        ReviewDetail { label: "Scope".into(), value: "Stop every listed installed or retained App, across all owners, including the caller when present. Unsaved GUI work may be lost.".into() },
        ReviewDetail { label: "All App identities".into(), value: review.all_apps.join(", ") },
        ReviewDetail { label: "Known dependency/payload users".into(), value: if review.known_affected_apps.is_empty() { "None could be established precisely".into() } else { review.known_affected_apps.join(", ") } },
        ReviewDetail { label: "APT plan digest (comparison only)".into(), value: review.plan_digest.clone() },
        ReviewDetail { label: "Impact digest (comparison only)".into(), value: review.impact_digest.clone() },
    ];
    disclosures.extend(review.uncertainties.iter().map(|reason| ReviewDetail {
        label: "Why the stop cannot be limited safely".into(),
        value: reason.clone(),
    }));
    let mut result = SystemReview {
        schema_version: clawd_client::system_review::SCHEMA_VERSION,
        id: review.id.clone(), revision: 0,
        kind: ReviewKind::PackageImpact,
        status,
        status_message: Some("This permits only the all-App stop for this exact Root transaction. It is not package success, Critical package authority, App delivery confirmation, a runtime grant or AI consent.".into()),
        subject: ReviewSubject {
            app_id: None, name: "All-App stop for a system package transaction".into(),
            version: None, publisher: None,
        },
        initiator: format!("Authenticated package transaction {}", review.request_id),
        context: vec![
            ReviewDetail { label: "Requested at (Unix seconds)".into(), value: review.requested_at.to_string() },
            ReviewDetail { label: "Expires at (Unix seconds)".into(), value: review.expires_at.to_string() },
        ],
        permissions: Vec::new(), disclosures, changes: None,
        contract_digest: Some(review.impact_digest.clone()),
        actions: if status == ReviewStatus::Pending {
            vec![ReviewAction::Cancel, ReviewAction::ConfirmGlobalAppStop]
        } else { Vec::new() },
    };
    result.validate().map_err(|error| error.to_string())?;
    let fingerprint =
        crate::crypto::sha256_hex(&serde_json::to_vec(&result).map_err(|error| error.to_string())?);
    result.revision =
        crate::approvals::presentation::revision(review.owner_uid, &review.id, &fingerprint)?;
    Ok(result)
}

pub(super) fn submit(owner: u32, request: &str, impact: &Impact) -> Result<String, String> {
    if !impact.needs_confirmation() {
        return Err("A bounded stop does not need global consent".into());
    }
    super::initialize()?;
    let record = super::read(request)?;
    if record.owner_uid != owner
        || record
            .impact
            .as_ref()
            .is_none_or(|current| current.digest != impact.digest)
    {
        return Err("The Root package request does not match the impact review".into());
    }
    let plan = record
        .plan
        .as_ref()
        .ok_or("Missing Root package plan for impact confirmation")?;
    let mut pending = 0;
    let mut entries = 0;
    for entry in std::fs::read_dir(directory()).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        if entry.path().extension() != Some(std::ffi::OsStr::new("json")) {
            continue;
        }
        entries += 1;
        if entries > 4096 {
            return Err("Package impact review history exceeds its bound".into());
        }
        let Some(review) =
            crate::approvals::presentation::read_owned_json::<Review>(&entry.path(), MAX_BYTES)?
        else {
            continue;
        };
        if review.owner_uid == owner
            && matches!(review.state, State::Pending | State::Approved)
            && now()? < review.expires_at
        {
            pending += 1;
        }
        if pending >= 32 {
            return Err("The owner has too many pending package impact reviews".into());
        }
    }
    let requested_at = now()?;
    let id = format!("pg-{}", uuid::Uuid::new_v4().simple());
    save(&Review {
        schema_version: SCHEMA,
        id: id.clone(),
        request_id: request.into(),
        owner_uid: owner,
        plan_digest: impact.plan_digest.clone(),
        impact_digest: impact.digest.clone(),
        operation: record.operation.clone(),
        plan_summary: plan.summary(),
        all_apps: impact.all_apps.clone(),
        known_affected_apps: impact.affected_apps.clone(),
        uncertainties: impact.uncertainties.clone(),
        requested_at,
        expires_at: requested_at + LIFETIME,
        state: State::Pending,
        decided_by: None,
        decided_at: None,
    })?;
    Ok(id)
}

pub(super) async fn wait(owner: u32, id: &str) -> Result<(), String> {
    loop {
        match status(&read(id, owner)?)? {
            ReviewStatus::Confirmed => return Ok(()),
            ReviewStatus::Pending => tokio::time::sleep(std::time::Duration::from_secs(1)).await,
            _ => {
                return Err(
                    "The separate all-App stop was denied, expired, changed or already consumed"
                        .into(),
                )
            }
        }
    }
}

pub(crate) fn get_view(owner: u32, id: &str) -> Result<SystemReview, String> {
    view(&read(id, owner)?)
}

pub(crate) fn pending_views(owner: u32) -> Result<Vec<(u64, SystemReview)>, String> {
    if !directory().exists() {
        return Ok(Vec::new());
    }
    let mut result = Vec::new();
    let mut entries = 0;
    for entry in std::fs::read_dir(directory()).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let file = entry.path();
        if file.extension() != Some(std::ffi::OsStr::new("json")) {
            continue;
        }
        entries += 1;
        if entries > 4096 {
            return Err("Package impact review history exceeds its bound".into());
        }
        let Some(raw) =
            crate::approvals::presentation::read_owned_json::<Review>(&file, MAX_BYTES)?
        else {
            continue;
        };
        if raw.owner_uid != owner {
            continue;
        }
        if file != path(&raw.id)? {
            return Err("Package impact review filename does not match its identity".into());
        }
        let review = read(&raw.id, owner)?;
        if matches!(
            status(&review)?,
            ReviewStatus::Pending | ReviewStatus::Confirmed
        ) {
            result.push((review.requested_at, view(&review)?));
        }
    }
    Ok(result)
}

pub(crate) fn decide(owner: u32, decision: &ReviewDecision) -> Result<SystemReview, String> {
    let lock = path(&decision.id)?.with_extension("lock");
    crate::filelock::with_exclusive_path_lock(&lock, || {
        let mut review = read(&decision.id, owner)?;
        decision
            .validate_for(&view(&review)?)
            .map_err(|error| error.to_string())?;
        review.state = match decision.action {
            ReviewAction::ConfirmGlobalAppStop => State::Approved,
            ReviewAction::Cancel => State::Denied,
            _ => {
                return Err(
                    "App-install and permission choices cannot approve an all-App stop".into(),
                )
            }
        };
        review.decided_by = Some(owner);
        review.decided_at = Some(now()?);
        save(&review)?;
        view(&review)
    })
}

pub(super) fn consume(owner: u32, id: &str, impact: &Impact) -> Result<(), String> {
    let lock = path(id)?.with_extension("lock");
    crate::filelock::with_exclusive_path_lock(&lock, || {
        let mut review = read(id, owner)?;
        if status(&review)? != ReviewStatus::Confirmed
            || review.plan_digest != impact.plan_digest
            || review.impact_digest != impact.digest
        {
            return Err(
                "No separate protected owner confirmation matches this exact plan and impact"
                    .into(),
            );
        }
        review.state = State::Consumed;
        save(&review)
    })
}

pub(super) fn require_approved(owner: u32, id: &str, impact: &Impact) -> Result<(), String> {
    let review = read(id, owner)?;
    if status(&review)? != ReviewStatus::Confirmed
        || review.impact_digest != impact.digest
        || review.plan_digest != impact.plan_digest
    {
        return Err("The exact all-App stop is not separately confirmed".into());
    }
    Ok(())
}

pub(super) fn require_consumed(owner: u32, id: &str, impact: &Impact) -> Result<(), String> {
    let review = read(id, owner)?;
    if status(&review)? != ReviewStatus::Consumed
        || review.impact_digest != impact.digest
        || review.plan_digest != impact.plan_digest
    {
        return Err("The exact all-App stop has no protected consumption receipt".into());
    }
    Ok(())
}
