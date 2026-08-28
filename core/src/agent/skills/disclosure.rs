//! Progressive disclosure for installed Agent Skills.
//!
//! Level 1 exposes only manifest metadata in the system prompt. Level 2
//! returns one selected `SKILL.md` body and its resource names. Level 3 reads
//! one explicitly requested resource beneath that skill directory.

use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde_json::{json, Value};

use super::loader::{LoadResult, LoadedSkill, SkillOrigin};
use super::provenance::{Guard, GuardOutcome, Provenance};

const MAX_PROMPT_SKILLS: usize = 32;
pub const MAX_LIST_PAGE: usize = 100;
const MAX_NAME_CHARS: usize = 128;
const MAX_DESCRIPTION_CHARS: usize = 512;
const MAX_TRIGGERS: usize = 8;
const MAX_TRIGGER_CHARS: usize = 128;
const MAX_INSTRUCTION_BYTES: usize = 64 * 1024;
const MAX_RESOURCE_BYTES: u64 = 64 * 1024;
const MAX_RESOURCE_DEPTH: usize = 8;
const MAX_RESOURCE_ENTRIES: usize = 512;

pub const SKILL_CATALOG_TAG: &str = "untrusted_skill_catalog";
pub const SKILL_CONTENT_TAG: &str = "untrusted_skill_content";

/// Render the metadata-only catalogue injected into the system prompt.
pub fn render_prompt_catalog(load: &LoadResult) -> Option<String> {
    if load.skills.is_empty() {
        return None;
    }
    let entries = catalog_page(load, 0, MAX_PROMPT_SKILLS);
    let payload = serde_json::to_string_pretty(&json!({
        "total": load.skills.len(),
        "shown": entries.len(),
        "truncated": entries.len() < load.skills.len(),
        "skills": entries,
    }))
    .ok()?;
    let wrapped = crate::agent::safety::untrusted::wrap_untrusted(SKILL_CATALOG_TAG, &payload);
    Some(format!(
        "Progressive skill disclosure:\n\
         - The catalogue below contains metadata only; no skill instructions have been loaded.\n\
         - When a skill clearly matches the current task, call `cos_skill` with `command=read` before acting on it.\n\
         - Read a referenced child file with `command=resource` only when that specific detail is needed.\n\
         - Do not call `read` when metadata says `disclosable=false`; report the size problem instead.\n\
         - If a disclosure call fails, do not retry it unchanged.\n\
         - Do not read every skill speculatively.\n\n{wrapped}"
    ))
}

/// One bounded metadata page visible at disclosure level 1.
pub fn catalog_page(load: &LoadResult, offset: usize, limit: usize) -> Vec<Value> {
    load.skills
        .values()
        .skip(offset)
        .take(limit.min(MAX_LIST_PAGE))
        .map(|skill| {
            json!({
                "id": skill.id,
                "name": truncate_field(&skill.manifest.name, MAX_NAME_CHARS),
                "description": skill
                    .manifest
                    .description
                    .as_deref()
                    .map(|value| truncate_field(value, MAX_DESCRIPTION_CHARS)),
                "triggers": skill
                    .manifest
                    .triggers
                    .iter()
                    .take(MAX_TRIGGERS)
                    .map(|value| truncate_field(value, MAX_TRIGGER_CHARS))
                    .collect::<Vec<_>>(),
                "source": skill.origin.as_str(),
                "trust": skill.trust_label(),
                "content_digest": skill.content_digest(),
                "instruction_bytes": skill.body_bytes,
                "disclosable": instruction_disclosable(skill),
            })
        })
        .collect()
}

