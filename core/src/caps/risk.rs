//! Risk levels and their UI affordances.
//!
//! Every capability in the catalog carries a [`Risk`] rating. The
//! authorization dialog aggregates risks across requested caps and
//! displays the worst one prominently — `Critical` items render with
//! emphasis and require explicit per-item confirmation, while `Low`
//! items are listed without fanfare.

use crate::i18n::LocalizedStr;

/// Coarse-grained "how bad can it get if misused" rating.
///
/// `Ord` is implemented so `risks.iter().max()` returns the overall
/// risk of a set of caps.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Risk {
    /// Routine; mention in passing.
    Low,
    /// Worth a glance.
    Medium,
    /// Visually emphasised in approval dialogs.
    High,
    /// Demands per-item confirmation; can never be batched into "allow all".
    Critical,
}

impl Risk {
    /// Stable lowercase label for persistence and audit records.
    pub fn as_str(self) -> &'static str {
        match self {
            Risk::Low => "low",
            Risk::Medium => "medium",
            Risk::High => "high",
            Risk::Critical => "critical",
        }
    }

    /// Localised one-word label (e.g. "Low risk", "Critical").
    pub fn label(self) -> LocalizedStr {
        match self {
            Risk::Low => LocalizedStr::new("Low risk"),
            Risk::Medium => LocalizedStr::new("Medium risk"),
            Risk::High => LocalizedStr::new("High risk"),
            Risk::Critical => LocalizedStr::new("Critical risk"),
        }
    }

    /// Single-character / single-emoji UI badge.
    pub fn badge(self) -> &'static str {
        match self {
            Risk::Low => "·",
            Risk::Medium => "•",
            Risk::High => "▲",
            Risk::Critical => "⚠",
        }
    }

    /// ANSI-friendly colour name. UIs without colour can ignore.
    pub fn color(self) -> &'static str {
        match self {
            Risk::Low => "green",
            Risk::Medium => "yellow",
            Risk::High => "orange",
            Risk::Critical => "red",
        }
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/caps/risk.rs"
    ));
}
