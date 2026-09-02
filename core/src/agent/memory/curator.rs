//! Memory curator — extract durable user facts from conversation
//! history and append them to `MEMORY.md` so the agent gradually
//! learns about its operator across sessions.
//!
//! ## Design
//!
//!   1. Pull the last N messages from the session's slice of the
//!      memory DB (or from a specific session id).
//!   2. Hand the transcript to an *auxiliary* LLM with a fixed
//!      system prompt that asks it to emit `<fact>` tags with a
//!      category + confidence + free-text.
//!   3. Parse the tags out of the response (forgivingly — we don't
//!      require perfect JSON).
//!   4. Normalize structured slots, classify their lifetime, attach
//!      provenance, then apply confidence + secret filtering.
//!   5. Dedupe against the current non-expired projection and append
//!      survivors to a `## Curated facts (auto)` section in
//!      `MEMORY.md`. New sessions pick them up in their frozen prompt snapshot;
//!      the current session retains the source conversation already in context.
//!   6. Update a curation log so we don't re-extract from already-
//!      seen messages on the next run.
//!
//! ## Why an *auxiliary* LLM
//!
//! Fact extraction is small, repetitive, and high-volume (potentially
//! once per session). Sending it through the user's flagship model
//! is wasteful. The auxiliary handle (see
//! `crate::agent::llm::auxiliary`) wraps a cheaper provider/model
//! pairing the user has explicitly configured for these subtasks,
//! and falls through to the primary if absent.
//!
//! ## Storage
//!
//! Curation log lives at the explicitly composed
//! `agent/memory/curation_log.json` path, owner-partitioned for routed jobs.
//! Schema:
//!
//! ```json
//! {
//!   "version": 1,
//!   "sessions": {
//!     "<session-id>": {
//!       "last_curated_message_id": 42,
//!       "last_run_unix_s": 1720000000,
//!       "facts_added_total": 7
//!     }
//!   }
//! }
//! ```
//!
//! Writes are atomic via `.tmp` + rename. Reads are lock-free; a
//! corrupt log is treated as "no prior runs" rather than a fatal
//! error so a botched human edit can't paralyse the curator.
//!
//! ## What we *don't* extract
//!
//! The system prompt explicitly excludes:
//!
//!   * Short-term task details ("user is debugging the X feature
//!     today") — those belong in session memory, not durable notes.
//!   * Procedures and runbooks — those belong in Skills.
//!   * Secrets, credentials, tokens — flagged for the model to skip
//!     and *also* filtered post-hoc by [`looks_secret`].
//!   * Information that's already in `MEMORY.md` — deduped at write
//!     time.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::agent::llm::auxiliary::AuxiliaryClient;
use crate::agent::memory::notes::{current_structured_values, NotesStore, MEMORY_FILE};
use crate::agent::memory::ontology::{self, FactLifetime};
use crate::agent::memory::sqlite_fts::{MemoryDb, MessageRow};

// =====================================================================
// Public types
// =====================================================================

/// Per-curator behavior knobs. The defaults are what the runtime
/// uses when invoking via the `cos agent learn` CLI without flags.
#[derive(Debug, Clone)]
pub struct CuratorConfig {
    /// Max messages to feed the LLM in one extraction pass.
    pub max_messages: usize,
    /// Cap how many facts we'll accept per pass. Hard guardrail
    /// against an LLM that decides to write a memoir.
    pub max_facts_per_run: usize,
    /// Drop facts whose declared confidence is below this. The LLM
    /// declares confidence on each `<fact>` tag.
    pub min_confidence: f32,
    /// Skip a session if its last curated message is the most-recent
    /// message we'd be feeding in. (Avoids running the LLM when
    /// nothing new has happened.)
    pub skip_if_no_new_messages: bool,
    /// Optional override of the system prompt. `None` uses the
    /// canonical embedded prompt.
    pub system_prompt: Option<String>,
}

impl Default for CuratorConfig {
    fn default() -> Self {
        Self {
            max_messages: 50,
            max_facts_per_run: 12,
            min_confidence: 0.6,
            skip_if_no_new_messages: true,
            system_prompt: None,
        }
    }
}

/// Logical bucket for a fact. We expose the canonical four as
/// known variants and let the LLM smuggle anything else through
/// `Other` so unanticipated categories don't get silently dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactCategory {
    Preference,
    Identity,
    Environment,
    Skill,
    /// A problem that was diagnosed and fixed. Durable and reusable:
    /// "debugging X today" is noise, but "X crashed because of Y, fix
    /// was Z" is worth more than a preference the next time Y bites.
    Resolution,
    Other(String),
}

