//! Non-executing operation previews. Manifest claims are not observed effects.

use serde::Serialize;

use crate::apps::App;
use crate::caps::manifest::{ArgKind, EffectKind, EffectRecovery, Manifest};

pub mod cli;
pub mod receipts;
pub mod reporting;

pub const MAX_ARGUMENTS: usize = 64;
pub const MAX_ARGUMENT_BYTES: usize = 8192;

#[derive(Debug, Clone, Serialize)]
pub struct PlannedEffect {
    pub kind: EffectKind,
    pub label: String,
    pub recovery: EffectRecovery,
    pub target_arg: Option<String>,
    pub target_kind: Option<&'static str>,
    pub requested_targets: Vec<String>,
    pub target_state: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct OperationPreview {
    pub schema: u32,
    pub app_id: String,
    pub app_name: String,
    pub app_version: String,
    pub package_digest: String,
    pub operation: String,
    pub operation_label: String,
    pub effects_declared: bool,
    pub effects: Vec<PlannedEffect>,
    pub unresolved_arguments: Vec<String>,
    pub authorization_checked: bool,
    pub executed: bool,
    pub effects_confirmed: bool,
    pub notes: Vec<String>,
}

pub fn preview(
    app: &App,
    operation_name: &str,
    args: &[String],
) -> Result<OperationPreview, String> {
    validate_arguments(args)?;
    let package = app.require_verified()?;
    package
        .assert_current(&crate::provenance::trust_store())
        .map_err(|error| error.to_string())?;
    let manifest =
        Manifest::from_json(&package.manifest_text().map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    let (declarations, declared_effects, label, stdin) =
        if let Some(operation) = manifest.operations.get(operation_name) {
            (
                &operation.args,
                &operation.effects,
                &operation.label,
                operation.stdin,
            )
        } else {
            let tool = manifest.mcp_tool_for_command(operation_name)?;
            (&tool.args, &tool.effects, &tool.summary, false)
        };
    let supplied = crate::caps::args::bind_supplied_cli_args(declarations, args)?;
    // A missing path context keeps argument binding free of filesystem I/O.
    let (values, _) = crate::caps::manifest::resolve_effective_args(declarations, &supplied, None)?;
    let unresolved_arguments: Vec<_> = declarations
        .iter()
        .filter(|arg| !values.contains_key(&arg.name))
        .map(|arg| arg.name.clone())
        .collect();
    let mut effects = Vec::new();
    for effect in declared_effects {
        let mut planned = PlannedEffect {
            kind: effect.kind,
            label: effect.label.current().to_string(),
            recovery: effect.recovery,
            target_arg: effect.target_arg.clone(),
            target_kind: None,
            requested_targets: Vec::new(),
            target_state: "unspecified",
        };
        if let Some(name) = &effect.target_arg {
            let arg = declarations
                .iter()
                .find(|arg| arg.name == *name)
                .ok_or_else(|| "validated effect target disappeared".to_string())?;
            planned.target_kind = Some(match arg.kind {
                ArgKind::Path => "path",
                ArgKind::Host => "host",
                ArgKind::Name => "name",
                _ => return Err("effect target is not a resource argument".to_string()),
            });
            planned.target_state = "unresolved";
            if !unresolved_arguments.contains(name) {
                if let Some(value) = values.get(name) {
                    planned.requested_targets = if arg.repeatable {
                        value
                            .as_array()
                            .ok_or_else(|| {
                                "effect target must contain resource values".to_string()
                            })?
                            .iter()
                            .map(|value| {
                                value.as_str().map(str::to_string).ok_or_else(|| {
                                    "effect target must be a resource string".to_string()
                                })
                            })
                            .collect::<Result<_, _>>()?
                    } else {
                        vec![value
                            .as_str()
                            .ok_or_else(|| "effect target must be a resource string".to_string())?
                            .to_string()]
                    };
                    planned.target_state = "requested";
                }
            }
        }
        effects.push(planned);
    }
    let mut notes = vec![
        "Effects and recovery modes are App declarations, not OS-confirmed behavior.".into(),
        "Targets are requested values; final path resolution and all permissions are checked only during normal execution.".into(),
        "No App code was executed, and no object data or credentials were read; this is not a file diff or an execution grant.".into(),
    ];
    if effects.is_empty() {
        notes.push("This operation has no effect declarations. Its effects and recovery are unknown, not implicitly read-only.".into());
    }
    if !unresolved_arguments.is_empty() {
        notes.push("Optional arguments without supplied values or literal defaults remain unresolved. This preview does not choose a provider or inspect credentials.".into());
    }
    if stdin {
        notes.push("This operation supports explicit caller stdin; stdin contents are not included in this metadata preview.".into());
    }
    Ok(OperationPreview {
        schema: 1,
        app_id: manifest.id.clone(),
        app_name: manifest.name.current().to_string(),
        app_version: manifest.version.clone(),
        package_digest: package.content_digest().to_string(),
        operation: operation_name.to_string(),
        operation_label: label.current().to_string(),
        effects_declared: !effects.is_empty(),
        effects,
        unresolved_arguments,
        authorization_checked: false,
        executed: false,
        effects_confirmed: false,
        notes,
    })
}

pub fn validate_arguments(args: &[String]) -> Result<(), String> {
    if args.len() > MAX_ARGUMENTS {
        return Err(format!(
            "operation preview accepts at most {MAX_ARGUMENTS} arguments"
        ));
    }
    if args.iter().any(|arg| {
        arg.len() > MAX_ARGUMENT_BYTES
            || arg
                .chars()
                .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t'))
    }) {
        return Err(
            "operation preview argument is oversized or contains unsupported controls".into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/operations/mod.rs"
    ));
}
