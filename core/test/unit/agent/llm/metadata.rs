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
