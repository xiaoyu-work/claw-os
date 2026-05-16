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
//!   4. Filter by minimum confidence + dedupe against what's already
//!      in `MEMORY.md`.
//!   5. Append survivors to a `## Curated facts (auto)` section in
//!      `MEMORY.md`. The agent will pick them up on the next turn
//!      via the prompt builder's automatic notes injection.
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
//! Curation log lives at `data_dir/agent/memory/curation_log.json`.
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
use crate::agent::memory::notes::{NotesStore, MEMORY_FILE};
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
    Other(String),
}

impl FactCategory {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "preference" | "pref" => Self::Preference,
            "identity" | "id" => Self::Identity,
            "environment" | "env" => Self::Environment,
            "skill" | "skills" => Self::Skill,
            other => Self::Other(other.to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Preference => "preference",
            Self::Identity => "identity",
            Self::Environment => "environment",
            Self::Skill => "skill",
            Self::Other(s) => s.as_str(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExtractedFact {
    pub category: FactCategory,
    pub text: String,
    pub confidence: f32,
}

#[derive(Debug, Clone)]
pub struct CurationOutcome {
    pub session_id: Option<String>,
    pub messages_examined: usize,
    pub last_message_id: Option<i64>,
    /// Facts the LLM proposed.
    pub facts_proposed: Vec<ExtractedFact>,
    /// Facts that survived confidence/secret/dedupe filtering and
    /// were actually written to MEMORY.md.
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
}

impl Default for CurationLog {
    fn default() -> Self {
        Self {
            version: 1,
            sessions: BTreeMap::new(),
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
}

/// Default location for the curation log.
pub fn default_log_path() -> PathBuf {
    crate::paths::agent_state_dir()
        .join("memory")
        .join("curation_log.json")
}

// =====================================================================
// LLM prompting
// =====================================================================

const DEFAULT_SYSTEM_PROMPT: &str = r#"You are a careful note-taker that distills *durable user facts*
from a conversation between a user and an AI agent.

Output format: zero or more `<fact>` tags, one per line:

    <fact category="<category>" confidence="<0.0-1.0>">free text</fact>

Categories you may use:
  - preference   — preferences, likes / dislikes, working style
  - identity     — names, roles, affiliations, languages spoken
  - environment  — operating system, tooling, hardware, locations
  - skill        — technical or domain expertise the user has shown

Rules:
  1. Only emit facts that will *still be true next month* (durable),
     not short-term task state ("user is currently debugging X").
  2. Confidence is your honest estimate (0.0-1.0). Below 0.5, omit
     the fact entirely — better silent than wrong.
  3. NEVER emit secrets, API keys, tokens, passwords, or anything
     that looks like a credential. If you see one in the transcript,
     skip the surrounding fact entirely.
  4. Keep each fact to one short, declarative sentence ≤ 200 chars.
  5. If the conversation reveals nothing durable, emit no tags.
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
        for (k, v) in parse_attrs(attrs) {
            match k.as_str() {
                "category" => category = FactCategory::parse(&v),
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
        });
    }
    out
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
    let mut run = 0usize;
    for c in text.chars() {
        if c.is_ascii_alphanumeric() || c == '+' || c == '_' || c == '-' || c == '=' {
            run += 1;
            if run >= 24 {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
}

/// Existing curated lines in MEMORY.md, used to dedupe at write time.
fn existing_curated_lines(memory_md: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in memory_md.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("- [") {
            // e.g. "- [preference] User prefers Rust _(...)_"
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

/// Format a single fact as a MEMORY.md line.
pub fn render_fact_line(fact: &ExtractedFact, today: &str) -> String {
    format!(
        "- [{}] {} _({today}, conf {:.2})_",
        fact.category.as_str(),
        fact.text,
        fact.confidence
    )
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

fn today_yyyymmdd() -> String {
    // Lightweight date stamp. We only need date precision (not
    // time of day) for human readability of the audit trail.
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = secs / 86_400;
    let (y, m, d) = days_to_ymd(days as i64);
    format!("{y:04}-{m:02}-{d:02}")
}

// Civil date helper — same epoch math used elsewhere in cos
// (e.g. trace timestamping). 1970-01-01 + N days.
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

        let transcript = format_transcript(&messages, 800);

        let system_prompt = self
            .config
            .system_prompt
            .as_deref()
            .unwrap_or(DEFAULT_SYSTEM_PROMPT);

        let raw = self
            .aux
            .ask(Some(system_prompt), &transcript)
            .await
            .map_err(|e| CurationError::Llm(e.to_string()))?;

        let mut proposed = parse_facts(&raw);
        proposed.truncate(self.config.max_facts_per_run);

        // Confidence + secret filter.
        let mut survivors: Vec<ExtractedFact> = proposed
            .iter()
            .filter(|f| f.confidence >= self.config.min_confidence)
            .filter(|f| !looks_secret(&f.text))
            .cloned()
            .collect();

        // Dedupe against existing MEMORY.md curated lines (case-insensitive).
        let existing = self
            .notes
            .read(MEMORY_FILE)
            .map_err(CurationError::Notes)?
            .unwrap_or_default();
        let already: Vec<String> = existing_curated_lines(&existing)
            .into_iter()
            .map(|s| s.to_ascii_lowercase())
            .collect();
        survivors.retain(|f| !already.contains(&f.text.to_ascii_lowercase()));

        let added = if survivors.is_empty() {
            Vec::new()
        } else {
            let today = today_yyyymmdd();
            let lines: Vec<String> = survivors
                .iter()
                .map(|f| render_fact_line(f, &today))
                .collect();
            let next = append_lines_to_section(&existing, &lines);
            self.notes
                .write(MEMORY_FILE, &next)
                .map_err(CurationError::Notes)?;
            survivors.clone()
        };

        if let Some(id) = last_id {
            let mut log = CurationLog::load(&self.log_path);
            log.record_run(session_id, id, added.len());
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
    use super::*;

    // ---- FactCategory --------------------------------------------------

    #[test]
    fn category_parse_canonical() {
        assert_eq!(FactCategory::parse("preference"), FactCategory::Preference);
        assert_eq!(FactCategory::parse("PREF"), FactCategory::Preference);
        assert_eq!(FactCategory::parse("identity"), FactCategory::Identity);
        assert_eq!(FactCategory::parse("env"), FactCategory::Environment);
        assert_eq!(FactCategory::parse("skill"), FactCategory::Skill);
    }

    #[test]
    fn category_parse_unknown_preserved_in_other() {
        match FactCategory::parse("hobby") {
            FactCategory::Other(s) => assert_eq!(s, "hobby"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    // ---- parse_facts ---------------------------------------------------

    #[test]
    fn parse_facts_handles_well_formed_tags() {
        let out = parse_facts(
            r#"<fact category="preference" confidence="0.9">User prefers Rust over Go</fact>
            <fact category="environment" confidence="0.95">User is on Windows 11</fact>"#,
        );
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].category, FactCategory::Preference);
        assert_eq!(out[0].text, "User prefers Rust over Go");
        assert!((out[0].confidence - 0.9).abs() < 1e-4);
        assert_eq!(out[1].category, FactCategory::Environment);
    }

    #[test]
    fn parse_facts_tolerates_single_quotes() {
        let out = parse_facts(r#"<fact category='skill' confidence='0.7'>fluent in Rust</fact>"#);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].category, FactCategory::Skill);
    }

    #[test]
    fn parse_facts_tolerates_missing_confidence() {
        let out = parse_facts(r#"<fact category="identity">Name is Alex</fact>"#);
        assert_eq!(out.len(), 1);
        assert!((out[0].confidence - 0.5).abs() < 1e-4);
    }

    #[test]
    fn parse_facts_tolerates_extra_whitespace_and_newlines_inside_body() {
        let out =
            parse_facts("<fact category=\"identity\" confidence=\"0.8\">  hello\nworld  </fact>");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "hello\nworld");
    }

    #[test]
    fn parse_facts_skips_empty_body() {
        let out = parse_facts(
            r#"<fact category="preference" confidence="0.9"></fact>
            <fact category="identity" confidence="0.8">real fact</fact>"#,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "real fact");
    }

    #[test]
    fn parse_facts_empty_when_no_tags() {
        assert!(parse_facts("just prose").is_empty());
        assert!(parse_facts("").is_empty());
    }

    #[test]
    fn parse_facts_clamps_confidence_to_unit_interval() {
        let out = parse_facts(
            r#"<fact category="preference" confidence="2.5">over the moon</fact>
            <fact category="preference" confidence="-0.3">below floor</fact>"#,
        );
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].confidence, 1.0);
        assert_eq!(out[1].confidence, 0.0);
    }

    #[test]
    fn parse_facts_drops_orphaned_open_tag() {
        // Open without close → break out without emitting a phantom fact.
        let out = parse_facts(r#"<fact category="preference" confidence="0.9">unterminated"#);
        assert!(out.is_empty());
    }

    #[test]
    fn parse_facts_handles_two_back_to_back_tags() {
        // No whitespace between </fact> and the next <fact>.
        let out = parse_facts(
            r#"<fact category="preference" confidence="0.9">a</fact><fact category="skill" confidence="0.8">b</fact>"#,
        );
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].text, "a");
        assert_eq!(out[1].text, "b");
    }

    // ---- looks_secret -------------------------------------------------

    #[test]
    fn looks_secret_flags_credential_words() {
        assert!(looks_secret("the API key is sk-foo"));
        assert!(looks_secret("password=abc123"));
        assert!(looks_secret("Bearer xyz"));
        assert!(looks_secret("-----BEGIN PRIVATE KEY-----"));
    }

    #[test]
    fn looks_secret_flags_long_alphanumeric_runs() {
        assert!(looks_secret("user has token AKIAIOSFODNN7EXAMPLEKEYZ"));
    }

    #[test]
    fn looks_secret_does_not_flag_normal_facts() {
        assert!(!looks_secret("user prefers Rust"));
        assert!(!looks_secret("user lives in Beijing"));
        assert!(!looks_secret(""));
    }

    // ---- existing_curated_lines / dedupe ------------------------------

    #[test]
    fn existing_curated_lines_extracts_the_body_without_meta() {
        let md = r#"# memory

## Curated facts (auto)
- [preference] User prefers Rust _(2026-01-15, conf 0.90)_
- [identity] Name is Xiaoyu _(2026-01-15, conf 0.95)_

other content
"#;
        let lines = existing_curated_lines(md);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "User prefers Rust");
        assert_eq!(lines[1], "Name is Xiaoyu");
    }

    #[test]
    fn render_fact_line_format_round_trips() {
        let fact = ExtractedFact {
            category: FactCategory::Preference,
            text: "User prefers Rust".to_string(),
            confidence: 0.91,
        };
        let line = render_fact_line(&fact, "2026-01-15");
        assert_eq!(
            line,
            "- [preference] User prefers Rust _(2026-01-15, conf 0.91)_"
        );
    }

    #[test]
    fn ensure_section_creates_when_missing() {
        let s = ensure_section("");
        assert!(s.contains(SECTION_HEADER));
        let s2 = ensure_section("# top\n");
        assert!(s2.contains(SECTION_HEADER));
        // Idempotent.
        let s3 = ensure_section(&s2);
        assert_eq!(s2, s3);
    }

    #[test]
    fn append_lines_to_section_adds_under_header() {
        let md = "# memory\n\nsome notes";
        let next = append_lines_to_section(
            md,
            &["- [preference] foo _(2026-01-15, conf 0.90)_".to_string()],
        );
        assert!(next.contains(SECTION_HEADER));
        assert!(next.contains("foo"));
    }

    // ---- CurationLog --------------------------------------------------

    #[test]
    fn log_load_missing_file_is_default() {
        let dir = std::env::temp_dir().join(format!(
            "cos-curator-log-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        let path = dir.join("missing.json");
        let log = CurationLog::load(&path);
        assert_eq!(log.version, 1);
        assert!(log.sessions.is_empty());
    }

    #[test]
    fn log_round_trip_through_disk() {
        let dir = std::env::temp_dir().join(format!(
            "cos-curator-log-rt-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        let path = dir.join("log.json");
        let mut log = CurationLog::default();
        log.record_run("sess-A", 100, 3);
        log.record_run("sess-A", 142, 2); // updates same session
        log.record_run("sess-B", 7, 0);
        log.save(&path).expect("save ok");

        let loaded = CurationLog::load(&path);
        assert_eq!(loaded.last_id("sess-A"), Some(142));
        assert_eq!(loaded.last_id("sess-B"), Some(7));
        assert_eq!(loaded.last_id("sess-C"), None);
        assert_eq!(loaded.sessions["sess-A"].facts_added_total, 5);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn log_load_corrupt_falls_back_to_default() {
        let dir = std::env::temp_dir().join(format!(
            "cos-curator-log-corrupt-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("log.json");
        std::fs::write(&path, "{ this is not json").unwrap();
        let log = CurationLog::load(&path);
        assert_eq!(log.version, 1);
        assert!(log.sessions.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- format_transcript -------------------------------------------

    #[test]
    fn format_transcript_truncates_long_messages() {
        let rows = vec![MessageRow {
            id: 1,
            session_id: "s".into(),
            role: "user".into(),
            content: "x".repeat(2000),
            ts_ms: 0,
        }];
        let out = format_transcript(&rows, 100);
        assert!(out.contains("[user] "));
        assert!(out.contains("…"));
        assert!(out.len() < 200, "got len {}", out.len());
    }

    #[test]
    fn format_transcript_preserves_short_messages() {
        let rows = vec![MessageRow {
            id: 1,
            session_id: "s".into(),
            role: "assistant".into(),
            content: "short".into(),
            ts_ms: 0,
        }];
        let out = format_transcript(&rows, 100);
        assert!(out.contains("[assistant] short"));
        assert!(!out.contains("…"));
    }

    // ---- date math sanity --------------------------------------------

    #[test]
    fn days_to_ymd_known_anchors() {
        // Unix epoch: 1970-01-01.
        assert_eq!(days_to_ymd(0), (1970, 1, 1));
        // 2020-01-01 = 18262 days since epoch.
        assert_eq!(days_to_ymd(18262), (2020, 1, 1));
        // 2024-02-29 (leap day) = 19782 days since epoch.
        assert_eq!(days_to_ymd(19782), (2024, 2, 29));
    }

    // ---- end-to-end on in-memory db + temp notes ---------------------

    #[tokio::test]
    async fn curate_session_writes_facts_to_memory_md_and_log() {
        use crate::agent::llm::auxiliary::{AuxiliaryClient, AuxiliaryConfig};
        use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
        use crate::agent::llm::Provider;
        use crate::agent::memory::sqlite_fts::MemoryDb;
        use crate::config::AgentConfig;
        use std::sync::Arc;

        // Mock provider that always returns two well-formed facts.
        let cfg = AgentConfig::default();
        let provider = MockProvider::new("mock-aux", &cfg);
        provider.push_response(MockResponse::Text(
            r#"<fact category="preference" confidence="0.9">User prefers Rust over Go</fact>
<fact category="environment" confidence="0.95">User runs Windows 11 with PowerShell</fact>"#
                .to_string(),
        ));
        let provider: Arc<dyn Provider> = Arc::new(provider);
        let aux = AuxiliaryClient::new(provider, AuxiliaryConfig::new("mock", "mock-aux"));

        let db = MemoryDb::open_in_memory().expect("memory db");
        db.record_message("sess-1", "user", "I love Rust!").unwrap();
        db.record_message("sess-1", "assistant", "Noted.").unwrap();
        db.record_message("sess-1", "user", "I'm on Windows 11.")
            .unwrap();

        let dir = std::env::temp_dir().join(format!(
            "cos-curator-e2e-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let notes = NotesStore::at(dir.join("notes"));
        let log_path = dir.join("log.json");

        let curator = MemoryCurator::new(aux, notes.clone(), log_path.clone());
        let outcome = curator.curate_session(&db, "sess-1", false).await.unwrap();

        assert_eq!(outcome.facts_proposed.len(), 2);
        assert_eq!(outcome.facts_added.len(), 2);
        assert!(!outcome.skipped_no_new_messages);

        let mem = notes.read(MEMORY_FILE).unwrap().unwrap();
        assert!(mem.contains(SECTION_HEADER));
        assert!(mem.contains("User prefers Rust over Go"));
        assert!(mem.contains("User runs Windows 11 with PowerShell"));

        // Log persisted.
        let loaded = CurationLog::load(&log_path);
        assert_eq!(loaded.last_id("sess-1"), outcome.last_message_id);

        // Re-running should skip (no new messages).
        let again = curator.curate_session(&db, "sess-1", false).await.unwrap();
        assert!(again.skipped_no_new_messages);
        assert!(again.facts_added.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn curate_session_dedupes_against_existing_memory() {
        use crate::agent::llm::auxiliary::{AuxiliaryClient, AuxiliaryConfig};
        use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
        use crate::agent::llm::Provider;
        use crate::agent::memory::sqlite_fts::MemoryDb;
        use crate::config::AgentConfig;
        use std::sync::Arc;

        let cfg = AgentConfig::default();
        let provider = MockProvider::new("mock-aux", &cfg);
        provider.push_response(MockResponse::Text(
            r#"<fact category="preference" confidence="0.9">User prefers Rust over Go</fact>"#
                .to_string(),
        ));
        let provider: Arc<dyn Provider> = Arc::new(provider);
        let aux = AuxiliaryClient::new(provider, AuxiliaryConfig::new("mock", "mock-aux"));

        let db = MemoryDb::open_in_memory().unwrap();
        db.record_message("sess-1", "user", "stuff").unwrap();

        let dir = std::env::temp_dir().join(format!(
            "cos-curator-dedupe-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let notes = NotesStore::at(dir.join("notes"));

        // Pre-seed with the same fact (different casing / meta).
        notes
            .write(
                MEMORY_FILE,
                r#"# memory

## Curated facts (auto)
- [preference] user prefers rust over go _(2025-12-31, conf 0.85)_
"#,
            )
            .unwrap();

        let log_path = dir.join("log.json");
        let curator = MemoryCurator::new(aux, notes, log_path);
        let outcome = curator.curate_session(&db, "sess-1", false).await.unwrap();

        // LLM proposed one fact; dedupe filtered it out.
        assert_eq!(outcome.facts_proposed.len(), 1);
        assert_eq!(outcome.facts_added.len(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn curate_session_dry_run_skips_llm_call() {
        use crate::agent::llm::auxiliary::{AuxiliaryClient, AuxiliaryConfig};
        use crate::agent::llm::providers::mock::MockProvider;
        use crate::agent::llm::Provider;
        use crate::agent::memory::sqlite_fts::MemoryDb;
        use crate::config::AgentConfig;
        use std::sync::Arc;

        // Mock provider with NO scripted responses — so any LLM call
        // would either error or return empty. We assert dry_run stops
        // before getting there.
        let cfg = AgentConfig::default();
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new("mock-aux", &cfg));
        let aux = AuxiliaryClient::new(provider, AuxiliaryConfig::new("mock", "mock-aux"));

        let db = MemoryDb::open_in_memory().unwrap();
        db.record_message("sess-1", "user", "hi").unwrap();

        let dir = std::env::temp_dir().join(format!(
            "cos-curator-dry-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let notes = NotesStore::at(dir.join("notes"));

        let curator = MemoryCurator::new(aux, notes, dir.join("log.json"));
        let outcome = curator.curate_session(&db, "sess-1", true).await.unwrap();
        assert_eq!(outcome.messages_examined, 1);
        assert!(outcome.facts_proposed.is_empty());
        assert!(outcome.facts_added.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
