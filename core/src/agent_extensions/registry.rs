//! Explicit registry of selected, provenance-verified Agent extensions.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::manifest::ExtensionManifest;

#[derive(Debug, Clone)]
pub struct RegisteredExtension {
    pub manifest: ExtensionManifest,
    pub manifest_digest: String,
    pub package: crate::provenance::VerifiedPackage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuarantinedExtension {
    pub id: String,
    pub diagnostic: String,
}

#[derive(Debug, Clone, Default)]
pub struct ExtensionRegistry {
    pub registered: BTreeMap<String, RegisteredExtension>,
    pub quarantined: Vec<QuarantinedExtension>,
}

impl ExtensionRegistry {
    pub fn load_selected(root: &Path, selected: &[String]) -> Self {
        let mut registry = Self::default();
        let mut seen = BTreeSet::new();
        for id in selected {
            if !seen.insert(id.clone()) {
                continue;
            }
            if let Err(error) = validate_selected_id(id) {
                registry.quarantined.push(QuarantinedExtension {
                    id: id.clone(),
                    diagnostic: error,
                });
                continue;
            }
            let path = root.join(id);
            match load_one(&path, id) {
                Ok(extension) => {
                    registry.registered.insert(id.clone(), extension);
                }
                Err(error) => registry.quarantined.push(QuarantinedExtension {
                    id: id.clone(),
                    diagnostic: format!(
                        "extension `{id}` is quarantined and was not activated: {error}"
                    ),
                }),
            }
        }
        registry
    }
}

pub fn installed_root() -> PathBuf {
    std::env::var_os("COS_AGENT_EXTENSIONS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/usr/lib/cos/extensions"))
}

fn load_one(path: &Path, selected_id: &str) -> Result<RegisteredExtension, String> {
    let package = crate::provenance::verify(path, crate::provenance::PackageKind::AgentExtension)?;
    if package.id() != selected_id {
        return Err("verified package id does not match the selected directory".to_string());
    }
    let manifest = ExtensionManifest::parse_verified(&package)?;
    let manifest_digest = ExtensionManifest::manifest_digest(&package)?;
    Ok(RegisteredExtension {
        manifest,
        manifest_digest,
        package,
    })
}

fn validate_selected_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.len() > 128
        || id.starts_with('.')
        || !id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
        })
    {
        return Err("configured extension id is invalid".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent_extensions/registry.rs"
    ));
}
