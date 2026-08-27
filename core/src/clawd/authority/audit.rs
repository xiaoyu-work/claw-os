//! Typed authority facts.
//!
//! Every issuance, attenuation, use, exhaustion, expiry and revocation
//! is recorded through one of these structs. They are the only shapes
//! that reach the log, and they deliberately carry no handle, no
//! capability *value* that could be a secret name, and no
//! caller-authored string:
//!
//! * a grant is named by its keyed [`GrantRef`], which correlates
//!   records about the same grant and reverses to nothing;
//! * capabilities are recorded as verb plus scope *kind* plus a digest
//!   of the canonical scope, so `secret.read:openai/prod` is
//!   distinguishable from `secret.read:openai/test` in the trail
//!   without either name being written down;
//! * failures are recorded by their stable class, never by the
//!   `Display` text, which quotes the scope.
//!
//! The route audit record already written by [`super::super::audit`]
//! carries the request and response facts; these add the authority
//! decision behind them.

use serde::Serialize;

use crate::audit_policy::TextDigest;
use crate::caps::Cap;
use crate::session::journal::{self, GrantEnd, JournalEvent, Label, Partition, Reference};
use crate::session::SessionId;

use super::decision::Decision;
use super::grant::{Audience, AudienceSet, Issuer};
use super::handle::GrantRef;
use super::store::{AuthorityError, GrantView};

/// Mirror one authority fact into the session journal.
///
/// The chain gets the same keyed [`GrantRef`] the audit log carries and
/// nothing else: no handle, no capability value, no scope string.
/// Replaying the record cannot mint a grant — the authority holds the
/// live one, bound to a process and a use budget, and this is only
/// evidence that it did.
fn journal(session_id: Option<&str>, owner_uid: u32, event: JournalEvent) {
    let partition = match session_id.and_then(|id| id.parse::<SessionId>().ok()) {
        Some(sid) => Partition::Session(sid),
        None => Partition::Owner(owner_uid),
    };
    journal::record_best_effort(&partition, owner_uid, journal::EventSource::Kernel, event);
}

/// One capability, projected so nothing sensitive survives.
#[derive(Debug, Serialize)]
pub struct CapFacts {
    verb: &'static str,
    scope_kind: &'static str,
    /// Digest of the canonical scope string. Equal digests mean the
    /// same resource; the resource itself is not recoverable.
    scope: TextDigest,
}

impl CapFacts {
    pub fn of(cap: &Cap) -> Self {
        Self {
            verb: cap.verb.as_str(),
            scope_kind: scope_kind_name(cap),
            scope: crate::audit_policy::text_digest(&cap.scope.to_string()),
        }
    }

    fn many(caps: &[Cap]) -> Vec<Self> {
        caps.iter().map(Self::of).collect()
    }
}

fn scope_kind_name(cap: &Cap) -> &'static str {
    match cap.scope {
        crate::caps::Scope::Path(_) => "path",
        crate::caps::Scope::Host(_) => "host",
        crate::caps::Scope::Name(_) => "name",
        crate::caps::Scope::SelfRef(_) => "self",
        crate::caps::Scope::Wild => "wild",
    }
}

#[derive(Debug, Serialize)]
struct GrantLifecycleAudit<'a> {
    ts: chrono::DateTime<chrono::Utc>,
    event: &'static str,
    grant: &'a GrantRef,
    issuer: &'static str,
    audience: Vec<&'static str>,
    owner_uid: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    session: Option<TextDigest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    app_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent: Option<GrantRef>,
    depth: u16,
    expires_in_ms: u128,
    #[serde(skip_serializing_if = "Option::is_none")]
    uses_remaining: Option<u32>,
    caps: Vec<CapFacts>,
}

#[derive(Debug, Serialize)]
struct GrantUseAudit<'a> {
    ts: chrono::DateTime<chrono::Utc>,
    event: &'static str,
    grant: &'a GrantRef,
    route: &'static str,
    audience: &'static str,
    issuer: &'static str,
    owner_uid: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    session: Option<TextDigest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    app_id: Option<&'a str>,
    decision: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    uses_remaining: Option<u32>,
    caps: Vec<CapFacts>,
}

#[derive(Debug, Serialize)]
struct GrantRevocationAudit<'a> {
    ts: chrono::DateTime<chrono::Utc>,
    event: &'static str,
    scope: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    grant: Option<&'a GrantRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    subject: Option<TextDigest>,
    retired: usize,
}

/// One grant was minted.
pub fn record_issued(view: &GrantView, parent: Option<&GrantRef>) {
    let grant_ref = view.id.audit_ref();
    write(&GrantLifecycleAudit {
        ts: chrono::Utc::now(),
        event: if parent.is_some() {
            "clawd.grant.attenuated"
        } else {
            "clawd.grant.issued"
        },
        grant: &grant_ref,
        issuer: view.issuer.as_str(),
        audience: view.audience.names(),
        owner_uid: view.owner_uid,
        session: view
            .subject
            .session_id
            .as_deref()
            .map(crate::audit_policy::text_digest),
        app_id: view.subject.app_id.as_deref(),
        parent: parent.cloned(),
        depth: view.depth,
        expires_in_ms: view.expires_in.as_millis(),
        uses_remaining: view.uses_remaining,
        caps: CapFacts::many(&view.caps.iter().cloned().collect::<Vec<_>>()),
    });
    journal(
        view.subject.session_id.as_deref(),
        view.owner_uid,
        JournalEvent::CapabilityIssued {
            grant: Reference::new(grant_ref.as_str()),
            audience: Label::new(view.audience.names().first().copied().unwrap_or("none")),
            issuer: Label::new(view.issuer.as_str()),
            caps: view.caps.iter().count() as u32,
            uses: view.uses_remaining,
        },
    );
}

