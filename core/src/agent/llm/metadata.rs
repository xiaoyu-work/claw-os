//! Static metadata table for known LLM models.
//!
//! Centralised lookup so the runtime can size context windows
//! correctly and tools layers can check capability gates (vision /
//! tool-use) before sending requests the model can't honour.
//!
//! ## Lookup semantics
//!
//! [`lookup`] is the canonical entry point. It performs three passes,
//! in order:
//!
//!   1. Exact case-insensitive match on `name`.
//!   2. Prefix match (`<provider>/<model>` → strip provider prefix).
//!   3. `model` suffix match (e.g. registry uses `claude-sonnet-4-5`,
//!      caller passes `anthropic/claude-sonnet-4-5-20250929` → match
//!      on shared prefix).
//!
//! Returns `None` when nothing matches. Callers should treat unknown
//! models as having no metadata, **not** fall back to a default.
//!
//! Everything is `const` and lives in the binary — zero allocation,
//! zero file IO. There is intentionally no pricing data here: the
//! kernel measures AI usage in tokens only, never in dollars.


#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelMetadata {
    pub name: &'static str,
    pub provider: &'static str,
    pub context_window: u32,
    pub max_output_tokens: u32,
    pub supports_tools: bool,
    pub supports_vision: bool,
    pub supports_streaming: bool,
}

/// Known-models table. Keep alphabetised by `(provider, name)` to
/// keep diffs sane.
const TABLE: &[ModelMetadata] = &[
    // ---------- Anthropic ----------
    ModelMetadata {
        name: "claude-haiku-4-5",
        provider: "anthropic",
        context_window: 200_000,
        max_output_tokens: 8_192,
        supports_tools: true,
        supports_vision: true,
        supports_streaming: true,
    },
    ModelMetadata {
        name: "claude-opus-4-5",
        provider: "anthropic",
        context_window: 200_000,
        max_output_tokens: 32_000,
        supports_tools: true,
        supports_vision: true,
        supports_streaming: true,
    },
    ModelMetadata {
        name: "claude-sonnet-4-5",
        provider: "anthropic",
        context_window: 200_000,
        max_output_tokens: 64_000,
        supports_tools: true,
        supports_vision: true,
        supports_streaming: true,
    },
    // ---------- DeepSeek ----------
    ModelMetadata {
        name: "deepseek-chat",
        provider: "deepseek",
        context_window: 64_000,
        max_output_tokens: 8_192,
        supports_tools: true,
        supports_vision: false,
        supports_streaming: true,
    },
    ModelMetadata {
        name: "deepseek-reasoner",
        provider: "deepseek",
        context_window: 64_000,
        max_output_tokens: 8_192,
        supports_tools: true,
        supports_vision: false,
        supports_streaming: true,
    },
    // ---------- Google Gemini ----------
    ModelMetadata {
        name: "gemini-1.5-pro",
        provider: "gemini",
        context_window: 2_000_000,
        max_output_tokens: 8_192,
        supports_tools: true,
        supports_vision: true,
        supports_streaming: true,
    },
    ModelMetadata {
        name: "gemini-2.0-flash",
        provider: "gemini",
        context_window: 1_000_000,
        max_output_tokens: 8_192,
        supports_tools: true,
        supports_vision: true,
        supports_streaming: true,
    },
    ModelMetadata {
        name: "gemini-2.5-pro",
        provider: "gemini",
        context_window: 2_000_000,
        max_output_tokens: 65_536,
        supports_tools: true,
        supports_vision: true,
        supports_streaming: true,
    },
    // ---------- Meta / local llama.cpp ----------
    ModelMetadata {
        name: "llama-3.3-70b-instruct",
        provider: "local",
        context_window: 131_072,
        max_output_tokens: 8_192,
        supports_tools: true,
        supports_vision: false,
        supports_streaming: true,
    },
    // ---------- Mistral ----------
    ModelMetadata {
        name: "mistral-large-2411",
        provider: "mistral",
        context_window: 131_072,
        max_output_tokens: 4_096,
        supports_tools: true,
        supports_vision: false,
        supports_streaming: true,
    },
    // ---------- OpenAI ----------
    ModelMetadata {
        name: "gpt-4.1",
        provider: "openai",
        context_window: 1_047_576,
        max_output_tokens: 32_768,
        supports_tools: true,
        supports_vision: true,
        supports_streaming: true,
    },
    ModelMetadata {
        name: "gpt-4o",
        provider: "openai",
        context_window: 128_000,
        max_output_tokens: 16_384,
        supports_tools: true,
        supports_vision: true,
        supports_streaming: true,
    },
    ModelMetadata {
        name: "gpt-5",
        provider: "openai",
        context_window: 400_000,
        max_output_tokens: 128_000,
        supports_tools: true,
        supports_vision: true,
        supports_streaming: true,
    },
    ModelMetadata {
        name: "o1",
        provider: "openai",
        context_window: 200_000,
        max_output_tokens: 100_000,
        supports_tools: true,
        supports_vision: true,
        supports_streaming: false,
    },
    // ---------- xAI ----------
    ModelMetadata {
        name: "grok-4",
        provider: "xai",
        context_window: 256_000,
        max_output_tokens: 16_384,
        supports_tools: true,
        supports_vision: true,
        supports_streaming: true,
    },
];