impl FactCategory {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "preference" | "pref" => Self::Preference,
            "identity" | "id" => Self::Identity,
            "environment" | "env" => Self::Environment,
            "skill" | "skills" => Self::Skill,
            "resolution" | "fix" => Self::Resolution,
            other => Self::Other(other.to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Preference => "preference",
            Self::Identity => "identity",
            Self::Environment => "environment",
            Self::Skill => "skill",
            Self::Resolution => "resolution",
            Self::Other(s) => s.as_str(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExtractedFact {
    pub category: FactCategory,
    pub text: String,
    pub confidence: f32,
    /// What the fact is *about* (`editor`, `deploy`, `postgres`).
    pub entity: Option<String>,
    /// Which property of the entity (`name`, `region`, `cause`).
    pub attribute: Option<String>,
    /// The property's value (`helix`, `us-west-2`).
    pub value: Option<String>,
    /// Model-declared lifetime before policy, then canonical lifetime after
    /// normalization.
    pub lifetime: Option<FactLifetime>,
    /// Date on which the source message observed the fact (`YYYY-MM-DD`).
    pub observed_at: Option<String>,
    /// Bounded expiry for volatile observations.
    pub ttl_days: Option<u32>,
    /// Session containing the source message. Assigned by the curator, never
    /// trusted from model output.
    pub source_session_id: Option<String>,
    /// Message cited by the model and validated against the transcript, or the
    /// latest user message when the citation is absent or invalid.
    pub source_message_id: Option<i64>,
}

impl ExtractedFact {
    /// Identity of the *slot* this fact occupies: `entity.attribute`.
    ///
    /// Two facts sharing a key are successive states of one thing, so a
    /// later one supersedes an earlier one. Free-text facts cannot answer
    /// "is `editor = helix` replacing `editor = vim`, or are both true for
    /// different projects?" — a key can, which is why everything else in
    /// this module depends on it.
    pub fn key(&self) -> Option<String> {
        match (&self.entity, &self.attribute) {
            (Some(e), Some(a)) if !e.trim().is_empty() && !a.trim().is_empty() => Some(format!(
                "{}.{}",
                e.trim().to_ascii_lowercase(),
                a.trim().to_ascii_lowercase()
            )),
            _ => None,
        }
    }

    /// Body as written into `MEMORY.md`: `entity.attribute = value` when
    /// structured, else the free text the LLM produced.
    pub fn body(&self) -> String {
        match (self.key(), &self.value) {
            (Some(k), Some(v)) if !v.trim().is_empty() => format!("{k} = {}", v.trim()),
            _ => self.text.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CurationOutcome {
    pub session_id: Option<String>,
    pub messages_examined: usize,
    pub last_message_id: Option<i64>,
    /// Facts the LLM proposed.
    pub facts_proposed: Vec<ExtractedFact>,
    /// Facts that survived normalization, lifetime, confidence, secret, and
    /// dedupe filtering and were actually written to MEMORY.md.
    pub facts_added: Vec<ExtractedFact>,
    /// True when the run was skipped because no new messages arrived
    /// since the last curation pass.
    pub skipped_no_new_messages: bool,
}

#[derive(Debug)]
pub enum CurationError {
    /// The auxiliary LLM call failed.
    Llm(String),
    /// Reading messages from the DB failed.
    Memory(String),
    /// Writing MEMORY.md failed.
    Notes(String),
    /// Curation log IO failed (read or write).
    Log(String),
}

impl std::fmt::Display for CurationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Llm(m) => write!(f, "auxiliary LLM error: {m}"),
            Self::Memory(m) => write!(f, "memory DB error: {m}"),
            Self::Notes(m) => write!(f, "notes write error: {m}"),
            Self::Log(m) => write!(f, "curation log error: {m}"),
        }
    }
}

impl std::error::Error for CurationError {}

// =====================================================================
// Curation log on disk
// =====================================================================

/// State of a single curator run in the append-only run history.
///
/// Bracketing (issue #2, point 2): a run appends `InProgress` *before*
/// touching MEMORY.md or the auxiliary LLM, then flips to `Completed`
/// only after the final atomic MEMORY.md write returns success. A crash
/// or panic between the two therefore leaves an orphaned `InProgress`
/// entry — [`CurationLog::orphaned_runs`] surfaces those so partial
/// writes never masquerade as clean finishes. `Failed` is written when
/// the run reached a definitive error before completion; it is not the
/// same as "crashed" and does not count as orphaned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunPhase {
    InProgress,
    Completed,
    Failed,
}

/// A single row in the curator's run history. The schema is
/// append-only — a `Completed` entry closes the preceding
/// `InProgress` entry for the same `run_id`; `run_id` is unique per
/// [`MemoryCurator::curate_session`] invocation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CurationRunEntry {
    /// Unique per invocation. Format: `"{session_id}:{start_unix_s}:{nonce}"`.
    pub run_id: String,
    pub session_id: String,
    pub phase: RunPhase,
    pub at_unix_s: u64,
    /// Only populated on `Completed` / `Failed`; carries the
    /// last-seen message id so recovery can compare against a fresh
    /// `recent()` fetch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_message_id: Option<i64>,
    /// Only populated on `Completed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub facts_added: Option<usize>,
    /// Only populated on `Failed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionCurationEntry {
    pub last_curated_message_id: i64,
    pub last_run_unix_s: u64,
    #[serde(default)]
    pub facts_added_total: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CurationLog {
    pub version: u32,
    #[serde(default)]
    pub sessions: BTreeMap<String, SessionCurationEntry>,
    /// Append-only run history for crash detection. Bounded via
    /// [`Self::truncate_runs`] to keep the file small in long-lived
    /// deployments. Older entries roll off; the summary in `sessions`
    /// still reflects the durable outcome.
    #[serde(default)]
    pub runs: Vec<CurationRunEntry>,
}

impl Default for CurationLog {
    fn default() -> Self {
        Self {
            version: 2,
            sessions: BTreeMap::new(),
            runs: Vec::new(),
        }
    }
}

impl CurationLog {
    /// Read the log; treat a missing or unparseable file as empty.
    pub fn load(path: &Path) -> Self {
        match fs::read_to_string(path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), CurationError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| CurationError::Log(e.to_string()))?;
        }
        let tmp = path.with_extension("json.tmp");
        let body =
            serde_json::to_string_pretty(self).map_err(|e| CurationError::Log(e.to_string()))?;
        fs::write(&tmp, body).map_err(|e| CurationError::Log(e.to_string()))?;
        fs::rename(&tmp, path).map_err(|e| CurationError::Log(e.to_string()))?;
        Ok(())
    }

