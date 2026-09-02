//! [`LabeledSegment`] and [`ModelInput`] — the labelled boundary types
//! prompt assembly and the runtime hand to a provider request.
//!
//! A segment owns three things that always travel together: the bytes,
//! the [`SourceRef`] that produced them, and the [`TrustClass`] that
//! source confers. There is no setter for the class. Every operation
//! that changes the bytes — concatenation, summarisation, truncation,
//! replay — goes through a method here, and every one of those methods
//! takes the least-trusted class of its inputs.

use super::class::TrustClass;
use super::envelope::{self, Seal};
use super::source::{Projection, SourceKind, SourceRef};

/// One model-visible chunk with immutable provenance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LabeledSegment {
    source: SourceRef,
    class: TrustClass,
    lineage: Vec<SourceKind>,
    content: String,
}

impl LabeledSegment {
    /// Label `content` with the class its `source` confers.
    ///
    /// This is the only way to mint a segment at
    /// [`TrustClass::SystemPolicy`] or [`TrustClass::UserInstruction`],
    /// and it needs a [`SourceRef`], which only a trusted ingestion
    /// adapter constructs.
    pub fn new(source: SourceRef, content: impl Into<String>) -> Self {
        let class = source.class();
        Self {
            lineage: vec![source.kind()],
            source,
            class,
            content: content.into(),
        }
    }

    /// Label `content` by kind alone.
    pub fn of(kind: SourceKind, content: impl Into<String>) -> Self {
        Self::new(SourceRef::new(kind), content)
    }

    /// Label `content` by kind plus a bounded, secret-safe locator.
    pub fn from_locator(kind: SourceKind, locator: &str, content: impl Into<String>) -> Self {
        Self::new(SourceRef::with_locator(kind, locator), content)
    }

    /// Recover a segment from bytes.
    ///
    /// A well-formed envelope yields its declared (clamped) label; any
    /// other bytes yield [`SourceKind::LegacyStoredRow`] at
    /// [`TrustClass::LegacyUnknown`]. Nothing here can produce a class
    /// above [`TrustClass::parse_ceiling`].
    pub fn from_stored(content: &str) -> Self {
        match envelope::parse(content) {
            Some(parsed) => Self {
                lineage: vec![parsed.source.kind()],
                source: parsed.source,
                class: parsed.class,
                content: parsed.payload,
            },
            None => Self {
                source: SourceRef::new(SourceKind::LegacyStoredRow),
                class: TrustClass::LegacyUnknown,
                lineage: vec![SourceKind::LegacyStoredRow],
                content: content.to_string(),
            },
        }
    }

    pub fn source(&self) -> &SourceRef {
        &self.source
    }

    pub const fn class(&self) -> TrustClass {
        self.class
    }

    pub fn kind(&self) -> SourceKind {
        self.source.kind()
    }

    pub fn content(&self) -> &str {
        &self.content
    }

    /// Distinct sources that contributed bytes, oldest first.
    pub fn lineage(&self) -> &[SourceKind] {
        &self.lineage
    }

    pub fn is_empty(&self) -> bool {
        self.content.trim().is_empty()
    }

    /// Content address of the exact bytes, for audit rows that must not
    /// hold the bytes themselves.
    pub fn digest(&self) -> String {
        crate::crypto::sha256_hex(self.content.as_bytes())
    }

    pub fn byte_len(&self) -> usize {
        self.content.len()
    }

    /// Append `other`, taking the least-trusted class and merging
    /// lineage. Trust cannot rise across a concatenation.
    pub fn concat(mut self, other: &Self) -> Self {
        self.class = self.class.least(other.class);
        for kind in &other.lineage {
            if !self.lineage.contains(kind) {
                self.lineage.push(*kind);
            }
        }
        if !self.content.is_empty() && !other.content.is_empty() {
            self.content.push_str("\n\n");
        }
        self.content.push_str(&other.content);
        self
    }

    /// Replace the bytes with model-authored text standing in for this
    /// segment — a compression summary, a rewrite, a paraphrase.
    ///
    /// The result is always [`SourceKind::ModelCompressionSummary`] at
    /// [`TrustClass::ModelGenerated`] or lower, and keeps the lineage of
    /// everything it replaced.
    pub fn into_model_summary(self, summary: impl Into<String>) -> Self {
        let mut lineage = self.lineage;
        if !lineage.contains(&SourceKind::ModelCompressionSummary) {
            lineage.insert(0, SourceKind::ModelCompressionSummary);
        }
        Self {
            source: SourceRef::new(SourceKind::ModelCompressionSummary),
            class: self
                .class
                .least(SourceKind::ModelCompressionSummary.class()),
            lineage,
            content: summary.into(),
        }
    }

    /// Truncate to at most `max_bytes`, never raising trust.
    pub fn bounded(mut self, max_bytes: usize) -> Self {
        if self.content.len() <= max_bytes {
            return self;
        }
        let mut end = max_bytes;
        while end > 0 && !self.content.is_char_boundary(end) {
            end -= 1;
        }
        self.content.truncate(end);
        self
    }

