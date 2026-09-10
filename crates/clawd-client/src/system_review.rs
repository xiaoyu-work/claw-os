//! OS-owned review presentation, shared by terminal and desktop renderers.
//!
//! `system.review.pending {limit}` returns [`PendingReviews`], and
//! `system.review.show {id}` returns [`SystemReview`]. Owner cancellation uses
//! `system.review.cancel {review: ReviewDecision}`, without pkexec. Other
//! decisions use the privileged OS helper: `--system-review-json` reads a
//! bounded [`ReviewDecision`] from stdin and returns the latest review.
//! The broker must revalidate identity, revision, action and every selection.
//!
//! These are disclosure DTOs, not authority, grants, package models or an
//! approval store. Neither a revision nor a contract digest proves approval.
//! Confirmation of installation, activation or an update never applies choices.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;
pub const MAX_DECISION_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SystemReview {
    pub schema_version: u32,
    pub id: String,
    pub revision: u64,
    pub kind: ReviewKind,
    pub status: ReviewStatus,
    pub status_message: Option<String>,
    pub subject: ReviewSubject,
    pub initiator: String,
    pub context: Vec<ReviewDetail>,
    pub permissions: Vec<ReviewPermission>,
    pub disclosures: Vec<ReviewDetail>,
    pub changes: Option<ReviewChanges>,
    pub contract_digest: Option<String>,
    pub actions: Vec<ReviewAction>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewKind {
    Install,
    Activation,
    Capability,
    Update,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    Pending,
    Confirmed,
    Consumed,
    Completed,
    Cancelled,
    Stale,
    Expired,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReviewSubject {
    pub app_id: Option<String>,
    pub name: String,
    pub version: Option<String>,
    pub publisher: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReviewDetail {
    pub label: String,
    pub value: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReviewPermission {
    pub id: String,
    pub verb: String,
    pub label: String,
    pub blurb: String,
    pub risk: ReviewRisk,
    pub scope: ReviewScope,
    pub condition: Option<String>,
    pub uses: Vec<PermissionUse>,
    pub current: Option<PermissionChoice>,
    pub supported_choices: Vec<PermissionChoice>,
    pub unsupported_reason: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewRisk {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReviewScope {
    pub kind: ScopeKind,
    pub description: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScopeKind {
    Fixed,
    LateBound,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PermissionUse {
    pub function: String,
    pub purpose: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum PermissionChoice {
    Deny,
    Restore,
    Ask,
    AllowOnce,
    AllowSession,
    AllowForever,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ReviewAction {
    Cancel,
    ConfirmInstall,
    ConfirmActivation,
    ConfirmUpdate,
    ApplyChoices,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReviewChanges {
    pub summary: String,
    pub details: Vec<ReviewDetail>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PermissionSelection {
    pub permission_id: String,
    pub choice: PermissionChoice,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReviewDecision {
    pub id: String,
    pub revision: u64,
    pub action: ReviewAction,
    pub choices: Vec<PermissionSelection>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PendingReviews {
    pub reviews: Vec<SystemReview>,
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
#[error("invalid system review: {0}")]
pub struct ReviewError(pub String);

impl SystemReview {
    pub fn validate(&self) -> Result<(), ReviewError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(ReviewError("unsupported schema version".into()));
        }
        nonempty(&self.id, "request id")?;
        nonempty(&self.subject.name, "subject name")?;
        nonempty(&self.initiator, "initiator")?;
        for value in [
            &self.subject.app_id,
            &self.subject.version,
            &self.subject.publisher,
            &self.contract_digest,
            &self.status_message,
        ]
        .into_iter()
        .flatten()
        {
            nonempty(value, "optional subject or comparison detail")?;
        }
        let mut ids = BTreeSet::new();
        for permission in &self.permissions {
            for (value, field) in [
                (&permission.id, "permission id"),
                (&permission.verb, "permission verb"),
                (&permission.label, "OS permission label"),
                (&permission.blurb, "OS permission description"),
                (&permission.scope.description, "scope description"),
            ] {
                nonempty(value, field)?;
            }
            if !ids.insert(&permission.id) {
                return Err(ReviewError("duplicate permission id".into()));
            }
            unique(&permission.supported_choices, "supported choices")?;
            for value in [&permission.condition, &permission.unsupported_reason]
                .into_iter()
                .flatten()
            {
                nonempty(value, "permission explanation")?;
            }
            if permission.supported_choices.is_empty() {
                let reason = permission.unsupported_reason.as_deref().ok_or_else(|| {
                    ReviewError("missing explanation for unsupported permission choices".into())
                })?;
                nonempty(reason, "explanation for unsupported permission choices")?;
            }
            for usage in &permission.uses {
                nonempty(&usage.function, "affected function")?;
                nonempty(&usage.purpose, "App-supplied purpose")?;
            }
        }
        for detail in self
            .context
            .iter()
            .chain(&self.disclosures)
            .chain(self.changes.iter().flat_map(|changes| &changes.details))
        {
            nonempty(&detail.label, "detail label")?;
            nonempty(&detail.value, "detail value")?;
        }
        if let Some(changes) = &self.changes {
            nonempty(&changes.summary, "update summary")?;
        }
        unique(&self.actions, "decision actions")?;
        if self.status != ReviewStatus::Pending && !self.actions.is_empty() {
            return Err(ReviewError(
                "a handled or stale review cannot offer decisions".into(),
            ));
        }
        for action in &self.actions {
            let correct_kind = match action {
                ReviewAction::ConfirmInstall => self.kind == ReviewKind::Install,
                ReviewAction::ConfirmActivation => self.kind == ReviewKind::Activation,
                ReviewAction::ConfirmUpdate => self.kind == ReviewKind::Update,
                ReviewAction::Cancel | ReviewAction::ApplyChoices => true,
            };
            if !correct_kind {
                return Err(ReviewError(
                    "confirmation action does not match review kind".into(),
                ));
            }
            if *action == ReviewAction::ApplyChoices
                && !self
                    .permissions
                    .iter()
                    .any(|permission| !permission.supported_choices.is_empty())
            {
                return Err(ReviewError(
                    "permission decisions have no supported choices".into(),
                ));
            }
        }
        Ok(())
    }
}

impl PendingReviews {
    pub fn validate(&self) -> Result<(), ReviewError> {
        let mut ids = BTreeSet::new();
        for review in &self.reviews {
            review.validate()?;
            if !ids.insert(&review.id) {
                return Err(ReviewError("duplicate request id".into()));
            }
        }
        Ok(())
    }
}

impl ReviewDecision {
    /// Decode bounded, closed wire data without authorizing it or looking up a review.
    pub fn decode(bytes: &[u8]) -> Result<Self, ReviewError> {
        if bytes.len() > MAX_DECISION_BYTES {
            return Err(ReviewError("decision exceeds the input limit".into()));
        }
        serde_json::from_slice(bytes)
            .map_err(|error| ReviewError(format!("parse decision: {error}")))
    }

    /// Validate presentation consistency, never authorization or grant ownership.
    pub fn validate_for(&self, review: &SystemReview) -> Result<(), ReviewError> {
        review.validate()?;
        if self.id != review.id || self.revision != review.revision {
            return Err(ReviewError(
                "decision targets a different or stale revision".into(),
            ));
        }
        if review.status != ReviewStatus::Pending || !review.actions.contains(&self.action) {
            return Err(ReviewError("decision action is no longer available".into()));
        }
        if self.action != ReviewAction::ApplyChoices {
            if !self.choices.is_empty() {
                return Err(ReviewError(
                    "confirmation or cancellation cannot grant permissions".into(),
                ));
            }
        } else if self.choices.is_empty() {
            return Err(ReviewError(
                "select an explicit permission choice first".into(),
            ));
        }
        let mut ids = BTreeSet::new();
        for selection in &self.choices {
            if !ids.insert(&selection.permission_id) {
                return Err(ReviewError("duplicate permission selection".into()));
            }
            let permission = review
                .permissions
                .iter()
                .find(|permission| permission.id == selection.permission_id)
                .ok_or_else(|| ReviewError("unknown permission selection".into()))?;
            if !permission.supported_choices.contains(&selection.choice) {
                return Err(ReviewError(
                    "permission choice is not supported by the OS".into(),
                ));
            }
        }
        Ok(())
    }

    pub fn encode_for(&self, review: &SystemReview) -> Result<Vec<u8>, ReviewError> {
        self.validate_for(review)?;
        let bytes = serde_json::to_vec(self)
            .map_err(|error| ReviewError(format!("serialize decision: {error}")))?;
        if bytes.len() > MAX_DECISION_BYTES {
            return Err(ReviewError(
                "decision exceeds the helper input limit".into(),
            ));
        }
        Ok(bytes)
    }
}

fn nonempty(value: &str, field: &str) -> Result<(), ReviewError> {
    if value.trim().is_empty() {
        Err(ReviewError(format!("{field} is empty")))
    } else {
        Ok(())
    }
}

fn unique<T: Ord>(values: &[T], field: &str) -> Result<(), ReviewError> {
    if values.iter().collect::<BTreeSet<_>>().len() != values.len() {
        Err(ReviewError(format!("duplicate {field}")))
    } else {
        Ok(())
    }
}

/// Stable label keys, with the same English text for terminal and desktop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewText {
    Title,
    Install,
    Activation,
    Capability,
    Update,
    Pending,
    Confirmed,
    Consumed,
    Completed,
    Cancelled,
    Stale,
    Expired,
    Failed,
    Cancel,
    ConfirmInstall,
    ConfirmActivation,
    ConfirmUpdate,
    ApplyChoices,
    Deny,
    Restore,
    Ask,
    AllowOnce,
    AllowSession,
    AllowForever,
    Low,
    Medium,
    High,
    Critical,
    NoGrantNotice,
    CapabilityNotice,
    DigestNotice,
    Request,
    Revision,
    Subject,
    AppId,
    Version,
    Publisher,
    Unreported,
    Initiator,
    Context,
    Permissions,
    NoPermissions,
    Risk,
    Scope,
    LateBound,
    Condition,
    AppPurpose,
    AffectedFunction,
    CurrentChoice,
    Unchanged,
    Unsupported,
    Changes,
    Disclosures,
    ContractDigest,
    Actions,
    NoActions,
    Busy,
    Refresh,
    Close,
    Error,
    NoPending,
    RefreshRequired,
    SeparateChoices,
}

impl ReviewText {
    pub const fn key(self) -> &'static str {
        self.pair().0
    }
    pub const fn english(self) -> &'static str {
        self.pair().1
    }

    const fn pair(self) -> (&'static str, &'static str) {
        match self {
            Self::Title => ("review-title", "Claw OS system review"),
            Self::Install => ("review-kind-install", "Installation confirmation"),
            Self::Activation => ("review-kind-activation", "Activation confirmation"),
            Self::Capability => ("review-kind-capability", "Permission choices"),
            Self::Update => ("review-kind-update", "Update confirmation"),
            Self::Pending => ("review-pending", "Pending review"),
            Self::Confirmed => ("review-confirmed", "Confirmation recorded"),
            Self::Consumed => ("review-consumed", "Confirmation used; operation outcome is separate"),
            Self::Completed => ("review-completed", "Review completed"),
            Self::Cancelled => ("review-cancelled", "Review cancelled"),
            Self::Stale => ("review-stale", "Review changed; refresh before deciding"),
            Self::Expired => ("review-expired", "Review expired"),
            Self::Failed => ("review-failed", "Review failed"),
            Self::Cancel => ("review-cancel", "Cancel review"),
            Self::ConfirmInstall => ("review-confirm-install", "Confirm installation"),
            Self::ConfirmActivation => ("review-confirm-activation", "Confirm activation"),
            Self::ConfirmUpdate => ("review-confirm-update", "Confirm update"),
            Self::ApplyChoices => ("review-apply-choices", "Apply selected permission choices"),
            Self::Deny => ("review-choice-deny", "Deny"),
            Self::Restore => ("review-choice-restore", "Restore App policy"),
            Self::Ask => ("review-choice-ask", "Ask when used"),
            Self::AllowOnce => ("review-choice-once", "Allow once"),
            Self::AllowSession => ("review-choice-session", "Allow for this session"),
            Self::AllowForever => ("review-choice-forever", "Allow until revoked"),
            Self::Low => ("review-risk-low", "Low risk"),
            Self::Medium => ("review-risk-medium", "Medium risk"),
            Self::High => ("review-risk-high", "High risk"),
            Self::Critical => ("review-risk-critical", "Critical risk"),
            Self::NoGrantNotice => ("review-no-grant-notice", "This confirmation does not grant capabilities or AI consent."),
            Self::CapabilityNotice => ("review-capability-notice", "Only explicit supported choices are submitted. Unselected permissions are unchanged."),
            Self::DigestNotice => ("review-digest-notice", "The comparison digest and displayed JSON are not approval authority."),
            Self::Request => ("review-request", "Request"),
            Self::Revision => ("review-revision", "Revision"),
            Self::Subject => ("review-subject", "Subject"),
            Self::AppId => ("review-app-id", "App identity"),
            Self::Version => ("review-version", "Version"),
            Self::Publisher => ("review-publisher", "Publisher"),
            Self::Unreported => ("review-unreported", "Not reported by the OS"),
            Self::Initiator => ("review-initiator", "Initiated by"),
            Self::Context => ("review-context", "Context"),
            Self::Permissions => ("review-permissions", "Requested permissions"),
            Self::NoPermissions => ("review-no-permissions", "No capability requests are listed."),
            Self::Risk => ("review-risk", "Risk"),
            Self::Scope => ("review-scope", "Scope"),
            Self::LateBound => ("review-late-bound", "Selected/confirmed when used"),
            Self::Condition => ("review-condition", "Condition"),
            Self::AppPurpose => ("review-app-purpose", "App-supplied purpose (not OS policy)"),
            Self::AffectedFunction => ("review-function", "Affected function"),
            Self::CurrentChoice => ("review-current-choice", "Current choice"),
            Self::Unchanged => ("review-unchanged", "Leave unchanged"),
            Self::Unsupported => ("review-unsupported", "Choice limitations"),
            Self::Changes => ("review-changes", "Update changes"),
            Self::Disclosures => ("review-disclosures", "AI, service and desktop disclosures"),
            Self::ContractDigest => ("review-contract-digest", "Comparison digest"),
            Self::Actions => ("review-actions", "Available decisions"),
            Self::NoActions => ("review-no-actions", "No decisions are available for this review."),
            Self::Busy => ("review-busy", "Waiting for the OS decision; do not resubmit"),
            Self::Refresh => ("review-refresh", "Refresh from OS"),
            Self::Close => ("review-close", "Close"),
            Self::Error => ("review-error", "Review could not be refreshed or decided"),
            Self::NoPending => ("review-no-pending", "No pending reviews"),
            Self::RefreshRequired => ("review-refresh-required", "The request may have been handled elsewhere. Refresh before deciding."),
            Self::SeparateChoices => ("review-separate-choices", "Apply or clear selected permission choices before confirming."),
        }
    }
}

impl ReviewKind {
    pub const fn text(self) -> ReviewText {
        match self {
            Self::Install => ReviewText::Install,
            Self::Activation => ReviewText::Activation,
            Self::Capability => ReviewText::Capability,
            Self::Update => ReviewText::Update,
        }
    }
}

impl ReviewStatus {
    pub const fn text(self) -> ReviewText {
        match self {
            Self::Pending => ReviewText::Pending,
            Self::Confirmed => ReviewText::Confirmed,
            Self::Consumed => ReviewText::Consumed,
            Self::Completed => ReviewText::Completed,
            Self::Cancelled => ReviewText::Cancelled,
            Self::Stale => ReviewText::Stale,
            Self::Expired => ReviewText::Expired,
            Self::Failed => ReviewText::Failed,
        }
    }
}

impl ReviewRisk {
    pub const fn text(self) -> ReviewText {
        match self {
            Self::Low => ReviewText::Low,
            Self::Medium => ReviewText::Medium,
            Self::High => ReviewText::High,
            Self::Critical => ReviewText::Critical,
        }
    }
}

impl PermissionChoice {
    pub const fn text(self) -> ReviewText {
        match self {
            Self::Deny => ReviewText::Deny,
            Self::Restore => ReviewText::Restore,
            Self::Ask => ReviewText::Ask,
            Self::AllowOnce => ReviewText::AllowOnce,
            Self::AllowSession => ReviewText::AllowSession,
            Self::AllowForever => ReviewText::AllowForever,
        }
    }
}

impl ReviewAction {
    pub const fn text(self) -> ReviewText {
        match self {
            Self::Cancel => ReviewText::Cancel,
            Self::ConfirmInstall => ReviewText::ConfirmInstall,
            Self::ConfirmActivation => ReviewText::ConfirmActivation,
            Self::ConfirmUpdate => ReviewText::ConfirmUpdate,
            Self::ApplyChoices => ReviewText::ApplyChoices,
        }
    }
}

/// Escape controls, bidi controls and line breaks in externally supplied text.
pub fn display_text(value: &str) -> String {
    value.chars().flat_map(char::escape_debug).collect()
}

pub fn format_terminal(review: &SystemReview) -> Result<String, ReviewError> {
    format_terminal_with(review, |label| label.english().to_string())
}

/// Read-only formatting; the caller supplies localization, never decision logic.
pub fn format_terminal_with(
    review: &SystemReview,
    label: impl Fn(ReviewText) -> String,
) -> Result<String, ReviewError> {
    review.validate()?;
    let mut output = format!(
        "{}\n{} - {}\n",
        label(ReviewText::Title),
        label(review.kind.text()),
        label(review.status.text())
    );
    let mut field = |name: String, value: &str| {
        output.push_str(&format!("{name}: \"{}\"\n", display_text(value)));
    };
    field(label(ReviewText::Request), &review.id);
    field(label(ReviewText::Revision), &review.revision.to_string());
    field(label(ReviewText::Subject), &review.subject.name);
    for (key, value) in [
        (ReviewText::AppId, &review.subject.app_id),
        (ReviewText::Version, &review.subject.version),
        (ReviewText::Publisher, &review.subject.publisher),
    ] {
        field(
            label(key),
            value.as_deref().unwrap_or(&label(ReviewText::Unreported)),
        );
    }
    field(label(ReviewText::Initiator), &review.initiator);
    for detail in &review.context {
        field(
            format!(
                "{} / {}",
                label(ReviewText::Context),
                display_text(&detail.label)
            ),
            &detail.value,
        );
    }
    if let Some(message) = &review.status_message {
        field(label(review.status.text()), message);
    }
    output.push_str(&format!(
        "{}\n",
        label(if review.kind == ReviewKind::Capability {
            ReviewText::CapabilityNotice
        } else {
            ReviewText::NoGrantNotice
        })
    ));
    output.push_str(&format!("\n{}\n", label(ReviewText::Permissions)));
    if review.permissions.is_empty() {
        output.push_str(&format!("{}\n", label(ReviewText::NoPermissions)));
    }
    for permission in &review.permissions {
        output.push_str(&format!(
            "\n- \"{}\" ({}) [{}]\n  \"{}\"\n",
            display_text(&permission.label),
            display_text(&permission.verb),
            label(permission.risk.text()),
            display_text(&permission.blurb)
        ));
        let scope_label = if permission.scope.kind == ScopeKind::LateBound {
            ReviewText::LateBound
        } else {
            ReviewText::Scope
        };
        output.push_str(&format!(
            "  {}: \"{}\"\n",
            label(scope_label),
            display_text(&permission.scope.description)
        ));
        if let Some(condition) = &permission.condition {
            output.push_str(&format!(
                "  {}: \"{}\"\n",
                label(ReviewText::Condition),
                display_text(condition)
            ));
        }
        for usage in &permission.uses {
            output.push_str(&format!(
                "  {}: \"{}\"\n  {}: \"{}\"\n",
                label(ReviewText::AffectedFunction),
                display_text(&usage.function),
                label(ReviewText::AppPurpose),
                display_text(&usage.purpose)
            ));
        }
        output.push_str(&format!(
            "  {}: {}\n",
            label(ReviewText::CurrentChoice),
            label(
                permission
                    .current
                    .map_or(ReviewText::Unreported, PermissionChoice::text)
            )
        ));
        if review.actions.contains(&ReviewAction::ApplyChoices) {
            for choice in &permission.supported_choices {
                output.push_str(&format!("  [ ] {}\n", label(choice.text())));
            }
        }
        if let Some(reason) = &permission.unsupported_reason {
            output.push_str(&format!(
                "  {}: \"{}\"\n",
                label(ReviewText::Unsupported),
                display_text(reason)
            ));
        }
    }
    for detail in &review.disclosures {
        output.push_str(&format!(
            "\n{} / {}: \"{}\"\n",
            label(ReviewText::Disclosures),
            display_text(&detail.label),
            display_text(&detail.value)
        ));
    }
    if let Some(changes) = &review.changes {
        output.push_str(&format!(
            "\n{}: \"{}\"\n",
            label(ReviewText::Changes),
            display_text(&changes.summary)
        ));
        for detail in &changes.details {
            output.push_str(&format!(
                "  \"{}\": \"{}\"\n",
                display_text(&detail.label),
                display_text(&detail.value)
            ));
        }
    }
    if let Some(digest) = &review.contract_digest {
        output.push_str(&format!(
            "\n{}: \"{}\"\n{}\n",
            label(ReviewText::ContractDigest),
            display_text(digest),
            label(ReviewText::DigestNotice)
        ));
    }
    output.push_str(&format!("\n{}\n", label(ReviewText::Actions)));
    if review.actions.is_empty() {
        output.push_str(&format!("{}\n", label(ReviewText::NoActions)));
    }
    for action in &review.actions {
        output.push_str(&format!("- {}\n", label(action.text())));
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/system_review.rs"
    ));
}
