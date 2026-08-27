//! Notes-style memory: `MEMORY.md` (agent's working memory) + `USER.md`
//! (user preferences) + arbitrary user-named notes.
//!
//! Both canonical files live under `data_dir/agent/notes/`. Reads are
//! file-locked; writes are atomic (tmp + rename) via [`crate::filelock`].
//!
//! Two integration points:
//! - The system prompt builder (`agent/prompt`) snapshots `MEMORY.md` and
//!   `USER.md` when a persisted session freezes its canonical prompt. Writes
//!   become automatic context in new sessions; the current session already
//!   retains the conversation that produced them and can read them explicitly.
//! - The `cos_memory` LLM tool exposes read / write / append / list so the
//!   model itself can update its own memory after a task. (Auto-curator
//!   in Phase 8 will do this for the model.)

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::filelock;

use super::ontology::{self, FactLifetime};

/// Canonical file name for the agent's working memory.
pub const MEMORY_FILE: &str = "MEMORY.md";
/// Canonical file name for user preferences / persona.
pub const USER_FILE: &str = "USER.md";

const ALL_KNOWN: &[&str] = &[MEMORY_FILE, USER_FILE];

/// Per-file character budget when injecting notes into the system
/// prompt. 32 KiB chars (≈ ~8K tokens for English) is generous for
/// hand-written guidance but guards against a runaway paste blowing
/// the context window. The cap applies *only* to prompt assembly —
/// `NotesStore::read` always returns the full file, so
/// `cos_memory read MEMORY.md` still gives the model the unedited
/// contents on demand.
pub const MAX_NOTE_CHARS_FOR_PROMPT: usize = 32_768;

#[derive(Debug, Clone)]
pub struct NotesStore {
    dir: PathBuf,
}

impl NotesStore {
    /// Notes store rooted at the system-default `data_dir/agent/notes/`.
    pub fn system_default() -> Self {
        Self {
            dir: crate::paths::agent_notes_dir(),
        }
    }

    /// Notes store rooted at an explicit directory (for tests / overrides).
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path_of(&self, name: &str) -> Result<PathBuf, String> {
        validate_name(name)?;
        Ok(self.dir.join(name))
    }

    fn ensure_dir(&self) -> Result<(), String> {
        fs::create_dir_all(&self.dir)
            .map_err(|e| format!("create_dir_all {}: {e}", self.dir.display()))
    }

    /// Read a note. Returns `Ok(None)` if the file does not exist (not an
    /// error — missing memory just means no memory yet).
    pub fn read(&self, name: &str) -> Result<Option<String>, String> {
        let p = self.path_of(name)?;
        // `filelock::read_locked` already short-circuits to
        // `Ok(None)` via `path.is_file()` when the path doesn't
        // exist; we don't need a redundant string-match against the
        // OS error message (which used to misclassify e.g. a
        // permission-denied error as "not found" on locales where
        // the message contains "no such file" in translation).
        filelock::read_locked(&p)
    }

    /// Replace a note's contents atomically.
    pub fn write(&self, name: &str, content: &str) -> Result<(), String> {
        self.ensure_dir()?;
        let p = self.path_of(name)?;
        filelock::write_locked(&p, content)
    }

    /// Read-modify-write a note while holding one exclusive lock.
    pub(crate) fn update<F>(&self, name: &str, transform: F) -> Result<(), String>
    where
        F: FnOnce(Option<String>) -> String,
    {
        self.ensure_dir()?;
        let p = self.path_of(name)?;
        filelock::update_locked::<_, std::convert::Infallible>(&p, |current| {
            Ok(transform(current))
        })
        .map_err(|error| error.to_string())
    }

    /// Append a line to a note (creates the file if missing).
    pub fn append(&self, name: &str, line: &str) -> Result<(), String> {
        self.ensure_dir()?;
        let p = self.path_of(name)?;
        filelock::append_locked(&p, line)
    }

