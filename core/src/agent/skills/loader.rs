//! Filesystem-backed skills loader.
//!
//! Scans `<skill-name>/SKILL.md` directories and returns a registry the
//! runtime can consult. [`load_default`] merges the read-only vendor root with
//! the current user's writable root; [`load_dir`] remains available for an
//! explicit local root.
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
use std::sync::{Arc, OnceLock};

use super::manifest::{self, SkillManifest};
use crate::provenance::{self, PackageKind, VerifiedPackage, VerifyOptions};

/// One loaded skill: directory id, parsed manifest, file paths, and
/// the verified package snapshot every disclosure is bound to.
///
/// A `LoadedSkill` only exists when its package authenticated. The
/// snapshot is what `disclosure` reads the body and child resources
/// from, so a file changed after the catalog was built fails the
/// disclosure instead of injecting new text into the model.
#[derive(Debug, Clone)]
pub struct LoadedSkill {
    pub id: String,
    pub dir: PathBuf,
    pub manifest_path: PathBuf,
    pub manifest: SkillManifest,
    pub body: String,
    pub body_bytes: usize,
    pub origin: SkillOrigin,
    pub provenance: Arc<VerifiedPackage>,
}

impl PartialEq for LoadedSkill {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
            && self.dir == other.dir
            && self.manifest_path == other.manifest_path
            && self.manifest == other.manifest
            && self.body == other.body
            && self.body_bytes == other.body_bytes
            && self.origin == other.origin
            && self.provenance.content_digest() == other.provenance.content_digest()
    }
}

impl LoadedSkill {
    /// `vendor` / `publisher` / `developer`. Surfaced to the operator
    /// and, as a source label, to the model alongside the content.
    pub fn trust_label(&self) -> &'static str {
        self.provenance.source().as_str()
    }

    pub fn content_digest(&self) -> &str {
        self.provenance.content_digest()
    }

    /// Publisher key id when the skill was signed. Layered shadowing
    /// compares this rather than trusting directory precedence alone.
    pub fn publisher_key_id(&self) -> Option<&str> {
        match self.provenance.source() {
            provenance::TrustSource::Publisher { key_id } => Some(key_id.as_str()),
            _ => None,
        }
    }
}

/// Trust/source layer from which a skill was discovered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillOrigin {
    /// Read-only content shipped with the Claw Agent package.
    BuiltIn,
    /// Content installed into the current user's skill directory.
    User,
    /// An explicitly supplied directory, primarily for development/tests.
    Local,
}

impl SkillOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BuiltIn => "builtin",
            Self::User => "user",
            Self::Local => "local",
        }
    }
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
    /// Retain the instruction body after parsing. Metadata-only prompt
    /// assembly disables this and hydrates only the selected skill later.
    pub include_body: bool,
}

