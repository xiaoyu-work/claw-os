//! Skill curator — distil completed conversation traces into
//! draft skill manifests the user can review and accept.
//!
//! Library-only. Hermes' curator (~70KB Python) does both
//! analysis + LLM-driven authorship. Here we ship the
//! deterministic analysis half: scan the conversation for the
//! distinctive shape of a "this is a recurring task worth
//! capturing" moment and propose a [`SkillDraft`] with title,
//! description, and the set of tools that were actually used.
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
    let end = collapsed
        .find(|c: char| matches!(c, '.' | '?' | '!'))
        .map(|i| i + 1)
        .unwrap_or(collapsed.len().min(200));
    let s = &collapsed[..end.min(collapsed.len())];
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

#[cfg(test)]
mod tests {
    use super::*;

    fn user(content: &str, accept: bool) -> ConversationTurn {
        ConversationTurn {
            role: TurnRole::User,
            content: content.to_string(),
            tool_calls: vec![],
            user_acceptance: accept,
        }
    }

    fn assistant(content: &str, tools: Vec<&str>) -> ConversationTurn {
        ConversationTurn {
            role: TurnRole::Assistant,
            content: content.to_string(),
            tool_calls: tools.into_iter().map(String::from).collect(),
            user_acceptance: false,
        }
    }

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("PDF Extract Tool!"), "pdf-extract-tool");
        assert_eq!(slugify("  ___  "), "skill");
        assert_eq!(slugify(""), "skill");
        assert_eq!(slugify("HelloWorld"), "helloworld");
    }

    #[test]
    fn derive_title_caps_first_letter() {
        assert_eq!(derive_title("extract pdf metadata"), "Extract pdf metadata");
        assert_eq!(derive_title(""), "Untitled skill");
    }

    #[test]
    fn derive_title_truncates_long() {
        let long = "extract metadata from a really long pdf with many fields and stuff that goes on";
        let t = derive_title(long);
        assert!(t.ends_with('…'));
        assert!(t.chars().count() <= 62);
    }

    #[test]
    fn derive_description_first_sentence() {
        let s = derive_description("Extract metadata from PDFs.\nAlso compute checksums.");
        assert_eq!(s, "Extract metadata from PDFs.");
    }

    #[test]
    fn derive_description_empty_fallback() {
        assert_eq!(derive_description("   "), "(generated by curator)");
    }

    #[test]
    fn propose_rejects_too_few_assistant_turns() {
        let c = Curator::with_default_config();
        let turns = vec![user("do thing", true)];
        match c.propose(&turns) {
            CuratorOutcome::NotEnough { reason } => assert!(reason.contains("assistant turns")),
            other => panic!("expected NotEnough, got {other:?}"),
        }
    }

    #[test]
    fn propose_rejects_no_tool_use() {
        let c = Curator::with_default_config();
        let turns = vec![
            user("do thing", false),
            assistant("ok", vec![]),
            assistant("done", vec![]),
            user("thanks", true),
        ];
        match c.propose(&turns) {
            CuratorOutcome::NotEnough { reason } => assert!(reason.contains("distinct tools")),
            other => panic!("expected NotEnough, got {other:?}"),
        }
    }

    #[test]
    fn propose_rejects_without_user_acceptance() {
        let c = Curator::with_default_config();
        let turns = vec![
            user("extract metadata from pdf", false),
            assistant("ok", vec!["cos_fs"]),
            assistant("done", vec!["cos_fs"]),
        ];
        match c.propose(&turns) {
            CuratorOutcome::NotEnough { reason } => assert!(reason.contains("user-acceptance")),
            other => panic!("expected NotEnough, got {other:?}"),
        }
    }

    #[test]
    fn propose_emits_draft_when_thresholds_met() {
        let c = Curator::with_default_config();
        let turns = vec![
            user("extract metadata from this pdf file", false),
            assistant("checking", vec!["cos_fs"]),
            assistant("here it is: ...", vec!["cos_fs"]),
            user("perfect, thanks", true),
        ];
        match c.propose(&turns) {
            CuratorOutcome::Drafted(d) => {
                assert_eq!(d.suggested_id, "extract-metadata-from-this-pdf-file");
                assert_eq!(d.title, "Extract metadata from this pdf file");
                assert!(d.description.starts_with("extract metadata"));
                assert_eq!(d.allowed_tools, vec!["cos_fs".to_string()]);
                assert_eq!(d.turns_used, 4);
                // 1 distinct tool + accepted -> Medium.
                assert_eq!(d.confidence, SkillConfidence::Medium);
            }
            other => panic!("expected Drafted, got {other:?}"),
        }
    }

    #[test]
    fn high_confidence_requires_two_tools_and_four_assistants_and_acceptance() {
        let c = Curator::with_default_config();
        let turns = vec![
            user("research topic X", false),
            assistant("scan", vec!["cos_web"]),
            assistant("read", vec!["cos_fs"]),
            assistant("synth", vec!["cos_fs"]),
            assistant("write", vec!["cos_fs"]),
            user("perfect", true),
        ];
        match c.propose(&turns) {
            CuratorOutcome::Drafted(d) => assert_eq!(d.confidence, SkillConfidence::High),
            other => panic!("expected Drafted, got {other:?}"),
        }
    }

    #[test]
    fn config_can_disable_acceptance_requirement() {
        let c = Curator::new(CuratorConfig {
            require_user_acceptance: false,
            ..CuratorConfig::default()
        });
        let turns = vec![
            user("extract", false),
            assistant("ok", vec!["cos_fs"]),
            assistant("done", vec!["cos_fs"]),
        ];
        assert!(matches!(c.propose(&turns), CuratorOutcome::Drafted(_)));
    }

    #[test]
    fn max_turns_caps_inspection_window() {
        let c = Curator::new(CuratorConfig {
            max_turns: 3,
            min_assistant_turns: 1,
            min_distinct_tools: 1,
            require_user_acceptance: false,
        });
        // First user message in the full slice is "first" but max_turns=3
        // so curator only looks at the last 3 turns.
        let turns = vec![
            user("first earlier message", false),
            user("second earlier message", false),
            user("recent task message", false),
            assistant("ok", vec!["cos_fs"]),
            assistant("done", vec!["cos_fs"]),
        ];
        match c.propose(&turns) {
            CuratorOutcome::Drafted(d) => {
                assert_eq!(d.turns_used, 3);
                assert!(d.title.starts_with("Recent task"));
            }
            other => panic!("expected Drafted, got {other:?}"),
        }
    }

    #[test]
    fn distinct_tools_dedupe_across_turns() {
        let c = Curator::with_default_config();
        let turns = vec![
            user("do work", false),
            assistant("a", vec!["cos_fs", "cos_fs"]),
            assistant("b", vec!["cos_fs"]),
            user("ok", true),
        ];
        if let CuratorOutcome::Drafted(d) = c.propose(&turns) {
            assert_eq!(d.allowed_tools.len(), 1);
        } else {
            panic!("expected Drafted");
        }
    }
}
