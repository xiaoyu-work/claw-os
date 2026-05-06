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
        match filelock::read_locked(&p) {
            Ok(opt) => Ok(opt),
            Err(e) if is_not_found(&e) => Ok(None),
            Err(e) => Err(e),
        }
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
    /// nothing useful exists.
    pub fn assemble_for_prompt(&self) -> Option<String> {
        let mut out = String::new();
        for name in ALL_KNOWN {
            if let Ok(Some(text)) = self.read(name) {
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if !out.is_empty() {
                    out.push_str("\n\n");
                }
                out.push_str("# ");
                out.push_str(name);
                out.push_str("\n\n");
                out.push_str(trimmed);
            }
        }
        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    }
}

fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("note name must not be empty".into());
    }
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        return Err(format!(
            "note name '{name}' must not contain path separators or '..'"
        ));
    }
    if !name.ends_with(".md") {
        return Err(format!("note name '{name}' must end with .md"));
    }
    Ok(())
}

fn is_not_found(err: &str) -> bool {
    err.contains("(os error 2)") || err.to_ascii_lowercase().contains("not found")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(label: &str) -> PathBuf {
        let p = std::env::temp_dir()
            .join(format!("cos-notes-{}-{}", label, std::process::id()));
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
}