    pub fn last_id(&self, session_id: &str) -> Option<i64> {
        self.sessions
            .get(session_id)
            .map(|e| e.last_curated_message_id)
    }

    pub fn record_run(&mut self, session_id: &str, last_message_id: i64, facts_added: usize) {
        let entry = self
            .sessions
            .entry(session_id.to_string())
            .or_insert(SessionCurationEntry {
                last_curated_message_id: last_message_id,
                last_run_unix_s: 0,
                facts_added_total: 0,
            });
        entry.last_curated_message_id = last_message_id;
        entry.last_run_unix_s = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        entry.facts_added_total = entry.facts_added_total.saturating_add(facts_added as u64);
    }

    /// Append an `InProgress` row for a new curator run. Must be
    /// persisted (`save`) *before* the run touches MEMORY.md or the
    /// auxiliary LLM — that is what makes crash detection work.
    pub fn begin_run(&mut self, run_id: &str, session_id: &str) {
        self.runs.push(CurationRunEntry {
            run_id: run_id.to_string(),
            session_id: session_id.to_string(),
            phase: RunPhase::InProgress,
            at_unix_s: now_unix_s(),
            last_message_id: None,
            facts_added: None,
            error: None,
        });
    }

    /// Append a `Completed` row for `run_id`. Must be persisted
    /// *after* the MEMORY.md write succeeds.
    pub fn complete_run(
        &mut self,
        run_id: &str,
        session_id: &str,
        last_message_id: Option<i64>,
        facts_added: usize,
    ) {
        self.runs.push(CurationRunEntry {
            run_id: run_id.to_string(),
            session_id: session_id.to_string(),
            phase: RunPhase::Completed,
            at_unix_s: now_unix_s(),
            last_message_id,
            facts_added: Some(facts_added),
            error: None,
        });
    }

    /// Append a `Failed` row for `run_id`. Distinct from a crash:
    /// the process ran long enough to observe the error and record
    /// it, so no recovery action is needed — the caller can retry.
    pub fn fail_run(&mut self, run_id: &str, session_id: &str, error: &str) {
        self.runs.push(CurationRunEntry {
            run_id: run_id.to_string(),
            session_id: session_id.to_string(),
            phase: RunPhase::Failed,
            at_unix_s: now_unix_s(),
            last_message_id: None,
            facts_added: None,
            error: Some(error.to_string()),
        });
    }

    /// Return every `InProgress` entry that has no matching
    /// `Completed` or `Failed` for the same `run_id`. A non-empty
    /// result means at least one prior invocation crashed between
    /// LLM extraction and MEMORY.md finalisation — the curation log
    /// says nothing happened, but partial facts may already be on
    /// disk. Callers should compare `MEMORY.md` against
    /// `last_message_id` on recovery.
    pub fn orphaned_runs(&self) -> Vec<&CurationRunEntry> {
        use std::collections::HashSet;
        let closed: HashSet<&str> = self
            .runs
            .iter()
            .filter(|r| matches!(r.phase, RunPhase::Completed | RunPhase::Failed))
            .map(|r| r.run_id.as_str())
            .collect();
        self.runs
            .iter()
            .filter(|r| r.phase == RunPhase::InProgress && !closed.contains(r.run_id.as_str()))
            .collect()
    }

    /// Cap the on-disk run history at `keep` entries, dropping the
    /// oldest. Idempotent when already shorter than the cap. Callers
    /// invoke this after a successful `complete_run` / `fail_run`
    /// append so the file does not grow without bound.
    pub fn truncate_runs(&mut self, keep: usize) {
        if self.runs.len() > keep {
            let drop = self.runs.len() - keep;
            self.runs.drain(0..drop);
        }
    }
}

/// Default location for the curation log.
pub fn default_log_path() -> PathBuf {
    crate::paths::agent_curation_log_path()
}

fn now_unix_s() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Monotonically-incrementing counter used to disambiguate multiple
/// runs that begin in the same wall-clock second (test loops, or a
/// fast machine curating several sessions back to back).
fn next_run_nonce() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NONCE: AtomicU64 = AtomicU64::new(0);
    NONCE.fetch_add(1, Ordering::Relaxed)
}

/// Cap on retained run entries in the on-disk log. Bounded so a
/// long-lived agent doesn't accumulate an unbounded history; large
/// enough to survive any realistic burst of curator runs between
/// operator inspections.
const MAX_RETAINED_RUNS: usize = 256;

// =====================================================================
// LLM prompting
// =====================================================================

