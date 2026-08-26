//! Filesystem-backed skills loader.
//!
//! Scans a root directory for `<skill-name>/SKILL.md` files, parses
//! each via [`super::manifest::parse`], and returns a registry the
//! runtime can consult.
//!
//! ## Layout
//!
//! ```text
//! <skills_root>/
//! ├── pdf-extract/
//! │   ├── SKILL.md
//! │   └── extract.py
//! ├── arxiv/
//! │   └── SKILL.md
//! └── godmode/
//!     └── SKILL.md          <- disabled by default
//! ```
//!
//! ## Default-disabled skills
//!
//! Per Q11 in the migration plan, two categories are vendored but
//! refused on load unless the user opts in explicitly:
//!
//!   * any skill whose canonical name starts with `red-teaming/`
//!     or `godmode`
//!   * any skill whose canonical name is `yuanbao` or starts with
//!     `yuanbao/`
//!
//! These show up in [`LoadResult::disabled`] with the reason, never
//! in [`LoadResult::skills`].
//!
//! ## Per-skill failure isolation
//!
//! A malformed skill never blocks discovery of others. Each failure
//! is captured in [`LoadResult::errors`] as
//! `(skill_dir_name, error_message)` so the loader can keep going.
//!
//! ## ID derivation
//!
//! The skill's *id* is the directory name; the manifest's `name`
//! field is treated as a human-readable label and may differ. When
//! two skill directories declare the same id (i.e. the same
//! directory name) only the first wins (sorted alphabetically),
//! and the loser is recorded in `errors` — duplicate ids would let
//! one skill silently shadow another.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use super::manifest::{self, SkillManifest};

/// One loaded skill: directory id, parsed manifest, file paths.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedSkill {
    pub id: String,
    pub dir: PathBuf,
    pub manifest_path: PathBuf,
    pub manifest: SkillManifest,
    pub body: String,
}

/// Outcome of loading a skills root directory.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LoadResult {
    /// Successfully loaded skills, keyed by id (directory name).
    pub skills: BTreeMap<String, LoadedSkill>,
    /// Skills that exist on disk but were refused due to the default
    /// deny-list. Key is the id, value is the human-readable reason.
    pub disabled: BTreeMap<String, String>,
    /// Skills that failed to load (missing/malformed SKILL.md, IO
    /// error, etc.). Key is the id, value is the error message.
    pub errors: BTreeMap<String, String>,
}

impl LoadResult {
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty() && self.disabled.is_empty() && self.errors.is_empty()
    }

    pub fn loaded_count(&self) -> usize {
        self.skills.len()
    }
}

/// Configuration knobs for [`load_dir`].
#[derive(Debug, Clone)]
pub struct LoadOptions {
    /// Override the deny-list. Defaults to the Q11 list. Pass an
    /// empty `Vec` to disable all deny-listing.
    pub deny_list: Vec<DenyRule>,
    /// Maximum SKILL.md size in bytes. Files larger than this are
    /// rejected with a load error (defends against pathological
    /// inputs; default 1 MiB).
    pub max_manifest_bytes: u64,
}

impl Default for LoadOptions {
    fn default() -> Self {
        Self {
            deny_list: default_deny_list(),
            max_manifest_bytes: 1024 * 1024,
        }
    }
}

/// One pattern in the deny-list. Matched against the canonical
/// skill id (lower-cased directory name).
#[derive(Debug, Clone, PartialEq)]
pub enum DenyRule {
    Exact(String),
    Prefix(String),
}

impl DenyRule {
    fn matches(&self, id_lower: &str) -> bool {
        match self {
            DenyRule::Exact(s) => id_lower == s,
            DenyRule::Prefix(s) => id_lower == s || id_lower.starts_with(&format!("{s}-")),
        }
    }

    fn reason(&self) -> String {
        match self {
            DenyRule::Exact(s) => format!("disabled by default (matches `{s}`)"),
            DenyRule::Prefix(s) => format!("disabled by default (matches prefix `{s}`)"),
        }
    }
}

/// The Q11 default deny-list: red-teaming/godmode + yuanbao.
pub fn default_deny_list() -> Vec<DenyRule> {
    vec![
        DenyRule::Prefix("red-teaming".to_string()),
        DenyRule::Prefix("godmode".to_string()),
        DenyRule::Exact("godmode".to_string()),
        DenyRule::Prefix("yuanbao".to_string()),
        DenyRule::Exact("yuanbao".to_string()),
    ]
}

/// Load all skills from the system-default
/// [`crate::paths::agent_skills_dir`].
pub fn load_default() -> LoadResult {
    load_dir(&crate::paths::agent_skills_dir(), &LoadOptions::default())
}

/// Load all skills from the given root with the given options.
///
/// Missing root directory → empty `LoadResult` (not an error). Any
/// other IO error on the root → captured in `errors` under id
/// `<root>` and an empty result returned.
pub fn load_dir(root: &Path, opts: &LoadOptions) -> LoadResult {
    let mut out = LoadResult::default();

    let entries = match fs::read_dir(root) {
        Ok(it) => it,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return out,
        Err(err) => {
            out.errors.insert(
                root.display().to_string(),
                format!("read_dir failed: {err}"),
            );
            return out;
        }
    };

    let mut dirs: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            dirs.push(path);
        }
    }
    dirs.sort();

    for dir in dirs {
        let id = match dir.file_name().and_then(|n| n.to_str()) {
            Some(s) if !s.is_empty() && !s.starts_with('.') => s.to_string(),
            _ => continue,
        };

        if out.skills.contains_key(&id)
            || out.disabled.contains_key(&id)
            || out.errors.contains_key(&id)
        {
            // Filesystems that case-fold could collide; record loser.
            out.errors.insert(
                format!("{id} (duplicate)"),
                "duplicate skill id; first directory wins".to_string(),
            );
            continue;
        }

        let id_lower = id.to_ascii_lowercase();
        if let Some(rule) = opts.deny_list.iter().find(|r| r.matches(&id_lower)) {
            out.disabled.insert(id.clone(), rule.reason());
            continue;
        }

        match load_one(&id, &dir, opts) {
            Ok(skill) => {
                out.skills.insert(id, skill);
            }
            Err(err) => {
                out.errors.insert(id, err);
            }
        }
    }

    out
}

fn load_one(id: &str, dir: &Path, opts: &LoadOptions) -> Result<LoadedSkill, String> {
    let manifest_path = dir.join("SKILL.md");
    let metadata =
        fs::metadata(&manifest_path).map_err(|e| format!("SKILL.md not readable: {e}"))?;
    if !metadata.is_file() {
        return Err("SKILL.md is not a regular file".to_string());
    }
    if metadata.len() > opts.max_manifest_bytes {
        return Err(format!(
            "SKILL.md is {} bytes; exceeds max {} bytes",
            metadata.len(),
            opts.max_manifest_bytes
        ));
    }

    let raw =
        fs::read_to_string(&manifest_path).map_err(|e| format!("failed to read SKILL.md: {e}"))?;
    let doc = manifest::parse(&raw).map_err(|e| format!("manifest parse error: {e}"))?;

    Ok(LoadedSkill {
        id: id.to_string(),
        dir: dir.to_path_buf(),
        manifest_path,
        manifest: doc.manifest,
        body: doc.body,
    })
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/skills/loader.rs"
    ));
}
