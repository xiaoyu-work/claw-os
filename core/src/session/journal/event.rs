//! The versioned event schema the journal is authoritative for.
//!
//! Every variant is closed: each field is a bounded scalar, an
//! enumerated label, a count, or a reference type defined in this
//! module. There is deliberately no `serde_json::Value` field, no free
//! `String` and no byte payload, so nothing model-authored, no
//! credential and no raw broker parameter can reach a root-owned record
//! by riding inside an event.
//!
//! Two reference shapes carry everything that cannot be stored:
//!
//! * [`ContentRef`] — an immutable, content-addressed pointer into an
//!   owner-private store. It names bytes the owner already holds; it is
//!   not authority, cannot be presented anywhere, and reading it
//!   requires the owner's own permissions.
//! * [`TextDigest`](crate::audit_policy::TextDigest) — length plus a
//!   per-process keyed digest, for text that may never be stored at
//!   all (error messages, tool input, provider failures).
//!
//! Replaying any of these events creates nothing.
//! [`JournalEvent::CapabilityIssued`] and
//! [`JournalEvent::ApprovalDecided`] are records *about* decisions the
//! capability authority and the approvals store already took and still
//! own; the journal holds only their keyed references and outcomes.

use serde::{Deserialize, Deserializer, Serialize};

use crate::audit_policy::{self, TextDigest};

/// Schema version stamped on every record and bound into its MAC.
///
/// A reader that does not know a version refuses the record rather
/// than guessing at its meaning.
pub const SCHEMA_VERSION: u32 = 1;

/// Longest serialized event body accepted by the writer.
pub const MAX_EVENT_BYTES: usize = 4 * 1024;

// ---------------------------------------------------------------------------
// Bounded scalars
// ---------------------------------------------------------------------------

/// A short selector: 1..=64 bytes of `[A-Za-z0-9._-]`.
///
/// Constructing one always succeeds — an unusable value becomes
/// [`audit_policy::UNLOGGABLE`] — so a caller cannot make an event
/// unrepresentable, and deserializing one rejects anything that would
/// not have been accepted on the way in.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct Label(String);