impl Default for LoadOptions {
    fn default() -> Self {
        Self {
            deny_list: default_deny_list(),
            max_manifest_bytes: 1024 * 1024,
            include_body: true,
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
/// read-only and user-writable roots.
pub fn load_default() -> LoadResult {
    load_default_with_options(&LoadOptions::default())
}

/// Metadata-only variant used by prompt assembly and `cos_skill list`.
pub fn load_catalog_default() -> LoadResult {
    load_default_with_options(&LoadOptions {
        include_body: false,
        ..LoadOptions::default()
    })
}

fn load_default_with_options(opts: &LoadOptions) -> LoadResult {
    let system_origin = if std::env::var_os("COS_SYSTEM_SKILLS_DIR").is_some() {
        static WARNED: OnceLock<()> = OnceLock::new();
        if WARNED.set(()).is_ok() {
            tracing::warn!(
                "COS_SYSTEM_SKILLS_DIR overrides the package-owned Skill root; treating it as local content"
            );
        }
        SkillOrigin::Local
    } else {
        SkillOrigin::BuiltIn
    };
    load_layered_with_origin(
        &crate::paths::system_skills_dir(),
        &crate::paths::agent_skills_dir(),
        opts,
        system_origin,
    )
}

/// Load all skills from the given root with the given options.
///
/// Missing root directory → empty `LoadResult` (not an error). Any
/// other IO error on the root → captured in `errors` under id
/// `<root>` and an empty result returned.
pub fn load_dir(root: &Path, opts: &LoadOptions) -> LoadResult {
    load_dir_with_origin(root, opts, SkillOrigin::Local)
}

/// Load read-only built-in skills and user-installed skills into one
/// registry. Built-ins win on duplicate ids so user content cannot silently
/// replace instructions shipped by the Agent package.
pub fn load_layered(system_root: &Path, user_root: &Path, opts: &LoadOptions) -> LoadResult {
    load_layered_with_origin(system_root, user_root, opts, SkillOrigin::BuiltIn)
}

pub(crate) fn load_layered_with_origin(
    system_root: &Path,
    user_root: &Path,
    opts: &LoadOptions,
    system_origin: SkillOrigin,
) -> LoadResult {
    let mut out = load_dir_with_origin(system_root, opts, system_origin);
    let user = load_dir_with_origin(user_root, opts, SkillOrigin::User);

    for (id, skill) in user.skills {
        // Shadowing is decided on verified identity, not on which root
        // happened to be scanned first. A user-installed skill may
        // replace a built-in only when both packages authenticated to
        // the same publisher key; otherwise the built-in stands and the
        // attempt is reported.
        match out.skills.get(&id) {
            Some(existing) => {
                let same_publisher = existing
                    .publisher_key_id()
                    .zip(skill.publisher_key_id())
                    .map(|(a, b)| a == b)
                    .unwrap_or(false);
                if same_publisher {
                    out.skills.insert(id, skill);
                } else {
                    out.errors.insert(
                        format!("{id} (user shadow)"),
                        format!(
                            "user skill `{id}` cannot shadow the built-in skill: \
                             it is signed by a different publisher (built-in {}, user {})",
                            existing.publisher_key_id().unwrap_or("vendor"),
                            skill.publisher_key_id().unwrap_or("unsigned"),
                        ),
                    );
                }
            }
            None if contains_id(&out, &id) => {
                out.errors.insert(
                    format!("{id} (user shadow)"),
                    format!("user skill `{id}` collides with a built-in skill id"),
                );
            }
            None => {
                out.skills.insert(id, skill);
            }
        }
    }
    merge_diagnostics(&mut out.disabled, user.disabled, "user");
    merge_diagnostics(&mut out.errors, user.errors, "user");
    out
}

/// Re-read one catalogued skill with its full instruction body retained.
pub fn hydrate(skill: &LoadedSkill, opts: &LoadOptions) -> Result<LoadedSkill, String> {
    let mut hydrated = load_one(&skill.id, &skill.dir, opts)?;
    hydrated.origin = skill.origin;
    Ok(hydrated)
}

fn contains_id(result: &LoadResult, id: &str) -> bool {
    result.skills.contains_key(id)
        || result.disabled.contains_key(id)
        || result.errors.contains_key(id)
}

fn merge_diagnostics(
    target: &mut BTreeMap<String, String>,
    source: BTreeMap<String, String>,
    layer: &str,
) {
    for (id, message) in source {
        let key = if target.contains_key(&id) {
            format!("{id} ({layer})")
        } else {
            id
        };
        target.insert(key, message);
    }
}

fn load_dir_with_origin(
    root: &Path,
    opts: &LoadOptions,
    origin: SkillOrigin,
) -> LoadResult {
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
            Ok(mut skill) => {
                skill.origin = origin;
                out.skills.insert(id, skill);
            }
            Err(err) => {
                // Quarantine with the reason rather than dropping the
                // skill silently: an operator has to be able to see why
                // an installed skill stopped being offered.
                out.errors.insert(id, err);
            }
        }
    }

    out
}

/// Authenticate one skill package. Signature (or vendor/developer
/// trust) is required — there is no environment variable that turns
/// this off.
pub fn verify_skill_dir(id: &str, dir: &Path) -> Result<Arc<VerifiedPackage>, String> {
    let trust = provenance::trust_store();
    let options = VerifyOptions::new(PackageKind::Skill).expect_id(id);
    provenance::verify::verify_package_cached(dir, &options, &trust)
        .map_err(|e| provenance::quarantine_reason(PackageKind::Skill, id, &e))
}

fn load_one(id: &str, dir: &Path, opts: &LoadOptions) -> Result<LoadedSkill, String> {
    let manifest_path = dir.join("SKILL.md");
    let verified = verify_skill_dir(id, dir)?;

    // The manifest and body come out of the verified snapshot, so the
    // metadata offered to the model is the metadata that was signed.
    let raw = verified
        .read_verified_text("SKILL.md")
        .map_err(|e| format!("verified SKILL.md read failed: {e}"))?;
    if raw.len() as u64 > opts.max_manifest_bytes {
        return Err(format!(
            "SKILL.md is {} bytes; exceeds max {} bytes",
            raw.len(),
            opts.max_manifest_bytes
        ));
    }
    let doc = manifest::parse(&raw).map_err(|e| format!("manifest parse error: {e}"))?;

    let body_bytes = doc.body.len();
    Ok(LoadedSkill {
        id: id.to_string(),
        dir: dir.to_path_buf(),
        manifest_path,
        manifest: doc.manifest,
        body: if opts.include_body {
            doc.body
        } else {
            String::new()
        },
        body_bytes,
        origin: SkillOrigin::Local,
        provenance: verified,
    })
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/skills/loader.rs"
    ));
}
