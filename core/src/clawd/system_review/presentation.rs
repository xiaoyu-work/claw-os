//! Projection into the one OS terminal/desktop review contract.

use crate::approvals::{
    self, presentation::LegacyReview, system_review::ReviewRecord, GrantDuration,
};
use crate::caps::{catalog, manifest::ScopeBinding, Risk, Verb};
use crate::provenance::runtime::PackageRef;
use clawd_client::system_review::{
    PendingReviews, PermissionChoice, PermissionUse, ReviewAction, ReviewDetail, ReviewKind,
    ReviewPermission, ReviewRisk, ReviewScope, ReviewStatus, ReviewSubject, ScopeKind,
    SystemReview,
};

fn risk(value: Risk) -> ReviewRisk {
    match value {
        Risk::Low => ReviewRisk::Low,
        Risk::Medium => ReviewRisk::Medium,
        Risk::High => ReviewRisk::High,
        Risk::Critical => ReviewRisk::Critical,
    }
}

fn stamped(owner: u32, mut review: SystemReview) -> Result<SystemReview, String> {
    review.validate().map_err(|error| error.to_string())?;
    let bytes = serde_json::to_vec(&review)
        .map_err(|error| format!("encode system review projection: {error}"))?;
    review.revision =
        approvals::presentation::revision(owner, &review.id, &crate::crypto::sha256_hex(&bytes))?;
    Ok(review)
}

pub(super) fn app(record: &ReviewRecord) -> Result<SystemReview, String> {
    use crate::approvals::system_review::{ReviewKind as StoredKind, ReviewState};

    let disclosure = record.permission_review()?;
    let status = match record.effective_state()? {
        ReviewState::Pending => ReviewStatus::Pending,
        ReviewState::Approved => ReviewStatus::Confirmed,
        ReviewState::Consumed => ReviewStatus::Consumed,
        ReviewState::Denied => ReviewStatus::Cancelled,
        ReviewState::Expired => ReviewStatus::Expired,
        ReviewState::Stale => ReviewStatus::Stale,
    };
    let kind = match record.kind {
        StoredKind::AppInstall => ReviewKind::Install,
        StoredKind::AppActivation => ReviewKind::Activation,
    };
    let permissions = disclosure.permissions.iter().map(|permission| {
        let identity = serde_json::to_vec(&(
            permission.verb, &permission.scope, &permission.when,
        )).map_err(|error| format!("encode displayed permission identity: {error}"))?;
        Ok(ReviewPermission {
            id: crate::crypto::sha256_hex(&identity),
            verb: permission.verb.as_str().to_string(),
            label: permission.label.clone(),
            blurb: permission.description.clone(),
            risk: risk(permission.risk),
            scope: ReviewScope {
                kind: if matches!(permission.scope, ScopeBinding::Fixed { .. }) {
                    ScopeKind::Fixed
                } else {
                    ScopeKind::LateBound
                },
                description: crate::apps::permission_review::scope_description(&permission.scope)?,
            },
            condition: permission.when.as_ref().map(serde_json::to_string).transpose()
                .map_err(|error| format!("encode displayed permission condition: {error}"))?,
            uses: permission.uses.iter().map(|usage| PermissionUse {
                function: usage.entry.clone(),
                purpose: usage.purpose.current().to_string(),
            }).collect(),
            current: None,
            supported_choices: Vec::new(),
            unsupported_reason: Some(
                "Installation or activation review does not grant or change this permission; resource authorization is separate.".into(),
            ),
        })
    }).collect::<Result<Vec<_>, String>>()?;
    let mut disclosures = Vec::new();
    let arguments: std::collections::BTreeMap<_, _> = disclosure
        .permissions
        .iter()
        .flat_map(|permission| &permission.uses)
        .filter(|usage| !usage.arguments.is_empty())
        .map(|usage| (&usage.entry, &usage.arguments))
        .collect();
    for (entry, arguments) in arguments {
        disclosures.push(ReviewDetail {
            label: format!("Argument constraints for {entry}"),
            value: serde_json::to_string(arguments)
                .map_err(|error| format!("encode displayed App arguments: {error}"))?,
        });
    }
    if let Some(service) = &disclosure.service {
        disclosures.push(ReviewDetail {
            label: "Service lifecycle".into(),
            value: serde_json::to_string(&service.lifecycle).map_err(|error| error.to_string())?,
        });
        disclosures.push(ReviewDetail {
            label: "External Agent access requested".into(),
            value: service.access.external_agents.to_string(),
        });
    }
    if let Some(policy) = &disclosure.ai_policy {
        disclosures.push(ReviewDetail {
            label: "AI policy (separate consent required)".into(),
            value: serde_json::to_string_pretty(policy).map_err(|error| error.to_string())?,
        });
    }
    if let Some(desktop) = &disclosure.desktop {
        disclosures.push(ReviewDetail {
            label: "Desktop entry and file types requested".into(),
            value: serde_json::to_string(&desktop.mime_types).map_err(|error| error.to_string())?,
        });
    }
    let actions = if status == ReviewStatus::Pending {
        vec![
            ReviewAction::Cancel,
            match kind {
                ReviewKind::Activation => ReviewAction::ConfirmActivation,
                _ => ReviewAction::ConfirmInstall,
            },
        ]
    } else {
        Vec::new()
    };
    stamped(
        record.owner_uid,
        SystemReview {
            schema_version: clawd_client::system_review::SCHEMA_VERSION,
            id: record.id.clone(),
            revision: 0,
            kind,
            status,
            status_message: None,
            subject: ReviewSubject {
                app_id: Some(record.manifest.id.clone()),
                name: record.manifest.name.current().to_string(),
                version: Some(record.manifest.version.clone()),
                publisher: record.package.publisher_key_id.clone(),
            },
            initiator: record.requester.clone(),
            context: vec![
                ReviewDetail {
                    label: "Package digest".into(),
                    value: record.package.content_digest.clone(),
                },
                ReviewDetail {
                    label: "Trust tier".into(),
                    value: record.package.tier.clone(),
                },
                ReviewDetail {
                    label: "Requested at (Unix seconds)".into(),
                    value: record.requested_at.to_string(),
                },
                ReviewDetail {
                    label: "Expires at (Unix seconds)".into(),
                    value: record.expires_at.to_string(),
                },
            ],
            permissions,
            disclosures,
            changes: None,
            contract_digest: Some(record.contract_digest.clone()),
            actions,
        },
    )
}

