//! The closed trust lattice every model-visible segment is labelled with.
//!
//! A [`TrustClass`] answers one question: *how much authority did the
//! producer of these bytes already hold before they were written?* It
//! deliberately answers nothing about what the bytes say. Nothing in
//! this module inspects content, and no label makes model output safe.
//!
//! The class is **not** the chat role. A provider may only have
//! `system`/`user`/`assistant`/`tool` channels; the runtime still has to
//! know that a `MEMORY.md` note replayed in the system channel is
//! user-controlled context, and that an MCP tool description carried in
//! the same request is third-party extension metadata.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// How much authority the producer of a model-visible segment held.
///
/// Ordered from least to most authoritative. Concatenation always takes
/// the minimum (see [`TrustClass::least`]), so a transformation can
/// never raise trust.
///
/// # Construction
///
/// Values are produced by [`super::SourceKind::class`] — that is, by a
/// trusted ingestion adapter naming the source it is reading from.
/// Parsing text or JSON can only ever yield a class at or below
/// [`TrustClass::ModelGenerated`]: see [`TrustClass::from_stored_label`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum TrustClass {
    /// A stored row, an unrecognised source, or a segment that predates
    /// labelling. Treated as the most restrictive class there is.
    #[default]
    LegacyUnknown,
    /// Web pages, MCP results, App results, built-in tool output — any
    /// bytes an attacker who controls a third party can choose.
    UntrustedExternalContent,
    /// Text this or another model produced: assistant turns replayed as
    /// context, compression summaries, reasoning summaries.
    ModelGenerated,
    /// Metadata a packaged extension declares about itself: Skill
    /// catalogue entries, MCP tool names/descriptions/schemas, App tool
    /// descriptions. A signature authenticates *who published it*, never
    /// that its text is safe, so signed metadata stays here.
    ExtensionMetadata,
    /// Durable context the session owner controls but did not type in
    /// this turn: `USER.md`, `MEMORY.md`, recalled memory, nudges,
    /// per-session extras.
    UserControlledContext,
    /// What the authenticated session owner asked for in this turn.
    UserInstruction,
    /// Operator policy: the compiled scaffold and the operator-configured
    /// system prompt file. The only class the policy channel accepts.
    SystemPolicy,
}

impl TrustClass {
    /// Every class, least authoritative first.
    pub const ALL: &'static [TrustClass] = &[
        TrustClass::LegacyUnknown,
        TrustClass::UntrustedExternalContent,
        TrustClass::ModelGenerated,
        TrustClass::ExtensionMetadata,
        TrustClass::UserControlledContext,
        TrustClass::UserInstruction,
        TrustClass::SystemPolicy,
    ];

    /// Stable wire tag used in envelopes, audit rows and journal labels.
    ///
    /// The exhaustive match makes a new class fail to compile until it
    /// declares a tag.
    pub const fn wire_tag(self) -> &'static str {
        match self {
            Self::LegacyUnknown => "legacy-unknown",
            Self::UntrustedExternalContent => "untrusted-external",
            Self::ModelGenerated => "model-generated",
            Self::ExtensionMetadata => "extension-metadata",
            Self::UserControlledContext => "user-context",
            Self::UserInstruction => "user-instruction",
            Self::SystemPolicy => "system-policy",
        }
    }

    /// Position in the lattice; higher means more authoritative.
    pub const fn rank(self) -> u8 {
        match self {
            Self::LegacyUnknown => 0,
            Self::UntrustedExternalContent => 1,
            Self::ModelGenerated => 2,
            Self::ExtensionMetadata => 3,
            Self::UserControlledContext => 4,
            Self::UserInstruction => 5,
            Self::SystemPolicy => 6,
        }
    }

    /// The only class the immutable policy channel accepts.
    ///
    /// This is a *placement* rule for prompt assembly, not an
    /// authorization decision: see [`super::authority`].
    pub const fn is_policy(self) -> bool {
        matches!(self, Self::SystemPolicy)
    }

    /// Whether the runtime must present this segment inside a bounded
    /// data envelope rather than as free prose.
    pub const fn requires_envelope(self) -> bool {
        !self.is_policy()
    }

    /// Least-trusted of two classes. Concatenating, summarising,
    /// truncating or replaying content combines with this, so the result
    /// can never outrank its most restrictive input.
    pub const fn least(self, other: Self) -> Self {
        if self.rank() <= other.rank() {
            self
        } else {
            other
        }
    }

    /// Least-trusted class across an iterator. An empty iterator is
    /// [`TrustClass::LegacyUnknown`] — absence of evidence is not
    /// evidence of trust.
    pub fn least_of(classes: impl IntoIterator<Item = TrustClass>) -> Self {
        classes
            .into_iter()
            .fold(None::<TrustClass>, |acc, next| {
                Some(match acc {
                    Some(current) => current.least(next),
                    None => next,
                })
            })
            .unwrap_or(TrustClass::LegacyUnknown)
    }

    /// Ceiling any label recovered from bytes is clamped to.
    ///
    /// Stored rows, envelope markers and provider payloads are all
    /// *content*. Content may describe itself as user-controlled context
    /// or worse; it may never describe itself as an instruction the
    /// owner typed, and it may never describe itself as policy.
    pub const fn parse_ceiling() -> Self {
        Self::UserControlledContext
    }

    /// Recover a class from a persisted or in-band label.
    ///
    /// Unknown, missing, or over-privileged labels become
    /// [`TrustClass::LegacyUnknown`]. This is the only path by which a
    /// byte string turns into a class, and by construction it can never
    /// return [`TrustClass::SystemPolicy`] or
    /// [`TrustClass::UserInstruction`].
    pub fn from_stored_label(raw: &str) -> Self {
        let parsed = Self::ALL
            .iter()
            .copied()
            .find(|class| class.wire_tag() == raw)
            .unwrap_or(Self::LegacyUnknown);
        if parsed.rank() > Self::parse_ceiling().rank() {
            Self::LegacyUnknown
        } else {
            parsed
        }
    }
}

impl std::fmt::Display for TrustClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.wire_tag())
    }
}

impl Serialize for TrustClass {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.wire_tag())
    }
}

/// Deserialization is a *parse*, so it goes through the same clamp as
/// any other byte source. A crafted payload cannot deserialize itself
/// into policy.
impl<'de> Deserialize<'de> for TrustClass {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Ok(Self::from_stored_label(&raw))
    }
}