impl Label {
    pub fn new(value: &str) -> Self {
        Self(audit_policy::safe_identity(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Label {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Label {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        if raw == audit_policy::UNLOGGABLE || audit_policy::is_token(&raw) {
            Ok(Self(raw))
        } else {
            Err(serde::de::Error::custom("journal label is not a token"))
        }
    }
}

/// A reference that legitimately carries separators: a session name, a
/// canonical capability scope, a grant reference, an approval id.
/// 1..=256 bytes of `[A-Za-z0-9._:/@+~-]`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct Reference(String);

impl Reference {
    pub fn new(value: &str) -> Self {
        Self(audit_policy::safe_reference(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Reference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Reference {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        if raw == audit_policy::UNLOGGABLE || audit_policy::is_identifier(&raw) {
            Ok(Self(raw))
        } else {
            Err(serde::de::Error::custom(
                "journal reference is not an identifier",
            ))
        }
    }
}

/// A lowercase-hex SHA-256 digest.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct Digest(String);

impl Digest {
    /// Content address of `bytes`.
    pub fn of(bytes: &[u8]) -> Self {
        Self(crate::crypto::sha256_hex(bytes))
    }

    /// Accept a digest another subsystem already computed.
    pub fn parse(value: &str) -> Option<Self> {
        is_sha256_hex(value).then(|| Self(value.to_ascii_lowercase()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).ok_or_else(|| serde::de::Error::custom("not a sha-256 digest"))
    }
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// The operation identity a mutation bracket is opened and closed
/// under. Minted by the broker, never taken from a caller.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct OperationId(String);

impl OperationId {
    pub fn generate() -> Self {
        Self(uuid::Uuid::new_v4().simple().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for OperationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for OperationId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        if audit_policy::is_token(&raw) {
            Ok(Self(raw))
        } else {
            Err(serde::de::Error::custom("operation id is not a token"))
        }
    }
}

// ---------------------------------------------------------------------------
// References
// ---------------------------------------------------------------------------

/// Which owner-private store holds the bytes a [`ContentRef`] names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ContentStore {
    /// `<session>/turns.jsonl` and the conversation bodies beside it.
    SessionTurns,
    /// The rollback payload store, `<session>/files/inverse/`.
    SessionInverse,
    /// The owner's conversation memory database and its index.
    OwnerMemory,
    /// The prompt snapshot the runtime assembled for one turn.
    PromptSnapshot,
}

/// An immutable, content-addressed pointer into an owner-private store.
///
/// This is evidence, not capability: it says "bytes with this digest,
/// this many of them, in that store". Resolving it needs the owner's
/// own access to the store, and holding the reference grants none of
/// that access.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentRef {
    pub store: ContentStore,
    pub digest: Digest,
    pub bytes: u64,
}

impl ContentRef {
    pub fn new(store: ContentStore, digest: Digest, bytes: u64) -> Self {
        Self {
            store,
            digest,
            bytes,
        }
    }

    /// Content-address bytes the caller already holds.
    pub fn of(store: ContentStore, bytes: &[u8]) -> Self {
        Self {
            store,
            digest: Digest::of(bytes),
            bytes: bytes.len() as u64,
        }
    }
}

/// Who produced the content or action an event describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Origin {
    User,
    Model,
    Tool,
    App,
    Provider,
    System,
}

/// How much the producer of a segment is trusted by the runtime.
///
/// This is the journal's own provenance metadata. It is not a policy:
/// nothing here decides what a model may do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Trust {
    /// Authored by the local system or the account that owns the
    /// session.
    Trusted,
    /// Reached the context from outside: tool output, fetched content,
    /// App payloads, model text replayed as context.
    Untrusted,
    /// Provenance was not established.
    Unknown,
}

// ---------------------------------------------------------------------------
// Enumerated outcomes
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionOutcome {
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CancelSource {
    User,
    Owner,
    Daemon,
    Budget,
    Deadline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApprovalOutcome {
    Approved,
    Denied,
    Expired,
    Revoked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GrantEnd {
    /// The grant's use budget reached zero.
    UsesExhausted,
    /// The grant's deadline passed.
    Expired,
    /// The process the grant was bound to is gone.
    ProcessGone,
    /// An explicit revocation retired it.
    Revoked,
}

/// Which prompt segment a model-visible injection came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SegmentKind {
    SystemPrompt,
    Memory,
    Recall,
    Skill,
    SystemContext,
    ToolResult,
    Attachment,
    Compression,
}

/// Why a bracketed mutation cannot be reported as either committed or
/// failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Indeterminate {
    /// The handler ran and the completion record could not be
    /// committed, so the durable effect is unknown to the journal.
    CompletionUnrecorded,
    /// A previous daemon opened the bracket and never closed it.
    WriterLost,
}

/// What an operator concluded about a mutation the machine could not
/// resolve on its own.
///
/// This is a *statement*, not an action: recording it re-runs nothing,
/// rolls back nothing, and grants nothing. It exists so an unresolved
/// bracket stops refusing its own replay only when a human has said
/// what actually happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Resolution {
    /// The effect is abandoned. Nothing was or will be re-run.
    Abandoned,
    /// The operator verified the effect landed.
    Committed,
    /// The operator verified the effect was undone.
    RolledBack,
}

/// How an orphaned mutation was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RecoverySource {
    DaemonStart,
    SessionResume,
}

/// Why a partition was archived or trimmed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RetentionReason {
    SizeRotation,
    RetentionWindow,
    SessionArchived,
}

// ---------------------------------------------------------------------------
// The event
// ---------------------------------------------------------------------------

/// One authoritative lifecycle fact.
///
/// Internally tagged so the stored line names its own kind, matching
/// the other durable enums in this crate. Every field below is bounded
/// by construction; see the module docs for why there is no escape
/// hatch variant.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JournalEvent {
    // ---- session lifecycle -------------------------------------------------
    SessionStarted {
        owner_uid: u32,
        origin: Origin,
        /// The session's typed provenance marker, when it has one.
        delegation: Option<Label>,
        parent: Option<Reference>,
    },
    SessionResumed {
        owner_uid: u32,
        /// Writer epoch the resume ran under, so a reader can tell two
        /// daemon lifetimes apart without trusting timestamps.
        writer_epoch: u64,
    },
    SessionCompleted {
        outcome: SessionOutcome,
        turns: u32,
        mutations: u32,
    },
    SessionFailed {
        class: Label,
        detail: TextDigest,
    },
    SessionCancelled {
        by: CancelSource,
        turn: Option<u32>,
    },

    // ---- request / response references -------------------------------------
    UserRequestRecorded {
        turn: u32,
        content: ContentRef,
        origin: Origin,
        trust: Trust,
    },
    ModelResponseRecorded {
        turn: u32,
        content: ContentRef,
        origin: Origin,
        /// Label the runtime that produced the turn gave itself.
        runtime: Option<Label>,
        tool_calls: u32,
    },
    /// The provider-facing side of a turn: what ran, how it ended, and
    /// what it cost. Carries no content — the reference above does.
    ModelTurnCompleted {
        turn: u32,
        provider: Label,
        model: Label,
        success: bool,
        latency_ms: u64,
        input_tokens: u32,
        output_tokens: u32,
        tool_calls: u32,
        stop_reason: Label,
        error: Option<TextDigest>,
    },

    // ---- prompt composition ------------------------------------------------
    PromptSnapshotRecorded {
        turn: u32,
        snapshot: ContentRef,
        segments: u32,
    },
    PromptSegmentInjected {
        turn: u32,
        segment: ContentRef,
        segment_kind: SegmentKind,
        origin: Origin,
        trust: Trust,
    },

    // ---- tools -------------------------------------------------------------
    ToolProposed {
        turn: u32,
        tool: Label,
        tool_use_id: Label,
        known: bool,
        input: TextDigest,
    },
    ToolStarted {
        turn: u32,
        tool: Label,
        tool_use_id: Label,
        known: bool,
    },
    ToolFinished {
        turn: u32,
        tool: Label,
        tool_use_id: Label,
        known: bool,
        success: bool,
        latency_ms: u64,
        bytes_returned: u64,
        error: Option<TextDigest>,
    },

    // ---- capability authority references -----------------------------------
    CapabilityIssued {
        grant: Reference,
        audience: Label,
        issuer: Label,
        caps: u32,
        uses: Option<u32>,
    },
    CapabilityUsed {
        grant: Reference,
        route: Label,
        caps: u32,
        uses_remaining: Option<u32>,
    },
    CapabilityExhausted {
        grant: Reference,
        reason: GrantEnd,
    },
    CapabilityRevoked {
        grant: Reference,
        reason: GrantEnd,
        generation: u32,
    },

    // ---- approvals ---------------------------------------------------------
    ApprovalRequested {
        approval: Reference,
        verb: Label,
        scope: Reference,
    },
    ApprovalDecided {
        approval: Reference,
        verb: Label,
        outcome: ApprovalOutcome,
        generation: u32,
    },
    ApprovalConsumed {
        approval: Option<Reference>,
        verb: Label,
        scope: Reference,
        generation: u32,
    },

    // ---- durable mutations -------------------------------------------------
    MutationStarted {
        operation: OperationId,
        route: Label,
        /// Keyed digest of the request/idempotency key the broker
        /// derived. Correlates a retry with its original; reverses to
        /// nothing and cannot be presented as authority.
        idempotency: TextDigest,
        /// Keyed reference of the capability grant the route ran under.
        /// Never a handle.
        grant: Option<Reference>,
        /// Mutation seq in the session's own rollback log, when the
        /// operation recorded one.
        session_mutation: Option<u64>,
    },
    MutationCommitted {
        operation: OperationId,
        duration_ms: u64,
    },
    MutationFailed {
        operation: OperationId,
        duration_ms: u64,
        class: Label,
        error: TextDigest,
    },
    MutationIndeterminate {
        operation: OperationId,
        reason: Indeterminate,
    },
    /// An operator said what happened to a mutation the machine could
    /// not resolve. The only event that retires an unresolved bracket.
    MutationResolved {
        operation: OperationId,
        outcome: Resolution,
        /// `uid:<n>` of the root principal that decided. Never a name
        /// the caller chose.
        decided_by: Reference,
    },

    // ---- recovery ----------------------------------------------------------
    MutationOrphaned {
        operation: OperationId,
        route: Label,
        detected_by: RecoverySource,
        /// Writer epoch that opened the bracket and never closed it.
        opened_in_epoch: u64,
    },
    RecoveryScanned {
        detected_by: RecoverySource,
        writer_epoch: u64,
        events: u64,
        orphans: u32,
    },
    RetentionApplied {
        reason: RetentionReason,
        /// Lowest sequence still present in the live partition.
        retained_from_seq: u64,
        /// Content address of the archived segment, so a trimmed chain
        /// still names the bytes that used to close the gap.
        archive: Option<ContentRef>,
    },
}

impl JournalEvent {
    /// Stable name recorded in the MAC and used by the ACL.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::SessionStarted { .. } => "session_started",
            Self::SessionResumed { .. } => "session_resumed",
            Self::SessionCompleted { .. } => "session_completed",
            Self::SessionFailed { .. } => "session_failed",
            Self::SessionCancelled { .. } => "session_cancelled",
            Self::UserRequestRecorded { .. } => "user_request_recorded",
            Self::ModelResponseRecorded { .. } => "model_response_recorded",
            Self::ModelTurnCompleted { .. } => "model_turn_completed",
            Self::PromptSnapshotRecorded { .. } => "prompt_snapshot_recorded",
            Self::PromptSegmentInjected { .. } => "prompt_segment_injected",
            Self::ToolProposed { .. } => "tool_proposed",
            Self::ToolStarted { .. } => "tool_started",
            Self::ToolFinished { .. } => "tool_finished",
            Self::CapabilityIssued { .. } => "capability_issued",
            Self::CapabilityUsed { .. } => "capability_used",
            Self::CapabilityExhausted { .. } => "capability_exhausted",
            Self::CapabilityRevoked { .. } => "capability_revoked",
            Self::ApprovalRequested { .. } => "approval_requested",
            Self::ApprovalDecided { .. } => "approval_decided",
            Self::ApprovalConsumed { .. } => "approval_consumed",
            Self::MutationStarted { .. } => "mutation_started",
            Self::MutationCommitted { .. } => "mutation_committed",
            Self::MutationFailed { .. } => "mutation_failed",
            Self::MutationIndeterminate { .. } => "mutation_indeterminate",
            Self::MutationResolved { .. } => "mutation_resolved",
            Self::MutationOrphaned { .. } => "mutation_orphaned",
            Self::RecoveryScanned { .. } => "recovery_scanned",
            Self::RetentionApplied { .. } => "retention_applied",
        }
    }

    /// The operation a mutation bracket event belongs to.
    pub fn operation(&self) -> Option<&OperationId> {
        match self {
            Self::MutationStarted { operation, .. }
            | Self::MutationCommitted { operation, .. }
            | Self::MutationFailed { operation, .. }
            | Self::MutationIndeterminate { operation, .. }
            | Self::MutationResolved { operation, .. }
            | Self::MutationOrphaned { operation, .. } => Some(operation),
            _ => None,
        }
    }

    /// Whether this event opens a bracket.
    pub fn opens_mutation(&self) -> bool {
        matches!(self, Self::MutationStarted { .. })
    }

    /// Whether this event *retires* a bracket.
    ///
    /// `MutationIndeterminate` and `MutationOrphaned` deliberately do
    /// not: they say the outcome is unknown, which is the state a
    /// replay must keep being refused in. Only a definite outcome or an
    /// operator's [`Resolution`] retires the bracket.
    pub fn resolves_mutation(&self) -> bool {
        matches!(
            self,
            Self::MutationCommitted { .. }
                | Self::MutationFailed { .. }
                | Self::MutationResolved { .. }
        )
    }

    /// Whether this event flags a bracket as unresolved.
    pub fn flags_mutation(&self) -> bool {
        matches!(
            self,
            Self::MutationIndeterminate { .. } | Self::MutationOrphaned { .. }
        )
    }

    /// Whether this record is one the reserve exists to guarantee.
    ///
    /// These are the records that close, flag or recover a durable
    /// mutation, plus the minimal lifecycle closure of a session.
    /// Nothing here is driven by a model, a peer or a tool: their
    /// volume is bounded by the other classes so that these always fit.
    pub fn is_closure(&self) -> bool {
        matches!(
            self,
            Self::MutationCommitted { .. }
                | Self::MutationFailed { .. }
                | Self::MutationIndeterminate { .. }
                | Self::MutationResolved { .. }
                | Self::MutationOrphaned { .. }
                | Self::RecoveryScanned { .. }
                | Self::RetentionApplied { .. }
                | Self::SessionCompleted { .. }
                | Self::SessionFailed { .. }
                | Self::SessionCancelled { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/session/journal/event.rs"
    ));
}