/// Disclosure level 2: one selected skill's instructions and resource names.
pub fn disclose_instructions(skill: &LoadedSkill) -> Result<Value, String> {
    ensure_disclosure_allowed(skill)?;
    if !instruction_disclosable(skill) {
        return Err(format!(
            "skill `{}` instructions are {} bytes; exceeds disclosure cap {}. Split detailed guidance into child resources.",
            skill.id,
            skill.body_bytes,
            MAX_INSTRUCTION_BYTES
        ));
    }

    // Re-read the body from the verified snapshot at disclosure time.
    // The catalog may have been built minutes ago; if SKILL.md changed
    // since, this fails instead of injecting unverified text into the
    // model's context.
    let raw = skill
        .provenance
        .read_verified_text("SKILL.md")
        .map_err(|e| format!("skill `{}` failed its disclosure integrity check: {e}", skill.id))?;
    let doc = super::manifest::parse(&raw)
        .map_err(|e| format!("skill `{}` manifest re-parse failed: {e}", skill.id))?;
    if doc.body.len() > MAX_INSTRUCTION_BYTES {
        return Err(format!(
            "skill `{}` instructions are {} bytes; exceeds disclosure cap {}",
            skill.id,
            doc.body.len(),
            MAX_INSTRUCTION_BYTES
        ));
    }

    let (resources, resources_truncated) = list_resources(&skill.dir)?;
    Ok(json!({
        "disclosure_level": "instructions",
        "id": skill.id,
        "name": doc.manifest.name,
        "description": doc.manifest.description,
        "source": skill.origin.as_str(),
        "trust": skill.trust_label(),
        "content_digest": skill.content_digest(),
        "allowed_tools": doc.manifest.allowed_tools,
        "instructions": doc.body,
        "resources": resources,
        "resources_truncated": resources_truncated,
    }))
}

pub fn instruction_disclosable(skill: &LoadedSkill) -> bool {
    skill.body_bytes <= MAX_INSTRUCTION_BYTES
}

/// Disclosure level 3: one explicitly selected child resource.
pub fn disclose_resource(skill: &LoadedSkill, resource: &str) -> Result<Value, String> {
    ensure_disclosure_allowed(skill)?;
    let (_path, relative) = resolve_resource(skill, resource)?;
    // Content comes from the verified snapshot by digest, never from a
    // fresh path resolution: a resource swapped after the catalog was
    // built must fail rather than reach the model.
    let content = skill
        .provenance
        .read_verified_text(&relative)
        .map_err(|e| format!("resource `{relative}` failed its integrity check: {e}"))?;
    if content.len() as u64 > MAX_RESOURCE_BYTES {
        return Err(format!(
            "resource `{relative}` is {} bytes; exceeds disclosure cap {MAX_RESOURCE_BYTES}",
            content.len()
        ));
    }
    Ok(json!({
        "disclosure_level": "resource",
        "id": skill.id,
        "source": skill.origin.as_str(),
        "trust": skill.trust_label(),
        "content_digest": skill.content_digest(),
        "path": relative,
        "bytes": content.len(),
        "content": content,
    }))
}

fn ensure_disclosure_allowed(skill: &LoadedSkill) -> Result<(), String> {
    if skill.origin == SkillOrigin::BuiltIn {
        return Ok(());
    }
    let provenance = match skill.origin {
        SkillOrigin::BuiltIn => Provenance::Vendor,
        SkillOrigin::User => Provenance::Unknown,
        SkillOrigin::Local => Provenance::Local,
    };
    match Guard::with_default_config().check(skill, provenance) {
        GuardOutcome::Allow => Ok(()),
        GuardOutcome::Deny { reason } => Err(format!("skill disclosure denied: {reason}")),
        GuardOutcome::RequireConfirmation { reason } => Err(format!(
            "skill disclosure requires explicit operator review: {reason}"
        )),
    }
}

fn truncate_field(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let mut out: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        out.push('…');
    }
    out
}

fn list_resources(root: &Path) -> Result<(Vec<Value>, bool), String> {
    let mut resources = Vec::new();
    let mut truncated = false;
    collect_resources(root, root, 0, &mut resources, &mut truncated)?;
    resources.sort_by(|a, b| {
        a.get("path")
            .and_then(Value::as_str)
            .cmp(&b.get("path").and_then(Value::as_str))
    });
    Ok((resources, truncated))
}

