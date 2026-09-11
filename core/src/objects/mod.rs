//! App-owned object references and authenticated, non-executing descriptions.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::apps::{self, App};
use crate::caps::manifest::{ArgBinding, ArgKind, ObjectType};

pub use claw_os_sdk::generated::ObjectRef;
pub use claw_os_sdk::objects::{format_reference, parse_reference, validate, ObjectRefError};

#[derive(Debug, thiserror::Error)]
pub enum ObjectError {
    #[error("invalid App object reference: {0}")]
    Invalid(#[from] ObjectRefError),
    #[error("App object is unavailable: {0}")]
    Unavailable(String),
    #[error("App `{app_id}` does not declare object type `{object_type}`")]
    Undeclared { app_id: String, object_type: String },
    #[error("App object cannot be resolved: {0}")]
    Resolution(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct ObjectInvocation {
    pub app_id: String,
    pub operation: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ObjectDescription {
    pub object: ObjectRef,
    pub reference: String,
    pub app_name: String,
    pub app_version: String,
    pub object_label: String,
    pub object_summary: String,
    pub invocation: ObjectInvocation,
    pub provenance: Value,
}

#[derive(Debug, Serialize)]
pub struct AppObjectTypes {
    pub app_id: String,
    pub app_name: String,
    pub app_version: String,
    pub objects: BTreeMap<String, ObjectType>,
}

#[derive(Debug, Serialize)]
pub struct CatalogueIssue {
    pub app_id: String,
    pub error: String,
}

#[derive(Debug, Serialize)]
pub struct ObjectCatalogue {
    pub apps: Vec<AppObjectTypes>,
    pub quarantined: Vec<CatalogueIssue>,
}

pub trait ObjectCatalog {
    fn describe(&self, object: &ObjectRef) -> Result<ObjectDescription, ObjectError>;
    fn list(&self, app_id: Option<&str>) -> Result<ObjectCatalogue, ObjectError>;
}

pub struct InstalledObjectCatalog {
    root: PathBuf,
}

impl InstalledObjectCatalog {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Keep the verified package with its invocation so execution cannot
    /// rediscover a different manifest after choosing the operation.
    pub fn prepare(&self, object: &ObjectRef) -> Result<(App, ObjectDescription), ObjectError> {
        validate(object)?;
        let app =
            apps::find_verified(&self.root, &object.app_id).map_err(ObjectError::Unavailable)?;
        let description = describe_verified(&app, object)?;
        Ok((app, description))
    }
}

impl ObjectCatalog for InstalledObjectCatalog {
    fn describe(&self, object: &ObjectRef) -> Result<ObjectDescription, ObjectError> {
        self.prepare(object).map(|(_, description)| description)
    }

    fn list(&self, app_id: Option<&str>) -> Result<ObjectCatalogue, ObjectError> {
        if let Some(app_id) = app_id {
            let app = apps::find_verified(&self.root, app_id).map_err(ObjectError::Unavailable)?;
            assert_current(&app)?;
            return Ok(ObjectCatalogue {
                apps: vec![catalogue_entry(app)],
                quarantined: Vec::new(),
            });
        }
        let discovery = apps::discover_all(&self.root);
        let mut entries = Vec::new();
        let mut quarantined: Vec<_> = discovery
            .quarantined
            .into_iter()
            .map(|(app_id, app)| CatalogueIssue {
                app_id,
                error: app
                    .quarantine_reason()
                    .unwrap_or("App package is not authenticated")
                    .to_string(),
            })
            .collect();
        for (app_id, app) in discovery.verified {
            match assert_current(&app) {
                Ok(()) if !app.manifest.objects.is_empty() => entries.push(catalogue_entry(app)),
                Ok(()) => {}
                Err(error) => quarantined.push(CatalogueIssue {
                    app_id,
                    error: error.to_string(),
                }),
            }
        }
        Ok(ObjectCatalogue {
            apps: entries,
            quarantined,
        })
    }
}

pub fn default_catalog() -> InstalledObjectCatalog {
    InstalledObjectCatalog::new(
        std::env::var_os("COS_APPS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/usr/lib/cos/apps")),
    )
}

/// Classify an attempted App reference without interpreting ordinary resources.
/// Invalid App URI spellings remain visible as diagnostics, not URL fallbacks.
pub fn is_app_reference(reference: &str) -> bool {
    reference
        .split_once(':')
        .is_some_and(|(scheme, _)| scheme.eq_ignore_ascii_case("app"))
}

fn catalogue_entry(app: App) -> AppObjectTypes {
    AppObjectTypes {
        app_id: app.manifest.id,
        app_name: app.manifest.name.current().to_string(),
        app_version: app.manifest.version,
        objects: app.manifest.objects,
    }
}

fn assert_current(app: &App) -> Result<(), ObjectError> {
    let verified = app.require_verified().map_err(ObjectError::Unavailable)?;
    verified
        .assert_current(&crate::provenance::trust_store())
        .map_err(|error| ObjectError::Unavailable(error.to_string()))?;
    verified
        .manifest_text()
        .map(|_| ())
        .map_err(|error| ObjectError::Unavailable(error.to_string()))
}

fn describe_verified(app: &App, object: &ObjectRef) -> Result<ObjectDescription, ObjectError> {
    assert_current(app)?;
    let declaration = app
        .manifest
        .objects
        .get(&object.object_type)
        .ok_or_else(|| ObjectError::Undeclared {
            app_id: object.app_id.clone(),
            object_type: object.object_type.clone(),
        })?;
    let resolver = &declaration.resolve;
    let operation = app
        .manifest
        .operations
        .get(&resolver.operation)
        .ok_or_else(|| {
            ObjectError::Resolution("resolver operation is no longer declared".into())
        })?;
    let id_arg = operation
        .args
        .iter()
        .find(|arg| arg.name == resolver.id_arg)
        .ok_or_else(|| ObjectError::Resolution("resolver ID argument is missing".into()))?;
    if id_arg.kind == ArgKind::Path && !Path::new(&object.object_id).is_absolute() {
        return Err(ObjectError::Resolution(
            "path-backed object IDs must be absolute; references cannot depend on a caller's working directory".into(),
        ));
    }

    let mut args = Vec::new();
    if let Some(revision) = &object.revision {
        let revision_arg = resolver.revision_arg.as_ref().ok_or_else(|| {
            ObjectError::Resolution(
                "this object type does not support revision-pinned resolution".into(),
            )
        })?;
        args.push(format!("--{revision_arg}={revision}"));
    }
    match id_arg.effective_binding() {
        ArgBinding::Flag => args.push(format!("--{}={}", resolver.id_arg, object.object_id)),
        ArgBinding::Positional => {
            args.push("--".to_string());
            args.push(object.object_id.clone());
        }
    }
    Ok(ObjectDescription {
        object: object.clone(),
        reference: format_reference(object)?,
        app_name: app.manifest.name.current().to_string(),
        app_version: app.manifest.version.clone(),
        object_label: declaration.label.current().to_string(),
        object_summary: declaration.summary.current().to_string(),
        invocation: ObjectInvocation {
            app_id: object.app_id.clone(),
            operation: resolver.operation.clone(),
            args,
        },
        provenance: app.provenance_facts(),
    })
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/objects/mod.rs"
    ));
}