const DEFAULT_SYSTEM_PROMPT: &str = r#"You are a careful note-taker that distills durable knowledge
from a conversation between a user and an AI agent.

Output format: zero or more `<fact>` tags, one per line:

    <fact category="<category>" entity="<entity>" attribute="<attribute>" value="<value>" lifetime="<durable|observed>" ttl_days="<optional integer>" source_message_id="<user message id>" confidence="<0.0-1.0>">free text</fact>

Categories you may use:
  - preference   — preferences, likes / dislikes, working style
  - identity     — names, roles, affiliations, languages spoken
  - environment  — stable conventions or observed system/tool state
  - skill        — technical or domain expertise the user has shown
  - resolution   — a problem that was diagnosed and fixed

Structure every fact as entity + attribute + value:

    <fact category="preference" entity="editor" attribute="name" value="helix" lifetime="durable" source_message_id="42" confidence="0.9">User switched to helix</fact>
    <fact category="environment" entity="python" attribute="version" value="3.13.1" lifetime="observed" ttl_days="30" source_message_id="47" confidence="0.9">Python 3.13.1 was observed</fact>
    <fact category="resolution" entity="postgres" attribute="cause" value="connection pool exhausted" lifetime="durable" source_message_id="51" confidence="0.8">Staging DB dropped connections because the pool was too small</fact>

This matters: two facts sharing entity+attribute are treated as
successive states of the same thing, so a later one supersedes an
earlier one. `editor/name` lets "helix" replace "vim"; free text does
not. Reuse the *same* entity and attribute names for the same slot.

Rules:
  1. Always provide entity, attribute, value, lifetime, and the id of the
     user message that supports the fact.
  2. Use these canonical aliases:
       operating_system.{name,distribution,base_distribution} -> os.distribution
       {claw_agent,cos_agent,system_agent}.deployment_state -> claw_os.installation
       installed/install_status/installation_state -> installation
       installed_version/package_version/version_number -> version
       desired_version/version_preference -> preferred_version
  3. Preferences, profile facts, demonstrated skills, and reusable
     resolutions are durable. A resolution records what broke and why or
     what fixed it; merely spending today debugging something is not durable.
     A preference for a version must use `preferred_version`; labeling the
     live `version` slot as a preference does not make it durable.
  4. Current OS, package/tool versions, installation state, process/service
     state, memory size, and sensor availability are observations regardless
     of category. Mark them lifetime="observed"; ttl_days is optional and
     policy-bounded. Omit an observation unless a user source message supports it.
  5. Never emit current task/session state. Never emit procedures, command
     sequences, runbooks, or imperative workflows: those belong in Skills.
  6. Confidence is your honest estimate (0.0-1.0). Below 0.5, omit
     the fact entirely — better silent than wrong.
  7. NEVER emit secrets, API keys, tokens, passwords, or anything
     that looks like a credential. If you see one in the transcript,
     skip the surrounding fact entirely.
  8. Keep each fact to one short, declarative sentence ≤ 200 chars.
  9. If the conversation reveals nothing eligible, emit no tags.
     An empty response is the correct answer most of the time.

Do not write any prose outside the `<fact>` tags. No preamble, no
conclusion, no markdown headers. Tags only.
"#;

/// The default system prompt, exposed for tooling that wants to
/// preview / customise it.
pub fn default_system_prompt() -> &'static str {
    DEFAULT_SYSTEM_PROMPT
}

/// Format a slice of message rows as a transcript suitable for the
/// LLM's user message. Truncates per-message body to keep the
/// prompt within auxiliary's `max_tokens`.
pub fn format_transcript(messages: &[MessageRow], per_msg_cap: usize) -> String {
    let mut buf = String::new();
    for m in messages {
        let body = if m.content.chars().count() > per_msg_cap {
            let truncated: String = m.content.chars().take(per_msg_cap).collect();
            format!("{truncated}…")
        } else {
            m.content.clone()
        };
        buf.push('[');
        buf.push_str(&m.role);
        buf.push_str(" message_id=");
        buf.push_str(&m.id.to_string());
        buf.push_str("] ");
        buf.push_str(&body);
        buf.push_str("\n\n");
    }
    buf
}

// =====================================================================
// `<fact>` parser — forgiving regex-style extraction
// =====================================================================

