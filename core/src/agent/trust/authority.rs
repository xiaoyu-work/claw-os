//! The type wall between provenance and authority.
//!
//! A [`TrustClass`] is evidence about where bytes came from. It is not
//! a capability, not a role, not an approval, and not a policy
//! decision. This module exists so that separation is expressed in the
//! type system rather than in a comment:
//!
//! * [`Evidence`] is sealed and implemented only for [`TrustClass`],
//!   [`SourceKind`] and [`LabeledSegment`]. It has no method that
//!   returns anything a policy check consumes.
//! * [`NoAuthority`] is the uninhabitable result of asking a segment
//!   for authority. It carries no data and has no constructor outside
//!   this module, so nothing can pattern-match its way to a grant.
//! * There is deliberately no `From`/`TryFrom`/`Into` between anything
//!   in [`super`] and `caps`, `policy`, the approvals store or the tool
//!   registry. `trust_class_confers_no_authority` in the unit tests
//!   scans the crate for one and fails if it appears.
//!
//! What *does* decide is unchanged: `crate::caps` for capability
//! scopes, the guarded tool registry and guardrails for tool exposure,
//! the approvals store for user decisions, and `clawd` for privileged
//! operations. Each of those takes a typed kernel or user decision.
//! None of them takes a segment, a label, or model text.

use super::class::TrustClass;
use super::segment::LabeledSegment;
use super::source::SourceKind;

mod sealed {
    pub trait Sealed {}
    impl Sealed for super::TrustClass {}
    impl Sealed for super::SourceKind {}
    impl Sealed for super::LabeledSegment {}
}

/// Marker for values that describe provenance and nothing else.
///
/// Implementors are audit evidence. The trait is sealed so no
/// downstream type can claim to be evidence, and it exposes only a
/// display label so no caller can mistake it for a decision.
pub trait Evidence: sealed::Sealed {
    /// A bounded, secret-safe label for logs and diagnostics.
    fn evidence_label(&self) -> String;
}

impl Evidence for TrustClass {
    fn evidence_label(&self) -> String {
        self.wire_tag().to_string()
    }
}

impl Evidence for SourceKind {
    fn evidence_label(&self) -> String {
        self.tag().to_string()
    }
}

impl Evidence for LabeledSegment {
    fn evidence_label(&self) -> String {
        format!("{}@{}", self.source().label(), self.class().wire_tag())
    }
}

/// The result of asking model-visible content for authority.
///
/// It has no fields, no public constructor and no methods that produce
/// a capability, scope, role or approval. Calling
/// [`authority_of`] is the honest way to write "this input decides
/// nothing" at a call site that might otherwise be tempted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoAuthority(());

/// Model-visible content confers no authority, whatever its label.
///
/// Even [`TrustClass::SystemPolicy`] returns [`NoAuthority`]: policy
/// text tells the model what the operator wants, while the capability
/// authority decides what actually runs.
pub const fn authority_of<E: Evidence>(_evidence: &E) -> NoAuthority {
    NoAuthority(())
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/trust/authority.rs"
    ));
}
