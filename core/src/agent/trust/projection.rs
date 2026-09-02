//! The typed request prelude: which labelled segment goes in which
//! provider channel.
//!
//! Requirement: the immutable policy channel must hold *only* operator
//! policy. Fencing user-controlled notes, Skill metadata or tool output
//! inside the `system` string is not enough — a provider that treats
//! `system` as "the rules" now has attacker-influenced bytes sitting in
//! the rules, and every provider that merges system content merges them
//! together.
//!
//! [`PromptProjection`] is therefore a three-part shape:
//!
//! | Part | Class | Channel |
//! | --- | --- | --- |
//! | `policy` | [`TrustClass::SystemPolicy`] only | `system` / `developer`, verbatim |
//! | `prelude` | everything else | bounded `user` data messages, fenced |
//! | `instruction` | [`TrustClass::UserInstruction`] | `user`, verbatim, last |
//!
//! Order is preserved inside the prelude, so lineage reads the same way
//! it was assembled. A provider that has no `developer` role may merge
//! policy with policy; it may never merge policy with a prelude
//! segment, because the two never share a message.

use crate::agent::llm::{ContentBlock, Message, Role};

use super::class::TrustClass;
use super::envelope::Seal;
use super::segment::{LabeledSegment, SegmentManifestEntry};
use super::source::Projection;

/// One request's segments, split by the channel they are allowed to
/// occupy.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PromptProjection {
    policy: Vec<LabeledSegment>,
    prelude: Vec<LabeledSegment>,
    instruction: Option<LabeledSegment>,
}

impl PromptProjection {
    pub fn new() -> Self {
        Self::default()
    }

    /// Route `segment` to its channel by class, not by caller intent.
    ///
    /// A segment that is not [`TrustClass::SystemPolicy`] cannot reach
    /// the policy channel however it is pushed, and a policy segment
    /// cannot be demoted into the prelude by a mislabelled call site.
    pub fn push(&mut self, segment: LabeledSegment) {
        if segment.is_empty() {
            return;
        }
        match segment.class() {
            TrustClass::SystemPolicy => self.policy.push(segment),
            TrustClass::UserInstruction => self.instruction = Some(segment),
            _ => self.prelude.push(segment),
        }
    }

    pub fn with(mut self, segment: LabeledSegment) -> Self {
        self.push(segment);
        self
    }

    /// Extend the prelude, preserving order.
    pub fn extend_prelude(&mut self, segments: impl IntoIterator<Item = LabeledSegment>) {
        for segment in segments {
            self.push(segment);
        }
    }

    /// Replace the policy channel with a restored or newly frozen
    /// snapshot.
    ///
    /// The snapshot is the session's content-addressed policy text,
    /// which by construction only ever held
    /// [`TrustClass::SystemPolicy`] segments, so it re-enters as one
    /// [`SourceKind::SystemScaffold`](super::SourceKind::SystemScaffold)
    /// segment. A snapshot written by an older prompt version is
    /// refused upstream by the version gate rather than trusted here.
    pub fn replace_policy(&mut self, frozen: String) {
        self.policy = if frozen.trim().is_empty() {
            Vec::new()
        } else {
            vec![LabeledSegment::of(
                super::SourceKind::SystemScaffold,
                frozen,
            )]
        };
    }

    pub fn policy_segments(&self) -> &[LabeledSegment] {
        &self.policy
    }

    pub fn prelude_segments(&self) -> &[LabeledSegment] {
        &self.prelude
    }

    pub fn instruction_segment(&self) -> Option<&LabeledSegment> {
        self.instruction.as_ref()
    }

    /// The provider's `system` / `developer` content.
    ///
    /// Only policy segments are joined here, and they are joined
    /// verbatim because policy *is* the instruction channel. The
    /// function cannot emit anything else: non-policy segments never
    /// reach `self.policy`.
    pub fn system_text(&self) -> String {
        let mut out = String::new();
        for segment in &self.policy {
            debug_assert!(
                segment.class().is_policy(),
                "non-policy segment reached the policy channel: {}",
                segment.source()
            );
            if !out.is_empty() {
                out.push_str("\n\n---\n\n");
            }
            out.push_str(segment.content());
        }
        out
    }

    /// The bounded data messages that precede the owner's turn.
    ///
    /// One message per segment, each fenced with its own source and
    /// class, so a long or hostile segment cannot displace or merge
    /// with its neighbour. Segments whose registry projection is a tool
    /// channel are still emitted in the user channel here: this is the
    /// request *prelude*, not a reply to a tool call, and inventing a
    /// `tool` message without a matching `tool_use` id would break
    /// every provider's correlation contract.
    pub fn prelude_messages(&self, seal: &Seal) -> Vec<Message> {
        self.prelude
            .iter()
            .map(|segment| Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: segment.render_fenced(seal),
                }],
            })
            .collect()
    }

    /// The owner's own turn, verbatim, in the user channel.
    pub fn instruction_message(&self) -> Option<Message> {
        self.instruction
            .as_ref()
            .map(|segment| Message::user_text(segment.content()))
    }

    /// Prelude then instruction, in request order.
    pub fn request_messages(&self, seal: &Seal) -> Vec<Message> {
        let mut messages = self.prelude_messages(seal);
        messages.extend(self.instruction_message());
        messages
    }

    /// Least-trusted class across every segment in the request.
    pub fn effective_class(&self) -> TrustClass {
        TrustClass::least_of(
            self.policy
                .iter()
                .chain(&self.prelude)
                .chain(self.instruction.as_ref())
                .map(LabeledSegment::class),
        )
    }

    /// Secret-safe provenance rows for audit and session diagnostics.
    pub fn manifest(&self) -> Vec<SegmentManifestEntry> {
        self.policy
            .iter()
            .chain(&self.prelude)
            .chain(self.instruction.as_ref())
            .map(LabeledSegment::manifest_entry)
            .collect()
    }

    /// Invariant check used by tests and debug builds: the policy
    /// channel holds only policy, and the prelude holds no policy.
    pub fn channels_are_separated(&self) -> bool {
        self.policy.iter().all(|s| s.class().is_policy())
            && self.prelude.iter().all(|s| !s.class().is_policy())
            && self
                .instruction
                .as_ref()
                .is_none_or(|s| s.class() == TrustClass::UserInstruction)
    }
}

/// Whether a registry projection is allowed to reach the policy
/// channel. Exposed so the registry coverage test and the runtime
/// agree on one rule.
pub const fn reaches_policy_channel(projection: Projection) -> bool {
    matches!(projection, Projection::PolicyChannel)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/trust/projection_shape.rs"
    ));
}
