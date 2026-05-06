//! High-level **context engine** that composes per-turn project context
//! from the lower-level pure utilities ([`subdir_hints`] + [`references`]).
//!
//! Hermes parity: this is the Rust analogue of `agent/context_engine.py`.
//! It is **pure** — no LLM calls, no I/O beyond the directory scan that
//! [`subdir_hints`] does. The runtime layer decides what to do with the
//! returned [`ContextBlock`] (typically prepended to the system prompt
//! or injected as a leading `developer`/`user` message).
//!
//! ## What lives in a [`ContextBlock`]
//!
//! 1. **Subdir hints**  — what's on disk at the working directory
//!    (Cargo.toml, package.json, .git, .vscode, Dockerfile, ...).
//!    Rendered via [`subdir_hints::render_summary`] and grouped by label.
//! 2. **References** — `@`-prefixed file/URL mentions in the user's
//!    latest turn (e.g. `@notes.md`, `@./src/lib.rs`, `@https://...`).
//!    Extracted via [`references::extract_unique`] so a body that says
//!    `@a.txt @a.txt` only surfaces once.
//! 3. **Free-form notes**  — caller-supplied lines (e.g. "running on
//!    Windows", "user has 12 MB of free disk"); appended verbatim.
//!
//! The engine renders each section into an XML-ish tag block to make it
//! easy for the model to anchor on, e.g.:
//!
//! ```text
//! <PROJECT_CONTEXT>
//! Project hints (cwd: C:\Users\me\proj):
//!   - Git repository
//!   - Rust crate (Cargo.toml)
//!
//! User references in this turn:
//!   - notes.md (Path)
//!   - https://example.com (Url)
//!
//! Notes:
//!   - host: Windows 11
//! </PROJECT_CONTEXT>
//! ```
//!
//! Sections that produce no content are simply omitted; if **all**
//! sections are empty, the engine returns `None` (caller should not
//! emit an empty block).
//!
//! ## Determinism
//!
//! Every section sort-orders its inputs, so the same `(cwd, user_text,
//! notes)` triple always yields the same string. Tests rely on this.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::agent::context::references::{self, Reference, ReferenceKind};
use crate::agent::context::subdir_hints::{self, Hint};

/// Configuration knobs for a context build. Defaults are conservative:
/// single-level scan, no recursion (depth = 0).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextOptions {
    /// Working directory to inspect for hints. `None` skips the
    /// scan entirely.
    pub cwd: Option<PathBuf>,
    /// How deep into `cwd` to recurse. `0` = single-level (just `cwd`).
    pub scan_depth: usize,
    /// User-message body to mine for `@`-references. `None` skips.
    pub user_text: Option<String>,
    /// Free-form notes to append to the rendered block. Lines are
    /// printed verbatim (one per element).
    pub notes: Vec<String>,
    /// Whether to deduplicate references via
    /// [`references::extract_unique`]. Default `true`.
    pub dedup_refs: bool,
    /// Cap on the number of references included in the block. `None`
    /// = unbounded.
    pub max_refs: Option<usize>,
    /// Cap on the number of subdir hints included. `None` = unbounded.
    pub max_hints: Option<usize>,
}

impl Default for ContextOptions {
    fn default() -> Self {
        Self {
            cwd: None,
            scan_depth: 0,
            user_text: None,
            notes: Vec::new(),
            dedup_refs: true,
            max_refs: Some(20),
            max_hints: Some(50),
        }
    }
}

impl ContextOptions {
    pub fn with_cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }
    pub fn with_user_text(mut self, text: impl Into<String>) -> Self {
        self.user_text = Some(text.into());
        self
    }
    pub fn with_scan_depth(mut self, depth: usize) -> Self {
        self.scan_depth = depth;
        self
    }
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }
}

/// Output of [`build`] — both the rendered block (`Display`-friendly)
/// and the structured pieces (so callers can do their own rendering).
#[derive(Debug, Clone)]
pub struct ContextBlock {
    pub hints: Vec<Hint>,
    pub references: Vec<Reference>,
    pub notes: Vec<String>,
    pub cwd: Option<PathBuf>,
    rendered: Option<String>,
}

impl ContextBlock {
    /// `true` if this block has nothing to render — every section is
    /// empty.
    pub fn is_empty(&self) -> bool {
        self.hints.is_empty() && self.references.is_empty() && self.notes.is_empty()
    }

    /// Returns the pre-rendered XML-ish block, or an empty string if
    /// the block is empty.
    pub fn rendered(&self) -> &str {
        self.rendered.as_deref().unwrap_or("")
    }

    /// Returns the rendered block as `Some(_)` if non-empty, else `None`
    /// — convenient for callers that conditionally splice context into
    /// their prompt.
    pub fn rendered_opt(&self) -> Option<&str> {
        match &self.rendered {
            Some(s) if !s.is_empty() => Some(s.as_str()),
            _ => None,
        }
    }