    /// Delete a note. Returns Ok even if it didn't exist.
    pub fn delete(&self, name: &str) -> Result<(), String> {
        let p = self.path_of(name)?;
        match fs::remove_file(&p) {
            Ok(_) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("remove {}: {e}", p.display())),
        }
    }

    /// List all `.md` files in the notes directory.
    pub fn list(&self) -> Result<Vec<String>, String> {
        let mut out: Vec<String> = Vec::new();
        let read_dir = match fs::read_dir(&self.dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(format!("read_dir {}: {e}", self.dir.display())),
        };
        for entry in read_dir.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                if name.ends_with(".md") {
                    out.push(name.to_string());
                }
            }
        }
        out.sort();
        Ok(out)
    }

    /// Return concatenated content of `MEMORY.md` and `USER.md` for the
    /// system prompt. Either or both may be missing — `None` is returned if
    /// nothing useful exists. Each file is independently capped at
    /// [`MAX_NOTE_CHARS_FOR_PROMPT`] characters; oversized notes are
    /// truncated with a marker so the model is told what happened.
    pub fn assemble_for_prompt(&self) -> Option<String> {
        self.assemble_for_prompt_with_cap(MAX_NOTE_CHARS_FOR_PROMPT)
    }

    /// Same as [`assemble_for_prompt`] with an explicit per-file cap.
    /// Exposed for tests; production callers should use the default.
    pub fn assemble_for_prompt_with_cap(&self, cap_chars: usize) -> Option<String> {
        self.assemble_for_prompt_relevant(None, cap_chars)
    }

    /// Assemble notes for the prompt, selecting MEMORY.md entries by
    /// relevance to `query` when the file exceeds `cap_chars`.
    ///
    /// ## Tiers
    ///
    /// - **USER.md** — always-on in full (persona / preferences), capped.
    /// - **MEMORY.md** headings, prose, and any line containing
    ///   [`ALWAYS_TAG`] (case-insensitive) — always-on.
    /// - Other current MEMORY.md bullet entries — *contextual*: under budget
    ///   every projected entry is kept; over budget only the entries most
    ///   relevant to `query` are kept (or, when `query` is `None`, the
    ///   earliest entries), with a marker noting how many were dropped. The
    ///   full append-only file remains available via `cos_memory read`.
    ///
    /// Replacing blind truncation with relevance selection means a large,
    /// long-lived `MEMORY.md` no longer silently drops potentially-relevant
    /// facts just because they sit past the byte cap.
    pub fn assemble_for_prompt_relevant(
        &self,
        query: Option<&str>,
        cap_chars: usize,
    ) -> Option<String> {
        let mut out = String::new();
        for name in ALL_KNOWN {
            if let Ok(Some(text)) = self.read(name) {
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    continue;
                }
                // USER.md is the always-on persona tier: keep it whole
                // (capped). MEMORY.md is append-only, so project the
                // canonical current view first — otherwise aliases,
                // contradictions, expired observations, or corrected values
                // could reach the model together — then rank what remains.
                let selected = if name == &MEMORY_FILE {
                    let tails = project_chain_tails(trimmed);
                    select_memory_for_prompt(tails.trim(), query, cap_chars)
                } else {
                    truncate_for_prompt(trimmed, cap_chars).into_owned()
                };
                if selected.trim().is_empty() {
                    continue;
                }
                if !out.is_empty() {
                    out.push_str("\n\n");
                }
                out.push_str("# ");
                out.push_str(name);
                out.push_str("\n\n");
                out.push_str(&selected);
            }
        }
        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    }
}

/// Marker that pins a `MEMORY.md` entry to the always-on tier whenever a
/// session prompt snapshot is assembled. Case-insensitive.
pub const ALWAYS_TAG: &str = "[always]";

