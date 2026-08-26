//! Skill curator — distil completed conversation traces into
//! draft skill manifests the user can review and accept.
//!
//! Library-only. Ships the deterministic analysis half: scan the
//! conversation for the distinctive shape of a "this is a
//! recurring task worth capturing" moment and propose a
//! [`SkillDraft`] with title, description, and the set of tools
//! that were actually used.
//!
//! Downstream, the runtime can hand the draft to an LLM for body
//! authorship or surface it to the user verbatim. A full LLM
//! authorship pass lands in a follow-up commit.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConversationTurn {
    pub role: TurnRole,
    pub content: String,
    /// Tool calls made during this turn (assistant role).
    #[serde(default)]
    pub tool_calls: Vec<String>,
    /// True if this turn was the user's confirmation of a
    /// successful outcome ("thanks", "great", "exactly what I
    /// wanted", etc.). Caller-supplied so we don't sprinkle a
    /// sentiment heuristic here.
    #[serde(default)]
    pub user_acceptance: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TurnRole {
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SkillDraft {
    pub suggested_id: String,
    pub title: String,
    pub description: String,
    pub allowed_tools: Vec<String>,
    pub turns_used: usize,
    pub confidence: SkillConfidence,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SkillConfidence {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone)]
pub struct CuratorConfig {
    /// Minimum number of assistant turns before we'll consider
    /// distilling. Below this the conversation is too short to
    /// teach a recurring pattern.
    pub min_assistant_turns: usize,
    /// Minimum number of distinct tools used. A skill with no
    /// tool usage is just an answer; we ignore those.
    pub min_distinct_tools: usize,
    /// Require an explicit `user_acceptance == true` turn before
    /// proposing. Set false for offline retros.
    pub require_user_acceptance: bool,
    /// Hard cap on conversation turns we'll inspect. Long sessions
    /// usually contain multiple unrelated tasks; truncate from the
    /// tail.
    pub max_turns: usize,
}

impl Default for CuratorConfig {
    fn default() -> Self {
        Self {
            min_assistant_turns: 2,
            min_distinct_tools: 1,
            require_user_acceptance: true,
            max_turns: 80,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum CuratorOutcome {
    Drafted(SkillDraft),
    NotEnough { reason: String },
}

pub struct Curator {
    config: CuratorConfig,
}

impl Curator {
    pub fn new(config: CuratorConfig) -> Self {
        Self { config }
    }

    pub fn with_default_config() -> Self {
        Self::new(CuratorConfig::default())
    }

    /// Inspect a conversation and either propose a SkillDraft or
    /// report why one couldn't be derived. Pure function — no IO.
    pub fn propose(&self, turns: &[ConversationTurn]) -> CuratorOutcome {
        let trimmed: &[ConversationTurn] = if turns.len() > self.config.max_turns {
            &turns[turns.len() - self.config.max_turns..]
        } else {
            turns
        };

        let assistant_turns: usize = trimmed
            .iter()
            .filter(|t| t.role == TurnRole::Assistant)
            .count();
        if assistant_turns < self.config.min_assistant_turns {
            return CuratorOutcome::NotEnough {
                reason: format!(
                    "only {} assistant turns (min {})",
                    assistant_turns, self.config.min_assistant_turns
                ),
            };
        }

        let mut tools: BTreeSet<String> = BTreeSet::new();
        for t in trimmed {
            for tc in &t.tool_calls {
                tools.insert(tc.clone());
            }
        }
        if tools.len() < self.config.min_distinct_tools {
            return CuratorOutcome::NotEnough {
                reason: format!(
                    "only {} distinct tools used (min {})",
                    tools.len(),
                    self.config.min_distinct_tools
                ),
            };
        }

        let accepted = trimmed.iter().any(|t| t.user_acceptance);
        if self.config.require_user_acceptance && !accepted {
            return CuratorOutcome::NotEnough {
                reason: "no user-acceptance turn observed".to_string(),
            };
        }

        let first_user_msg = trimmed
            .iter()
            .find(|t| t.role == TurnRole::User)
            .map(|t| t.content.as_str())
            .unwrap_or("");
        let title = derive_title(first_user_msg);
        let suggested_id = slugify(&title);
        let description = derive_description(first_user_msg);

        let confidence = if accepted && tools.len() >= 2 && assistant_turns >= 4 {
            SkillConfidence::High
        } else if accepted || tools.len() >= 2 {
            SkillConfidence::Medium
        } else {
            SkillConfidence::Low
        };

        CuratorOutcome::Drafted(SkillDraft {
            suggested_id,
            title,
            description,
            allowed_tools: tools.into_iter().collect(),
            turns_used: trimmed.len(),
            confidence,
        })
    }
}

/// Derive a 1-line title from the first user message. Truncates
/// at 60 chars on a word boundary; capitalises the first letter.
pub fn derive_title(user_msg: &str) -> String {
    let cleaned = user_msg
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .trim_matches(|c: char| matches!(c, '"' | '\''));
    if cleaned.is_empty() {
        return "Untitled skill".to_string();
    }
    let truncated = if cleaned.chars().count() <= 60 {
        cleaned.to_string()
    } else {
        let prefix: String = cleaned.chars().take(60).collect();
        match prefix.rfind(' ') {
            Some(idx) if idx > 20 => format!("{}…", &prefix[..idx]),
            _ => format!("{prefix}…"),
        }
    };
    let mut chars = truncated.chars();
    match chars.next() {
        Some(c) => format!("{}{}", c.to_uppercase(), chars.as_str()),
        None => "Untitled skill".to_string(),
    }
}

/// First sentence (or first 200 chars), normalised.
pub fn derive_description(user_msg: &str) -> String {
    let collapsed = user_msg.replace(['\r', '\n'], " ");
    let collapsed = collapsed.trim();
    if collapsed.is_empty() {
        return "(generated by curator)".to_string();
    }
    // `find` returns a byte index; `collapsed.len().min(200)` is also a
    // byte budget. Both can land in the middle of a multi-byte UTF-8
    // codepoint (CJK, emoji, accented Latin), so walk back to the next
    // char boundary before slicing — otherwise non-ASCII inputs that
    // lack sentence terminators panic with `byte index N is not a char
    // boundary`. See agent::util::char_safe_truncate.
    let end = collapsed
        .find(['.', '?', '!'])
        .map(|i| i + 1)
        .unwrap_or(collapsed.len().min(200));
    let s = crate::agent::util::char_safe_truncate(collapsed, end);
    s.trim().to_string()
}

/// Slugify a title into a kebab-case skill id. ASCII-only.
/// Non-alphanumeric runs collapse to a single `-`.
pub fn slugify(title: &str) -> String {
    let mut out = String::new();
    let mut last_dash = true;
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            for d in c.to_lowercase() {
                out.push(d);
            }
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "skill".to_string()
    } else {
        out
    }
}

/// Extract tool-call names from a stored assistant message body.
///
/// `render_message_content` (memory/sqlite_fts.rs) writes tool_use
/// blocks as `[tool_use:NAME] {json}` lines. This parser walks each
/// line of the stored content and pulls the NAME out of every such
/// header. Returns an empty Vec when no tool calls are present.
///
/// Lossy by design: we don't try to recover the JSON `input` blob —
/// the curator only needs distinct tool names. This means schema
/// migration is optional: existing memory.db rows already contain
/// enough information to feed Curator::propose.
pub fn extract_tool_calls_from_content(content: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix("[tool_use:") else {
            continue;
        };
        let Some(end) = rest.find(']') else {
            continue;
        };
        let name = rest[..end].trim();
        if !name.is_empty() {
            out.push(name.to_string());
        }
    }
    out
}

/// Map a stored memory message into a ConversationTurn for the
/// curator. Tool-result messages collapse into `TurnRole::Tool` with
/// no tool_calls (those came from the *previous* assistant message).
/// `user_acceptance` is left to the caller to set — sentiment
/// detection isn't this module's job.
pub fn message_to_turn(role: &str, content: &str) -> Option<ConversationTurn> {
    let content = crate::agent::memory::history::sanitize_stored_content(role, content);
    let role = match role {
        "user" => TurnRole::User,
        "assistant" => TurnRole::Assistant,
        "tool" => TurnRole::Tool,
        // System and unknown roles are not curated.
        _ => return None,
    };
    let tool_calls = if matches!(role, TurnRole::Assistant) {
        extract_tool_calls_from_content(&content)
    } else {
        Vec::new()
    };
    Some(ConversationTurn {
        role,
        content,
        tool_calls,
        user_acceptance: false,
    })
}

/// Naive sentiment heuristic: returns true when `content` reads as
/// a user acceptance ("thanks", "perfect", "exactly", etc.). Used
/// when the runtime hasn't supplied an explicit acceptance signal.
/// English-only; conservative to avoid false positives.
pub fn looks_like_acceptance(content: &str) -> bool {
    let lower = content.to_lowercase();
    let trimmed = lower.trim();
    if trimmed.is_empty() {
        return false;
    }
    // Multi-word phrases scanned against the full body.
    const PHRASES: &[&str] = &[
        "exactly what i wanted",
        "exactly what i needed",
        "works perfectly",
        "that worked",
        "that's it",
        "thats it",
        "great work",
        "nice work",
    ];
    if PHRASES.iter().any(|p| trimmed.contains(p)) {
        return true;
    }
    // Standalone single-word reactions (allow trailing punctuation).
    let stripped = trimmed.trim_end_matches(['.', '!', '?', ',', ' ']);
    matches!(
        stripped,
        "thanks" | "thank you" | "perfect" | "great" | "awesome" | "amazing"
    )
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/curator.rs"
    ));
}
