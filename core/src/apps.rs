use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};

use crate::caps::manifest::{Arg, ArgKind, Manifest, Operation};
use crate::provenance::{self, PackageKind, VerifiedPackage, VerifyOptions};

/// An app manifest loaded from `app.json`. There is one manifest format
/// — [`caps::manifest::Manifest`](crate::caps::manifest::Manifest) — and
/// the loader rejects anything that doesn't validate.
pub type AppManifest = crate::caps::manifest::Manifest;

/// Discovered app: manifest, path on disk, and the provenance verdict.
///
/// `provenance` is `Ok` only when the whole package authenticated. An
/// `Err` App is **quarantined**: it still appears in listings (with the
/// reason) so an operator can see and fix it, but no authority-bearing
/// caller may use its manifest. Capability derivation, AI policy,
/// session launch and command dispatch all go through
/// [`App::require_verified`].
#[derive(Debug, Clone)]
pub struct App {
    pub manifest: AppManifest,
    pub dir: PathBuf,
    pub provenance: Result<Arc<VerifiedPackage>, String>,
}

impl App {
    pub fn main_py(&self) -> PathBuf {
        self.dir.join("main.py")
    }

    pub fn is_verified(&self) -> bool {
        self.provenance.is_ok()
    }

    /// The verified snapshot, or the quarantine diagnostic.
    pub fn require_verified(&self) -> Result<&Arc<VerifiedPackage>, String> {
        self.provenance.as_ref().map_err(|reason| reason.clone())
    }

    /// `vendor` / `publisher` / `developer` / `quarantined`. Surfaced in
    /// listings and audit records, never used as an authority decision
    /// on its own.
    pub fn trust_label(&self) -> &'static str {
        match &self.provenance {
            Ok(pkg) => pkg.source().as_str(),
            Err(_) => "quarantined",
        }
    }

    pub fn quarantine_reason(&self) -> Option<&str> {
        self.provenance.as_ref().err().map(String::as_str)
    }

    /// Audit-safe provenance projection for logs and listings.
    pub fn provenance_facts(&self) -> Value {
        match &self.provenance {
            Ok(pkg) => pkg.audit_facts(),
            Err(reason) => json!({
                "kind": "app",
                "id": self.manifest.id,
                "trust": "quarantined",
                "reason": reason,
            }),
        }
    }
}

/// Outcome of a discovery scan, keeping quarantined installs visible.
#[derive(Debug, Clone, Default)]
pub struct Discovery {
    /// Apps whose provenance authenticated.
    pub verified: BTreeMap<String, App>,
    /// Structurally valid apps whose provenance failed, with the reason.
    pub quarantined: BTreeMap<String, App>,
}

impl Discovery {
    /// Every discovered app, verified first. Listing surfaces use this
    /// so a quarantined install is reported rather than vanishing.
    pub fn all(self) -> BTreeMap<String, App> {
        let mut out = self.quarantined;
        out.extend(self.verified);
        out
    }

    pub fn diagnostics(&self) -> Vec<String> {
        self.quarantined
            .iter()
            .filter_map(|(id, app)| {
                app.quarantine_reason()
                    .map(|reason| format!("app `{id}`: {reason}"))
            })
            .collect()
    }
}

/// Recursively scan `apps_dir` for directories containing a valid
/// `app.json`. A nested path is normalized with `-` separators, so
/// `gateway/slack/app.json` must declare `id: "gateway-slack"`.
///
/// Every candidate is authenticated against the provenance trust store
/// before its manifest is accepted. A structurally valid App whose
/// provenance fails is returned quarantined, with a diagnostic — never
/// silently dropped, and never usable for capability derivation.
pub fn discover(apps_dir: &Path) -> BTreeMap<String, App> {
    discover_all(apps_dir).all()
}

/// Verified apps only. Authority-bearing callers use this.
pub fn discover_verified(apps_dir: &Path) -> BTreeMap<String, App> {
    discover_all(apps_dir).verified
}

pub fn discover_all(apps_dir: &Path) -> Discovery {
    let mut apps = BTreeMap::new();
    discover_dir(apps_dir, apps_dir, 0, &mut apps);
    let mut out = Discovery::default();
    for (id, app) in apps {
        if app.is_verified() {
            out.verified.insert(id, app);
        } else {
            out.quarantined.insert(id, app);
        }
    }
    out
}

pub fn find(apps_dir: &Path, app_id: &str) -> Option<App> {
    discover(apps_dir).remove(app_id)
}

/// Resolve one app and require a good provenance verdict.
pub fn find_verified(apps_dir: &Path, app_id: &str) -> Result<App, String> {
    match find(apps_dir, app_id) {
        Some(app) => match &app.provenance {
            Ok(_) => Ok(app),
            Err(reason) => Err(reason.clone()),
        },
        None => Err(format!("app `{app_id}` is not installed")),
    }
}

fn verify_app_dir(dir: &Path, id: &str) -> Result<Arc<VerifiedPackage>, String> {
    let trust = provenance::trust_store();
    let options = VerifyOptions::new(PackageKind::App).expect_id(id);
    provenance::verify::verify_package_cached(dir, &options, &trust)
        .map_err(|e| provenance::quarantine_reason(PackageKind::App, id, &e))
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
        let structural = match AppManifest::from_json(&data) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let normalized_id = match normalized_relative_id(root, &path) {
            Some(id) => id,
            None => continue,
        };
        if normalized_id != structural.id {
            continue;
        }
        // Provenance decides which manifest bytes are authoritative. A
        // verified App re-parses its manifest out of the verified
        // snapshot so the bytes used for capability derivation are the
        // bytes that were signed, not whatever the path holds now.
        let provenance = verify_app_dir(&path, &structural.id);
        let manifest = match &provenance {
            Ok(pkg) => match pkg
                .manifest_text()
                .map_err(|e| e.to_string())
                .and_then(|text| AppManifest::from_json(&text).map_err(|e| e.to_string()))
            {
                Ok(verified_manifest) if verified_manifest.id == structural.id => verified_manifest,
                Ok(_) => continue,
                Err(_) => continue,
            },
            Err(_) => structural,
        };
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
                    provenance,
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
        "stdin": operation.stdin,
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
        "type": if arg.repeatable { "array" } else { value_type },
        "required": arg.required,
        "repeatable": arg.repeatable,
        "description": arg.label.current(),
        "kind": arg.effective_binding().as_str(),
        "binding": arg.effective_binding().as_str(),
    });
    if let Some(required_when) = &arg.required_when {
        schema["required_when"] =
            serde_json::to_value(required_when).expect("NeedCondition serializes");
    }
    if arg.repeatable {
        schema["items"] = json!({"type": value_type});
    }
    if !arg.aliases.is_empty() {
        schema["aliases"] = serde_json::json!(arg.aliases);
    }
    if arg.positional_alias {
        schema["positional_alias"] = serde_json::Value::Bool(true);
    }
    if !arg.choices.is_empty() {
        if arg.repeatable {
            schema["items"]["enum"] = serde_json::Value::Array(arg.choices.clone());
        } else {
            schema["enum"] = serde_json::Value::Array(arg.choices.clone());
        }
    }
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