/// Project the current governed view of curated entries.
///
/// `MEMORY.md` is append-only: when a fact changes, the new value is
/// appended rather than overwriting the old one, so the file keeps the
/// whole history (`editor.name = vim` … later … `editor.name = helix`).
/// Aliases are canonicalized for comparison, volatile observations expire,
/// and cross-slot contradictions such as `tool.installation = not_found`
/// versus `tool.version = 1.2.3` are resolved by append order.
///
/// The file is the store and this is the view: only the newest current,
/// non-conflicting tail is projected. Superseded and expired entries stay on
/// disk and remain readable via `cos_memory read MEMORY.md`.
///
/// Unstructured lines have no key and are always kept — we cannot tell
/// whether two prose sentences describe the same slot, which is exactly
/// why facts are structured now.
fn project_chain_tails(content: &str) -> String {
    project_chain_tails_at(content, current_epoch_days())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CurrentStructuredValue {
    pub value: String,
    pub lifetime: Option<FactLifetime>,
}

pub(crate) fn current_structured_values(
    content: &str,
) -> std::collections::HashMap<String, CurrentStructuredValue> {
    projection_at(content, current_epoch_days()).current
}

fn project_chain_tails_at(content: &str, now_days: u64) -> String {
    let projection = projection_at(content, now_days);
    if projection.structured_indices.is_empty() {
        return content.to_string();
    }

    let mut out = String::new();
    for (index, line) in content.lines().enumerate() {
        if projection.structured_indices.contains(&index) && !projection.keep.contains(&index) {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

#[derive(Debug)]
struct CuratedEntry {
    index: usize,
    key: String,
    entity: String,
    attribute: String,
    value: String,
    metadata: CuratedMetadata,
}

#[derive(Debug, Default)]
struct CuratedMetadata {
    lifetime: Option<FactLifetime>,
    observed_at_days: Option<u64>,
    ttl_days: Option<u32>,
}

#[derive(Debug, Default)]
struct Projection {
    structured_indices: std::collections::HashSet<usize>,
    keep: std::collections::HashSet<usize>,
    current: std::collections::HashMap<String, CurrentStructuredValue>,
}

fn projection_at(content: &str, now_days: u64) -> Projection {
    use std::collections::{HashMap, HashSet};

    let entries: Vec<CuratedEntry> = content
        .lines()
        .enumerate()
        .filter_map(|(index, line)| parse_curated_entry(index, line))
        .collect();

    let structured_indices = entries.iter().map(|entry| entry.index).collect();
    let mut tail_by_key: HashMap<&str, usize> = HashMap::new();
    for entry in &entries {
        tail_by_key.insert(&entry.key, entry.index);
    }
    let mut keep: HashSet<usize> = tail_by_key.values().copied().collect();

    // A version proves presence. A later explicit not-found observation proves
    // absence. Whichever was appended later supersedes the contradictory
    // cross-slot fact, while both remain in the raw history.
    let mut absent_installation: HashMap<&str, usize> = HashMap::new();
    let mut versions: HashMap<&str, usize> = HashMap::new();
    for entry in &entries {
        if !keep.contains(&entry.index) {
            continue;
        }
        if entry.attribute == "installation" && ontology::installation_is_absent(&entry.value) {
            absent_installation.insert(&entry.entity, entry.index);
        } else if entry.attribute == "version" {
            versions.insert(&entry.entity, entry.index);
        }
    }
    for (entity, installation_index) in absent_installation {
        if let Some(version_index) = versions.get(entity).copied() {
            keep.remove(&installation_index.min(version_index));
        }
    }

    let by_index: HashMap<usize, &CuratedEntry> =
        entries.iter().map(|entry| (entry.index, entry)).collect();
    keep.retain(|index| {
        by_index
            .get(index)
            .is_some_and(|entry| !entry.metadata.is_excluded(now_days))
    });

    let current = keep
        .iter()
        .filter_map(|index| by_index.get(index))
        .map(|entry| {
            (
                entry.key.clone(),
                CurrentStructuredValue {
                    value: entry.value.clone(),
                    lifetime: entry.metadata.effective_lifetime(),
                },
            )
        })
        .collect();

    Projection {
        structured_indices,
        keep,
        current,
    }
}

fn parse_curated_entry(index: usize, line: &str) -> Option<CuratedEntry> {
    let trimmed = line.trim();
    let rest = trimmed.strip_prefix("- [")?;
    let close = rest.find("] ")?;
    let after = &rest[close + 2..];
    let (body, metadata) = match after.rsplit_once(" _(") {
        Some((body, metadata)) if metadata.ends_with(")_") => (
            body.trim(),
            parse_curated_metadata(metadata.strip_suffix(")_").unwrap_or(metadata)),
        ),
        _ => (after.trim(), CuratedMetadata::default()),
    };
    let (key, value) = ontology::split_structured_body(body)?;
    let (entity, attribute) = key.split_once('.')?;
    let slot = ontology::normalize_slot(entity, attribute, &value)?;
    Some(CuratedEntry {
        index,
        key: slot.key(),
        entity: slot.entity,
        attribute: slot.attribute,
        value: slot.value,
        metadata,
    })
}

fn parse_curated_metadata(metadata: &str) -> CuratedMetadata {
    let mut out = CuratedMetadata::default();
    for field in metadata.split(',').map(str::trim) {
        let Some((key, value)) = field.split_once('=').or_else(|| field.split_once(':')) else {
            continue;
        };
        match key.trim() {
            "lifetime" => out.lifetime = FactLifetime::parse(value),
            "observed_at" => out.observed_at_days = ontology::date_to_epoch_days(value),
            "ttl" | "ttl_days" => {
                out.ttl_days = value
                    .trim()
                    .trim_end_matches(['d', 'D'])
                    .parse::<u32>()
                    .ok()
            }
            _ => {}
        }
    }
    out
}

impl CuratedMetadata {
    fn effective_lifetime(&self) -> Option<FactLifetime> {
        self.lifetime
            .or_else(|| self.ttl_days.map(|_| FactLifetime::Observed))
    }

    fn is_excluded(&self, now_days: u64) -> bool {
        match self.effective_lifetime() {
            Some(FactLifetime::Session | FactLifetime::Procedure) => true,
            Some(FactLifetime::Durable) => false,
            Some(FactLifetime::Observed) => self.is_expired_observation(now_days),
            None => false,
        }
    }

    fn is_expired_observation(&self, now_days: u64) -> bool {
        let Some(observed_at) = self.observed_at_days else {
            return true;
        };
        let ttl_days = self
            .ttl_days
            .unwrap_or(ontology::DEFAULT_OBSERVED_TTL_DAYS)
            .clamp(1, ontology::MAX_OBSERVED_TTL_DAYS);
        now_days >= observed_at.saturating_add(u64::from(ttl_days))
    }
}

fn current_epoch_days() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() / 86_400)
        .unwrap_or(0)
}

/// Is this line a contextual bullet entry (rankable / droppable), as
/// opposed to a pinned structural line (heading, prose, or an entry
/// explicitly tagged always-on)?
fn is_contextual_bullet(line: &str) -> bool {
    let t = line.trim_start();
    let is_bullet = t.starts_with("- ") || t.starts_with("* ");
    is_bullet && !line.to_ascii_lowercase().contains(ALWAYS_TAG)
}

/// Tokenise into lowercase alphanumeric words of length >= 3 (drops
/// punctuation and trivially-short tokens). Used by the lexical relevance
/// scorer below.
fn tokenize(text: &str) -> std::collections::HashSet<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 3)
        .map(|w| w.to_string())
        .collect()
}