    /// Secret-safe provenance row for audit and session diagnostics.
    pub fn manifest_entry(&self) -> SegmentManifestEntry {
        SegmentManifestEntry {
            source: self.source.label(),
            class: self.class,
            bytes: self.byte_len(),
            digest: self.digest(),
            lineage: self
                .lineage
                .iter()
                .map(|kind| kind.tag().to_string())
                .collect(),
        }
    }

    /// Render for a model request under `seal`.
    ///
    /// Policy is emitted verbatim because it *is* the instruction
    /// channel; the owner's own turn text is emitted verbatim because
    /// fencing it would hide the request from the model. Everything
    /// else is fenced.
    pub fn render(&self, seal: &Seal) -> String {
        match self.source.kind().projection() {
            Projection::PolicyChannel | Projection::UserChannelVerbatim => self.content.clone(),
            Projection::AssistantChannel => self.content.clone(),
            _ => envelope::render(seal, &self.source, self.class, &self.content),
        }
    }

    /// Render fenced regardless of the source's normal projection.
    ///
    /// Used where a segment is being placed in a channel more
    /// authoritative than its own, which must never happen unfenced.
    pub fn render_fenced(&self, seal: &Seal) -> String {
        envelope::render(seal, &self.source, self.class, &self.content)
    }
}

/// The ordered, labelled segments of one model request.
///
/// `ModelInput` is what prompt assembly produces and what the runtime
/// projects into a provider request. Its invariant — asserted by
/// [`ModelInput::policy_text`] — is that only
/// [`TrustClass::SystemPolicy`] segments reach the policy channel.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModelInput {
    segments: Vec<LabeledSegment>,
}

impl ModelInput {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, segment: LabeledSegment) {
        if segment.is_empty() {
            return;
        }
        self.segments.push(segment);
    }

    pub fn with(mut self, segment: LabeledSegment) -> Self {
        self.push(segment);
        self
    }

    pub fn segments(&self) -> &[LabeledSegment] {
        &self.segments
    }

    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    pub fn len(&self) -> usize {
        self.segments.len()
    }

    /// Least-trusted class across every segment.
    pub fn effective_class(&self) -> TrustClass {
        TrustClass::least_of(self.segments.iter().map(LabeledSegment::class))
    }

    /// The immutable policy channel text.
    ///
    /// Deprecated shape kept only for callers that hold a flat
    /// `ModelInput`. Prefer [`super::PromptProjection`], which cannot
    /// place a non-policy segment in the policy channel at all. This
    /// function fences a non-policy segment rather than trusting it,
    /// but fencing inside `system` is weaker than not being there.
    pub fn policy_text(&self, seal: &Seal) -> String {
        let mut out = String::new();
        for segment in &self.segments {
            let rendered = if segment.class().is_policy() {
                segment.content().to_string()
            } else {
                segment.render_fenced(seal)
            };
            if rendered.trim().is_empty() {
                continue;
            }
            if !out.is_empty() {
                out.push_str("\n\n---\n\n");
            }
            out.push_str(&rendered);
        }
        out
    }

    /// Split this input into the typed request channels.
    pub fn project(&self) -> super::PromptProjection {
        let mut projection = super::PromptProjection::new();
        projection.extend_prelude(self.segments.iter().cloned());
        projection
    }

    /// Render every segment for a non-policy channel.
    pub fn channel_text(&self, seal: &Seal) -> String {
        let mut out = String::new();
        for segment in &self.segments {
            let rendered = segment.render(seal);
            if rendered.trim().is_empty() {
                continue;
            }
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            out.push_str(&rendered);
        }
        out
    }

    /// Fold the whole input into one segment, taking the least-trusted
    /// class. Used where a downstream API accepts only a single string.
    pub fn collapse(&self) -> Option<LabeledSegment> {
        let mut iter = self.segments.iter();
        let first = iter.next()?.clone();
        Some(iter.fold(first, |acc, next| acc.concat(next)))
    }

    /// Secret-safe provenance rows for audit and session diagnostics.
    pub fn manifest(&self) -> Vec<SegmentManifestEntry> {
        self.segments
            .iter()
            .map(LabeledSegment::manifest_entry)
            .collect()
    }
}

impl FromIterator<LabeledSegment> for ModelInput {
    fn from_iter<T: IntoIterator<Item = LabeledSegment>>(iter: T) -> Self {
        let mut input = Self::new();
        for segment in iter {
            input.push(segment);
        }
        input
    }
}

/// One row of inspectable provenance: what the model saw, from where,
/// at what class, addressed by digest rather than by content.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct SegmentManifestEntry {
    pub source: String,
    pub class: TrustClass,
    pub bytes: usize,
    pub digest: String,
    pub lineage: Vec<String>,
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/trust/segment.rs"
    ));
}
