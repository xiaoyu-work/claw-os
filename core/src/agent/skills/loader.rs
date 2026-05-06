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
            out.errors
                .insert(root.display().to_string(), format!("read_dir failed: {err}"));
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

        if out.skills.contains_key(&id) || out.disabled.contains_key(&id)
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
    let metadata = fs::metadata(&manifest_path)
        .map_err(|e| format!("SKILL.md not readable: {e}"))?;
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

    let raw = fs::read_to_string(&manifest_path)
        .map_err(|e| format!("failed to read SKILL.md: {e}"))?;
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
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write_skill(root: &Path, id: &str, contents: &str) {
        let dir = root.join(id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("SKILL.md"), contents).unwrap();
    }

    fn minimal(id: &str) -> String {
        format!("---\nname: {id}\n---\nbody for {id}\n")
    }

    #[test]
    fn missing_root_returns_empty() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().join("does-not-exist");
        let r = load_dir(&root, &LoadOptions::default());
        assert!(r.is_empty());
    }

    #[test]
    fn empty_root_returns_empty() {
        let tmp = tempdir().unwrap();
        let r = load_dir(tmp.path(), &LoadOptions::default());
        assert!(r.is_empty());
    }

    #[test]
    fn loads_single_skill() {
        let tmp = tempdir().unwrap();
        write_skill(tmp.path(), "pdf", &minimal("pdf"));
        let r = load_dir(tmp.path(), &LoadOptions::default());
        assert_eq!(r.loaded_count(), 1);
        let s = r.skills.get("pdf").unwrap();
        assert_eq!(s.id, "pdf");
        assert_eq!(s.manifest.name, "pdf");
        assert_eq!(s.body, "body for pdf\n");
        assert_eq!(s.manifest_path.file_name().unwrap(), "SKILL.md");
    }

    #[test]
    fn loads_multiple_skills_alphabetised() {
        let tmp = tempdir().unwrap();
        write_skill(tmp.path(), "zebra", &minimal("zebra"));
        write_skill(tmp.path(), "alpha", &minimal("alpha"));
        write_skill(tmp.path(), "mango", &minimal("mango"));
        let r = load_dir(tmp.path(), &LoadOptions::default());
        let names: Vec<&str> = r.skills.keys().map(String::as_str).collect();
        assert_eq!(names, vec!["alpha", "mango", "zebra"]);
    }

    #[test]
    fn skill_id_is_directory_name_not_manifest_name() {
        let tmp = tempdir().unwrap();
        // Manifest declares a different "name" than the dir.
        write_skill(
            tmp.path(),
            "dirname",
            "---\nname: human-friendly-label\n---\n",
        );
        let r = load_dir(tmp.path(), &LoadOptions::default());
        let s = r.skills.get("dirname").expect("loaded by dir name");
        assert_eq!(s.id, "dirname");
        assert_eq!(s.manifest.name, "human-friendly-label");
    }

    #[test]
    fn missing_skill_md_recorded_in_errors() {
        let tmp = tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("orphan")).unwrap();
        let r = load_dir(tmp.path(), &LoadOptions::default());
        assert_eq!(r.loaded_count(), 0);
        assert!(r.errors.contains_key("orphan"));
        let msg = &r.errors["orphan"];
        assert!(msg.contains("SKILL.md"), "got: {msg}");
    }

    #[test]
    fn malformed_manifest_recorded_in_errors_and_others_still_load() {
        let tmp = tempdir().unwrap();
        write_skill(tmp.path(), "good", &minimal("good"));
        write_skill(tmp.path(), "bad", "no frontmatter at all\n");
        let r = load_dir(tmp.path(), &LoadOptions::default());
        assert!(r.skills.contains_key("good"));
        assert!(r.errors.contains_key("bad"));
    }

    #[test]
    fn red_teaming_disabled_by_default() {
        let tmp = tempdir().unwrap();
        write_skill(tmp.path(), "red-teaming", &minimal("red-teaming"));
        write_skill(tmp.path(), "red-teaming-attacker", &minimal("rt-attacker"));
        write_skill(tmp.path(), "godmode", &minimal("godmode"));
        let r = load_dir(tmp.path(), &LoadOptions::default());
        assert!(r.skills.is_empty());
        assert!(r.disabled.contains_key("red-teaming"));
        assert!(r.disabled.contains_key("red-teaming-attacker"));
        assert!(r.disabled.contains_key("godmode"));
    }

    #[test]
    fn yuanbao_disabled_by_default() {
        let tmp = tempdir().unwrap();
        write_skill(tmp.path(), "yuanbao", &minimal("yuanbao"));
        write_skill(tmp.path(), "yuanbao-tools", &minimal("yuanbao-tools"));
        let r = load_dir(tmp.path(), &LoadOptions::default());
        assert!(r.disabled.contains_key("yuanbao"));
        assert!(r.disabled.contains_key("yuanbao-tools"));
    }

    #[test]
    fn deny_match_is_case_insensitive() {
        let tmp = tempdir().unwrap();
        write_skill(tmp.path(), "Godmode", &minimal("Godmode"));
        let r = load_dir(tmp.path(), &LoadOptions::default());
        assert!(r.disabled.contains_key("Godmode"));
    }

    #[test]
    fn deny_prefix_does_not_match_unrelated_skills() {
        let tmp = tempdir().unwrap();
        // `red-team-strategy` would match `red-team-` if we used a
        // raw startswith; ensure the boundary check holds: only
        // `red-teaming` (exact) and `red-teaming-...` are denied.
        write_skill(tmp.path(), "red-team-strategy", &minimal("red-team-strategy"));
        let r = load_dir(tmp.path(), &LoadOptions::default());
        assert!(r.skills.contains_key("red-team-strategy"));
        assert!(!r.disabled.contains_key("red-team-strategy"));
    }

    #[test]
    fn empty_deny_list_loads_everything() {
        let tmp = tempdir().unwrap();
        write_skill(tmp.path(), "godmode", &minimal("godmode"));
        let opts = LoadOptions {
            deny_list: Vec::new(),
            ..LoadOptions::default()
        };
        let r = load_dir(tmp.path(), &opts);
        assert!(r.skills.contains_key("godmode"));
        assert!(r.disabled.is_empty());
    }

    #[test]
    fn oversize_manifest_rejected() {
        let tmp = tempdir().unwrap();
        let huge = format!(
            "---\nname: huge\ndescription: |\n  {}\n---\n",
            "x".repeat(2048)
        );
        write_skill(tmp.path(), "huge", &huge);
        let opts = LoadOptions {
            max_manifest_bytes: 100,
            ..LoadOptions::default()
        };
        let r = load_dir(tmp.path(), &opts);
        assert!(r.skills.is_empty());
        let msg = r.errors.get("huge").expect("error recorded");
        assert!(msg.contains("exceeds"), "got: {msg}");
    }

    #[test]
    fn dotfiles_and_files_at_root_ignored() {
        let tmp = tempdir().unwrap();
        write_skill(tmp.path(), ".hidden", &minimal("hidden"));
        fs::write(tmp.path().join("loose.md"), "stray file").unwrap();
        write_skill(tmp.path(), "ok", &minimal("ok"));
        let r = load_dir(tmp.path(), &LoadOptions::default());
        assert_eq!(r.loaded_count(), 1);
        assert!(r.skills.contains_key("ok"));
    }

    #[test]
    fn skill_md_as_directory_errors_out() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("weird");
        fs::create_dir_all(dir.join("SKILL.md")).unwrap();
        let r = load_dir(tmp.path(), &LoadOptions::default());
        let msg = r.errors.get("weird").expect("error");
        assert!(msg.contains("not a regular file"), "got: {msg}");
    }

    #[test]
    fn missing_name_field_recorded_in_errors() {
        let tmp = tempdir().unwrap();
        write_skill(tmp.path(), "noname", "---\ndescription: x\n---\n");
        let r = load_dir(tmp.path(), &LoadOptions::default());
        let msg = r.errors.get("noname").expect("error");
        assert!(msg.contains("name"), "got: {msg}");
    }

    #[test]
    fn allowed_tools_round_trip() {
        let tmp = tempdir().unwrap();
        write_skill(
            tmp.path(),
            "with-tools",
            "---\nname: with-tools\nallowed-tools: [cos_fs, cos_exec]\n---\n",
        );
        let r = load_dir(tmp.path(), &LoadOptions::default());
        let s = &r.skills["with-tools"];
        assert_eq!(s.manifest.allowed_tools, vec!["cos_fs", "cos_exec"]);
    }

    #[test]
    fn load_default_does_not_panic_on_missing_dir() {
        // Don't mess with COS_DATA_DIR (parallel-test safety); just
        // make sure the function survives the default path being
        // absent. If the system happens to have skills, we still
        // get a valid LoadResult.
        let _ = load_default();
    }
}
