//! Declarative bindings from App-owned object identities to existing operations.

use serde::{Deserialize, Serialize};

use super::{is_valid_id, Arg, ArgBinding, ArgKind, Manifest, ManifestError};
use crate::i18n::LocalizedText;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectType {
    pub label: LocalizedText,
    #[serde(default)]
    pub summary: LocalizedText,
    pub resolve: ObjectResolver,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectResolver {
    pub operation: String,
    pub id_arg: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision_arg: Option<String>,
}

pub(super) fn validate(manifest: &Manifest) -> Result<(), ManifestError> {
    if manifest.objects.is_empty() {
        return Ok(());
    }
    if manifest.id.len() > 128 || manifest.objects.len() > 64 {
        return Err(invalid(
            "objects",
            "object declarations require an App ID of at most 128 bytes and at most 64 types",
        ));
    }
    for (name, object) in &manifest.objects {
        if !is_valid_id(name) || name.len() > 64 {
            return Err(invalid(
                name,
                "type name must match [a-z][a-z0-9_-]* and fit 64 bytes",
            ));
        }
        validate_text(name, "label", &object.label, 240, false)?;
        if !object.summary.is_empty() {
            validate_text(name, "summary", &object.summary, 4096, true)?;
        }
        let resolver = &object.resolve;
        let operation = manifest
            .operations
            .get(&resolver.operation)
            .ok_or_else(|| invalid(name, "resolver operation is not declared"))?;
        if operation.stdin {
            return Err(invalid(
                name,
                "resolver operations cannot require caller stdin",
            ));
        }
        if !is_valid_id(&resolver.id_arg)
            || resolver
                .revision_arg
                .as_deref()
                .is_some_and(|arg| !is_valid_id(arg))
            || resolver.revision_arg.as_deref() == Some(resolver.id_arg.as_str())
        {
            return Err(invalid(
                name,
                "resolver argument names must be valid and distinct",
            ));
        }
        let id_arg = operation
            .args
            .iter()
            .find(|arg| arg.name == resolver.id_arg)
            .ok_or_else(|| invalid(name, "resolver ID argument is not declared"))?;
        if !id_arg.required || !string_argument(id_arg) {
            return Err(invalid(
                name,
                "resolver ID must be a required, singular string argument",
            ));
        }
        if let Some(revision_name) = &resolver.revision_arg {
            let revision = operation
                .args
                .iter()
                .find(|arg| arg.name == *revision_name)
                .ok_or_else(|| invalid(name, "resolver revision argument is not declared"))?;
            if revision.required
                || !string_argument(revision)
                || !matches!(revision.kind, ArgKind::Name | ArgKind::Text)
                || revision.effective_binding() != ArgBinding::Flag
            {
                return Err(invalid(
                    name,
                    "revision must be an optional singular name/text flag",
                ));
            }
        }
        for arg in &operation.args {
            if arg.trusted_resolver.is_some()
                || arg.default_from.is_some()
                || arg.required_when.is_some()
            {
                return Err(invalid(
                    name,
                    "resolver arguments cannot depend on trusted, derived, or conditional inputs",
                ));
            }
            if arg.name != resolver.id_arg
                && (arg.required || arg.effective_binding() == ArgBinding::Positional)
            {
                return Err(invalid(
                    name,
                    "other resolver arguments must be optional flags",
                ));
            }
        }
    }
    Ok(())
}

fn string_argument(arg: &Arg) -> bool {
    !arg.repeatable
        && matches!(
            arg.kind,
            ArgKind::Path | ArgKind::Host | ArgKind::Name | ArgKind::Text
        )
}

fn validate_text(
    object_type: &str,
    field: &str,
    value: &LocalizedText,
    max_bytes: usize,
    multiline: bool,
) -> Result<(), ManifestError> {
    value
        .validate()
        .map_err(|error| invalid(object_type, &format!("{field}: {error}")))?;
    for (_, text) in value.iter() {
        if text.len() > max_bytes
            || text
                .chars()
                .any(|ch| ch.is_control() && !(multiline && matches!(ch, '\n' | '\r' | '\t')))
        {
            return Err(invalid(
                object_type,
                &format!("{field} is too long or contains controls"),
            ));
        }
    }
    Ok(())
}

fn invalid(object_type: &str, detail: &str) -> ManifestError {
    ManifestError::ObjectInvalid {
        object_type: object_type.to_string(),
        detail: detail.to_string(),
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/caps/manifest/objects.rs"
    ));
}