/// One grant was exercised.
pub fn record_use(decision: &Decision, required: &[Cap], uses_remaining: Option<u32>) {
    write(&GrantUseAudit {
        ts: chrono::Utc::now(),
        event: "clawd.grant.use",
        grant: decision.grant_ref(),
        route: decision_route(decision),
        audience: decision.audience().as_str(),
        issuer: decision.issuer().as_str(),
        owner_uid: decision.owner_uid(),
        session: decision.session_id().map(crate::audit_policy::text_digest),
        app_id: decision.app_id(),
        decision: "allow",
        reason: None,
        uses_remaining,
        caps: CapFacts::many(required),
    });
    if uses_remaining == Some(0) {
        write(&GrantUseAudit {
            ts: chrono::Utc::now(),
            event: "clawd.grant.exhausted",
            grant: decision.grant_ref(),
            route: decision_route(decision),
            audience: decision.audience().as_str(),
            issuer: decision.issuer().as_str(),
            owner_uid: decision.owner_uid(),
            session: decision.session_id().map(crate::audit_policy::text_digest),
            app_id: decision.app_id(),
            decision: "retire",
            reason: Some("exhausted"),
            uses_remaining: Some(0),
            caps: Vec::new(),
        });
    }
    journal(
        decision.session_id(),
        decision.owner_uid(),
        JournalEvent::CapabilityUsed {
            grant: Reference::new(decision.grant_ref().as_str()),
            route: Label::new(decision_route(decision)),
            caps: required.len() as u32,
            uses_remaining,
        },
    );
    if uses_remaining == Some(0) {
        journal(
            decision.session_id(),
            decision.owner_uid(),
            JournalEvent::CapabilityExhausted {
                grant: Reference::new(decision.grant_ref().as_str()),
                reason: GrantEnd::UsesExhausted,
            },
        );
    }
}

/// One grant refused a capability.
pub fn record_denied(decision: &Decision, required: &[Cap], error: &AuthorityError) {
    write(&GrantUseAudit {
        ts: chrono::Utc::now(),
        event: "clawd.grant.use",
        grant: decision.grant_ref(),
        route: decision_route(decision),
        audience: decision.audience().as_str(),
        issuer: decision.issuer().as_str(),
        owner_uid: decision.owner_uid(),
        session: decision.session_id().map(crate::audit_policy::text_digest),
        app_id: decision.app_id(),
        decision: "deny",
        reason: Some(error.class()),
        uses_remaining: None,
        caps: CapFacts::many(required),
    });
}

/// A route asked to be authorized for nothing.
///
/// Recorded rather than silently refused: an empty requirement is a
/// programming mistake on a privileged path, and the trail should show
/// which route made it.
pub fn record_empty_requirement(decision: &Decision) {
    write(&GrantUseAudit {
        ts: chrono::Utc::now(),
        event: "clawd.grant.use",
        grant: decision.grant_ref(),
        route: decision_route(decision),
        audience: decision.audience().as_str(),
        issuer: decision.issuer().as_str(),
        owner_uid: decision.owner_uid(),
        session: decision.session_id().map(crate::audit_policy::text_digest),
        app_id: decision.app_id(),
        decision: "deny",
        reason: Some("empty_requirement"),
        uses_remaining: None,
        caps: Vec::new(),
    });
}

/// A request could not resolve any grant at all.
///
/// There is no grant reference to record — that is the point of the
/// record — so it carries the route, the audience, the class of the
/// refusal and a digest of the session the request named.
#[derive(Debug, Serialize)]
struct GrantResolutionAudit {
    ts: chrono::DateTime<chrono::Utc>,
    event: &'static str,
    route: &'static str,
    audience: &'static str,
    owner_uid: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    session: Option<TextDigest>,
    reason: &'static str,
}

pub fn record_unresolved(
    route: &'static str,
    audience: Audience,
    owner_uid: u32,
    session_id: Option<&str>,
    error: &AuthorityError,
) {
    write(&GrantResolutionAudit {
        ts: chrono::Utc::now(),
        event: "clawd.grant.unresolved",
        route,
        audience: audience.as_str(),
        owner_uid,
        session: session_id.map(crate::audit_policy::text_digest),
        reason: error.class(),
    });
}

/// A lineage was retired.
pub fn record_revoked(scope: &'static str, subject: Option<&str>, retired: usize) {
    if retired == 0 {
        return;
    }
    write(&GrantRevocationAudit {
        ts: chrono::Utc::now(),
        event: "clawd.grant.revoked",
        scope,
        grant: None,
        subject: subject.map(crate::audit_policy::text_digest),
        retired,
    });
}

fn decision_route(decision: &Decision) -> &'static str {
    decision.audit_route()
}

fn write<T: serde::Serialize>(record: &T) {
    if let Err(error) = super::super::audit::append_jsonl(record) {
        tracing::error!(error = %error, "failed to write capability authority audit record");
    }
}

/// Convenience for describing an audience set in a log line.
pub fn audience_names(set: AudienceSet) -> Vec<&'static str> {
    set.names()
}

/// Convenience for describing an issuer in a log line.
pub fn issuer_name(issuer: Issuer) -> &'static str {
    issuer.as_str()
}