/// Parse `<fact>` tags out of LLM output. Tolerates extra whitespace,
/// missing confidence (defaults to 0.5), unknown categories, and
/// trailing prose. Returns facts in order of appearance.
pub fn parse_facts(llm_output: &str) -> Vec<ExtractedFact> {
    let mut out = Vec::new();
    let bytes = llm_output.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Find next `<fact`
        let open = match find_subseq(bytes, b"<fact", i) {
            Some(p) => p,
            None => break,
        };
        // Find the closing `>` of the open tag.
        let attr_end = match memchr(bytes, b'>', open) {
            Some(p) => p,
            None => break,
        };
        let attrs = &llm_output[open + 5..attr_end];
        // Find the closing `</fact>`.
        let close = match find_subseq(bytes, b"</fact>", attr_end) {
            Some(p) => p,
            None => break,
        };
        let body = llm_output[attr_end + 1..close].trim().to_string();
        i = close + 7;

        if body.is_empty() {
            continue;
        }

        let mut category = FactCategory::Other("unknown".to_string());
        let mut confidence = 0.5f32;
        let mut entity: Option<String> = None;
        let mut attribute: Option<String> = None;
        let mut value: Option<String> = None;
        let mut lifetime: Option<FactLifetime> = None;
        let mut observed_at: Option<String> = None;
        let mut ttl_days: Option<u32> = None;
        let mut source_message_id: Option<i64> = None;
        for (k, v) in parse_attrs(attrs) {
            match k.as_str() {
                "category" => category = FactCategory::parse(&v),
                "entity" => entity = non_empty(v),
                "attribute" | "attr" => attribute = non_empty(v),
                "value" | "val" => value = non_empty(v),
                "lifetime" => lifetime = FactLifetime::parse(&v),
                "observed_at" | "observed" => observed_at = non_empty(v),
                "ttl" | "ttl_days" => ttl_days = parse_ttl_days(&v),
                "source_message_id" | "message_id" | "source" => {
                    source_message_id = v.trim().parse::<i64>().ok()
                }
                "confidence" => {
                    if let Ok(n) = v.parse::<f32>() {
                        if n.is_finite() {
                            confidence = n.clamp(0.0, 1.0);
                        }
                    }
                }
                _ => {}
            }
        }

        out.push(ExtractedFact {
            category,
            text: body,
            confidence,
            entity,
            attribute,
            value,
            lifetime,
            observed_at,
            ttl_days,
            source_session_id: None,
            source_message_id,
        });
    }
    out
}

/// `Some(trimmed)` for a non-blank attribute, `None` otherwise — a model
/// that emits `entity=""` is treated as having omitted it rather than
/// creating an empty slot key.
fn non_empty(s: String) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

fn parse_ttl_days(value: &str) -> Option<u32> {
    value
        .trim()
        .trim_end_matches(['d', 'D'])
        .parse::<u32>()
        .ok()
}

fn parse_attrs(attrs: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut chars = attrs.chars().peekable();
    while chars.peek().is_some() {
        // skip whitespace
        while let Some(&c) = chars.peek() {
            if c.is_whitespace() {
                chars.next();
            } else {
                break;
            }
        }
        let mut key = String::new();
        while let Some(&c) = chars.peek() {
            if c == '=' || c.is_whitespace() {
                break;
            }
            key.push(c);
            chars.next();
        }
        if key.is_empty() {
            break;
        }
        // skip `=`
        if chars.peek() == Some(&'=') {
            chars.next();
        }
        // value — quoted or bare
        let value = if chars.peek() == Some(&'"') || chars.peek() == Some(&'\'') {
            let quote = chars.next().unwrap();
            let mut v = String::new();
            for c in chars.by_ref() {
                if c == quote {
                    break;
                }
                v.push(c);
            }
            v
        } else {
            let mut v = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                v.push(c);
                chars.next();
            }
            v
        };
        out.push((key, value));
    }
    out
}

fn find_subseq(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from >= haystack.len() {
        return None;
    }
    for i in from..=haystack.len().saturating_sub(needle.len()) {
        if &haystack[i..i + needle.len()] == needle {
            return Some(i);
        }
    }
    None
}

fn memchr(haystack: &[u8], byte: u8, from: usize) -> Option<usize> {
    haystack[from..]
        .iter()
        .position(|b| *b == byte)
        .map(|p| p + from)
}

// =====================================================================
// Filters
// =====================================================================

/// Conservative secret-like-token detector. Errs on the side of
/// dropping things that *might* be a credential rather than chancing
/// it. Not a security boundary — the LLM is also instructed to skip
/// these — just the second line of defence.
pub fn looks_secret(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let credential_words = [
        "api key",
        "api-key",
        "apikey",
        "secret",
        "password",
        "passwd",
        "token",
        "credential",
        "bearer ",
        "ssh-rsa",
        "ssh-ed25519",
        "-----begin",
    ];
    if credential_words.iter().any(|w| lower.contains(w)) {
        return true;
    }
    // Heuristic: long base64-ish runs (≥ 24 chars) suggest tokens.
    // We deliberately treat `/` as a *separator* (not part of the
    // alphabet) so that legitimate filesystem paths like
    // `/Users/alice/.config/something-very-long` no longer false-
    // positive as secrets. Real base64-ish tokens — JWTs, API keys —
    // never embed `/` inside the secret material at this length;
    // even base64-url uses `_` / `-` rather than `/`.
    let mut run = String::new();
    for c in text.chars() {
        if c.is_ascii_alphanumeric() || c == '+' || c == '_' || c == '-' || c == '=' {
            run.push(c);
        } else {
            if secret_like_run(&run) {
                return true;
            }
            run.clear();
        }
    }
    secret_like_run(&run)
}

fn secret_like_run(run: &str) -> bool {
    run.len() >= 24
}

fn looks_like_uuid(value: &str) -> bool {
    if value.len() != 36 {
        return false;
    }
    value.char_indices().all(|(index, c)| match index {
        8 | 13 | 18 | 23 => c == '-',
        _ => c.is_ascii_hexdigit(),
    })
}