/// Relevance of a memory `entry` to the current `query`: the number of
/// distinct query tokens that also appear in the entry. Deterministic,
/// synchronous, dependency-free — good enough to decide *which* facts to
/// drop under budget pressure. (A semantic embedder could replace this
/// scorer later without changing the selection logic.)
fn lexical_relevance(query_tokens: &std::collections::HashSet<String>, entry: &str) -> usize {
    if query_tokens.is_empty() {
        return 0;
    }
    let entry_tokens = tokenize(entry);
    query_tokens.iter().filter(|t| entry_tokens.contains(*t)).count()
}

/// Select which of `memory.md`'s contextual entries to inject, keeping
/// all pinned (structural / always-on) lines and filling the remaining
/// budget with the entries most relevant to `query`.
///
/// Fast path: when the whole file fits in `cap_chars` it is returned
/// unchanged — so existing small-memory behaviour is byte-for-byte
/// preserved and relevance logic only engages under real budget pressure.
fn select_memory_for_prompt(content: &str, query: Option<&str>, cap_chars: usize) -> String {
    if content.chars().count() <= cap_chars {
        return content.to_string();
    }

    let lines: Vec<&str> = content.lines().collect();
    let contextual_idx: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| is_contextual_bullet(l))
        .map(|(i, _)| i)
        .collect();

    // Pinned lines (everything not a contextual bullet) are always kept.
    let pinned_chars: usize = lines
        .iter()
        .enumerate()
        .filter(|(i, _)| !contextual_idx.contains(i))
        .map(|(_, l)| l.chars().count() + 1) // +1 for the newline
        .sum();

    let mut remaining = cap_chars.saturating_sub(pinned_chars);
    // Reserve room for the omission marker so appending it later can't
    // push us back over `cap` and trigger the safety-truncate (which
    // would chop selected-but-late entries out of the document-order
    // reassembly). Only reserve when there are contextual entries that
    // could be dropped.
    const OMIT_MARKER_RESERVE: usize = 160;
    if !contextual_idx.is_empty() {
        remaining = remaining.saturating_sub(OMIT_MARKER_RESERVE.min(remaining));
    }

    // Rank contextual entries: by relevance to the query (desc), then by
    // original position (asc) for a stable, readable result. With no
    // query, score is 0 for all → pure original order (bullet-aligned
    // truncation, never a mid-line cut).
    let query_tokens = query.map(tokenize).unwrap_or_default();
    let mut ranked: Vec<(usize, usize)> = contextual_idx
        .iter()
        .map(|&i| (i, lexical_relevance(&query_tokens, lines[i])))
        .collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    let mut keep: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for (idx, _score) in ranked {
        let cost = lines[idx].chars().count() + 1;
        if cost <= remaining {
            keep.insert(idx);
            remaining -= cost;
        }
    }

    let dropped = contextual_idx.len() - keep.len();

    // Reassemble in original document order for readability.
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        let is_ctx = contextual_idx.contains(&i);
        if !is_ctx || keep.contains(&i) {
            out.push_str(line);
            out.push('\n');
        }
    }
    if dropped > 0 {
        out.push_str(&format!(
            "\n[\u{2026}] ({dropped} less-relevant memory entries omitted for this turn; full memory via `cos_memory read MEMORY.md`)\n"
        ));
    }

    // Degenerate safety net: if pinned content alone blew the budget,
    // hard-cap the whole thing.
    truncate_for_prompt(out.trim_end(), cap_chars).into_owned()
}