fn collect_resources(
    root: &Path,
    current: &Path,
    depth: usize,
    resources: &mut Vec<Value>,
    truncated: &mut bool,
) -> Result<(), String> {
    if depth > MAX_RESOURCE_DEPTH {
        *truncated = true;
        return Ok(());
    }
    let read_dir = match fs::read_dir(current) {
        Ok(entries) => entries,
        Err(error) if depth == 0 => {
            return Err(format!(
                "read skill resources {}: {error}",
                current.display()
            ));
        }
        Err(_) => {
            *truncated = true;
            return Ok(());
        }
    };
    let mut entries = Vec::new();
    for entry in read_dir {
        match entry {
            Ok(entry) => entries.push(entry),
            Err(_) => *truncated = true,
        }
    }
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        if resources.len() >= MAX_RESOURCE_ENTRIES {
            *truncated = true;
            break;
        }
        let file_name = entry.file_name();
        if file_name.to_string_lossy().starts_with('.') {
            continue;
        }
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => {
                *truncated = true;
                continue;
            }
        };
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if file_type.is_dir() {
            collect_resources(root, &path, depth + 1, resources, truncated)?;
        } else if file_type.is_file() {
            let relative = path
                .strip_prefix(root)
                .map_err(|error| format!("resolve skill resource path: {error}"))?;
            if relative == Path::new("SKILL.md") {
                continue;
            }
            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(_) => {
                    *truncated = true;
                    continue;
                }
            };
            resources.push(json!({
                "path": portable_path(relative),
                "bytes": metadata.len(),
            }));
        }
    }
    Ok(())
}

fn resolve_resource(skill: &LoadedSkill, raw: &str) -> Result<(PathBuf, String), String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("missing required field: path".to_string());
    }
    let relative = Path::new(raw);
    let mut normalized = PathBuf::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) if !part.to_string_lossy().starts_with('.') => {
                normalized.push(part)
            }
            Component::Normal(_) => {
                return Err("hidden skill resources may not be disclosed".to_string());
            }
            _ => {
                return Err("resource path must be relative and may not contain `..`".to_string());
            }
        }
    }
    if normalized.as_os_str().is_empty()
        || portable_path(&normalized).eq_ignore_ascii_case("SKILL.md")
    {
        return Err("use `command=read` for SKILL.md instructions".to_string());
    }

    let root = fs::canonicalize(&skill.dir)
        .map_err(|error| format!("resolve skill directory: {error}"))?;
    let mut cursor = root.clone();
    for component in normalized.components() {
        let Component::Normal(part) = component else {
            return Err("invalid resource path component".to_string());
        };
        cursor.push(part);
        let metadata = fs::symlink_metadata(&cursor)
            .map_err(|error| format!("resource `{}` is not readable: {error}", raw))?;
        if metadata.file_type().is_symlink() {
            return Err("skill resources may not traverse symlinks".to_string());
        }
    }

    let resolved =
        fs::canonicalize(&cursor).map_err(|error| format!("resolve resource `{raw}`: {error}"))?;
    if !resolved.starts_with(&root) {
        return Err("resource path escapes the skill directory".to_string());
    }
    let metadata =
        fs::metadata(&resolved).map_err(|error| format!("inspect resource `{raw}`: {error}"))?;
    if !metadata.is_file() {
        return Err(format!("resource `{raw}` is not a regular file"));
    }
    if metadata.len() > MAX_RESOURCE_BYTES {
        return Err(format!(
            "resource `{raw}` is {} bytes; exceeds disclosure cap {}",
            metadata.len(),
            MAX_RESOURCE_BYTES
        ));
    }
    Ok((resolved, portable_path(&normalized)))
}

fn read_bounded_text(path: &Path) -> Result<String, String> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = options
        .open(path)
        .map_err(|error| format!("open skill resource {}: {error}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(MAX_RESOURCE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read skill resource {}: {error}", path.display()))?;
    if bytes.len() as u64 > MAX_RESOURCE_BYTES {
        return Err(format!(
            "resource exceeds disclosure cap {}",
            MAX_RESOURCE_BYTES
        ));
    }
    String::from_utf8(bytes).map_err(|_| "skill resource is not valid UTF-8 text".to_string())
}

fn portable_path(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/skills/disclosure.rs"
    ));
}