/// Look up metadata for a model by name.
///
/// Tries, in order:
///   1. Case-insensitive exact match on `name`.
///   2. If `name` is `<provider>/<rest>`, strip the prefix and recurse
///      on `rest` (also case-insensitive).
///   3. Suffix match: any registry entry whose `name` is a prefix of
///      the lookup name (e.g. registry `claude-sonnet-4-5`, lookup
///      `claude-sonnet-4-5-20250929` → match). Useful for dated
///      release tags.
pub fn lookup(name: &str) -> Option<&'static ModelMetadata> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();

    if let Some(hit) = TABLE.iter().find(|m| m.name.eq_ignore_ascii_case(&lower)) {
        return Some(hit);
    }

    if let Some(idx) = lower.find('/') {
        let rest = &lower[idx + 1..];
        if !rest.is_empty() {
            if let Some(hit) = TABLE.iter().find(|m| m.name.eq_ignore_ascii_case(rest)) {
                return Some(hit);
            }
        }
    }

    // Dated-release tag suffix match: registry name is a prefix of
    // the lookup, separated by `-` (so `claude-sonnet-4` does not
    // accidentally match `claude-sonnet-45-mini`).
    let bare = lower.rsplit_once('/').map_or(lower.as_str(), |(_, r)| r);
    let mut best: Option<&'static ModelMetadata> = None;
    for m in TABLE {
        let entry = m.name.to_ascii_lowercase();
        if bare.len() > entry.len()
            && bare.starts_with(&entry)
            && bare.as_bytes()[entry.len()] == b'-'
        {
            let take = match best {
                Some(b) => b.name.len() < m.name.len(),
                None => true,
            };
            if take {
                best = Some(m);
            }
        }
    }
    best
}

/// Return all registered models for a provider, alphabetically by
/// model name. Empty vec if provider unknown.
pub fn list_for_provider(provider: &str) -> Vec<&'static ModelMetadata> {
    let mut hits: Vec<_> = TABLE
        .iter()
        .filter(|m| m.provider.eq_ignore_ascii_case(provider))
        .collect();
    hits.sort_by_key(|m| m.name);
    hits
}

/// Distinct provider names known to the metadata table, alphabetised.
pub fn known_providers() -> Vec<&'static str> {
    let mut seen: Vec<&'static str> = Vec::new();
    for m in TABLE {
        if !seen.contains(&m.provider) {
            seen.push(m.provider);
        }
    }
    seen.sort_unstable();
    seen
}