/// Cap a note to at most `cap_chars` characters. If the input is
/// already small enough, it is returned unchanged (no allocation).
/// Otherwise the head is kept and a single-line marker is appended.
/// `cap_chars == 0` returns just the marker so callers always learn
/// the original size.
///
/// Multibyte-safe: counts and slices by `char` boundaries, so a note
/// full of CJK or emoji characters never panics on a partial UTF-8
/// boundary.
pub fn truncate_for_prompt(text: &str, cap_chars: usize) -> std::borrow::Cow<'_, str> {
    let total_chars = text.chars().count();
    if total_chars <= cap_chars {
        return std::borrow::Cow::Borrowed(text);
    }
    // We need room for both the kept body and the marker. Reserve
    // a small portion of the budget for the marker so an extreme
    // cap (e.g. 16) still produces a usable string. The marker is
    // bounded; subtract its conservative upper-bound length.
    let marker_reserve = MARKER_RESERVE_CHARS.min(cap_chars);
    let body_budget = cap_chars.saturating_sub(marker_reserve);
    let mut out = String::with_capacity(text.len().min(body_budget * 4 + 128));
    out.extend(text.chars().take(body_budget));
    out.push_str(&format!(
        "\n\n[…] (truncated for prompt; kept {kept} of {total} chars)",
        kept = body_budget,
        total = total_chars,
    ));
    std::borrow::Cow::Owned(out)
}

const MARKER_RESERVE_CHARS: usize = 80;

fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("note name must not be empty".into());
    }
    // Reject path separators outright but only the *exact* `..`
    // component, not any substring containing `..`. The old
    // substring check rejected legitimate names like "foo..bar.md"
    // even though they're single-component filenames.
    if name.contains('/') || name.contains('\\') {
        return Err(format!(
            "note name '{name}' must not contain path separators"
        ));
    }
    if name == ".." || name == "." || name.starts_with("../") || name.starts_with("./") {
        return Err(format!(
            "note name '{name}' must not be a path-traversal component"
        ));
    }
    // Path::Component sanity check — handles platform-specific
    // surprises like backslash on Windows we missed above.
    for c in std::path::Path::new(name).components() {
        match c {
            std::path::Component::Normal(_) => {}
            _ => {
                return Err(format!(
                    "note name '{name}' must be a single filename component"
                ));
            }
        }
    }
    if !name.ends_with(".md") {
        return Err(format!("note name '{name}' must end with .md"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/memory/notes.rs"
    ));
}
