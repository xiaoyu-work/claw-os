//! Notes-style memory: `MEMORY.md` (agent's working memory) + `USER.md`
//! (user preferences) + arbitrary user-named notes.
//!
//! Both canonical files live under `data_dir/agent/notes/`. Reads are
//! file-locked; writes are atomic (tmp + rename) via [`crate::filelock`].
//!
//! Two integration points:
//! - The system prompt builder (`agent/prompt`) injects `MEMORY.md` and
//!   `USER.md` automatically at every turn so the model always has them.
//! - The `cos_memory` LLM tool exposes read / write / append / list so the
//!   model itself can update its own memory after a task. (Auto-curator
//!   in Phase 8 will do this for the model.)

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::filelock;

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
        let mut out = String::new();
        for name in ALL_KNOWN {
            if let Ok(Some(text)) = self.read(name) {
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let capped = truncate_for_prompt(trimmed, cap_chars);
                if !out.is_empty() {
                    out.push_str("\n\n");
                }
                out.push_str("# ");
                out.push_str(name);
                out.push_str("\n\n");
                out.push_str(&capped);
            }
        }
        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    }
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
    use super::*;

    fn tmpdir(label: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("cos-notes-{}-{}", label, std::process::id()));
        let _ = fs::remove_dir_all(&p);
        p
    }

    #[test]
    fn read_missing_returns_none() {
        let dir = tmpdir("read-missing");
        let s = NotesStore::at(&dir);
        assert_eq!(s.read("MEMORY.md").unwrap(), None);
    }

    #[test]
    fn write_then_read_roundtrips() {
        let dir = tmpdir("write-read");
        let s = NotesStore::at(&dir);
        s.write("MEMORY.md", "remember: pineapples").unwrap();
        let got = s.read("MEMORY.md").unwrap().unwrap();
        assert!(got.contains("pineapples"));
    }

    #[test]
    fn append_creates_then_extends() {
        let dir = tmpdir("append");
        let s = NotesStore::at(&dir);
        s.append("MEMORY.md", "fact: one").unwrap();
        s.append("MEMORY.md", "fact: two").unwrap();
        let got = s.read("MEMORY.md").unwrap().unwrap();
        assert!(got.contains("fact: one") && got.contains("fact: two"));
    }

    #[test]
    fn list_returns_md_files_only() {
        let dir = tmpdir("list");
        let s = NotesStore::at(&dir);
        s.write("MEMORY.md", "x").unwrap();
        s.write("USER.md", "y").unwrap();
        fs::write(dir.join("note.txt"), "ignored").ok();
        let names = s.list().unwrap();
        assert!(names.contains(&"MEMORY.md".to_string()));
        assert!(names.contains(&"USER.md".to_string()));
        assert!(!names.iter().any(|n| n == "note.txt"));
    }

    #[test]
    fn delete_is_idempotent() {
        let dir = tmpdir("delete");
        let s = NotesStore::at(&dir);
        s.write("MEMORY.md", "x").unwrap();
        s.delete("MEMORY.md").unwrap();
        s.delete("MEMORY.md").unwrap();
        assert_eq!(s.read("MEMORY.md").unwrap(), None);
    }

    #[test]
    fn assemble_for_prompt_concatenates_both_when_present() {
        let dir = tmpdir("assemble-both");
        let s = NotesStore::at(&dir);
        s.write("MEMORY.md", "I learned X").unwrap();
        s.write("USER.md", "User prefers Y").unwrap();
        let assembled = s.assemble_for_prompt().unwrap();
        assert!(assembled.contains("# MEMORY.md"));
        assert!(assembled.contains("I learned X"));
        assert!(assembled.contains("# USER.md"));
        assert!(assembled.contains("User prefers Y"));
    }

    #[test]
    fn assemble_for_prompt_skips_empty_files() {
        let dir = tmpdir("assemble-empty");
        let s = NotesStore::at(&dir);
        s.write("MEMORY.md", "   \n").unwrap();
        assert!(s.assemble_for_prompt().is_none());
    }

    #[test]
    fn assemble_for_prompt_returns_none_when_dir_missing() {
        let dir = tmpdir("assemble-missing");
        let s = NotesStore::at(&dir);
        assert!(s.assemble_for_prompt().is_none());
    }

    #[test]
    fn name_with_slash_is_rejected() {
        let dir = tmpdir("name-slash");
        let s = NotesStore::at(&dir);
        assert!(s.write("../escape.md", "x").is_err());
        assert!(s.write("a/b.md", "x").is_err());
    }

    #[test]
    fn name_without_md_extension_is_rejected() {
        let dir = tmpdir("name-ext");
        let s = NotesStore::at(&dir);
        assert!(s.write("MEMORY", "x").is_err());
        assert!(s.write("MEMORY.txt", "x").is_err());
    }

    #[test]
    fn truncate_for_prompt_passes_through_short_input() {
        let s = "hello world";
        let out = truncate_for_prompt(s, 32);
        assert_eq!(out, "hello world");
        assert!(matches!(out, std::borrow::Cow::Borrowed(_)));
    }

    #[test]
    fn truncate_for_prompt_caps_long_input() {
        let s = "x".repeat(2_000);
        let out = truncate_for_prompt(&s, 200);
        let chars: usize = out.chars().count();
        assert!(chars <= 200, "got {chars} chars, expected <= 200");
        assert!(
            out.contains("[…]"),
            "expected truncation marker, got {out:?}"
        );
        assert!(out.contains("of 2000 chars"));
    }

    #[test]
    fn truncate_for_prompt_is_multibyte_safe() {
        // 1000 four-byte chars × 1 char-budget per "char" → must
        // never panic on a UTF-8 boundary and the kept slice must
        // be valid UTF-8 (the pushed prefix is built from chars()).
        let s = "🦀".repeat(1_000);
        let out = truncate_for_prompt(&s, 100);
        // Marker present + start contains crab.
        assert!(out.starts_with('🦀'));
        assert!(out.contains("[…]"));
    }

    #[test]
    fn truncate_for_prompt_with_zero_cap_keeps_just_marker() {
        let s = "abc".repeat(100);
        let out = truncate_for_prompt(&s, 0);
        assert!(out.contains("[…]"));
        assert!(out.contains("of 300 chars"));
    }

    #[test]
    fn assemble_for_prompt_with_cap_truncates_oversized_note() {
        let dir = tmpdir("assemble-cap");
        let s = NotesStore::at(&dir);
        let big = "y".repeat(5_000);
        s.write("MEMORY.md", &big).unwrap();
        let assembled = s.assemble_for_prompt_with_cap(500).unwrap();
        assert!(assembled.contains("# MEMORY.md"));
        assert!(assembled.contains("[…]"));
        // The total must be small. Header + body + marker;
        // header is ~14 chars, marker ~80 chars, body capped to
        // (500 - 80) = 420.
        let body_chars = assembled.chars().count();
        assert!(
            body_chars < 700,
            "expected ≤ ~600 chars after capping, got {body_chars}"
        );
    }

    #[test]
    fn assemble_for_prompt_with_cap_passes_through_small_notes() {
        let dir = tmpdir("assemble-cap-small");
        let s = NotesStore::at(&dir);
        s.write("MEMORY.md", "tiny note").unwrap();
        let assembled = s.assemble_for_prompt_with_cap(1_024).unwrap();
        assert!(assembled.contains("tiny note"));
        assert!(!assembled.contains("[…]"));
    }

    #[test]
    fn assemble_for_prompt_default_uses_max_note_chars_constant() {
        // 100 KiB content, default cap 32 KiB → must truncate.
        let dir = tmpdir("assemble-default-cap");
        let s = NotesStore::at(&dir);
        let big = "z".repeat(MAX_NOTE_CHARS_FOR_PROMPT * 4);
        s.write("MEMORY.md", &big).unwrap();
        let assembled = s.assemble_for_prompt().unwrap();
        assert!(assembled.contains("[…]"));
        let chars = assembled.chars().count();
        // Header (~14) + body (≤ MAX_NOTE_CHARS_FOR_PROMPT) +
        // marker (~80). Allow some slack.
        assert!(
            chars < MAX_NOTE_CHARS_FOR_PROMPT + 256,
            "assembled prompt ({chars} chars) exceeded cap"
        );
    }

    #[test]
    fn assemble_for_prompt_per_file_cap_is_independent() {
        let dir = tmpdir("assemble-per-file");
        let s = NotesStore::at(&dir);
        // Both files individually within cap; make sure MEMORY's
        // truncation marker doesn't bleed into USER.
        let big = "a".repeat(2_000);
        s.write("MEMORY.md", &big).unwrap();
        s.write("USER.md", "small content").unwrap();
        let assembled = s.assemble_for_prompt_with_cap(500).unwrap();
        assert!(assembled.contains("# MEMORY.md"));
        assert!(assembled.contains("# USER.md"));
        assert!(assembled.contains("[…]"));
        assert!(assembled.contains("small content"));
        // Each # heading appears exactly once.
        assert_eq!(assembled.matches("# MEMORY.md").count(), 1);
        assert_eq!(assembled.matches("# USER.md").count(), 1);
    }
}
