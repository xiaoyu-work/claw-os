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
    use super::*;

    #[test]
    fn ordering_lets_us_take_the_max() {
        let risks = [Risk::Low, Risk::Critical, Risk::Medium, Risk::High];
        assert_eq!(risks.iter().copied().max(), Some(Risk::Critical));
    }

    #[test]
    fn ord_is_total_and_intuitive() {
        assert!(Risk::Low < Risk::Medium);
        assert!(Risk::Medium < Risk::High);
        assert!(Risk::High < Risk::Critical);
    }

    #[test]
    fn label_returns_english_string() {
        assert_eq!(Risk::Critical.label().en(), "Critical risk");
    }

    #[test]
    fn serde_uses_lowercase_strings() {
        let v = serde_json::to_string(&Risk::High).unwrap();
        assert_eq!(v, "\"high\"");
        let back: Risk = serde_json::from_str("\"critical\"").unwrap();
        assert_eq!(back, Risk::Critical);
    }
}
