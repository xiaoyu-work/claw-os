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
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/llm/metadata.rs"
    ));
}