/// Check every model-controlled fact field and the body selected for
/// persistence. Structured fields must be checked independently because
/// [`ExtractedFact::body`] can replace `text` with `entity.attribute = value`.
fn fact_looks_secret(fact: &ExtractedFact) -> bool {
    looks_secret(fact.category.as_str())
        || looks_secret(&fact.text)
        || fact.entity.as_deref().is_some_and(looks_secret)
        || fact.attribute.as_deref().is_some_and(looks_secret)
        || fact.value.as_deref().is_some_and(looks_secret)
        || fact.observed_at.as_deref().is_some_and(looks_secret)
        || fact
            .source_session_id
            .as_deref()
            .is_some_and(|source| !looks_like_uuid(source) && looks_secret(source))
        || looks_secret(&fact.body())
}

/// Existing curated entries in MEMORY.md, used to dedupe at write time.
///
/// Returns the rendered body of each curated line, e.g.
/// `"editor.name = helix"` for a structured fact or the raw sentence for
/// a free-text one.
fn existing_curated_lines(memory_md: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in memory_md.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("- [") {
            // e.g. "- [preference] editor.name = helix _(...)_"
            if let Some(close) = rest.find("] ") {
                let after = &rest[close + 2..];
                // Strip trailing italics like "_(2026-01-15, conf 0.90)_"
                let body = after.split(" _(").next().unwrap_or(after).trim();
                out.push(body.to_string());
            }
        }
    }
    out
}

/// Split a rendered curated body into `(key, value)` when it is
/// structured, i.e. `entity.attribute = value`.
///
/// Shared with prompt assembly, which must recognise the same shape to
/// project only chain tails.
pub fn split_curated_body(body: &str) -> Option<(String, String)> {
    ontology::split_structured_body(body)
}

/// The most recent value recorded for each structured key.
#[cfg(test)]
fn latest_values(existing: &[String]) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    for body in existing {
        if let Some((k, v)) = split_curated_body(body) {
            // Later lines win: the section is append-only, so the last
            // occurrence of a key is its current state.
            out.insert(k, v);
        }
    }
    out
}

/// Format a single fact as a MEMORY.md line.
pub fn render_fact_line(fact: &ExtractedFact, today: &str) -> String {
    if fact.lifetime.is_none()
        && fact.observed_at.is_none()
        && fact.ttl_days.is_none()
        && fact.source_session_id.is_none()
        && fact.source_message_id.is_none()
    {
        return format!(
            "- [{}] {} _({today}, conf {:.2})_",
            fact.category.as_str(),
            fact.body(),
            fact.confidence
        );
    }

    let observed_at = fact.observed_at.as_deref().unwrap_or("unknown");
    let mut metadata = vec![format!("observed_at={observed_at}")];
    if let Some(ttl_days) = fact.ttl_days {
        metadata.push(format!("ttl={ttl_days}d"));
    }
    let source_session = fact
        .source_session_id
        .as_deref()
        .map(sanitize_metadata_value)
        .unwrap_or_else(|| "unknown".to_string());
    metadata.push(format!("source_session:{source_session}"));
    metadata.push(format!(
        "source_message:{}",
        fact.source_message_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| "unknown".to_string())
    ));
    metadata.push(format!("conf={:.2}", fact.confidence));
    if let Some(lifetime) = fact.lifetime {
        metadata.push(format!("lifetime={}", lifetime.as_str()));
    }
    format!(
        "- [{}] {} _({})_",
        fact.category.as_str(),
        fact.body(),
        metadata.join(", ")
    )
}