    /// Structured JSON view, suitable for the CLI / debug.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "cwd":   self.cwd.as_ref().map(|p| p.display().to_string()),
            "hints": self.hints.iter().map(hint_to_json).collect::<Vec<_>>(),
            "references": self.references.iter().map(reference_to_json).collect::<Vec<_>>(),
            "notes":     self.notes,
            "rendered":  self.rendered,
            "is_empty":  self.is_empty(),
        })
    }
}

/// Light-weight JSON-friendly view of a single subdir hint.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct HintView {
    kind: String,
    label: &'static str,
    rel: String,
    is_dir: bool,
}

fn hint_to_json(h: &Hint) -> serde_json::Value {
    serde_json::json!({
        "kind":   format!("{:?}", h.kind),
        "label":  h.label,
        "rel":    h.rel,
        "is_dir": h.is_dir,
    })
}

fn reference_to_json(r: &Reference) -> serde_json::Value {
    serde_json::json!({
        "raw":   r.raw,
        "kind":  format!("{:?}", r.kind),
        "start": r.start,
        "end":   r.end,
    })
}

/// Build a [`ContextBlock`] for the given options.
///
/// The function is pure with respect to `opts.user_text` and `opts.notes`,
/// and only does I/O when `opts.cwd` is `Some` (one directory scan).
pub fn build(opts: &ContextOptions) -> ContextBlock {
    // 1. Hints.
    let mut hints: Vec<Hint> = match &opts.cwd {
        Some(cwd) => {
            if opts.scan_depth == 0 {
                subdir_hints::scan_dir(cwd)
            } else {
                subdir_hints::scan_dir_recursive(cwd, opts.scan_depth)
            }
        }
        None => Vec::new(),
    };
    if let Some(cap) = opts.max_hints {
        hints.truncate(cap);
    }

    // 2. References.
    let mut references: Vec<Reference> = match &opts.user_text {
        Some(text) if !text.is_empty() => {
            if opts.dedup_refs {
                references::extract_unique(text)
            } else {
                references::extract(text)
            }
        }
        _ => Vec::new(),
    };
    if let Some(cap) = opts.max_refs {
        references.truncate(cap);
    }

    let notes = opts.notes.clone();
    let rendered = render_block(&hints, &references, &notes, opts.cwd.as_deref());

    ContextBlock {
        hints,
        references,
        notes,
        cwd: opts.cwd.clone(),
        rendered,
    }
}

/// Render the XML-ish PROJECT_CONTEXT block. Returns `None` when every
/// section is empty.
fn render_block(
    hints: &[Hint],
    references: &[Reference],
    notes: &[String],
    cwd: Option<&Path>,
) -> Option<String> {
    if hints.is_empty() && references.is_empty() && notes.is_empty() {
        return None;
    }
    let mut out = String::with_capacity(256);
    out.push_str("<PROJECT_CONTEXT>\n");

    if !hints.is_empty() {
        // Optionally prefix with cwd so the model knows the scan root.
        if let Some(p) = cwd {
            out.push_str(&format!("cwd: {}\n", p.display()));
        }
        out.push_str(&subdir_hints::render_summary(hints));
        out.push('\n');
    }

    if !references.is_empty() {
        if !hints.is_empty() {
            out.push('\n');
        }
        out.push_str("User references in this turn:\n");
        for r in references {
            out.push_str(&format!("  - {} ({})\n", r.raw, kind_label(r.kind)));
        }
    }

    if !notes.is_empty() {
        if !hints.is_empty() || !references.is_empty() {
            out.push('\n');
        }
        out.push_str("Notes:\n");
        for n in notes {
            out.push_str(&format!("  - {n}\n"));
        }
    }

    out.push_str("</PROJECT_CONTEXT>");
    Some(out)
}

