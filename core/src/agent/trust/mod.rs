//! Immutable trust provenance for everything a model can see.
//!
//! # Why
//!
//! Claw's agent is kernel-resident: a model turn can reach processes,
//! credentials, the policy engine and the desktop through gated `cos_*`
//! tools. Its context is assembled from sources with wildly different
//! authority — the compiled operator scaffold, the owner's message,
//! `MEMORY.md`, a Skill's catalogue entry, an MCP server's tool
//! description, a fetched web page, the model's own prior text. Before
//! this module they arrived as `String`s and were distinguishable only
//! by chat role, and chat role is transport, not trust: providers
//! expose `system`/`user`/`assistant`/`tool` and nothing else, so
//! user-controlled memory and third-party extension metadata were being
//! concatenated into the same `system` string as operator policy.
//!
//! # What this is
//!
//! * [`TrustClass`] — a closed, ordered lattice of *producer authority*,
//!   independent of chat role.
//! * [`SourceKind`] — the closed registry of every way bytes reach a
//!   model request. Each kind declares its class, persistence,
//!   provider projection and audit strategy in one exhaustive `match`,
//!   so a new model-visible source cannot compile without declaring
//!   provenance.
//! * [`SourceRef`] — kind plus a bounded, secret-safe locator.
//! * [`LabeledSegment`] / [`ModelInput`] — the boundary types prompt
//!   assembly and the runtime hand to a provider request. Every
//!   transformation on them takes the least-trusted class of its
//!   inputs.
//! * [`envelope`] — the bounded, per-request-sealed data fence used
//!   wherever a provider has no per-segment metadata field, which is
//!   every provider Claw supports.
//! * [`authority`] — the type wall stating that none of the above
//!   decides anything.
//!
//! # Threat statement (honest version)
//!
//! This module **does not** detect prompt injection, and a label does
//! **not** make model output trustworthy. A malicious web page or MCP
//! server can still persuade the model to propose any text or any tool
//! call it likes. What labelling buys is narrower and checkable:
//!
//! * untrusted bytes cannot enter the immutable policy channel;
//! * untrusted bytes cannot gain trust by being concatenated,
//!   summarised, stored, replayed or re-serialised;
//! * untrusted bytes cannot forge the fence around themselves, so they
//!   cannot impersonate a neighbouring trusted segment;
//! * every model-visible byte is reconstructable from audit provenance.
//!
//! The security boundary is still capabilities, guardrails, approvals
//! and the sandbox. Those never read a label, and a model that ignores
//! every marker in this module gains nothing by doing so.

pub mod authority;
pub mod class;
pub mod envelope;
pub mod projection;
pub mod segment;
pub mod source;

pub use authority::{authority_of, Evidence, NoAuthority};
pub use class::TrustClass;
pub use envelope::{Seal, MAX_ENVELOPE_BYTES, MAX_SEGMENT_BYTES};
pub use projection::PromptProjection;
pub use segment::{LabeledSegment, ModelInput, SegmentManifestEntry};
pub use source::{AuditStrategy, Persistence, Projection, SourceKind, SourceProfile, SourceRef};

/// Project a source kind onto the journal's coarser provenance
/// vocabulary.
///
/// The journal predates this registry and keeps a three-value
/// `Trust`. Mapping is deliberately lossy *downwards*: only
/// [`TrustClass::SystemPolicy`] and [`TrustClass::UserInstruction`]
/// record as `Trusted`, so a widening of the lattice can never widen a
/// journal record.
pub fn journal_trust(class: TrustClass) -> crate::session::journal::Trust {
    use crate::session::journal::Trust;
    match class {
        TrustClass::SystemPolicy | TrustClass::UserInstruction => Trust::Trusted,
        TrustClass::LegacyUnknown => Trust::Unknown,
        _ => Trust::Untrusted,
    }
}

/// Project a source kind onto the journal's `SegmentKind`.
pub fn journal_segment_kind(kind: SourceKind) -> crate::session::journal::SegmentKind {
    use crate::session::journal::SegmentKind;
    match kind {
        SourceKind::SystemScaffold | SourceKind::RootOperatorPolicyFile => {
            SegmentKind::SystemPrompt
        }
        SourceKind::UserProfileNotes
        | SourceKind::MemoryNotes
        | SourceKind::AppMemory
        | SourceKind::OperatorPromptFile => SegmentKind::Memory,
        SourceKind::RecalledMemory => SegmentKind::Recall,
        SourceKind::SkillCatalogMetadata
        | SourceKind::SkillInstructions
        | SourceKind::SkillResource => SegmentKind::Skill,
        SourceKind::BuiltinToolResult
        | SourceKind::AppToolResult
        | SourceKind::McpToolResult
        | SourceKind::WebPageContent
        | SourceKind::BuiltinToolMetadata
        | SourceKind::AppToolMetadata
        | SourceKind::McpToolMetadata => SegmentKind::ToolResult,
        SourceKind::MediaTranscript | SourceKind::UserReference => SegmentKind::Attachment,
        SourceKind::ModelCompressionSummary => SegmentKind::Compression,
        _ => SegmentKind::SystemContext,
    }
}

/// Project a source kind onto the journal's `Origin`.
pub fn journal_origin(kind: SourceKind) -> crate::session::journal::Origin {
    use crate::session::journal::Origin;
    match kind {
        SourceKind::UserMessage | SourceKind::UserReference => Origin::User,
        SourceKind::ModelResponse
        | SourceKind::ModelCompressionSummary
        | SourceKind::ModelReasoning => Origin::Model,
        SourceKind::AppToolResult
        | SourceKind::AppToolMetadata
        | SourceKind::AppMemory
        | SourceKind::TransientAppContext => Origin::App,
        SourceKind::BuiltinToolResult
        | SourceKind::McpToolResult
        | SourceKind::McpToolMetadata
        | SourceKind::BuiltinToolMetadata
        | SourceKind::WebPageContent
        | SourceKind::MediaTranscript
        | SourceKind::HookOutput => Origin::Tool,
        _ => Origin::System,
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/trust/mod.rs"
    ));
}

#[cfg(test)]
mod adversarial_tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/trust/adversarial.rs"
    ));
}

#[cfg(test)]
mod projection_tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/trust/projection.rs"
    ));
}

#[cfg(test)]
mod builder_tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/trust/builders.rs"
    ));
}

#[cfg(test)]
mod migration_tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/trust/migration.rs"
    ));
}

#[cfg(test)]
mod ingestion_tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/trust/ingestion.rs"
    ));
}

#[cfg(test)]
mod policy_source_tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/trust/policy_source.rs"
    ));
}

#[cfg(test)]
mod compatibility_tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/trust/compatibility.rs"
    ));
}
