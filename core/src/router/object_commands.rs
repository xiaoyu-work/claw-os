//! Object CLI presentation; resolution reuses the ordinary App execution path.

use serde_json::{json, Value};

use crate::objects::{self, InstalledObjectCatalog, ObjectCatalog, ObjectRef};

pub(super) fn run(command: &str, args: &[String]) -> Result<Value, String> {
    run_with(
        &InstalledObjectCatalog::new(super::apps_dir()),
        command,
        args,
        |app, invocation| {
            super::app_commands::require_runnable(app)?;
            super::run_app_command(
                &invocation.app_id,
                &invocation.operation,
                &invocation.args,
                app,
                None,
            )
        },
    )
}

fn run_with(
    catalog: &InstalledObjectCatalog,
    command: &str,
    args: &[String],
    execute: impl FnOnce(
        &crate::apps::App,
        &objects::ObjectInvocation,
    ) -> Result<Option<String>, String>,
) -> Result<Value, String> {
    match command {
        "catalog" if args.len() <= 1 => serde_json::to_value(
            catalog
                .list(args.first().map(String::as_str))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        "reference" => {
            let object = reference_args(args)?;
            let reference = objects::format_reference(&object).map_err(|error| error.to_string())?;
            Ok(json!({"object": object, "reference": reference}))
        }
        "describe" | "resolve" if args.len() == 1 => {
            let object = objects::parse_reference(&args[0]).map_err(|error| error.to_string())?;
            let (app, description) = catalog.prepare(&object).map_err(|error| error.to_string())?;
            if command == "describe" {
                return serde_json::to_value(description).map_err(|error| error.to_string());
            }
            let result = execute(&app, &description.invocation)?
                .ok_or_else(|| "App object resolver returned no JSON result".to_string())?;
            let value: Value = serde_json::from_str(&result)
                .map_err(|error| format!("App object resolver returned invalid JSON: {error}"))?;
            if value.get("error").is_some() {
                return Err(result);
            }
            Ok(value)
        }
        _ => Err(
            "usage: cos object catalog [APP] | reference APP TYPE ID [--revision REV] | describe URI | resolve URI"
                .to_string(),
        ),
    }
}

fn reference_args(args: &[String]) -> Result<ObjectRef, String> {
    let mut positional = Vec::new();
    let mut revision = None;
    let mut index = 0;
    let mut options = true;
    while index < args.len() {
        let value = &args[index];
        if options && value == "--" {
            options = false;
        } else if options && (value == "--revision" || value.starts_with("--revision=")) {
            if revision.is_some() {
                return Err("--revision may only be specified once".into());
            }
            revision = Some(match value.strip_prefix("--revision=") {
                Some(revision) => revision.to_string(),
                None => {
                    index += 1;
                    args.get(index)
                        .ok_or("--revision requires a value")?
                        .clone()
                }
            });
        } else if options && value.starts_with("--") {
            return Err(format!(
                "unknown object-reference option: {value}; use -- before literal IDs"
            ));
        } else {
            positional.push(value.clone());
        }
        index += 1;
    }
    let [app_id, object_type, object_id]: [String; 3] = positional
        .try_into()
        .map_err(|_| "reference requires exactly APP TYPE ID".to_string())?;
    let object = ObjectRef {
        app_id,
        object_type,
        object_id,
        revision,
    };
    objects::validate(&object).map_err(|error| error.to_string())?;
    Ok(object)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/router/object_commands.rs"
    ));
}