pub(super) fn legacy(owner: u32, review: LegacyReview) -> Result<SystemReview, String> {
    let (request, mut status) = match review {
        LegacyReview::Pending(request) => (*request, ReviewStatus::Pending),
        LegacyReview::Resolved(resolved) => (
            resolved.request,
            match resolved.decision.outcome {
                approvals::Outcome::Approved => ReviewStatus::Completed,
                approvals::Outcome::Denied => ReviewStatus::Cancelled,
            },
        ),
    };
    if request.owner_uid != Some(owner) {
        return Err("approval is not owned by this user".into());
    }
    let verb = Verb::parse(&request.verb).ok_or("approval has an unknown capability")?;
    let (capability, current_risk) = approvals::canonical_capability(verb, request.scope.clone())?;
    let meta = catalog::lookup(verb).ok_or("approval capability has no OS metadata")?;
    let restoration = request
        .session
        .starts_with(approvals::app_policy::SESSION_PREFIX);
    let mut subject = ReviewSubject {
        app_id: None,
        name: if restoration {
            "App permission restoration"
        } else {
            "System capability request"
        }
        .into(),
        version: None,
        publisher: None,
    };
    let mut uses = Vec::new();
    let mut status_message = None;
    let mut supported_choices = Vec::new();
    if status == ReviewStatus::Pending {
        if restoration {
            match super::super::app_permissions::approval_app(&request) {
                Ok(app) => {
                    subject = ReviewSubject {
                        app_id: Some(app.manifest.id.clone()),
                        name: app.manifest.name.current().to_string(),
                        version: Some(app.manifest.version.clone()),
                        publisher: PackageRef::of(app.require_verified()?).publisher_key_id,
                    };
                    let disclosure =
                        crate::apps::permission_review::PermissionReview::from_manifest(
                            &app.manifest,
                        )?;
                    for permission in disclosure.permissions {
                        let matches = permission.verb == verb
                            && match &permission.scope {
                                ScopeBinding::Fixed { scope } => *scope == request.scope,
                                ScopeBinding::Wild => request.scope == crate::caps::Scope::Wild,
                                _ => false,
                            };
                        if matches {
                            uses.extend(permission.uses.into_iter().map(|usage| PermissionUse {
                                function: usage.entry,
                                purpose: usage.purpose.current().to_string(),
                            }));
                        }
                    }
                    supported_choices = vec![PermissionChoice::Deny, PermissionChoice::Restore];
                }
                Err(error) => {
                    status = ReviewStatus::Stale;
                    status_message = Some(error);
                }
            }
        } else {
            match approvals::supported_durations(&request) {
                Ok(durations) => {
                    supported_choices.push(PermissionChoice::Deny);
                    supported_choices.extend(durations.into_iter().map(
                        |duration| match duration {
                            GrantDuration::Once => PermissionChoice::AllowOnce,
                            GrantDuration::Session => PermissionChoice::AllowSession,
                            GrantDuration::Forever => PermissionChoice::AllowForever,
                        },
                    ));
                }
                Err(error) => {
                    status = ReviewStatus::Stale;
                    status_message = Some(error);
                }
            }
        }
    }
    let unsupported_reason = supported_choices
        .is_empty()
        .then(|| "This request is no longer available for a permission decision.".to_string());
    let actions = if status == ReviewStatus::Pending {
        vec![ReviewAction::Cancel, ReviewAction::ApplyChoices]
    } else {
        Vec::new()
    };
    let mut context = vec![
        ReviewDetail {
            label: "Session".into(),
            value: request.session.clone(),
        },
        ReviewDetail {
            label: "Requested at (Unix seconds)".into(),
            value: request.requested_at.to_string(),
        },
    ];
    if !request.reason.trim().is_empty() {
        context.push(ReviewDetail {
            label: "Requester-supplied reason".into(),
            value: request.reason.clone(),
        });
    }
    if let Some(consent) = request.context {
        context.push(ReviewDetail {
            label: "Execution consent context".into(),
            value: serde_json::to_string(&consent).map_err(|error| error.to_string())?,
        });
    }
    if let Some(digest) = &request.operation_digest {
        context.push(ReviewDetail {
            label: "Exact operation digest".into(),
            value: digest.clone(),
        });
    }
    let disclosures = if restoration {
        vec![ReviewDetail {
            label: "Restoration".into(),
            value: "Restores the existing App policy only; it does not mint execution authority."
                .into(),
        }]
    } else {
        [("Once", GrantDuration::Once), ("Session", GrantDuration::Session), ("Reusable", GrantDuration::Forever)]
            .into_iter().map(|(label, duration)| {
                let (seconds, uses) = duration.limits();
                ReviewDetail {
                    label: format!("{label} grant ceiling"),
                    value: format!("At most {uses} uses and {seconds} seconds; exact owner, scope, session and execution bounds still apply. Revocation can end it earlier."),
                }
            }).collect()
    };
    stamped(
        owner,
        SystemReview {
            schema_version: clawd_client::system_review::SCHEMA_VERSION,
            id: request.id.clone(),
            revision: 0,
            kind: ReviewKind::Capability,
            status,
            status_message,
            subject,
            initiator: request
                .requester
                .clone()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| format!("uid:{owner}")),
            context,
            permissions: vec![ReviewPermission {
                id: request.id,
                verb: request.verb,
                label: meta.label.current().to_string(),
                blurb: meta.blurb.current().to_string(),
                risk: risk(current_risk),
                scope: ReviewScope {
                    kind: ScopeKind::Fixed,
                    description: capability.scope.to_string(),
                },
                condition: None,
                uses,
                current: (restoration && status == ReviewStatus::Pending)
                    .then_some(PermissionChoice::Deny),
                supported_choices,
                unsupported_reason,
            }],
            disclosures,
            changes: None,
            contract_digest: None,
            actions,
        },
    )
}

pub(super) fn get(owner: u32, id: &str) -> Result<SystemReview, String> {
    if id.starts_with("rv-") {
        app(&crate::approvals::system_review::get(owner, id)?)
    } else {
        legacy(owner, approvals::presentation::legacy_get(owner, id)?)
    }
}

pub(super) fn pending(owner: u32, limit: usize) -> Result<PendingReviews, String> {
    let mut records = crate::approvals::system_review::pending(owner, limit)?
        .iter()
        .map(|record| Ok((record.requested_at, app(record)?)))
        .collect::<Result<Vec<_>, String>>()?;
    for request in approvals::presentation::legacy_pending(owner)? {
        let at = request.requested_at;
        records.push((at, legacy(owner, LegacyReview::Pending(Box::new(request)))?));
    }
    records.sort_by_key(|(at, review)| (*at, review.id.clone()));
    let pending = PendingReviews {
        reviews: records
            .into_iter()
            .take(limit)
            .map(|(_, view)| view)
            .collect(),
    };
    pending.validate().map_err(|error| error.to_string())?;
    Ok(pending)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/system_review/presentation.rs"
    ));
}
