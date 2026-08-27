use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::caps::manifest::{Arg, ArgKind, Manifest, Operation};

/// An app manifest loaded from `app.json`. There is one manifest format
/// — [`caps::manifest::Manifest`](crate::caps::manifest::Manifest) — and
/// the loader rejects anything that doesn't validate.
pub type AppManifest = crate::caps::manifest::Manifest;

/// Discovered app: manifest + path on disk.
#[derive(Debug, Clone)]
pub struct App {
    pub manifest: AppManifest,
    pub dir: PathBuf,
}

impl App {
    pub fn main_py(&self) -> PathBuf {
        self.dir.join("main.py")
    }
}

/// Recursively scan `apps_dir` for directories containing a valid
/// `app.json`. A nested path is normalized with `-` separators, so
/// `gateway/slack/app.json` must declare `id: "gateway-slack"`.
///
/// Apps whose manifest fails to parse or validate are silently skipped
/// — keeping a broken app installed must never poison discovery for
/// the rest of the system. Operators can run `cos app doctor` (TBD) to
/// see why a specific app is missing.
pub fn discover(apps_dir: &Path) -> BTreeMap<String, App> {
    let mut apps = BTreeMap::new();
    discover_dir(apps_dir, apps_dir, 0, &mut apps);
    apps
}

pub fn find(apps_dir: &Path, app_id: &str) -> Option<App> {
    discover(apps_dir).remove(app_id)
}

fn discover_dir(root: &Path, dir: &Path, depth: usize, apps: &mut BTreeMap<String, App>) {
    if depth > 8 {
        return;
    }
    let mut entries = match fs::read_dir(dir) {
        Ok(entries) => entries.flatten().collect::<Vec<_>>(),
        Err(_) => return,
    };
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() || file_type.is_symlink() {
            continue;
        }
        let manifest_path = path.join("app.json");
        if !manifest_path.is_file() {
            discover_dir(root, &path, depth + 1, apps);
            continue;
        }
        let data = match fs::read_to_string(&manifest_path) {
            Ok(d) => d,
            Err(_) => continue,
        };
        let manifest = match AppManifest::from_json(&data) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let normalized_id = match normalized_relative_id(root, &path) {
            Some(id) => id,
            None => continue,
        };
        if normalized_id != manifest.id {
            continue;
        }
        let replace = apps
            .get(&manifest.id)
            .map(|existing| relative_depth(root, &path) < relative_depth(root, &existing.dir))
            .unwrap_or(true);
        if replace {
            apps.insert(
                manifest.id.clone(),
                App {
                    manifest,
                    dir: path,
                },
            );
        }
    }
}

fn relative_depth(root: &Path, path: &Path) -> usize {
    path.strip_prefix(root)
        .map(|relative| relative.components().count())
        .unwrap_or(usize::MAX)
}

fn normalized_relative_id(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    let parts = relative
        .components()
        .map(|component| component.as_os_str().to_str())
        .collect::<Option<Vec<_>>>()?;
    if parts.is_empty()
        || parts.iter().any(|part| {
            part.is_empty()
                || !part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'-' | b'_'))
        })
    {
        return None;
    }
    Some(parts.join("-"))
}

/// Build the stable, non-executing schema for an App manifest. Schema
/// introspection must never run third-party entrypoint code.
pub fn manifest_schema(manifest: &Manifest) -> Value {
    let commands: serde_json::Map<String, Value> = manifest
        .operations
        .iter()
        .map(|(name, operation)| (name.clone(), operation_schema(operation)))
        .collect();
    Value::Object(commands)
}

pub fn operation_schema(operation: &Operation) -> Value {
    let parameters = operation
        .args
        .iter()
        .map(arg_schema)
        .collect::<Vec<_>>();
    json!({
        "description": operation.summary.current(),
        "parameters": parameters,
    })
}

fn arg_schema(arg: &Arg) -> Value {
    let value_type = match arg.kind {
        ArgKind::Path | ArgKind::Host | ArgKind::Name | ArgKind::Text => "string",
        ArgKind::Number => "number",
        ArgKind::Integer => "integer",
        ArgKind::Bool => "boolean",
    };
    let mut schema = json!({
        "name": arg.name,
        "type": value_type,
        "required": arg.required,
        "description": arg.label.current(),
        "kind": arg.binding.as_str(),
        "binding": arg.binding.as_str(),
    });
    if let Some(default) = &arg.default {
        schema["default"] = default.clone();
    }
    if let Some(default_from) = &arg.default_from {
        schema["default_from"] = json!(default_from);
    }
    schema
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/apps.rs"));
}