fn sanitize_metadata_value(value: &str) -> String {
    value
        .chars()
        .take(128)
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn normalize_provenance_session_id(value: &str) -> String {
    let trimmed = value.trim();
    let valid = !trimmed.is_empty()
        && trimmed.chars().count() <= 128
        && trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'));
    if valid && (looks_like_uuid(trimmed) || !looks_secret(trimmed)) {
        trimmed.to_string()
    } else {
        "redacted".to_string()
    }
}

/// Render a fact only after its normalized model-controlled fields and
/// fallback date are free of secret-like content.
fn render_persistable_fact_line(fact: &ExtractedFact, today: &str) -> Option<String> {
    if fact_looks_secret(fact) || looks_secret(today) {
        return None;
    }
    let line = render_fact_line(fact, today);
    let mut validation_fact = fact.clone();
    if validation_fact
        .source_session_id
        .as_deref()
        .is_some_and(looks_like_uuid)
    {
        validation_fact.source_session_id = Some("trusted-session-uuid".to_string());
    }
    let validation_line = render_fact_line(&validation_fact, today);
    (!looks_secret(&validation_line)).then_some(line)
}

const SECTION_HEADER: &str = "## Curated facts (auto)";

fn ensure_section(memory_md: &str) -> String {
    if memory_md.contains(SECTION_HEADER) {
        return memory_md.to_string();
    }
    let mut out = memory_md.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(SECTION_HEADER);
    out.push('\n');
    out
}

fn append_lines_to_section(memory_md: &str, new_lines: &[String]) -> String {
    let with_section = ensure_section(memory_md);
    let mut out = with_section.trim_end().to_string();
    out.push('\n');
    for line in new_lines {
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn dedupe_against_current_memory(
    mut facts: Vec<ExtractedFact>,
    memory_md: &str,
) -> Vec<ExtractedFact> {
    let existing_bodies = existing_curated_lines(memory_md);
    let current = current_structured_values(memory_md);
    let already: Vec<String> = existing_bodies
        .iter()
        .map(|body| body.to_ascii_lowercase())
        .collect();

    facts.retain(|fact| match (fact.key(), &fact.value) {
        (Some(key), Some(value)) => current.get(&key).is_none_or(|current| {
            let unchanged = current.value.eq_ignore_ascii_case(value.trim());
            let needs_governed_observation = fact.lifetime == Some(FactLifetime::Observed)
                && current.lifetime != Some(FactLifetime::Observed);
            !unchanged || needs_governed_observation
        }),
        _ => !already.contains(&fact.body().to_ascii_lowercase()),
    });

    let mut seen_keys = std::collections::HashSet::new();
    let mut deduped = Vec::new();
    for fact in facts.into_iter().rev() {
        if let Some(key) = fact.key() {
            if !seen_keys.insert(key) {
                continue;
            }
        }
        deduped.push(fact);
    }
    deduped.reverse();
    deduped
}

fn append_facts_locked(
    notes: &NotesStore,
    candidates: Vec<ExtractedFact>,
    today: &str,
) -> Result<Vec<ExtractedFact>, String> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    let mut added = Vec::new();
    notes.update(MEMORY_FILE, |current| {
        let existing = current.unwrap_or_default();
        let survivors = dedupe_against_current_memory(candidates, &existing);
        let mut lines = Vec::with_capacity(survivors.len());
        for fact in survivors {
            if let Some(line) = render_persistable_fact_line(&fact, today) {
                added.push(fact);
                lines.push(line);
            }
        }
        if lines.is_empty() {
            existing
        } else {
            append_lines_to_section(&existing, &lines)
        }
    })?;
    Ok(added)
}

fn today_yyyymmdd() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    ontology::date_from_unix_s(secs)
}

// Civil date helper — same epoch math used elsewhere in cos
// (e.g. trace timestamping). 1970-01-01 + N days.
#[cfg(test)]
fn days_to_ymd(days_since_epoch: i64) -> (i32, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146_096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = (yoe as i64 + era * 400) as i32;
    let doy = (doe - (365 * yoe + yoe / 4 - yoe / 100)) as u32; // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

fn normalize_fact_for_persistence(
    fact: &ExtractedFact,
    session_id: &str,
    messages: &[MessageRow],
    today: &str,
) -> Option<ExtractedFact> {
    let governed = ontology::normalize_fact(
        fact.category.as_str(),
        fact.entity.as_deref()?,
        fact.attribute.as_deref()?,
        fact.value.as_deref()?,
        fact.lifetime,
        fact.ttl_days,
    )?;

    let source = fact.source_message_id.and_then(|id| {
        messages
            .iter()
            .find(|message| message.id == id && message.role == "user")
    });
    if governed.lifetime == FactLifetime::Observed && source.is_none() {
        return None;
    }
    let today_days = ontology::date_to_epoch_days(today);
    let observed_at = source
        .and_then(|message| ontology::date_from_unix_ms(message.ts_ms))
        .map(|source_date| {
            let source_is_future = ontology::date_to_epoch_days(&source_date)
                .zip(today_days)
                .is_some_and(|(source, today)| source > today);
            if source_is_future {
                today.to_string()
            } else {
                source_date
            }
        });

    let mut normalized = fact.clone();
    normalized.entity = Some(governed.slot.entity);
    normalized.attribute = Some(governed.slot.attribute);
    normalized.value = Some(governed.slot.value);
    normalized.lifetime = Some(governed.lifetime);
    normalized.ttl_days = governed.ttl_days;
    normalized.observed_at = observed_at;
    normalized.source_session_id = Some(normalize_provenance_session_id(session_id));
    normalized.source_message_id = source.map(|message| message.id);
    Some(normalized)
}

// =====================================================================
// Public curator
// =====================================================================

/// The memory curator. Holds references to its dependencies but is
/// otherwise stateless — call `curate_session` as often as you like.
#[derive(Clone)]
pub struct MemoryCurator {
    aux: AuxiliaryClient,
    notes: NotesStore,
    log_path: PathBuf,
    config: CuratorConfig,
}

impl MemoryCurator {
    pub fn new(aux: AuxiliaryClient, notes: NotesStore, log_path: PathBuf) -> Self {
        Self {
            aux,
            notes,
            log_path,
            config: CuratorConfig::default(),
        }
    }

    pub fn notes_dir(&self) -> &Path {
        self.notes.dir()
    }

    pub fn with_config(mut self, config: CuratorConfig) -> Self {
        self.config = config;
        self
    }

    pub fn config(&self) -> &CuratorConfig {
        &self.config
    }

    pub fn log_path(&self) -> &Path {
        &self.log_path
    }

    #[cfg(test)]
    pub(crate) fn save_empty_log(&self) -> Result<(), CurationError> {
        CurationLog::default().save(&self.log_path)
    }

    /// Run extraction over the most recent `max_messages` of a
    /// session. Set `dry_run = true` to skip both LLM call and disk
    /// writes (returns "facts_proposed: empty").
    ///
    /// Note: `dry_run = true` short-circuits *before* any LLM call,
    /// so it's free to run.
    pub async fn curate_session(
        &self,
        db: &MemoryDb,
        session_id: &str,
        dry_run: bool,
    ) -> Result<CurationOutcome, CurationError> {
        let messages = db
            .recent(session_id, self.config.max_messages)
            .map_err(|e| CurationError::Memory(e.to_string()))?;

        let last_id = messages.last().map(|m| m.id);

        // Skip if nothing new since last curation.
        if self.config.skip_if_no_new_messages {
            let log = CurationLog::load(&self.log_path);
            if let (Some(prev), Some(now)) = (log.last_id(session_id), last_id) {
                if prev >= now {
                    return Ok(CurationOutcome {
                        session_id: Some(session_id.to_string()),
                        messages_examined: messages.len(),
                        last_message_id: last_id,
                        facts_proposed: Vec::new(),
                        facts_added: Vec::new(),
                        skipped_no_new_messages: true,
                    });
                }
            }
        }

        if messages.is_empty() {
            return Ok(CurationOutcome {
                session_id: Some(session_id.to_string()),
                messages_examined: 0,
                last_message_id: None,
                facts_proposed: Vec::new(),
                facts_added: Vec::new(),
                skipped_no_new_messages: false,
            });
        }

        if dry_run {
            return Ok(CurationOutcome {
                session_id: Some(session_id.to_string()),
                messages_examined: messages.len(),
                last_message_id: last_id,
                facts_proposed: Vec::new(),
                facts_added: Vec::new(),
                skipped_no_new_messages: false,
            });
        }

        // ---- Three-phase bracketing (issue #2, point 2) ----------------
        //
        // Append `InProgress` and fsync it BEFORE any LLM call or
        // MEMORY.md mutation. If we crash between here and the
        // matching `complete_run` below, `orphaned_runs()` will
        // surface this entry on the next load. Recovery can then
        // compare MEMORY.md against `last_curated_message_id` to
        // decide whether partial facts leaked through.
        let run_id = format!(
            "{session_id}:{start}:{nonce}",
            start = now_unix_s(),
            nonce = next_run_nonce(),
        );
        {
            let mut log = CurationLog::load(&self.log_path);
            log.begin_run(&run_id, session_id);
            log.save(&self.log_path)?;
        }

        // From here on, any early return must go through
        // `record_failure` so we don't leave a phantom InProgress
        // entry behind for a *definitive* failure. Crashes stay
        // orphaned by design — that's the whole point.
        let record_failure = |err: &CurationError| {
            let mut log = CurationLog::load(&self.log_path);
            log.fail_run(&run_id, session_id, &err.to_string());
            log.truncate_runs(MAX_RETAINED_RUNS);
            // Best-effort; if the log write itself fails we surface
            // the original error, not the log write error.
            let _ = log.save(&self.log_path);
        };

        let transcript = format_transcript(&messages, 800);

        let system_prompt = self
            .config
            .system_prompt
            .as_deref()
            .unwrap_or(DEFAULT_SYSTEM_PROMPT);

        let raw = match self.aux.ask(Some(system_prompt), &transcript).await {
            Ok(r) => r,
            Err(e) => {
                let err = CurationError::Llm(e.to_string());
                record_failure(&err);
                return Err(err);
            }
        };

        let mut proposed = parse_facts(&raw);
        proposed.truncate(self.config.max_facts_per_run);

        let today = today_yyyymmdd();

        // Normalize before the secret check is considered complete. We check
        // both forms so normalization can neither hide a secret from the raw
        // model output nor introduce an unchecked persisted representation.
        let survivors: Vec<ExtractedFact> = proposed
            .iter()
            .filter(|f| f.confidence >= self.config.min_confidence)
            .filter(|f| !fact_looks_secret(f))
            .filter_map(|f| normalize_fact_for_persistence(f, session_id, &messages, &today))
            .filter(|f| {
                f.lifetime.is_some_and(FactLifetime::is_memory_eligible) && !fact_looks_secret(f)
            })
            .collect();

        // Final read, dedupe, and append happen under one exclusive lock.
        // The LLM call remains outside the lock, but a concurrent curator or
        // note updater cannot land between the final read and atomic rename.
        let added = match append_facts_locked(&self.notes, survivors, &today) {
            Ok(added) => added,
            Err(error) => {
                let err = CurationError::Notes(error);
                record_failure(&err);
                return Err(err);
            }
        };

        // ---- Close the bracket (issue #2, point 2) ---------------------
        //
        // MEMORY.md write has returned success. Only NOW do we append
        // `Completed` — anything earlier would let a crash between
        // the atomic MEMORY.md rename and this write masquerade as a
        // clean finish.
        if let Some(id) = last_id {
            let mut log = CurationLog::load(&self.log_path);
            log.record_run(session_id, id, added.len());
            log.complete_run(&run_id, session_id, Some(id), added.len());
            log.truncate_runs(MAX_RETAINED_RUNS);
            log.save(&self.log_path)?;
        } else {
            let mut log = CurationLog::load(&self.log_path);
            log.complete_run(&run_id, session_id, None, added.len());
            log.truncate_runs(MAX_RETAINED_RUNS);
            log.save(&self.log_path)?;
        }

        Ok(CurationOutcome {
            session_id: Some(session_id.to_string()),
            messages_examined: messages.len(),
            last_message_id: last_id,
            facts_proposed: proposed,
            facts_added: added,
            skipped_no_new_messages: false,
        })
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/memory/curator.rs"
    ));
}
