//! A single capability and bags of capabilities.
//!
//! A [`Cap`] is `(verb, scope)`. A [`CapSet`] is the set of caps a
//! particular session, role, or grant carries. Every gated operation
//! in the OS becomes one question: "does this session's CapSet cover
//! the cap this action requires?"

use std::collections::BTreeSet;

use super::scope::Scope;
use super::verb::Verb;

/// One capability the holder may exercise.
#[derive(Clone, Debug, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Cap {
    pub verb: Verb,
    pub scope: Scope,
}

impl Cap {
    pub fn new(verb: Verb, scope: Scope) -> Self {
        Self { verb, scope }
    }

    /// Quick constructor for verbs that don't take a scope (e.g.
    /// `ui.notify`). The scope is set to [`Scope::Wild`] so cover
    /// checks behave correctly.
    pub fn unscoped(verb: Verb) -> Self {
        Self {
            verb,
            scope: Scope::Wild,
        }
    }

    /// Does the holder's cap (`self`) cover a requested action?
    /// Same verb + scope cover.
    pub fn covers(&self, requested: &Cap) -> bool {
        self.verb == requested.verb && self.scope.covers(&requested.scope)
    }
}

// ---------------------------------------------------------------------------
// CapSet
// ---------------------------------------------------------------------------

/// A set of capabilities. Order is not significant; duplicates are
/// elided on insertion. Serialised as a flat array for compactness.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct CapSet {
    caps: Vec<Cap>,
}

impl CapSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_caps(caps: impl IntoIterator<Item = Cap>) -> Self {
        let mut set = Self::new();
        for c in caps {
            set.insert(c);
        }
        set
    }

    /// Add one cap. No-op if an equal cap is already present.
    pub fn insert(&mut self, cap: Cap) {
        if !self.caps.contains(&cap) {
            self.caps.push(cap);
        }
    }

    pub fn extend(&mut self, caps: impl IntoIterator<Item = Cap>) {
        for c in caps {
            self.insert(c);
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &Cap> {
        self.caps.iter()
    }

    pub fn len(&self) -> usize {
        self.caps.len()
    }

    pub fn is_empty(&self) -> bool {
        self.caps.is_empty()
    }

    /// True if any cap in this set covers `requested`.
    pub fn covers(&self, requested: &Cap) -> bool {
        self.caps.iter().any(|c| c.covers(requested))
    }

    /// True if every cap in `other` is covered by some cap in `self`.
    /// Used for child-agent derivation: `parent.covers_all(&child)`.
    pub fn covers_all(&self, other: &CapSet) -> bool {
        other.caps.iter().all(|c| self.covers(c))
    }

    /// Return the subset of `requested` that this CapSet covers. Used
    /// when spawning a child agent: the parent's `intersect` yields
    /// exactly the caps the child is actually allowed to take.
    pub fn intersect(&self, requested: &CapSet) -> CapSet {
        let kept: Vec<Cap> = requested
            .caps
            .iter()
            .filter(|c| self.covers(c))
            .cloned()
            .collect();
        CapSet::from_caps(kept)
    }

    /// Verbs that appear in this set (deduplicated, in insertion order).
    /// Useful for renderers that group caps by verb.
    pub fn verbs(&self) -> Vec<Verb> {
        let mut seen: BTreeSet<&'static str> = BTreeSet::new();
        let mut out = Vec::new();
        for c in &self.caps {
            if seen.insert(c.verb.as_str()) {
                out.push(c.verb);
            }
        }
        out
    }
}

impl FromIterator<Cap> for CapSet {
    fn from_iter<T: IntoIterator<Item = Cap>>(iter: T) -> Self {
        CapSet::from_caps(iter)
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/caps/cap.rs"
    ));
}