/// Total entries in the metadata table.
pub fn entry_count() -> usize {
    TABLE.len()
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_lookup_hits() {
        let hit = lookup("claude-sonnet-4-5").expect("known model");
        assert_eq!(hit.provider, "anthropic");
        assert_eq!(hit.context_window, 200_000);
        assert!(hit.supports_tools);
        assert!(hit.supports_vision);
    }

    #[test]
    fn unknown_model_returns_none() {
        assert!(lookup("not-a-real-model").is_none());
    }

    #[test]
    fn empty_or_whitespace_returns_none() {
        assert!(lookup("").is_none());
        assert!(lookup("   ").is_none());
    }

    #[test]
    fn lookup_is_case_insensitive() {
        let canonical = lookup("gpt-4o").expect("hit");
        let upper = lookup("GPT-4o").expect("upper hit");
        let mixed = lookup("Gpt-4O").expect("mixed hit");
        assert_eq!(canonical.name, upper.name);
        assert_eq!(canonical.name, mixed.name);
    }

    #[test]
    fn provider_prefix_is_stripped() {
        let hit = lookup("openai/gpt-5").expect("with provider prefix");
        assert_eq!(hit.name, "gpt-5");
        assert_eq!(hit.provider, "openai");
    }

    #[test]
    fn provider_prefix_with_unknown_model_returns_none() {
        assert!(lookup("openai/totally-fictional-model").is_none());
    }

    #[test]
    fn dated_release_tag_suffix_match() {
        // Anthropic publishes dated tags like
        // claude-sonnet-4-5-20250929 — should resolve back to the
        // base entry.
        let hit = lookup("claude-sonnet-4-5-20250929").expect("dated suffix");
        assert_eq!(hit.name, "claude-sonnet-4-5");
    }

    #[test]
    fn dated_suffix_with_provider_prefix() {
        let hit = lookup("anthropic/claude-opus-4-5-20251115").expect("hit");
        assert_eq!(hit.name, "claude-opus-4-5");
    }

    #[test]
    fn suffix_match_does_not_cross_segment_boundary() {
        // Without the explicit `-` boundary check, this would match
        // anything starting with `claude`. Guard against that.
        assert!(lookup("claudefoo").is_none());
        assert!(lookup("claude-sonnet-4-5suffix").is_none());
    }

    #[test]
    fn suffix_match_picks_longest_prefix() {
        // No real conflict in the current table, but the algorithm
        // should still prefer the most specific entry. Use a synthetic
        // dated tag of an existing entry.
        let hit = lookup("gpt-4.1-2025-04-14").expect("hit");
        assert_eq!(hit.name, "gpt-4.1");
    }

    #[test]
    fn list_for_provider_alphabetical() {
        let openai = list_for_provider("openai");
        let names: Vec<&str> = openai.iter().map(|m| m.name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
        assert!(!openai.is_empty());
    }

    #[test]
    fn list_for_provider_case_insensitive() {
        let lower = list_for_provider("anthropic");
        let upper = list_for_provider("Anthropic");
        assert_eq!(lower.len(), upper.len());
    }

    #[test]
    fn list_for_provider_unknown_returns_empty() {
        assert!(list_for_provider("nope").is_empty());
    }

    #[test]
    fn known_providers_are_distinct_and_sorted() {
        let provs = known_providers();
        let mut sorted = provs.clone();
        sorted.sort_unstable();
        assert_eq!(provs, sorted);
        let mut deduped = provs.clone();
        deduped.dedup();
        assert_eq!(provs.len(), deduped.len());
        assert!(provs.contains(&"anthropic"));
        assert!(provs.contains(&"openai"));
    }

    #[test]
    fn entry_count_matches_table_length() {
        assert!(entry_count() >= 10, "table should cover the top-9+");
    }

    #[test]
    fn all_entries_have_nonzero_context_window() {
        for m in TABLE {
            assert!(m.context_window > 0, "{} has zero context_window", m.name);
            assert!(
                m.max_output_tokens > 0,
                "{} has zero max_output_tokens",
                m.name
            );
            assert!(
                m.max_output_tokens <= m.context_window,
                "{} max_output {} exceeds context {}",
                m.name,
                m.max_output_tokens,
                m.context_window
            );
        }
    }

    #[test]
    fn provider_field_matches_table_grouping() {
        // Sanity: provider strings are non-empty and lower-snake-ish.
        for m in TABLE {
            assert!(!m.provider.is_empty(), "{} has empty provider", m.name);
            assert_eq!(
                m.provider,
                m.provider.to_ascii_lowercase(),
                "{} provider should be lowercase",
                m.name
            );
        }
    }

    #[test]
    fn vision_capable_models_form_subset() {
        // Pure smoke: at least one model in each of the major
        // providers should support vision.
        for prov in ["anthropic", "openai", "gemini"] {
            let any_vision = list_for_provider(prov).iter().any(|m| m.supports_vision);
            assert!(any_vision, "{prov} should have at least one vision model");
        }
    }
}