fn kind_label(k: ReferenceKind) -> &'static str {
    match k {
        ReferenceKind::Url => "Url",
        ReferenceKind::RelativePath => "RelativePath",
        ReferenceKind::AbsolutePath => "AbsolutePath",
        ReferenceKind::Path => "Path",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "cos-context-engine-{}-{}",
            name,
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    // ---- empty / defaults ----

    #[test]
    fn empty_options_yield_empty_block() {
        let block = build(&ContextOptions::default());
        assert!(block.is_empty());
        assert_eq!(block.rendered(), "");
        assert_eq!(block.rendered_opt(), None);
        assert_eq!(block.hints.len(), 0);
        assert_eq!(block.references.len(), 0);
        assert_eq!(block.notes.len(), 0);
    }

    #[test]
    fn empty_user_text_does_not_yield_references() {
        let block = build(&ContextOptions::default().with_user_text(""));
        assert!(block.is_empty());
    }

    // ---- hints ----

    #[test]
    fn hints_only_render_in_block() {
        let dir = temp_dir("hints-only");
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        let block = build(&ContextOptions::default().with_cwd(&dir));
        assert!(!block.is_empty());
        assert_eq!(block.hints.len(), 1);
        assert_eq!(block.hints[0].label, "Rust crate");
        let rendered = block.rendered();
        assert!(rendered.contains("<PROJECT_CONTEXT>"));
        assert!(rendered.contains("</PROJECT_CONTEXT>"));
        assert!(rendered.contains("Project hints"));
        assert!(rendered.contains("Rust crate"));
        assert!(!rendered.contains("User references"));
        assert!(!rendered.contains("Notes:"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hints_recursive_scan_finds_nested_manifests() {
        let dir = temp_dir("hints-recur");
        let nested = dir.join("apps").join("web");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("package.json"), "{}").unwrap();
        let depth0 = build(&ContextOptions::default().with_cwd(&dir));
        assert_eq!(depth0.hints.len(), 0);
        let depth3 = build(
            &ContextOptions::default()
                .with_cwd(&dir)
                .with_scan_depth(3),
        );
        assert_eq!(depth3.hints.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hint_cap_is_respected() {
        let dir = temp_dir("hints-cap");
        std::fs::write(dir.join("Cargo.toml"), "").unwrap();
        std::fs::write(dir.join("package.json"), "{}").unwrap();
        std::fs::write(dir.join("Dockerfile"), "FROM alpine").unwrap();
        let mut opts = ContextOptions::default().with_cwd(&dir);
        opts.max_hints = Some(2);
        let block = build(&opts);
        assert_eq!(block.hints.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- references ----

    #[test]
    fn references_only_render_in_block() {
        let block = build(
            &ContextOptions::default()
                .with_user_text("see @notes.md and @https://example.com/x"),
        );
        assert_eq!(block.references.len(), 2);
        let r = block.rendered();
        assert!(r.contains("<PROJECT_CONTEXT>"));
        assert!(r.contains("User references in this turn:"));
        assert!(r.contains("notes.md (Path)"));
        assert!(r.contains("https://example.com/x (Url)"));
        assert!(!r.contains("Project hints"));
    }

    #[test]
    fn references_dedup_default_collapses_duplicates() {
        let block = build(
            &ContextOptions::default().with_user_text("@a @a @a"),
        );
        assert_eq!(block.references.len(), 1);
    }

    #[test]
    fn references_dedup_off_keeps_all_occurrences() {
        let mut opts = ContextOptions::default().with_user_text("@a @a @a");
        opts.dedup_refs = false;
        let block = build(&opts);
        assert_eq!(block.references.len(), 3);
    }

    #[test]
    fn reference_cap_is_respected() {
        let body = "@a @b @c @d @e";
        let mut opts = ContextOptions::default().with_user_text(body);
        opts.max_refs = Some(2);
        let block = build(&opts);
        assert_eq!(block.references.len(), 2);
    }

    // ---- notes ----

    #[test]
    fn notes_only_render_in_block() {
        let block = build(
            &ContextOptions::default()
                .with_note("host: Windows 11")
                .with_note("user has 12 MB free"),
        );
        assert_eq!(block.notes.len(), 2);
        let r = block.rendered();
        assert!(r.contains("<PROJECT_CONTEXT>"));
        assert!(r.contains("Notes:"));
        assert!(r.contains("host: Windows 11"));
        assert!(r.contains("user has 12 MB free"));
    }

    // ---- composition ----

    #[test]
    fn full_block_concatenates_all_sections_in_order() {
        let dir = temp_dir("hints-full");
        std::fs::write(dir.join("Cargo.toml"), "").unwrap();
        let block = build(
            &ContextOptions::default()
                .with_cwd(&dir)
                .with_user_text("see @lib.rs")
                .with_note("test mode"),
        );
        let r = block.rendered();
        let pos_hints = r.find("Project hints").expect("hints present");
        let pos_refs = r.find("User references").expect("refs present");
        let pos_notes = r.find("Notes:").expect("notes present");
        assert!(pos_hints < pos_refs);
        assert!(pos_refs < pos_notes);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn deterministic_output_for_same_input() {
        let dir = temp_dir("hints-determ");
        std::fs::write(dir.join("Cargo.toml"), "").unwrap();
        std::fs::write(dir.join("package.json"), "{}").unwrap();
        let opts = ContextOptions::default()
            .with_cwd(&dir)
            .with_user_text("@a @b")
            .with_note("z")
            .with_note("y");
        let a = build(&opts);
        let b = build(&opts);
        assert_eq!(a.rendered(), b.rendered());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- to_json ----

    #[test]
    fn to_json_roundtrips_basic_fields() {
        let block = build(
            &ContextOptions::default().with_user_text("@a"),
        );
        let v = block.to_json();
        assert_eq!(v.get("is_empty").and_then(|b| b.as_bool()), Some(false));
        let refs = v.get("references").and_then(|r| r.as_array()).unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(
            refs[0].get("raw").and_then(|s| s.as_str()),
            Some("a")
        );
        assert!(v.get("rendered").and_then(|s| s.as_str()).unwrap_or("").contains("PROJECT_CONTEXT"));
    }

    #[test]
    fn to_json_empty_block_marked_empty() {
        let block = build(&ContextOptions::default());
        let v = block.to_json();
        assert_eq!(v.get("is_empty").and_then(|b| b.as_bool()), Some(true));
        assert!(v.get("rendered").map(|x| x.is_null()).unwrap_or(false));
    }
}
